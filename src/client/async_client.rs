use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use reqwest::{header::CONTENT_TYPE, Client as HttpClient};
use serde_json::json;
use tracing::{debug, instrument, trace, warn};

use crate::endpoints::Endpoint;
use crate::event::BatchRequest;
use crate::feature_flag_evaluations::{
    EvaluateFlagsOptions, EvaluatedFlagRecord, FeatureFlagEvaluations, FeatureFlagEvaluationsHost,
    FlagCalledEventParams,
};
use crate::feature_flags::{
    match_feature_flag, FeatureFlag, FeatureFlagsResponse, FlagDetail, FlagValue,
};
use crate::local_evaluation::{AsyncFlagPoller, FlagCache, LocalEvaluationConfig, LocalEvaluator};
use crate::{event::InnerEvent, Error, Event};

use super::{CaptureMode, ClientOptions};

/// Cap on the number of `distinct_id` entries in the `$feature_flag_called`
/// dedup cache. On overflow the entire map is reset (matches the JS SDK).
const MAX_FLAG_CALLED_CACHE_SIZE: usize = 50_000;

async fn check_response(response: reqwest::Response) -> Result<(), Error> {
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "Unknown error".to_string());

    match Error::from_http_response(status, body) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// A [`Client`] facilitates interactions with the PostHog API over HTTP.
pub struct Client {
    options: ClientOptions,
    client: HttpClient,
    local_evaluator: Option<LocalEvaluator>,
    _flag_poller: Option<AsyncFlagPoller>,
    flag_event_host: OnceLock<Arc<dyn FeatureFlagEvaluationsHost>>,
}

/// Implementation of [`FeatureFlagEvaluationsHost`] that emits dedup-aware
/// `$feature_flag_called` events through a clone of the async [`Client`]'s
/// HTTP transport. The event ship is fire-and-forget: errors are logged at
/// `debug` level but do not surface to the caller, matching the JS SDK.
struct AsyncFlagEventHost {
    http_client: HttpClient,
    api_key: String,
    capture_url: String,
    disabled: bool,
    disable_geoip: bool,
    is_server: bool,
    dedup_cache: Mutex<HashMap<String, HashSet<String>>>,
    /// Tokio runtime handle captured at host construction (which always runs
    /// inside the runtime that hosts `evaluate_flags`). This lets snapshot
    /// access methods spawn `$feature_flag_called` shipping from any thread —
    /// including ones without an entered runtime — by routing through the
    /// captured handle instead of the free `tokio::spawn` (which would panic).
    runtime: tokio::runtime::Handle,
}

