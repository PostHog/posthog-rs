use std::collections::{HashMap, HashSet};
#[cfg(feature = "error-tracking")]
use std::error::Error as StdError;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use reqwest::{header::CONTENT_TYPE, header::USER_AGENT, Client as HttpClient};
use serde_json::json;
use tracing::{debug, instrument, trace, warn};

use super::get_default_user_agent;
use crate::endpoints::Endpoint;
#[cfg(feature = "error-tracking")]
use crate::error_tracking::{build_exception_event, CaptureExceptionOptions};
use crate::feature_flag_evaluations::{
    EvaluateFlagsOptions, EvaluatedFlagRecord, FeatureFlagEvaluations, FeatureFlagEvaluationsHost,
    FlagCalledEventParams,
};
use crate::feature_flags::{match_feature_flag, FeatureFlag, FeatureFlagsResponse, FlagValue};
use crate::local_evaluation::{AsyncFlagPoller, FlagCache, LocalEvaluationConfig, LocalEvaluator};
use crate::{Error, Event};

fn is_retryable_feature_flags_error(err: &reqwest::Error) -> bool {
    if err.is_timeout() {
        return true;
    }

    let mut source = std::error::Error::source(err);
    while let Some(error) = source {
        if let Some(io_error) = error.downcast_ref::<std::io::Error>() {
            return matches!(
                io_error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::UnexpectedEof
            );
        }
        source = std::error::Error::source(error);
    }

    !err.to_string()
        .to_lowercase()
        .contains("connection refused")
}

use super::common::{
    already_reported, build_dedup_key, extract_flag_details, flag_called_event,
    flag_event_dedup_cache, local_record, remote_record_from_detail, report_flags_error,
    DetailedFlagsResponse, FlagEventDedupCache,
};
use super::transport::{Completion, Control, TransportHandle};
use super::ClientOptions;

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

/// Implementation of [`FeatureFlagEvaluationsHost`] that emits dedup-aware
/// `$feature_flag_called` events through the same background capture transport
/// as any other event.
struct AsyncFlagEventHost {
    options: ClientOptions,
    transport: Option<Arc<TransportHandle>>,
    dedup_cache: FlagEventDedupCache,
}

impl AsyncFlagEventHost {
    fn from_options(options: &ClientOptions, transport: Option<Arc<TransportHandle>>) -> Self {
        Self {
            options: options.clone(),
            transport,
            dedup_cache: flag_event_dedup_cache(),
        }
    }

    fn enqueue(&self, event: Event) {
        if let Some(transport) = &self.transport {
            transport.enqueue(event);
        }
    }
}

impl FeatureFlagEvaluationsHost for AsyncFlagEventHost {
    fn capture_flag_called_event_if_needed(&self, params: FlagCalledEventParams) {
        let dedup_key = build_dedup_key(&params.key, params.response.as_ref(), &params.groups);
        if already_reported(&self.dedup_cache, &params.distinct_id, &dedup_key) {
            return;
        }

        if let Some(event) =
            flag_called_event(params, self.options.disable_geoip, self.options.is_server)
        {
            self.enqueue(event);
        }
    }

    fn log_warning(&self, message: &str) {
        // Surface filter-helper misuse via tracing — users can silence these
        // with their tracing-subscriber level filter (e.g. `posthog_rs=error`).
        warn!("{message}");
    }
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

