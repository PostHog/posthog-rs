use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use reqwest::{header::CONTENT_TYPE, Client as HttpClient};
use serde_json::json;

use crate::endpoints::Endpoint;
use crate::error::{TransportError, ValidationError};
use crate::feature_flags::{
    match_feature_flag, FeatureFlag, FeatureFlagsResponse, FlagDetail, FlagValue,
};
use crate::local_evaluation::{AsyncFlagPoller, FlagCache, LocalEvaluationConfig, LocalEvaluator};
use crate::{event::InnerEvent, Error, Event};

use super::ClientOptions;

/// Maximum number of distinct IDs to track for feature flag deduplication
const MAX_DICT_SIZE: usize = 50_000;

/// A [`Client`] facilitates interactions with the PostHog API over HTTP.
pub struct Client {
    options: ClientOptions,
    client: HttpClient,
    local_evaluator: Option<LocalEvaluator>,
    _flag_poller: Option<AsyncFlagPoller>,
    /// Tracks which feature flags have been called for deduplication.
    /// Maps distinct_id -> set of feature flag keys that have been reported.
    distinct_ids_feature_flags_reported: Arc<RwLock<HashMap<String, HashSet<String>>>>,
}

/// This function constructs a new client using the options provided.
pub async fn client<C: Into<ClientOptions>>(options: C) -> Client {
    let options = options.into();
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

            let poller = AsyncFlagPoller::new(config, cache.clone());
            poller.start().await;

            (Some(LocalEvaluator::new(cache)), Some(poller))
        } else {
            eprintln!("[FEATURE FLAGS] Local evaluation enabled but personal_api_key not set");
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
        distinct_ids_feature_flags_reported: Arc::new(RwLock::new(HashMap::new())),
    }
}

impl Client {
    /// Capture the provided event, sending it to PostHog.
    pub async fn capture(&self, event: Event) -> Result<(), Error> {
        if self.options.is_disabled() {
            return Ok(());
        }

        // Note: Infinite loop prevention for $feature_flag_called events
        // If we ever implement automatic feature flag evaluation for all events
        // (auto-adding of $feature/* properties), we must skip flag evaluation
        // when event.event_name() == "$feature_flag_called" to prevent infinite
        // loops. Current implementation doesn't auto-evaluate flags in capture(),
        // so no loop risk exists today.

        let inner_event = InnerEvent::new(event, self.options.api_key.clone());

        let payload = serde_json::to_string(&inner_event)
            .map_err(|e| ValidationError::SerializationFailed(e.to_string()))?;

        let mut url = self.options.endpoints().build_url(Endpoint::Capture);
        if self.options.disable_geoip {
            let separator = if url.contains('?') { "&" } else { "?" };
            url.push_str(&format!("{separator}disable_geoip=1"));
        }

        let request = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, "application/json");

        // Apply gzip compression if enabled
        let request = if self.options.gzip {
            // Note: reqwest will automatically compress the body when gzip feature is enabled
            // and Content-Encoding header is set
            request.header("Content-Encoding", "gzip").body(payload)
        } else {
            request.body(payload)
        };

        request.send().await.map_err(TransportError::from)?;

