use std::collections::HashMap;
use std::time::Duration;

use reqwest::{header::CONTENT_TYPE, Client as HttpClient};
use serde_json::json;
use tracing::{debug, instrument, trace, warn};

use crate::endpoints::{Endpoint, EndpointManager};
use crate::event::BatchRequest;
use crate::feature_flags::{match_feature_flag, FeatureFlag, FeatureFlagsResponse, FlagValue};
use crate::local_evaluation::{AsyncFlagPoller, FlagCache, LocalEvaluationConfig, LocalEvaluator};
use crate::{event::InnerEvent, Error, Event};

use super::ClientOptions;

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
}

/// This function constructs a new client using the options provided.
pub async fn client<C: Into<ClientOptions>>(options: C) -> Client {
    let mut options = options.into();
    // Ensure endpoint_manager is properly initialized based on the host
    options.endpoint_manager = EndpointManager::new(options.host.clone());
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
    }
}

impl Client {
    /// Capture the provided event, sending it to PostHog.
    #[instrument(skip(self, event), level = "debug")]
    pub async fn capture(&self, mut event: Event) -> Result<(), Error> {
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
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        check_response(response).await
    }

    /// Capture a collection of events with a single request. This function may be
    /// more performant than capturing a list of events individually.
    pub async fn capture_batch(&self, events: Vec<Event>) -> Result<(), Error> {
        if self.options.is_disabled() {
            return Ok(());
        }

        let inner_events = self.prepare_inner_events(events);
        let payload = serde_json::to_string(&inner_events)
            .map_err(|e| Error::Serialization(e.to_string()))?;
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

    /// Capture a collection of events as a historical migration. Events are sent
    /// to the `/batch/` endpoint and routed to the historical ingestion topic,
    /// bypassing the main ingestion pipeline.
    pub async fn capture_batch_historical(&self, events: Vec<Event>) -> Result<(), Error> {
        if self.options.is_disabled() {
            return Ok(());
        }

        let inner_events = self.prepare_inner_events(events);
        let batch_request = BatchRequest {
            api_key: self.options.api_key.clone(),
            historical_migration: true,
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

    fn prepare_inner_events(&self, events: Vec<Event>) -> Vec<InnerEvent> {
        let disable_geoip = self.options.disable_geoip;
        events
            .into_iter()
            .map(|mut event| {
                if disable_geoip {
                    event.insert_prop("$geoip_disable", true).ok();
                }
                InnerEvent::new(event, self.options.api_key.clone())
            })
            .collect()
    }

    /// Get all feature flags for a user
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

    /// Get a specific feature flag value for a user
    #[must_use = "feature flag result should be used"]
    #[instrument(skip_all, level = "debug")]
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
            let empty = HashMap::new();
            let props = person_properties.as_ref().unwrap_or(&empty);
            match evaluator.evaluate_flag(&key_str, &distinct_id_str, props) {
                Ok(Some(value)) => {
                    debug!(flag = %key_str, ?value, "Flag evaluated locally");
                    return Ok(Some(value));
                }
                Ok(None) => {
                    debug!(flag = %key_str, "Flag not found locally, falling back to API");
                }
                Err(e) => {
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

    /// Check if a feature flag is enabled for a user
    #[must_use = "feature flag enabled check result should be used"]
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

    /// Get a feature flag payload for a user
    #[must_use = "feature flag payload result should be used"]
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
}