    let (local_evaluator, flag_poller) = if options.enable_local_evaluation
        && !options.is_disabled()
    {
        if let Some(ref personal_key) = options.personal_api_key {
            let cache = FlagCache::new();

            let config = LocalEvaluationConfig {
                personal_api_key: personal_key.clone(),
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
            warn!("Local evaluation enabled but personal_api_key not set, falling back to API evaluation");
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

    /// Get all remote feature flags and payloads for a user.
    ///
    /// For new code, prefer [`Client::evaluate_flags`] so flag reads are
    /// deduplicated and can be attached to captured events with
    /// [`Event::with_flags`](crate::Event::with_flags).
    ///
    /// # Parameters
    ///
    /// - `distinct_id`: User distinct ID.
    /// - `groups`: Optional group keys for group-targeted flags.
    /// - `person_properties`: Optional person properties for release
    ///   conditions.
    /// - `group_properties`: Optional group properties for group-targeted
    ///   release conditions.
    ///
    /// # Returns
    ///
    /// A tuple of `(feature_flags, feature_flag_payloads)`, each keyed by flag
    /// key. Disabled clients return two empty maps.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Connection`] for request failures or non-success HTTP
    /// statuses, and [`Error::Serialization`] when the response cannot be
    /// parsed.
    #[must_use = "feature flags result should be used"]
    pub async fn get_feature_flags<S: Into<String>>(
        &self,
        distinct_id: S,
        groups: Option<HashMap<String, String>>,
        person_properties: Option<HashMap<String, serde_json::Value>>,
        group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
    ) -> Result<
        (
            HashMap<String, FlagValue>,
            HashMap<String, serde_json::Value>,
        ),
        Error,
    > {
        if self.options.is_disabled() {
            trace!("Client is disabled, skipping feature flags request");
            return Ok((HashMap::new(), HashMap::new()));
        }

        let flags_endpoint = self.options.endpoints().build_url(Endpoint::Flags);

        let mut payload = json!({
            "api_key": self.options.api_key,
            "distinct_id": distinct_id.into(),
        });

        if let Some(groups) = groups {
            payload["groups"] = json!(groups);
        }

        if let Some(person_properties) = person_properties {
            payload["person_properties"] = json!(person_properties);
        }

        if let Some(group_properties) = group_properties {
            payload["group_properties"] = json!(group_properties);
        }

        // Add geoip disable parameter if configured
        if self.options.disable_geoip {
            payload["disable_geoip"] = json!(true);
        }

        let response = self
            .send_feature_flags_request(&flags_endpoint, &payload)
            .await?;

        let distinct_id = payload.get("distinct_id").and_then(|v| v.as_str());
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
                distinct_id,
                Some(status.as_u16()),
                Some(&text),
                &err,
            );
            return Err(err);
        }

        let status = response.status().as_u16();
        let flags_response = match response.json::<FeatureFlagsResponse>().await {
            Ok(r) => r,
            Err(e) => {
                let err =
                    Error::Serialization(format!("Failed to parse feature flags response: {e}"));
                report_flags_error(
                    &self.options.on_error,
                    &flags_endpoint,
                    distinct_id,
                    Some(status),
                    None,
                    &err,
                );
                return Err(err);
            }
        };

        Ok(flags_response.normalize())
    }

    /// Get a specific feature flag value for a user.
    ///
    /// # Parameters
    ///
    /// - `key`: Feature flag key.
    /// - `distinct_id`: User distinct ID.
    /// - `groups`: Optional group keys for group-targeted flags.
    /// - `person_properties`: Optional person properties for release
    ///   conditions.
    /// - `group_properties`: Optional group properties for group-targeted
    ///   release conditions.
    ///
    /// # Returns
    ///
    /// `Ok(Some(value))` when the flag is returned, `Ok(None)` when it is not
    /// returned or local-only evaluation cannot resolve it.
    ///
    /// # Errors
    ///
    /// Returns errors from remote `/flags` requests or response parsing.
    #[must_use = "feature flag result should be used"]
    #[instrument(skip_all, level = "debug")]
    #[deprecated(
        since = "0.6.0",
        note = "Use Client::evaluate_flags() to fetch a snapshot, then call .get_flag(key) on it. \
                The snapshot deduplicates $feature_flag_called events and supports attaching \
                rich metadata to captured events via Event::with_flags()."
    )]
    pub async fn get_feature_flag<K: Into<String>, D: Into<String>>(
        &self,
        key: K,
        distinct_id: D,
        groups: Option<HashMap<String, String>>,
        person_properties: Option<HashMap<String, serde_json::Value>>,
        group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
    ) -> Result<Option<FlagValue>, Error> {
        let key_str = key.into();
        let distinct_id_str = distinct_id.into();

        // Try local evaluation first if available
        if let Some(ref evaluator) = self.local_evaluator {
            let empty_props = HashMap::new();
            let empty_groups: HashMap<String, String> = HashMap::new();
            let empty_group_props: HashMap<String, HashMap<String, serde_json::Value>> =
                HashMap::new();
            let props = person_properties.as_ref().unwrap_or(&empty_props);
            let groups_ref = groups.as_ref().unwrap_or(&empty_groups);
            let group_props_ref = group_properties.as_ref().unwrap_or(&empty_group_props);
            match evaluator.evaluate_flag(
                &key_str,
                &distinct_id_str,
                props,
                groups_ref,
                group_props_ref,
            ) {
                Ok(Some(value)) => {
                    debug!(flag = %key_str, ?value, "Flag evaluated locally");
                    return Ok(Some(value));
                }
                Ok(None) => {
                    if self.options.local_evaluation_only {
                        debug!(flag = %key_str, "Flag not found locally, skipping remote fallback");
                        return Ok(None);
                    }
                    debug!(flag = %key_str, "Flag not found locally, falling back to API");
                }
                Err(e) => {
                    if self.options.local_evaluation_only {
                        debug!(flag = %key_str, error = %e.message, "Inconclusive local evaluation, skipping remote fallback");
                        return Ok(None);
                    }
                    debug!(flag = %key_str, error = %e.message, "Inconclusive local evaluation, falling back to API");
                }
            }
        }

        // Fall back to API
        trace!(flag = %key_str, "Fetching flag from API");
        let (feature_flags, _payloads) = self
            .get_feature_flags(distinct_id_str, groups, person_properties, group_properties)
            .await?;
        Ok(feature_flags.get(&key_str).cloned())
    }

    /// Check if a feature flag is enabled for a user.
    ///
    /// # Returns
    ///
    /// `true` for `FlagValue::Boolean(true)` or any multivariate variant,
    /// `false` for disabled or missing flags.
    ///
    /// # Errors
    ///
    /// Returns errors from [`Client::get_feature_flag`].
    #[must_use = "feature flag enabled check result should be used"]
    #[deprecated(
        since = "0.6.0",
        note = "Use Client::evaluate_flags() to fetch a snapshot, then call .is_enabled(key) \
                on it. The snapshot deduplicates $feature_flag_called events and supports \
                attaching rich metadata to captured events via Event::with_flags()."
    )]
    #[allow(deprecated)] // calls deprecated get_feature_flag internally
    pub async fn is_feature_enabled<K: Into<String>, D: Into<String>>(
        &self,
        key: K,
        distinct_id: D,
        groups: Option<HashMap<String, String>>,
        person_properties: Option<HashMap<String, serde_json::Value>>,
        group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
    ) -> Result<bool, Error> {
        let flag_value = self
            .get_feature_flag(
                key.into(),
                distinct_id.into(),
                groups,
                person_properties,
                group_properties,
            )
            .await?;
        Ok(match flag_value {
            Some(FlagValue::Boolean(b)) => b,
            Some(FlagValue::String(_)) => true, // Variants are considered enabled
            None => false,
        })
    }

    /// Get a feature flag payload for a user.
    ///
    /// # Parameters
    ///
    /// - `key`: Feature flag key.
    /// - `distinct_id`: User distinct ID.
    ///
    /// # Returns
    ///
    /// The JSON payload for the flag, if one was returned. This method does not
    /// emit `$feature_flag_called` events.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Connection`] for request failures and
    /// [`Error::Serialization`] when the response cannot be parsed.
    #[must_use = "feature flag payload result should be used"]
    #[deprecated(
        since = "0.6.0",
        note = "Use Client::evaluate_flags() to fetch a snapshot, then call \
                .get_flag_payload(key) on it. Reading the payload from a snapshot is \
                event-free, matching this method's behavior, and avoids the per-call \
                /flags request."
    )]
    pub async fn get_feature_flag_payload<K: Into<String>, D: Into<String>>(
        &self,
        key: K,
        distinct_id: D,
    ) -> Result<Option<serde_json::Value>, Error> {
        if self.options.is_disabled() {
            trace!("Client is disabled, skipping feature flag payload request");
            return Ok(None);
        }

        let key_str = key.into();
        let flags_endpoint = self.options.endpoints().build_url(Endpoint::Flags);

        let mut payload = json!({
            "api_key": self.options.api_key,
            "distinct_id": distinct_id.into(),
        });

        // Add geoip disable parameter if configured
        if self.options.disable_geoip {
            payload["disable_geoip"] = json!(true);
        }

        let distinct_id = payload.get("distinct_id").and_then(|v| v.as_str());
        let response = match self
            .client
            .post(&flags_endpoint)
            .header(CONTENT_TYPE, "application/json")
            .header(USER_AGENT, get_default_user_agent())
            .json(&payload)
            .timeout(Duration::from_secs(
                self.options.feature_flags_request_timeout_seconds,
            ))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let err = Error::Connection(e.to_string());
                report_flags_error(
                    &self.options.on_error,
                    &flags_endpoint,
                    distinct_id,
                    None,
                    None,
                    &err,
                );
                return Err(err);
            }
        };

        if !response.status().is_success() {
            return Ok(None);
        }

        let status = response.status().as_u16();
        let flags_response: FeatureFlagsResponse = match response.json().await {
            Ok(r) => r,
            Err(e) => {
                let err = Error::Serialization(format!("Failed to parse response: {e}"));
                report_flags_error(
                    &self.options.on_error,
                    &flags_endpoint,
                    distinct_id,
                    Some(status),
                    None,
                    &err,
                );
                return Err(err);
            }
        };

        let (_flags, payloads) = flags_response.normalize();
        Ok(payloads.get(&key_str).cloned())
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
        options
            .person_properties
            .get_or_insert_with(HashMap::new)
            .entry("distinct_id".to_string())
            .or_insert_with(|| json!(distinct_id.clone()));
        options.groups.get_or_insert_with(HashMap::new);
        options.group_properties.get_or_insert_with(HashMap::new);

        let mut records: HashMap<String, EvaluatedFlagRecord> = HashMap::new();
        let mut locally_evaluated_keys: HashSet<String> = HashSet::new();

        if let Some(evaluator) = &self.local_evaluator {
            let person_props_owned = options.person_properties.clone().unwrap_or_default();
            let groups_owned = options.groups.clone().unwrap_or_default();
            let group_props_owned = options.group_properties.clone().unwrap_or_default();
            let local_results = evaluator.evaluate_all_flags(
                &distinct_id,
                &person_props_owned,
                &groups_owned,
                &group_props_owned,
            );
            for (key, result) in local_results {
                if let Some(filter) = &options.flag_keys {
                    if !filter.iter().any(|k| k == &key) {
                        continue;
                    }
                }
                if let Ok(value) = result {
                    records.insert(key.clone(), local_record(value));
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

        if !options.only_evaluate_locally && !local_covers_request {
            // Don't lose successful local evaluations if `/flags` fails — degrade
            // to a snapshot built from the local results we already have. The
            // alternative (returning Err) wastes useful data and surprises
            // callers who would otherwise get partial coverage.
            match self.fetch_flag_details(&distinct_id, &options).await {
                Ok(response) => {
                    request_id = response.request_id;
                    errors_while_computing = response.errors_while_computing_flags;
                    quota_limited = response.quota_limited;
                    for (key, detail) in response.flags {
                        if locally_evaluated_keys.contains(&key) {
                            continue;
                        }
                        records.insert(key, remote_record_from_detail(detail));
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
                Arc::new(AsyncFlagEventHost::from_options(
                    &self.options,
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
                        is_retryable_feature_flags_error(&e),
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

        let mut person_properties = options.person_properties.clone().unwrap_or_default();
        person_properties
            .entry("distinct_id".to_string())
            .or_insert_with(|| json!(distinct_id));
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
    /// `shutdown().await` first, which makes this a no-op.
    fn drop(&mut self) {
        let Some(transport) = &self.transport else {
            return;
        };
        if transport.begin_close() {
            let (tx, rx) = std::sync::mpsc::channel();
            if transport.send_control(Control::Shutdown(Completion::Blocking(tx))) {
                let _ = rx.recv();
            }
        }
        // Always join — even if this caller lost the `begin_close` race or its
        // shutdown wait was cancelled — so every shutdown/drop path waits for the
        // worker and the flush stays durable. The winner has already sent the
        // Shutdown (synchronously, before any await), so the worker will exit.
        transport.join();
    }
}