impl AsyncFlagEventHost {
    fn from_options(options: &ClientOptions, http_client: HttpClient) -> Self {
        let capture_url = options.endpoints().build_url(Endpoint::Capture);
        Self {
            http_client,
            api_key: options.api_key.clone(),
            capture_url,
            disabled: options.is_disabled(),
            disable_geoip: options.disable_geoip,
            is_server: options.is_server,
            dedup_cache: Mutex::new(HashMap::new()),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// Returns `true` when the helper has already shipped this
    /// `(distinct_id, key, response)` combination and the caller should skip.
    fn already_reported(&self, distinct_id: &str, dedup_key: &str) -> bool {
        let mut cache = self.dedup_cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(seen) = cache.get(distinct_id) {
            if seen.contains(dedup_key) {
                return true;
            }
        }
        if cache.len() >= MAX_FLAG_CALLED_CACHE_SIZE {
            cache.clear();
        }
        cache
            .entry(distinct_id.to_string())
            .or_default()
            .insert(dedup_key.to_string());
        false
    }

    fn spawn_ship(&self, event: Event) {
        if self.disabled {
            return;
        }
        let inner_event = InnerEvent::new(event, self.api_key.clone());
        let payload = match serde_json::to_string(&inner_event) {
            Ok(p) => p,
            Err(e) => {
                debug!(error = %e, "failed to serialize $feature_flag_called event");
                return;
            }
        };
        let http_client = self.http_client.clone();
        let url = self.capture_url.clone();
        self.runtime.spawn(async move {
            let response = match http_client
                .post(&url)
                .header(CONTENT_TYPE, "application/json")
                .body(payload)
                .send()
                .await
            {
                Ok(r) => r,
                Err(send_err) => {
                    let message = send_err.to_string();
                    debug!("failed to send $feature_flag_called event: {message}");
                    return;
                }
            };
            if let Err(check_err) = check_response(response).await {
                let message = check_err.to_string();
                debug!("$feature_flag_called event rejected by server: {message}");
            }
        });
    }
}

impl FeatureFlagEvaluationsHost for AsyncFlagEventHost {
    fn capture_flag_called_event_if_needed(&self, params: FlagCalledEventParams) {
        let dedup_key = build_dedup_key(&params.key, params.response.as_ref(), &params.groups);
        if self.already_reported(&params.distinct_id, &dedup_key) {
            return;
        }

        let mut event = Event::new(
            "$feature_flag_called".to_string(),
            params.distinct_id.clone(),
        );
        for (k, v) in params.properties {
            if event.insert_prop(k, v).is_err() {
                return;
            }
        }
        for (group_name, group_id) in &params.groups {
            event.add_group(group_name, group_id);
        }
        if params.disable_geoip.unwrap_or(self.disable_geoip) {
            let _ = event.insert_prop("$geoip_disable", true);
        }
        if self.is_server {
            let _ = event.insert_prop("$is_server", true);
        }
        self.spawn_ship(event);
    }

    fn log_warning(&self, message: &str) {
        // Surface filter-helper misuse via tracing — users can silence these
        // with their tracing-subscriber level filter (e.g. `posthog_rs=error`).
        warn!("{message}");
    }
}

fn build_dedup_key(
    flag_key: &str,
    response: Option<&FlagValue>,
    groups: &HashMap<String, String>,
) -> String {
    let response_repr = match response {
        Some(FlagValue::Boolean(true)) => "true".to_string(),
        Some(FlagValue::Boolean(false)) => "false".to_string(),
        Some(FlagValue::String(s)) => s.clone(),
        None => "::null::".to_string(),
    };
    if groups.is_empty() {
        format!("{flag_key}_{response_repr}")
    } else {
        // Canonicalize so two equal group maps with different insertion orders
        // produce the same dedup key — necessary for group-scoped flags to fire
        // exactly once per distinct group context.
        let mut sorted: Vec<(&String, &String)> = groups.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        let groups_repr: String = sorted
            .iter()
            .map(|(k, v)| format!("{}={}", pct(k), pct(v)))
            .collect::<Vec<_>>()
            .join(";");
        format!("{flag_key}_{response_repr}_{groups_repr}")
    }
}

fn pct(s: &str) -> String {
    s.replace('%', "%25")
        .replace('=', "%3D")
        .replace(';', "%3B")
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
            poller.start().await;

            (Some(LocalEvaluator::new(cache)), Some(poller))
        } else {
            warn!("Local evaluation enabled but personal_api_key not set, falling back to API evaluation");
            (None, None)
        }
    } else {
        (None, None)
    };

