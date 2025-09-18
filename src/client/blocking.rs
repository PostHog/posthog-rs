use std::collections::HashMap;
use std::time::Duration;

use reqwest::{blocking::Client as HttpClient, header::CONTENT_TYPE};
use serde_json::json;

use crate::feature_flags::{match_feature_flag, FeatureFlag, FeatureFlagsResponse, FlagValue};
use crate::{event::InnerEvent, Error, Event};

use super::ClientOptions;

/// A [`Client`] facilitates interactions with the PostHog API over HTTP.
pub struct Client {
    options: ClientOptions,
    client: HttpClient,
}

/// This function constructs a new client using the options provided.
pub fn client<C: Into<ClientOptions>>(options: C) -> Client {
    let options = options.into();
    let client = HttpClient::builder()
        .timeout(Duration::from_secs(options.request_timeout_seconds))
        .build()
        .unwrap(); // Unwrap here is as safe as `HttpClient::new`
    Client { options, client }
}

impl Client {
    /// Capture the provided event, sending it to PostHog.
    pub fn capture(&self, event: Event) -> Result<(), Error> {
        let inner_event = InnerEvent::new(event, self.options.api_key.clone());

        let payload =
            serde_json::to_string(&inner_event).map_err(|e| Error::Serialization(e.to_string()))?;

        self.client
            .post(&self.options.api_endpoint)
            .header(CONTENT_TYPE, "application/json")
            .body(payload)
            .send()
            .map_err(|e| Error::Connection(e.to_string()))?;

        Ok(())
    }

    /// Capture a collection of events with a single request. This function may be
    /// more performant than capturing a list of events individually.
    pub fn capture_batch(&self, events: Vec<Event>) -> Result<(), Error> {
        let events: Vec<_> = events
            .into_iter()
            .map(|event| InnerEvent::new(event, self.options.api_key.clone()))
            .collect();

        let payload =
            serde_json::to_string(&events).map_err(|e| Error::Serialization(e.to_string()))?;

        self.client
            .post(&self.options.api_endpoint)
            .header(CONTENT_TYPE, "application/json")
            .body(payload)
            .send()
            .map_err(|e| Error::Connection(e.to_string()))?;

        Ok(())
    }

    /// Get all feature flags for a user
    pub fn get_feature_flags(
        &self,
        distinct_id: String,
        groups: Option<HashMap<String, String>>,
        person_properties: Option<HashMap<String, serde_json::Value>>,
        group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
    ) -> Result<FeatureFlagsResponse, Error> {
        let decide_endpoint = self
            .options
            .api_endpoint
            .replace("/i/v0/e/", "/decide/?v=3");

        let mut payload = json!({
            "api_key": self.options.api_key,
            "distinct_id": distinct_id,
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

        let response = self
            .client
            .post(&decide_endpoint)
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .map_err(|e| Error::Connection(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::Connection(format!(
                "API request failed with status {}: {}",
                status, text
            )));
        }

        response.json::<FeatureFlagsResponse>().map_err(|e| {
            Error::Serialization(format!("Failed to parse feature flags response: {}", e))
        })
    }

    /// Get a specific feature flag value for a user
    pub fn get_feature_flag(
        &self,
        key: String,
        distinct_id: String,
        groups: Option<HashMap<String, String>>,
        person_properties: Option<HashMap<String, serde_json::Value>>,
        group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
    ) -> Result<Option<FlagValue>, Error> {
        let response =
            self.get_feature_flags(distinct_id, groups, person_properties, group_properties)?;
        Ok(response.feature_flags.get(&key).cloned())
    }

    /// Check if a feature flag is enabled for a user  
    pub fn is_feature_enabled(
        &self,
        key: String,
        distinct_id: String,
        groups: Option<HashMap<String, String>>,
        person_properties: Option<HashMap<String, serde_json::Value>>,
        group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
    ) -> Result<bool, Error> {
        let flag_value = self.get_feature_flag(
            key.into(),
            distinct_id.into(),
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

    /// Get a feature flag payload for a user
    pub fn get_feature_flag_payload<K: Into<String>, D: Into<String>>(
        &self,
        key: K,
        distinct_id: D,
    ) -> Result<Option<serde_json::Value>, Error> {
        let key_str = key.into();
        let decide_endpoint = self
            .options
            .api_endpoint
            .replace("/i/v0/e/", "/decide/?v=3");

        let payload = json!({
            "api_key": self.options.api_key,
            "distinct_id": distinct_id.into(),
        });

        let response = self
            .client
            .post(&decide_endpoint)
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .map_err(|e| Error::Connection(e.to_string()))?;

        if !response.status().is_success() {
            return Ok(None);
        }

        let flags_response: FeatureFlagsResponse = response
            .json()
            .map_err(|e| Error::Serialization(format!("Failed to parse response: {}", e)))?;

        Ok(flags_response.feature_flag_payloads.get(&key_str).cloned())
    }

    /// Evaluate a feature flag locally (requires feature flags to be loaded)
    pub fn evaluate_feature_flag_locally(
        &self,
        flag: &FeatureFlag,
        distinct_id: &str,
        person_properties: &HashMap<String, serde_json::Value>,
    ) -> Result<FlagValue, Error> {
        match_feature_flag(flag, distinct_id, person_properties)
            .map_err(|e| Error::Connection(e.message))
    }
}
