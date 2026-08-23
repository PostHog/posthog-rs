use std::collections::{HashMap, HashSet};
#[cfg(feature = "error-tracking")]
use std::error::Error as StdError;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use reqwest::{header::CONTENT_TYPE, header::USER_AGENT, Client as HttpClient};
use serde_json::json;
#[cfg(feature = "error-tracking")]
use tracing::trace;
use tracing::{debug, instrument, warn};

use super::get_default_user_agent;
use crate::endpoints::Endpoint;
#[cfg(feature = "error-tracking")]
use crate::error_tracking::{build_exception_event, CaptureExceptionOptions};
use crate::feature_flag_evaluations::{
    EvaluateFlagsOptions, EvaluatedFlagRecord, FeatureFlagEvaluations, FeatureFlagEvaluationsHost,
};
use crate::feature_flags::{match_feature_flag, FeatureFlag, FeatureFlagsResponse, FlagValue};
use crate::local_evaluation::{AsyncFlagPoller, FlagCache, LocalEvaluationConfig, LocalEvaluator};
use crate::{Error, Event};

use super::common::{
    extract_flag_details, local_record, remote_record_from_detail, report_flags_error,
    DetailedFlagsResponse, FlagEventHost,
};
use super::transport::{Completion, Control, TransportHandle};
use super::{CaptureSummary, ClientOptions};

/// A [`Client`] facilitates interactions with the PostHog API over HTTP.
pub struct Client {
    options: ClientOptions,
    client: HttpClient,
    local_evaluator: Option<LocalEvaluator>,
    _flag_poller: Option<AsyncFlagPoller>,
    flag_event_host: OnceLock<Arc<dyn FeatureFlagEvaluationsHost>>,
    /// Background event transport. `None` for disabled clients.
    transport: Option<Arc<TransportHandle>>,
}

/// Construct an async PostHog client from an API key or [`ClientOptions`].
///
/// # Parameters
///
/// - `options`: Either a project API key (for example `"phc_..."`) or a
///   configured [`ClientOptions`] value.
///
/// # Returns
///
/// A [`Client`] that performs capture and feature flag requests asynchronously.
///
/// # Remarks
///
/// This constructor is available with the default `async-client` feature and
/// must be awaited. Passing a blank API key creates a disabled client.
pub async fn client<C: Into<ClientOptions>>(options: C) -> Client {
    let options = options.into().sanitize();
    let client = HttpClient::builder()
        .timeout(Duration::from_secs(options.request_timeout_seconds))
        .build()
        .unwrap(); // Unwrap here is as safe as `HttpClient::new`

    let (local_evaluator, flag_poller) =
        if options.enable_local_evaluation && !options.is_disabled() {
            if let Some(ref secret_key) = options.secret_key {
                let cache = FlagCache::new();

                let config = LocalEvaluationConfig {
                    secret_key: secret_key.clone(),
                    project_api_key: options.api_key.clone(),
                    api_host: options.endpoints().api_host(),
                    poll_interval: Duration::from_secs(options.poll_interval_seconds),
                    request_timeout: Duration::from_secs(options.request_timeout_seconds),
                };

                let mut poller = AsyncFlagPoller::new(config, cache.clone());
                poller.set_on_error(options.on_error.clone());
                poller.start().await;

                (Some(LocalEvaluator::new(cache)), Some(poller))
            } else {
                let warning = if options.local_evaluation_only {
                    "Missing secret_key; local-only evaluation will return empty results"
                } else {
                    "Local evaluation enabled without secret_key; using remote API fallback"
                };
                warn!("{warning}");
                (None, None)
            }
        } else {
            (None, None)
        };

    let transport = if options.is_disabled() {
        None
    } else {
        Some(Arc::new(TransportHandle::spawn(options.clone())))
    };

    Client {
        options,
        client,
        local_evaluator,
        _flag_poller: flag_poller,
        flag_event_host: OnceLock::new(),
        transport,
    }
}

impl Client {
    /// Capture the provided event, sending it to PostHog.
    ///
    /// # Parameters
    ///
    /// - `event`: Event name, distinct ID, properties, timestamp, groups, and
    ///   optional feature flag state to send.
    ///
    /// # Remarks
    ///
    /// Fire-and-forget: the event is handed to the background worker, which
    /// batches, sends, and retries it. Returns once the event is queued — not
    /// once it is delivered, and delivery failures are not surfaced to the
    /// caller. Disabled clients and a full queue drop the event (the latter
    /// with a single warning).
    #[instrument(skip(self, event), level = "debug")]
    pub fn capture(&self, event: Event) {
        if let Some(transport) = &self.transport {
            transport.enqueue(event);
        }
    }

