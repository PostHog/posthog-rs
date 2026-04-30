use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use reqwest::{blocking::Client as HttpClient, header::CONTENT_TYPE};
use serde_json::json;
use tracing::{debug, instrument, trace, warn};

use crate::endpoints::{Endpoint, EndpointManager};
use crate::event::BatchRequest;
use crate::feature_flag_evaluations::{
    EvaluateFlagsOptions, EvaluatedFlagRecord, FeatureFlagEvaluations, FeatureFlagEvaluationsHost,
    FlagCalledEventParams,
};
use crate::feature_flags::{
    match_feature_flag, FeatureFlag, FeatureFlagsResponse, FlagDetail, FlagValue,
};
use crate::local_evaluation::{FlagCache, FlagPoller, LocalEvaluationConfig, LocalEvaluator};
use crate::{event::InnerEvent, Error, Event};

use super::ClientOptions;

/// Cap on the number of `distinct_id` entries in the `$feature_flag_called`
/// dedup cache. On overflow the entire map is reset (matches the JS SDK).
const MAX_FLAG_CALLED_CACHE_SIZE: usize = 50_000;

fn check_response(response: reqwest::blocking::Response) -> Result<(), Error> {
    let status = response.status().as_u16();
    let body = response
        .text()
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
    _flag_poller: Option<FlagPoller>,
    flag_event_host: OnceLock<Arc<dyn FeatureFlagEvaluationsHost>>,
}

/// Implementation of [`FeatureFlagEvaluationsHost`] that emits dedup-aware
/// `$feature_flag_called` events through a clone of the blocking [`Client`]'s
/// HTTP transport. Constructed lazily and cached on the [`Client`] so all
/// snapshots share a single dedup cache.
struct BlockingFlagEventHost {
    http_client: HttpClient,
    api_key: String,
    endpoints: EndpointManager,
    disabled: bool,
    disable_geoip: bool,
    dedup_cache: Mutex<HashMap<String, HashSet<String>>>,
}