        Ok(())
    }

    /// Capture a collection of events with a single request. This function may be
    /// more performant than capturing a list of events individually.
    pub async fn capture_batch(&self, events: Vec<Event>) -> Result<(), Error> {
        if self.options.is_disabled() {
            return Ok(());
        }

        let events: Vec<_> = events
            .into_iter()
            .map(|event| InnerEvent::new(event, self.options.api_key.to_string()))
            .collect();

        let payload = serde_json::to_string(&events)
            .map_err(|e| ValidationError::SerializationFailed(e.to_string()))?;

        let mut url = self.options.endpoints().batch_event_endpoint();
        if self.options.disable_geoip {
            let separator = if url.contains('?') { "&" } else { "?" };
            url.push_str(&format!("{separator}disable_geoip=1"));
        }

        let request = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, "application/json");

        // Apply gzip compression if enabled
        let request = if self.options.gzip {
            // Note: reqwest will automatically compress the body when gzip feature is enabled
            // and Content-Encoding header is set
            request.header("Content-Encoding", "gzip").body(payload)
        } else {
            request.body(payload)
        };

        request.send().await.map_err(TransportError::from)?;

        Ok(())
    }

    /// Internal method to capture $feature_flag_called events.
    ///
    /// Handles deduplication and event construction inline.
    #[allow(clippy::too_many_arguments)]
    async fn capture_feature_flag_called(
        &self,
        distinct_id: &str,
        flag_key: &str,
        flag_response: Option<&FlagValue>,
        payload: Option<serde_json::Value>,
        locally_evaluated: bool,
        groups: Option<HashMap<String, String>>,
        disable_geoip: Option<bool>,
        request_id: Option<String>,
        flag_details: Option<&FlagDetail>,
    ) {
        // Create the reported key for deduplication
        // Format: "{key}_::null::" for None, "{key}_{value}" otherwise
        let feature_flag_reported_key = match flag_response {
            None => format!("{}_::null::", flag_key),
            Some(flag_value) => format!("{}_{}", flag_key, flag_value),
        };

        // Check if already reported for deduplication
        {
            let reported = self.distinct_ids_feature_flags_reported.read().unwrap();
            if let Some(flags) = reported.get(distinct_id) {
                if flags.contains(&feature_flag_reported_key) {
                    // Already reported, skip
                    return;
                }
            }
        }

        // Build event properties inline
        let mut event = Event::new("$feature_flag_called", distinct_id);

        // Add required properties
        event.insert_prop("$feature_flag", flag_key).ok();
        event
            .insert_prop("$feature_flag_response", flag_response)
            .ok();
        event
            .insert_prop("locally_evaluated", locally_evaluated)
            .ok();

        // Add $feature/{key} property
        event
            .insert_prop(format!("$feature/{}", flag_key), flag_response)
            .ok();

        // Add optional properties
        if let Some(p) = payload {
            event.insert_prop("$feature_flag_payload", p).ok();
        }

        // Add request_id if provided
        if let Some(req_id) = request_id {
            event.insert_prop("$feature_flag_request_id", req_id).ok();
        }

        // Add flag_details metadata if provided
        if let Some(details) = flag_details {
            // Add reason
            if let Some(reason) = &details.reason {
                if let Some(desc) = &reason.description {
                    event.insert_prop("$feature_flag_reason", desc.clone()).ok();
                }
            }

            // Add metadata (version and id)
            if let Some(metadata) = &details.metadata {
                event
                    .insert_prop("$feature_flag_version", metadata.version)
                    .ok();
                event.insert_prop("$feature_flag_id", metadata.id).ok();
            }
        }

        // Add groups if present
        if let Some(g) = groups {
            for (group_name, group_id) in g {
                event.add_group(&group_name, &group_id);
            }
        }

        // Set disable_geoip on event if provided
        if let Some(disable_geo) = disable_geoip {
            if disable_geo {
                event.insert_prop("$geoip_disable", true).ok();
            }
        }

        // Capture the event (ignore errors to not break user code)
        if let Err(e) = self.capture(event).await {
            eprintln!(
                "[FEATURE FLAGS] Failed to capture $feature_flag_called event: {}",
                e
            );
        }

        // Mark as reported (even if capture failed to avoid retry storms)
        {
            let mut reported = self.distinct_ids_feature_flags_reported.write().unwrap();

            // Check size limit and evict if necessary
            if reported.len() >= MAX_DICT_SIZE && !reported.contains_key(distinct_id) {
                // Remove first entry (FIFO eviction)
                if let Some(first_key) = reported.keys().next().cloned() {
                    reported.remove(&first_key);
                }
            }

            reported
                .entry(distinct_id.to_string())
                .or_default()
                .insert(feature_flag_reported_key);
        }
    }

    /// Get all feature flags for a user
    pub async fn get_feature_flags<S: Into<String>>(
        &self,
        distinct_id: S,
        groups: Option<HashMap<String, String>>,
        person_properties: Option<HashMap<String, serde_json::Value>>,
        group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
    ) -> Result<FeatureFlagsResponse, Error> {
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

        #[allow(deprecated)]
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
            #[allow(deprecated)]
            return Err(Error::Connection(format!(
                "API request failed with status {status}: {text}"
            )));
        }

        #[allow(deprecated)]
        let flags_response = response.json::<FeatureFlagsResponse>().await.map_err(|e| {
            Error::Serialization(format!("Failed to parse feature flags response: {e}"))
        })?;

        Ok(flags_response)
    }

    /// Get a specific feature flag value with control over event capture
    pub async fn get_feature_flag_with_options<K: Into<String>, D: Into<String>>(
        &self,
        key: K,
        distinct_id: D,
        groups: Option<HashMap<String, String>>,
        person_properties: Option<HashMap<String, serde_json::Value>>,
        group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
        send_feature_flag_events: bool,
    ) -> Result<Option<FlagValue>, Error> {
        let key = key.into();
        let distinct_id = distinct_id.into();
        let mut locally_evaluated = false;
        let mut flag_value: Option<FlagValue> = None;
        let mut payload: Option<serde_json::Value> = None;
        let mut request_id: Option<String> = None;
        let mut flag_details_map: HashMap<String, FlagDetail> = HashMap::new();

        // Try local evaluation first if available
        if let Some(ref evaluator) = self.local_evaluator {
            let props = person_properties.clone().unwrap_or_default();
            match evaluator.evaluate_flag(&key, &distinct_id, &props) {
                Ok(Some(value)) => {
                    locally_evaluated = true;
                    flag_value = Some(value);
                }
                Ok(None) => {
                    // Flag not found locally, fall through to API
                }
                Err(e) => {
                    eprintln!(
                        "[FEATURE FLAGS] Local evaluation inconclusive: {}",
                        e.message
                    );
                    // Inconclusive match, fall through to API
                }
            }
        }

        // Fall back to API if not locally evaluated
        if flag_value.is_none() {
            match self
                .get_feature_flags(
                    distinct_id.clone(),
                    groups.clone(),
                    person_properties,
                    group_properties,
                )
                .await
            {
                Ok(response) => {
                    // Use helper methods to get flag value and payload
                    flag_value = response.get_flag_value(&key);
                    payload = response.get_flag_payload(&key);
                    request_id = response.request_id;
                    flag_details_map = response.flags;
                }
                Err(e) => {
                    eprintln!(
                        "[FEATURE FLAGS] Failed to get feature flags from API: {}",
                        e
                    );
                    // Return None on error (graceful degradation)
                    flag_value = None;
                }
            }
        }

        // Capture $feature_flag_called event if enabled
        if self.options.send_feature_flag_events && send_feature_flag_events {
            self.capture_feature_flag_called(
                &distinct_id,
                &key,
                flag_value.as_ref(),
                payload,
                locally_evaluated,
                groups,
                None, // disable_geoip - not yet supported
                request_id,
                flag_details_map.get(&key),
            )
            .await;
        }

        Ok(flag_value)
    }

    /// Get a specific feature flag value for a user.
    ///
    /// Automatically captures a `$feature_flag_called` event to PostHog
    /// unless `send_feature_flag_events` is disabled in ClientOptions.
    pub async fn get_feature_flag<K: Into<String>, D: Into<String>>(
        &self,
        key: K,
        distinct_id: D,
        groups: Option<HashMap<String, String>>,
        person_properties: Option<HashMap<String, serde_json::Value>>,
        group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
    ) -> Result<Option<FlagValue>, Error> {
        self.get_feature_flag_with_options(
            key,
            distinct_id,
            groups,
            person_properties,
            group_properties,
            true, // send_feature_flag_events
        )
        .await
    }

    /// Check if a feature flag is enabled for a user
    ///
    /// This method will automatically capture a `$feature_flag_called` event
    /// unless `send_feature_flag_events` is disabled in ClientOptions.
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
                key,
                distinct_id,
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

    /// Get a feature flag payload for a user
    pub async fn get_feature_flag_payload<K: Into<String>, D: Into<String>>(
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

        #[allow(deprecated)]
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

        #[allow(deprecated)]
        let flags_response: FeatureFlagsResponse = response
            .json()
            .await
            .map_err(|e| Error::Serialization(format!("Failed to parse response: {e}")))?;

        Ok(flags_response.get_flag_payload(&key_str))
    }

    /// Get all feature flags as a HashMap
    pub async fn get_all_flags(
        &self,
        distinct_id: String,
        groups: Option<HashMap<String, String>>,
        person_properties: Option<HashMap<String, serde_json::Value>>,
        group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
    ) -> Result<HashMap<String, FlagValue>, Error> {
        let response = self
            .get_feature_flags(distinct_id, groups, person_properties, group_properties)
            .await?;
        Ok(response.to_flag_values())
    }

    /// Get all feature flags and payloads
    pub async fn get_all_flags_and_payloads(
        &self,
        distinct_id: String,
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
        let response = self
            .get_feature_flags(distinct_id, groups, person_properties, group_properties)
            .await?;
        Ok((response.to_flag_values(), response.to_flag_payloads()))
    }

    /// Evaluate a feature flag locally (requires feature flags to be loaded)
    pub fn evaluate_feature_flag_locally(
        &self,
        flag: &FeatureFlag,
        distinct_id: &str,
        person_properties: &HashMap<String, serde_json::Value>,
    ) -> Result<FlagValue, Error> {
        #[allow(deprecated)]
        match_feature_flag(flag, distinct_id, person_properties)
            .map_err(|e| Error::Connection(e.message))
    }
}