    /// Merge two distinct IDs onto the same person by sending a `$create_alias`
    /// event.
    ///
    /// See <https://posthog.com/docs/product-analytics/identify#alias-assigning-multiple-distinct-ids-to-the-same-user>.
    ///
    /// # Parameters
    ///
    /// - `previous_id`: ID already known to PostHog, such as an anonymous ID.
    /// - `distinct_id`: ID it should be merged into, such as a logged-in user ID.
    ///
    /// # Remarks
    ///
    /// Fire-and-forget, like [`Client::capture`]. A blank ID on either side
    /// cannot describe a merge, so the event is dropped with a warning rather
    /// than sent.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example() {
    /// let client = posthog_rs::client("phc_project_api_key").await;
    ///
    /// // The visitor browsed anonymously, then logged in.
    /// client.alias("anon-abc123", "user-42");
    /// # }
    /// ```
    pub fn alias<P: Into<String>, D: Into<String>>(&self, previous_id: P, distinct_id: D) {
        if let Some(event) = Event::alias(previous_id.into(), distinct_id.into()) {
            self.capture(event);
        }
    }

    /// Flush queued events, returning once the worker has attempted delivery of
    /// everything queued before this call. Transient failures are kept for retry
    /// (the call still returns without error). A no-op for disabled clients.
    pub async fn flush(&self) {
        let Some(transport) = &self.transport else {
            return;
        };
        if transport.is_closed() {
            return;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        if transport.send_control(Control::Flush(Completion::Async(tx))) {
            let _ = rx.await;
        }
    }

    /// Whether the client is disabled (no transport; capture is a no-op). Used
    /// by the panic hook to skip building an event it could never send.
    #[cfg(feature = "error-tracking")]
    pub(crate) fn is_disabled(&self) -> bool {
        self.options.is_disabled()
    }

    /// The client's Error Tracking options, used by the panic hook to build
    /// panic exception events with the client's configured policy.
    #[cfg(feature = "error-tracking")]
    pub(crate) fn error_tracking_options(&self) -> &crate::error_tracking::ErrorTrackingOptions {
        self.options.error_tracking()
    }

    /// Unbounded synchronous flush: blocks until the worker has attempted
    /// delivery of everything queued. Test-only; the panic hook uses
    /// `flush_blocking_timeout`.
    #[cfg(test)]
    pub(crate) fn flush_blocking(&self) {
        if let Some(transport) = &self.transport {
            transport.flush_blocking();
        }
    }

    /// Synchronous, time-bounded flush for the panic hook: blocks (no runtime
    /// needed) up to `timeout` for the worker to attempt delivery, then returns.
    /// A no-op for disabled clients.
    #[cfg(feature = "error-tracking")]
    pub(crate) fn flush_blocking_timeout(&self, timeout: Duration) {
        if let Some(transport) = &self.transport {
            transport.flush_blocking_timeout(timeout);
        }
    }

    /// True when the calling thread is this client's transport worker thread —
    /// the panic hook skips capturing there.
    #[cfg(feature = "error-tracking")]
    pub(crate) fn on_transport_worker(&self) -> bool {
        self.transport
            .as_ref()
            .is_some_and(|t| t.on_worker_thread())
    }

    /// Enqueue a panic `$exception` without the tracing `capture` performs:
    /// `capture` is `#[instrument]` and its enqueue warns once on a full queue,
    /// both of which run subscriber code — unsafe on the already-panicking
    /// thread. The send still happens on the worker thread.
    #[cfg(feature = "error-tracking")]
    pub(crate) fn enqueue_panic_event(&self, event: Event) {
        if let Some(transport) = &self.transport {
            transport.enqueue_panic(event);
        }
    }

    /// Flush, stop the background worker, and join it. Idempotent: subsequent
    /// calls are no-ops. After shutdown, `capture` drops events. A no-op for
    /// disabled clients.
    pub async fn shutdown(&self) {
        let Some(transport) = &self.transport else {
            return;
        };
        if transport.begin_close() {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if transport.send_control(Control::Shutdown(Completion::Async(tx))) {
                let _ = rx.await;
            }
        }
        // Always join — even if this caller lost the `begin_close` race or its
        // shutdown wait was cancelled — so every shutdown/drop path waits for the
        // worker and the flush stays durable. The winner has already sent the
        // Shutdown (synchronously, before any await), so the worker will exit.
        transport.join();
    }

    /// Capture a Rust error personlessly, sending it to PostHog Error Tracking.
    ///
    /// The error's type, message, and full `source()` chain are sent as
    /// `$exception_list`, with a stacktrace of the capture site attached to
    /// the first entry (see `ErrorTrackingOptions::capture_stacktrace`).
    ///
    /// Accepts any [`std::error::Error`], including `&dyn Error`. A
    /// `Box<dyn Error>` does not implement `Error` itself, so pass the
    /// dereferenced trait object: `capture_exception(&*boxed)`.
    ///
    /// To associate the exception with a person or attach custom properties,
    /// groups, a fingerprint, or a severity level, use
    /// [`Client::capture_exception_with`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), posthog_rs::Error> {
    /// let client = posthog_rs::client("phc_project_api_key").await;
    /// let error = std::io::Error::other("checkout failed");
    ///
    /// client.capture_exception(&error).await?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "error-tracking")]
    pub async fn capture_exception<E>(&self, error: &E) -> Result<(), Error>
    where
        E: StdError + ?Sized,
    {
        self.capture_exception_with(error, CaptureExceptionOptions::default())
            .await
    }

    /// Capture a Rust error with optional context, sending it to PostHog
    /// Error Tracking.
    ///
    /// Set [`CaptureExceptionOptions::distinct_id`] to associate the exception
    /// with a person; without it the exception is captured personlessly.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), posthog_rs::Error> {
    /// use posthog_rs::CaptureExceptionOptions;
    ///
    /// let client = posthog_rs::client("phc_project_api_key").await;
    /// let error = std::io::Error::other("checkout failed");
    ///
    /// client
    ///     .capture_exception_with(
    ///         &error,
    ///         CaptureExceptionOptions::new()
    ///             .distinct_id("user-123")
    ///             .property("route", "/checkout")?,
    ///     )
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "error-tracking")]
    pub async fn capture_exception_with<E>(
        &self,
        error: &E,
        options: CaptureExceptionOptions,
    ) -> Result<(), Error>
    where
        E: StdError + ?Sized,
    {
        if self.options.is_disabled() {
            trace!("Client is disabled, skipping exception capture");
            return Ok(());
        }

        self.capture(build_exception_event(
            error,
            options,
            self.options.error_tracking(),
        )?);
        Ok(())
    }

    /// Capture a collection of events with a single request.
    ///
    /// Events are sent to the `/batch/` endpoint.
    ///
    /// # Parameters
    ///
    /// - `events`: Events to send in the batch.
    /// - `historical_migration`: Set to `true` to route events to the
    ///   historical ingestion topic, bypassing the main pipeline.
    ///
    /// # Remarks
    ///
    /// Fire-and-forget, like [`Client::capture`]. The batch is enqueued per event
    /// rather than atomically, so if the bounded queue fills partway through, the
    /// remaining events are dropped (with the usual single full-queue warning).
    pub fn capture_batch(&self, events: Vec<Event>, historical_migration: bool) {
        if let Some(transport) = &self.transport {
            if historical_migration {
                transport.enqueue_historical(events);
            } else {
                for event in events {
                    transport.enqueue(event);
                }
            }
        }
    }

    // ----- Immediate (inline) capture -------------------------------------
    //
    // `capture`/`capture_batch` above are fire-and-forget: they enqueue onto the
    // background worker and never report the outcome. The `*_immediate` variants
    // send inline and await a terminal result, for the rare caller that must know
    // a batch persisted before advancing its own durable state (e.g. committing
    // an upstream offset). They bypass the worker queue and do NOT fire `on_error`
    // hooks — the returned `Result`/`CaptureSummary` is the delivery signal.

    /// Capture a single event and await confirmation that the request completed.
    ///
    /// The immediate-delivery counterpart to [`Client::capture`]. This is a
    /// convenience wrapper over [`Client::capture_batch_immediate`] with a
    /// one-event batch; see it for full semantics.
    #[must_use = "the delivery outcome should be inspected"]
    pub async fn capture_immediate(&self, event: Event) -> Result<CaptureSummary, Error> {
        self.capture_batch_immediate(vec![event], false).await
    }

    /// Capture a batch of events and await confirmation that the request
    /// completed, returning a [`CaptureSummary`] describing the outcome.
    ///
    /// The immediate-delivery counterpart to [`Client::capture_batch`]. Prefer
    /// the fire-and-forget [`Client::capture`]/[`Client::capture_batch`] for
    /// normal analytics; reach for this only when the caller must know the batch
    /// persisted before advancing its own durable state.
    ///
    /// # Parameters
    ///
    /// - `events`: Events to send in a single request.
    /// - `historical_migration`: Route events to the historical ingestion topic.
    ///
    /// # Behavior
    ///
    /// Sends inline (bypassing the background worker) and retries transient
    /// failures per the client's retry configuration. A returned `Ok` can still
    /// report unpersisted events — inspect
    /// [`CaptureSummary::all_persisted`]. Does NOT fire `on_error` hooks: the
    /// returned `Result` is the delivery signal. Disabled clients and an empty
    /// (or fully `before_send`-filtered) batch return a default `CaptureSummary`.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when the request is rejected with a terminal status or
    /// the retry budget is exhausted without a successful response.
    #[must_use = "the delivery outcome should be inspected"]
    #[instrument(
        skip(self, events),
        fields(event_count = events.len(), historical_migration),
        level = "debug"
    )]
    pub async fn capture_batch_immediate(
        &self,
        events: Vec<Event>,
        historical_migration: bool,
    ) -> Result<CaptureSummary, Error> {
        if self.options.is_disabled() || events.is_empty() {
            return Ok(CaptureSummary::default());
        }
        self.send_immediate(events, historical_migration).await
    }

    /// Inline V1 capture: prepare once via the shared sans-IO helpers, then loop
    /// send/classify, awaiting `tokio::time::sleep` between retries. The setup and
    /// classification are shared with the blocking client; only this loop differs.
    async fn send_immediate(
        &self,
        events: Vec<Event>,
        historical_migration: bool,
    ) -> Result<CaptureSummary, Error> {
        use super::capture::{self, Step};

        let Some(mut prep) =
            capture::prepare_immediate(&self.options, events, historical_migration)
        else {
            return Ok(CaptureSummary::default());
        };
        let mut final_results = HashMap::new();
        let mut attempt: u32 = 1;

        loop {
            let (headers, body) = capture::build_attempt_parts(
                &self.options,
                &prep.request_id,
                attempt,
                &prep.created_at,
                prep.historical_migration,
                &prep.pending,
            )?;

            let step = match self
                .client
                .post(&prep.url)
                .headers(headers)
                .body(body)
                .send()
                .await
            {
                Err(e) => capture::after_transport_error(
                    &self.options,
                    &prep.request_id,
                    attempt,
                    e.to_string(),
                ),
                Ok(response) => {
                    let status = response.status().as_u16();
                    let retry_after = capture::parse_retry_after(response.headers());
                    let text = response
                        .text()
                        .await
                        .unwrap_or_else(|_| "Unknown error".to_string());
                    capture::after_response(
                        &self.options,
                        &prep.request_id,
                        attempt,
                        status,
                        retry_after,
                        &text,
                        &mut prep.pending,
                        &mut final_results,
                    )
                }
            };

            match step {
                Step::Done => {
                    return Ok(CaptureSummary::from_results(prep.submitted, final_results))
                }
                Step::Fail(e) => return Err(e),
                Step::Backoff(delay) => {
                    attempt += 1;
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    /// Number of events accepted but not yet delivered or dropped — those still
    /// in the channel, in the worker's current batch, or held for retry. Returns
    /// 0 for a disabled client.
    ///
    /// Gated behind the `test-harness` feature: it exposes internal queue depth
    /// for the SDK compliance harness and is not part of the normal public API.
    #[cfg(feature = "test-harness")]
    pub fn pending_events(&self) -> usize {
        self.transport.as_ref().map_or(0, |t| t.pending())
    }

    /// Evaluate a supplied feature flag definition locally.
    ///
    /// `groups` and `group_properties` are only consulted when the flag (or one
    /// of its conditions) targets a group; pass empty maps for person flags.
    ///
    /// # Parameters
    ///
    /// - `flag`: Feature flag definition to evaluate.
    /// - `distinct_id`: User distinct ID.
    /// - `person_properties`: Person properties available to release
    ///   conditions.
    /// - `groups`: Group keys for group-targeted flags.
    /// - `group_properties`: Group properties for group-targeted release
    ///   conditions.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InconclusiveMatch`] when the flag cannot be evaluated
    /// locally with the supplied context.
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_feature_flag_locally(
        &self,
        flag: &FeatureFlag,
        distinct_id: &str,
        person_properties: &HashMap<String, serde_json::Value>,
        groups: &HashMap<String, String>,
        group_properties: &HashMap<String, HashMap<String, serde_json::Value>>,
    ) -> Result<FlagValue, Error> {
        let group_type_mapping = self
            .local_evaluator
            .as_ref()
            .map(|ev| ev.cache().get_group_type_mapping())
            .unwrap_or_default();
        match_feature_flag(
            flag,
            distinct_id,
            person_properties,
            groups,
            group_properties,
            &group_type_mapping,
        )
        .map_err(|e| Error::InconclusiveMatch(e.message))
    }

    /// Evaluate feature flags for `distinct_id`, returning a
    /// [`FeatureFlagEvaluations`] snapshot.
    ///
    /// Each `is_enabled` / `get_flag` call on the returned snapshot fires a
    /// dedup-aware `$feature_flag_called` event with full metadata, and the
    /// snapshot can be passed to [`Event::with_flags`] so a downstream
    /// [`Client::capture`] inherits `$feature/<key>` and `$active_feature_flags`
    /// without an extra `/flags` request.
    ///
    /// # Parameters
    ///
    /// - `distinct_id`: User distinct ID. Empty values return an empty snapshot.
    /// - `options`: Optional groups, properties, GeoIP override, local-only
    ///   mode, and flag-key filtering.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Connection`] or [`Error::Serialization`] when remote
    /// evaluation is required and the `/flags` request fails before any local
    /// results are available.
    ///
    /// [`Event::with_flags`]: crate::Event::with_flags
    pub async fn evaluate_flags<S: Into<String>>(
        &self,
        distinct_id: S,
        options: EvaluateFlagsOptions,
    ) -> Result<FeatureFlagEvaluations, Error> {
        let distinct_id: String = distinct_id.into();
        let host = self.flag_event_host();

        if distinct_id.is_empty() || self.options.is_disabled() {
            return Ok(FeatureFlagEvaluations::empty(host));
        }

        let mut options = options;
        options.groups.get_or_insert_with(HashMap::new);
        options.group_properties.get_or_insert_with(HashMap::new);

        let mut records: HashMap<String, EvaluatedFlagRecord> = HashMap::new();
        let mut locally_evaluated_keys: HashSet<String> = HashSet::new();

        if let Some(evaluator) = &self.local_evaluator {
            let mut person_props_owned = options.person_properties.clone().unwrap_or_default();
            person_props_owned
                .entry("distinct_id".to_string())
                .or_insert_with(|| json!(distinct_id.clone()));
            let groups_owned = options.groups.clone().unwrap_or_default();
            let group_props_owned = options.group_properties.clone().unwrap_or_default();
            let local_results = evaluator.evaluate_all_flags_with_details(
                &distinct_id,
                &person_props_owned,
                &groups_owned,
                &group_props_owned,
            );
            // Pin the gate from the poller's current definitions snapshot at the
            // point local evaluation succeeded, so it travels with these records
            // rather than being re-read from shared state at event time.
            let local_minimal_gate = evaluator.cache().minimal_flag_called_events();
            for (key, result) in local_results {
                if let Some(filter) = &options.flag_keys {
                    if !filter.iter().any(|k| k == &key) {
                        continue;
                    }
                }
                if let Ok(value) = result.result {
                    records.insert(
                        key.clone(),
                        local_record(
                            value,
                            result.payload,
                            result.has_experiment,
                            local_minimal_gate,
                        ),
                    );
                    locally_evaluated_keys.insert(key);
                }
            }
        }

        let mut request_id: Option<String> = None;
        let mut errors_while_computing = false;
        let mut quota_limited = false;

        // Skip the remote round-trip when local evaluation has already covered
        // every requested flag. Without `flag_keys` we have to assume the caller
        // wants every flag the project has and still hit `/flags` to discover
        // any not loaded by the poller.
        let local_covers_request = options
            .flag_keys
            .as_ref()
            .is_some_and(|keys| keys.iter().all(|k| locally_evaluated_keys.contains(k)));

        if !options.only_evaluate_locally
            && !self.options.local_evaluation_only
            && !local_covers_request
        {
            // Don't lose successful local evaluations if `/flags` fails — degrade
            // to a snapshot built from the local results we already have. The
            // alternative (returning Err) wastes useful data and surprises
            // callers who would otherwise get partial coverage.
            match self.fetch_flag_details(&distinct_id, &options).await {
                Ok(response) => {
                    request_id = response.request_id;
                    errors_while_computing = response.errors_while_computing_flags;
                    quota_limited = response.quota_limited;
                    // The remote response is the source of these flags' values,
                    // so it is also the source of their minimization gate.
                    let remote_minimal_gate = response.minimal_flag_called_events;
                    for (key, detail) in response.flags {
                        if locally_evaluated_keys.contains(&key) {
                            continue;
                        }
                        records.insert(key, remote_record_from_detail(detail, remote_minimal_gate));
                    }
                }
                Err(e) => {
                    if records.is_empty() {
                        return Err(e);
                    }
                    debug!(
                        error = e.to_string(),
                        local_count = records.len(),
                        "/flags fetch failed; returning snapshot from local results only"
                    );
                    errors_while_computing = true;
                }
            }
        }

        Ok(FeatureFlagEvaluations::new(
            host,
            distinct_id,
            records,
            options.groups.unwrap_or_default(),
            options.disable_geoip,
            request_id,
            None,
            errors_while_computing,
            quota_limited,
        ))
    }

    fn flag_event_host(&self) -> Arc<dyn FeatureFlagEvaluationsHost> {
        self.flag_event_host
            .get_or_init(|| {
                Arc::new(FlagEventHost::new(
                    self.options.capture_defaults(),
                    self.transport.clone(),
                )) as Arc<dyn FeatureFlagEvaluationsHost>
            })
            .clone()
    }

    async fn send_feature_flags_request(
        &self,
        flags_endpoint: &str,
        payload: &serde_json::Value,
    ) -> Result<reqwest::Response, Error> {
        let mut attempt = 1;
        loop {
            let request = self
                .client
                .post(flags_endpoint)
                .header(CONTENT_TYPE, "application/json")
                .header(USER_AGENT, get_default_user_agent())
                .json(payload)
                .timeout(Duration::from_secs(
                    self.options.feature_flags_request_timeout_seconds,
                ));
            #[cfg(feature = "test-harness")]
            let request = {
                let mut request = request;
                if let Some(ref extra) = self.options.extra_capture_headers {
                    for (k, v) in extra {
                        request = request.header(k.as_str(), v.as_str());
                    }
                }
                request
            };
            let result = request.send().await;

            match result {
                Ok(response) => match super::retry::feature_flags_after_response(
                    &self.options,
                    attempt,
                    response.status().as_u16(),
                ) {
                    super::retry::FeatureFlagsResponseStep::Backoff(delay) => {
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    }
                    super::retry::FeatureFlagsResponseStep::Done => return Ok(response),
                },
                Err(e) => {
                    let err_msg = e.to_string();
                    match super::retry::feature_flags_after_transport_error(
                        &self.options,
                        attempt,
                        super::retry::is_retryable_feature_flags_error(&e),
                        err_msg,
                    ) {
                        super::retry::FeatureFlagsTransportStep::Backoff(delay) => {
                            tokio::time::sleep(delay).await;
                            attempt += 1;
                        }
                        super::retry::FeatureFlagsTransportStep::Fail(err) => {
                            report_flags_error(
                                &self.options.on_error,
                                flags_endpoint,
                                payload.get("distinct_id").and_then(|v| v.as_str()),
                                None,
                                None,
                                &err,
                            );
                            return Err(err);
                        }
                    }
                }
            }
        }
    }

    async fn fetch_flag_details(
        &self,
        distinct_id: &str,
        options: &EvaluateFlagsOptions,
    ) -> Result<DetailedFlagsResponse, Error> {
        let flags_endpoint = self.options.endpoints().build_url(Endpoint::Flags);

        let person_properties = options.person_properties.clone().unwrap_or_default();
        let groups = options.groups.clone().unwrap_or_default();
        let group_properties = options.group_properties.clone().unwrap_or_default();
        let effective_disable_geoip = options.disable_geoip.unwrap_or(self.options.disable_geoip);

        let mut payload = json!({
            "api_key": self.options.api_key,
            "distinct_id": distinct_id,
            "groups": groups,
            "person_properties": person_properties,
            "group_properties": group_properties,
            "geoip_disable": effective_disable_geoip,
        });
        if let Some(flag_keys) = &options.flag_keys {
            payload["flag_keys_to_evaluate"] = json!(flag_keys);
        }

        let response = self
            .send_feature_flags_request(&flags_endpoint, &payload)
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            let err = Error::Connection(format!("API request failed with status {status}: {text}"));
            report_flags_error(
                &self.options.on_error,
                &flags_endpoint,
                Some(distinct_id),
                Some(status.as_u16()),
                Some(&text),
                &err,
            );
            return Err(err);
        }

        let status = response.status().as_u16();
        let parsed = match response.json::<FeatureFlagsResponse>().await {
            Ok(p) => p,
            Err(e) => {
                let err =
                    Error::Serialization(format!("Failed to parse feature flags response: {e}"));
                report_flags_error(
                    &self.options.on_error,
                    &flags_endpoint,
                    Some(distinct_id),
                    Some(status),
                    None,
                    &err,
                );
                return Err(err);
            }
        };
        Ok(extract_flag_details(parsed))
    }
}

impl Drop for Client {
    /// Best-effort flush and worker join on drop. A blocking drain (the async
    /// `shutdown` can't run in a destructor), so dropping a `Client` inside an
    /// async task blocks that executor thread until the drain completes (up to
    /// `shutdown_timeout_ms` plus any in-flight request) — prefer an explicit
    /// `shutdown().await` first, which makes this a no-op. A drop from a transport
    /// callback instead queues shutdown without waiting for or joining the current
    /// worker thread.
    fn drop(&mut self) {
        if let Some(transport) = &self.transport {
            transport.close_blocking();
        }
    }
}

#[cfg(test)]
mod teardown_tests {
    use super::*;
    use std::sync::{mpsc, Mutex};

    #[tokio::test]
    async fn drop_from_worker_callback_closes_without_blocking() {
        let client_slot = Arc::new(Mutex::new(None::<Client>));
        let callback_slot = Arc::clone(&client_slot);
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let options = crate::ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .host("http://localhost:0".to_string())
            .flush_at(1usize)
            .before_send(move |_| {
                let client = callback_slot
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .take()
                    .expect("client installed before capture");
                drop(client);
                dropped_tx.send(()).unwrap();
                None
            })
            .build()
            .unwrap();
        let client = client(options).await;
        let transport = Arc::clone(client.transport.as_ref().unwrap());
        *client_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(client);

        client_slot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .unwrap()
            .capture(Event::new("drop-in-callback", "user-1"));
        dropped_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("client drop blocked its transport worker");

        // Reap the worker from an external thread and verify repeated close is safe.
        transport.close_blocking();
        transport.close_blocking();
        assert!(transport.is_closed());
    }
}

#[cfg(test)]
mod minimal_gate_tests {
    use super::*;
    use crate::client::minimal_gate_test_support::{definitions, RecordingHost};

    fn test_client(cache: FlagCache, host: Arc<dyn FeatureFlagEvaluationsHost>) -> Client {
        let options = ClientOptions::from(("phc_test", "http://localhost:0"));
        let client = Client {
            options,
            client: HttpClient::builder().build().unwrap(),
            local_evaluator: Some(LocalEvaluator::new(cache)),
            _flag_poller: None,
            flag_event_host: OnceLock::new(),
            transport: None,
        };
        client
            .flag_event_host
            .set(host)
            .unwrap_or_else(|_| panic!("host already set"));
        client
    }

    async fn evaluate(client: &Client) -> FeatureFlagEvaluations {
        client
            .evaluate_flags(
                "user-1",
                EvaluateFlagsOptions {
                    only_evaluate_locally: true,
                    ..Default::default()
                },
            )
            .await
            .expect("local evaluate_flags")
    }

    /// The minimization gate must be pinned to the definitions snapshot that
    /// produced the flag value, not re-read from the shared cache when the
    /// deferred event finally fires. Mutating the cache in the gap between
    /// evaluation and event capture must not reshape the event.
    #[tokio::test]
    async fn local_gate_pinned_at_evaluation_survives_cache_mutation_to_off() {
        let cache = FlagCache::new();
        cache.update(definitions(Some(false), true)); // gate ON at evaluation
        let host = Arc::new(RecordingHost::default());
        let client = test_client(cache.clone(), Arc::clone(&host) as _);

        let snapshot = evaluate(&client).await;
        // Poller refresh flips the gate OFF after the snapshot was produced.
        cache.update(definitions(Some(false), false));

        assert!(snapshot.is_enabled("gated"));
        let captured = host.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(
            captured[0].minimal,
            "event must reflect the gate pinned at evaluation (on), not the mutated cache (off)"
        );
    }

    #[tokio::test]
    async fn local_gate_pinned_at_evaluation_survives_cache_mutation_to_on() {
        let cache = FlagCache::new();
        cache.update(definitions(Some(false), false)); // gate OFF at evaluation
        let host = Arc::new(RecordingHost::default());
        let client = test_client(cache.clone(), Arc::clone(&host) as _);

        let snapshot = evaluate(&client).await;
        // Poller refresh flips the gate ON after the snapshot was produced.
        cache.update(definitions(Some(false), true));

        assert!(snapshot.is_enabled("gated"));
        let captured = host.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(
            !captured[0].minimal,
            "event must reflect the gate pinned at evaluation (off), not the mutated cache (on)"
        );
    }

    #[tokio::test]
    async fn local_has_experiment_is_threaded_from_definitions() {
        let cache = FlagCache::new();
        cache.update(definitions(Some(false), true));
        let host = Arc::new(RecordingHost::default());
        let client = test_client(cache, Arc::clone(&host) as _);

        assert!(evaluate(&client).await.is_enabled("gated"));
        let captured = host.captured.lock().unwrap();
        assert_eq!(
            captured[0].properties.get("$feature_flag_has_experiment"),
            Some(&serde_json::json!(false))
        );
        assert!(captured[0].minimal);
    }
}

#[cfg(test)]
mod local_payload_tests {
    use super::*;
    use crate::client::local_payload_test_support::payload_definitions;
    use crate::client::minimal_gate_test_support::RecordingHost;
    use serde_json::json;

    async fn snapshot() -> FeatureFlagEvaluations {
        let cache = FlagCache::new();
        cache.update(payload_definitions());
        let options = ClientOptions::from(("phc_test", "http://localhost:0"));
        let client = Client {
            options,
            client: HttpClient::builder().build().unwrap(),
            local_evaluator: Some(LocalEvaluator::new(cache)),
            _flag_poller: None,
            flag_event_host: OnceLock::new(),
            transport: None,
        };
        client
            .flag_event_host
            .set(Arc::new(RecordingHost::default()) as _)
            .unwrap_or_else(|_| panic!("host already set"));
        client
            .evaluate_flags(
                "user-1",
                EvaluateFlagsOptions {
                    only_evaluate_locally: true,
                    ..Default::default()
                },
            )
            .await
            .expect("local evaluate_flags")
    }

    /// Payloads live in the definitions manifest, so local evaluation must
    /// surface them the same way `/flags` does — including the JSON decoding,
    /// or the same flag would yield different payloads depending on which path
    /// evaluated it.
    #[tokio::test]
    async fn local_evaluation_surfaces_payloads_matching_the_remote_shape() {
        let snapshot = snapshot().await;

        assert_eq!(
            snapshot.get_flag_payload("json-string-payload"),
            Some(json!({"color": "blue"}))
        );
        assert_eq!(
            snapshot.get_flag_payload("parsed-payload"),
            Some(json!({"color": "blue"}))
        );
        assert_eq!(
            snapshot.get_flag_payload("quoted-string-payload"),
            Some(json!("just text"))
        );
        assert_eq!(
            snapshot.get_flag_payload("undecodable-payload"),
            Some(json!("not json"))
        );
    }

    #[tokio::test]
    async fn local_payload_is_keyed_by_the_matched_variant() {
        let snapshot = snapshot().await;

        assert_eq!(
            snapshot.get_flag("variant-payload"),
            Some(FlagValue::String("test".to_string()))
        );
        assert_eq!(
            snapshot.get_flag_payload("variant-payload"),
            Some(json!({"tier": 2}))
        );
    }

    #[tokio::test]
    async fn local_payload_is_absent_without_a_matching_payload() {
        let snapshot = snapshot().await;

        assert_eq!(snapshot.get_flag_payload("no-payload"), None);
        assert_eq!(snapshot.get_flag_payload("not-a-flag"), None);

        // A missing key also yields `None`, so pin the flag down first:
        // it was evaluated, it evaluated false, and its "true" payload
        // stayed behind.
        assert_eq!(
            snapshot.get_flag("disabled-with-payload"),
            Some(FlagValue::Boolean(false))
        );
        assert_eq!(snapshot.get_flag_payload("disabled-with-payload"), None);
    }
}