impl BlockingFlagEventHost {
    fn from_options(options: &ClientOptions, http_client: HttpClient) -> Self {
        Self {
            http_client,
            api_key: options.api_key.clone(),
            endpoints: options.endpoints().clone(),
            disabled: options.is_disabled(),
            disable_geoip: options.disable_geoip,
            dedup_cache: Mutex::new(HashMap::new()),
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

    fn ship_event(&self, event: Event) {
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
        let url = self.endpoints.build_url(Endpoint::Capture);
        let result = self
            .http_client
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .body(payload)
            .send();
        match result {
            Ok(response) => {
                if let Err(e) = check_response(response) {
                    debug!(error = %e, "$feature_flag_called event rejected by server");
                }
            }
            Err(e) => debug!(error = %e, "failed to send $feature_flag_called event"),
        }
    }
}

impl FeatureFlagEvaluationsHost for BlockingFlagEventHost {
    fn capture_flag_called_event_if_needed(&self, params: FlagCalledEventParams) {
        let dedup_key = build_dedup_key(&params.key, params.response.as_ref());
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
        self.ship_event(event);
    }

    fn log_warning(&self, message: &str) {
        // Surface filter-helper misuse via tracing — users can silence these
        // with their tracing-subscriber level filter (e.g. `posthog_rs=error`).
        warn!("{message}");
    }
}

fn build_dedup_key(flag_key: &str, response: Option<&FlagValue>) -> String {
    let response_repr = match response {
        Some(FlagValue::Boolean(true)) => "true".to_string(),
        Some(FlagValue::Boolean(false)) => "false".to_string(),
        Some(FlagValue::String(s)) => s.clone(),
        None => "::null::".to_string(),
    };
    format!("{flag_key}_{response_repr}")
}

/// This function constructs a new client using the options provided.
pub fn client<C: Into<ClientOptions>>(options: C) -> Client {
    let options = options.into().sanitize();
    let client = HttpClient::builder()
        .timeout(Duration::from_secs(options.request_timeout_seconds))
        .build()
        .unwrap(); // Unwrap here is as safe as `HttpClient::new`

    let (local_evaluator, flag_poller) = if options.enable_local_evaluation {
        if let Some(ref personal_key) = options.personal_api_key {
            let cache = FlagCache::new();

            let config = LocalEvaluationConfig {
                personal_api_key: personal_key.clone(),
                project_api_key: options.api_key.clone(),
                api_host: options.endpoints().api_host(),
                poll_interval: Duration::from_secs(options.poll_interval_seconds),
                request_timeout: Duration::from_secs(options.request_timeout_seconds),
            };

            let mut poller = FlagPoller::new(config, cache.clone());
            poller.start();

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
    #[instrument(skip(self, event), level = "debug")]
    pub fn capture(&self, mut event: Event) -> Result<(), Error> {
        if self.options.is_disabled() {
            trace!("Client is disabled, skipping capture");
            return Ok(());
        }

        // Add geoip disable property if configured
        if self.options.disable_geoip {
            event.insert_prop("$geoip_disable", true).ok();
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
            .map_err(|e| Error::Connection(e.to_string()))?;

        check_response(response)
    }

    /// Capture a collection of events with a single request. Events are sent to
    /// the `/batch/` endpoint. Set `historical_migration` to `true` to route
    /// events to the historical ingestion topic, bypassing the main pipeline.
    #[instrument(skip(self, events), fields(event_count = events.len()), level = "debug")]
    pub fn capture_batch(
        &self,
        events: Vec<Event>,
        historical_migration: bool,
    ) -> Result<(), Error> {
        if self.options.is_disabled() {
            trace!("Client is disabled, skipping batch capture");
            return Ok(());
        }

        let disable_geoip = self.options.disable_geoip;
        let inner_events: Vec<InnerEvent> = events
            .into_iter()
            .map(|mut event| {
                if disable_geoip {
                    event.insert_prop("$geoip_disable", true).ok();
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
            .map_err(|e| Error::Connection(e.to_string()))?;

        check_response(response)
    }

    /// Get all feature flags for a user
    #[must_use = "feature flags result should be used"]
    pub fn get_feature_flags<S: Into<String>>(
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
            .map_err(|e| Error::Connection(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::Connection(format!(
                "API request failed with status {status}: {text}"
            )));
        }

        let flags_response = response.json::<FeatureFlagsResponse>().map_err(|e| {
            Error::Serialization(format!("Failed to parse feature flags response: {e}"))
        })?;

        Ok(flags_response.normalize())
    }

    /// Get a specific feature flag value for a user.
    #[must_use = "feature flag result should be used"]
    #[instrument(skip_all, level = "debug")]
    #[deprecated(
        since = "0.6.0",
        note = "Use Client::evaluate_flags() to fetch a snapshot, then call .get_flag(key) on it. \
                The snapshot deduplicates $feature_flag_called events and supports attaching \
                rich metadata to captured events via Event::with_flags()."
    )]
    pub fn get_feature_flag<K: Into<String>, D: Into<String>>(
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
            let empty = HashMap::new();
            let props = person_properties.as_ref().unwrap_or(&empty);
            match evaluator.evaluate_flag(&key_str, &distinct_id_str, props) {
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
        let (feature_flags, _payloads) =
            self.get_feature_flags(distinct_id_str, groups, person_properties, group_properties)?;
        Ok(feature_flags.get(&key_str).cloned())
    }

    /// Check if a feature flag is enabled for a user.
    #[must_use = "feature flag enabled check result should be used"]
    #[deprecated(
        since = "0.6.0",
        note = "Use Client::evaluate_flags() to fetch a snapshot, then call .is_enabled(key) \
                on it. The snapshot deduplicates $feature_flag_called events and supports \
                attaching rich metadata to captured events via Event::with_flags()."
    )]
    #[allow(deprecated)] // calls deprecated get_feature_flag internally
    pub fn is_feature_enabled<K: Into<String>, D: Into<String>>(
        &self,
        key: K,
        distinct_id: D,
        groups: Option<HashMap<String, String>>,
        person_properties: Option<HashMap<String, serde_json::Value>>,
        group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
    ) -> Result<bool, Error> {
        let flag_value = self.get_feature_flag(
            key,
            distinct_id,
            groups,
            person_properties,
            group_properties,
        )?;
        Ok(match flag_value {
            Some(FlagValue::Boolean(b)) => b,
            Some(FlagValue::String(_)) => true, // Variants are considered enabled
            None => false,
        })
    }

    /// Get a feature flag payload for a user.
    #[must_use = "feature flag payload result should be used"]
    #[deprecated(
        since = "0.6.0",
        note = "Use Client::evaluate_flags() to fetch a snapshot, then call \
                .get_flag_payload(key) on it. Reading the payload from a snapshot is \
                event-free, matching this method's behavior, and avoids the per-call \
                /flags request."
    )]
    pub fn get_feature_flag_payload<K: Into<String>, D: Into<String>>(
        &self,
        key: K,
        distinct_id: D,
    ) -> Result<Option<serde_json::Value>, Error> {
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
            .map_err(|e| Error::Connection(e.to_string()))?;

        if !response.status().is_success() {
            return Ok(None);
        }

        let flags_response: FeatureFlagsResponse = response
            .json()
            .map_err(|e| Error::Serialization(format!("Failed to parse response: {e}")))?;

        let (_flags, payloads) = flags_response.normalize();
        Ok(payloads.get(&key_str).cloned())
    }

    /// Evaluate a feature flag locally (requires feature flags to be loaded)
    pub fn evaluate_feature_flag_locally(
        &self,
        flag: &FeatureFlag,
        distinct_id: &str,
        person_properties: &HashMap<String, serde_json::Value>,
    ) -> Result<FlagValue, Error> {
        match_feature_flag(flag, distinct_id, person_properties)
            .map_err(|e| Error::InconclusiveMatch(e.message))
    }

    /// Evaluate every feature flag for `distinct_id` in a single round-trip,
    /// returning a [`FeatureFlagEvaluations`] snapshot.
    ///
    /// Each `is_enabled` / `get_flag` call on the returned snapshot fires a
    /// dedup-aware `$feature_flag_called` event with full metadata, and the
    /// snapshot can be passed to [`Event::with_flags`] so a downstream
    /// [`Client::capture`] inherits `$feature/<key>` and `$active_feature_flags`
    /// without an extra `/flags` request.
    ///
    /// [`Event::with_flags`]: crate::Event::with_flags
    pub fn evaluate_flags<S: Into<String>>(
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
            let local_results = evaluator.evaluate_all_flags(&distinct_id, &person_props_owned);
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
            match self.fetch_flag_details(&distinct_id, &options) {
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
                Arc::new(BlockingFlagEventHost::from_options(
                    &self.options,
                    self.client.clone(),
                )) as Arc<dyn FeatureFlagEvaluationsHost>
            })
            .clone()
    }

    fn fetch_flag_details(
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
            .map_err(|e| Error::Connection(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::Connection(format!(
                "API request failed with status {status}: {text}"
            )));
        }

        let parsed = response.json::<FeatureFlagsResponse>().map_err(|e| {
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
        // The legacy decide format does not surface metadata; build a synthetic
        // detail so the snapshot still carries enabled/variant for each flag.
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