    Client {
        options,
        client,
        local_evaluator,
        _flag_poller: flag_poller,
        flag_event_host: OnceLock::new(),
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
    /// # Errors
    ///
    /// Returns [`Error::Serialization`] if the event cannot be serialized,
    /// [`Error::Connection`] for transport or unexpected HTTP failures,
    /// [`Error::RateLimit`] for HTTP 429, [`Error::BadRequest`] for HTTP 400 or
    /// 413, and [`Error::ServerError`] for HTTP 5xx.
    ///
    /// # Remarks
    ///
    /// Disabled clients skip the request and return `Ok(())`.
    #[instrument(skip(self, event), level = "debug")]
    pub async fn capture(&self, mut event: Event) -> Result<(), Error> {
        if self.options.is_disabled() {
            trace!("Client is disabled, skipping capture");
            return Ok(());
        }

        if self.options.capture_mode == CaptureMode::V1 {
            return self.capture_v1(vec![event]).await;
        }

        // Add geoip disable property if configured
        if self.options.disable_geoip {
            event.insert_prop("$geoip_disable", true).ok();
        }
        // Mark server-side events so ingestion doesn't attribute the host OS to the person.
        if self.options.is_server {
            event.insert_prop("$is_server", true).ok();
        }

        let inner_event = InnerEvent::new(event, self.options.api_key.clone());

        let payload =
            serde_json::to_string(&inner_event).map_err(|e| Error::Serialization(e.to_string()))?;

        let url = self.options.endpoints().build_url(Endpoint::Capture);

        let response = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .body(payload)
            .send()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        check_response(response).await
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
    /// # Errors
    ///
    /// Returns the same error categories as [`Client::capture`].
    pub async fn capture_batch(
        &self,
        events: Vec<Event>,
        historical_migration: bool,
    ) -> Result<(), Error> {
        if self.options.is_disabled() {
            return Ok(());
        }

        if self.options.capture_mode == CaptureMode::V1 {
            return self.capture_v1(events).await;
        }

        let disable_geoip = self.options.disable_geoip;
        let is_server = self.options.is_server;
        let inner_events: Vec<InnerEvent> = events
            .into_iter()
            .map(|mut event| {
                if disable_geoip {
                    event.insert_prop("$geoip_disable", true).ok();
                }
                if is_server {
                    event.insert_prop("$is_server", true).ok();
                }
                InnerEvent::new(event, self.options.api_key.clone())
            })
            .collect();

        let batch_request = BatchRequest {
            api_key: self.options.api_key.clone(),
            historical_migration,
            batch: inner_events,
        };
        let payload = serde_json::to_string(&batch_request)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        let url = self.options.endpoints().build_url(Endpoint::Batch);

        let response = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .body(payload)
            .send()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        check_response(response).await
    }

    async fn capture_v1(&self, _events: Vec<Event>) -> Result<(), Error> {
        Err(Error::ServerError {
            status: 500,
            message: "Capture V1 not yet implemented".to_string(),
        })
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
            .client
            .post(&flags_endpoint)
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .timeout(Duration::from_secs(
                self.options.feature_flags_request_timeout_seconds,
            ))
            .send()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::Connection(format!(
                "API request failed with status {status}: {text}"
            )));
        }

        let flags_response = response.json::<FeatureFlagsResponse>().await.map_err(|e| {
            Error::Serialization(format!("Failed to parse feature flags response: {e}"))
        })?;

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

        let response = self
            .client
            .post(&flags_endpoint)
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .timeout(Duration::from_secs(
                self.options.feature_flags_request_timeout_seconds,
            ))
            .send()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        if !response.status().is_success() {
            return Ok(None);
        }

        let flags_response: FeatureFlagsResponse = response
            .json()
            .await
            .map_err(|e| Error::Serialization(format!("Failed to parse response: {e}")))?;

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
                    self.client.clone(),
                )) as Arc<dyn FeatureFlagEvaluationsHost>
            })
            .clone()
    }

    async fn fetch_flag_details(
        &self,
        distinct_id: &str,
        options: &EvaluateFlagsOptions,
    ) -> Result<DetailedFlagsResponse, Error> {
        let flags_endpoint = self.options.endpoints().build_url(Endpoint::Flags);

        let mut payload = json!({
            "api_key": self.options.api_key,
            "distinct_id": distinct_id,
        });
        if let Some(groups) = &options.groups {
            payload["groups"] = json!(groups);
        }
        if let Some(person_properties) = &options.person_properties {
            payload["person_properties"] = json!(person_properties);
        }
        if let Some(group_properties) = &options.group_properties {
            payload["group_properties"] = json!(group_properties);
        }
        let effective_disable_geoip = options.disable_geoip.unwrap_or(self.options.disable_geoip);
        if effective_disable_geoip {
            payload["disable_geoip"] = json!(true);
        }
        if let Some(flag_keys) = &options.flag_keys {
            payload["flag_keys_to_evaluate"] = json!(flag_keys);
        }

        let response = self
            .client
            .post(&flags_endpoint)
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .timeout(Duration::from_secs(
                self.options.feature_flags_request_timeout_seconds,
            ))
            .send()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::Connection(format!(
                "API request failed with status {status}: {text}"
            )));
        }

        let parsed = response.json::<FeatureFlagsResponse>().await.map_err(|e| {
            Error::Serialization(format!("Failed to parse feature flags response: {e}"))
        })?;
        Ok(extract_flag_details(parsed))
    }
}

/// Normalised view of a `/flags?v=2` response surfacing the per-flag detail
/// shape needed by the snapshot path.
struct DetailedFlagsResponse {
    flags: HashMap<String, FlagDetail>,
    request_id: Option<String>,
    errors_while_computing_flags: bool,
    quota_limited: bool,
}

fn extract_flag_details(response: FeatureFlagsResponse) -> DetailedFlagsResponse {
    match response {
        FeatureFlagsResponse::V2 {
            flags,
            request_id,
            errors_while_computing_flags,
            quota_limited,
        } => DetailedFlagsResponse {
            flags,
            request_id,
            errors_while_computing_flags,
            quota_limited,
        },
        FeatureFlagsResponse::Legacy {
            feature_flags,
            feature_flag_payloads,
            errors,
        } => {
            let mut flags = HashMap::new();
            for (key, value) in feature_flags {
                let (enabled, variant) = match value {
                    FlagValue::Boolean(b) => (b, None),
                    FlagValue::String(s) => (true, Some(s)),
                };
                let payload = feature_flag_payloads.get(&key).cloned();
                flags.insert(
                    key.clone(),
                    FlagDetail {
                        key,
                        enabled,
                        variant,
                        reason: None,
                        metadata: payload.map(|payload| crate::feature_flags::FlagMetadata {
                            id: 0,
                            version: 0,
                            description: None,
                            payload: Some(payload),
                        }),
                    },
                );
            }
            DetailedFlagsResponse {
                flags,
                request_id: None,
                errors_while_computing_flags: errors.is_some_and(|e| !e.is_empty()),
                quota_limited: false,
            }
        }
    }
}

fn local_record(value: FlagValue) -> EvaluatedFlagRecord {
    let (enabled, variant) = match value {
        FlagValue::Boolean(b) => (b, None),
        FlagValue::String(s) => (true, Some(s)),
    };
    EvaluatedFlagRecord {
        enabled,
        variant,
        // Local definitions do not surface a payload through the poller today.
        payload: None,
        id: None,
        version: None,
        reason: Some("Evaluated locally".to_string()),
        locally_evaluated: true,
    }
}

fn remote_record_from_detail(detail: FlagDetail) -> EvaluatedFlagRecord {
    let metadata = detail.metadata;
    let reason = detail
        .reason
        .and_then(|r| r.description.or(Some(r.code)))
        .filter(|s| !s.is_empty());
    let id = metadata.as_ref().map(|m| m.id);
    let version = metadata.as_ref().map(|m| m.version);
    let payload = metadata.and_then(|m| m.payload).map(normalize_payload);
    EvaluatedFlagRecord {
        enabled: detail.enabled,
        variant: detail.variant,
        payload,
        id,
        version,
        reason,
        locally_evaluated: false,
    }
}

/// `metadata.payload` from `/flags?v=2` is sometimes a JSON-encoded string
/// (e.g. `"{\"color\":\"blue\"}"`) rather than already-parsed JSON. Try to
/// parse a `String` payload as JSON and fall back to the raw string on
/// failure so users can branch on a uniform [`serde_json::Value`].
fn normalize_payload(payload: serde_json::Value) -> serde_json::Value {
    match payload {
        serde_json::Value::String(raw) => {
            serde_json::from_str(&raw).unwrap_or(serde_json::Value::String(raw))
        }
        other => other,
    }
}
