use std::collections::HashMap;

use chrono::NaiveDateTime;
use semver::Version;
use serde::Serialize;
use uuid::Uuid;

use crate::Error;

/// An [`Event`] represents an interaction a user has with your app or
/// website. Examples include button clicks, pageviews, query completions, and signups.
/// See the [PostHog documentation](https://posthog.com/docs/data/events)
/// for a detailed explanation of PostHog Events.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Event {
    event: String,
    #[serde(rename = "$distinct_id")]
    distinct_id: String,
    properties: HashMap<String, serde_json::Value>,
    groups: HashMap<String, String>,
    timestamp: Option<NaiveDateTime>,
}

impl Event {
    /// Capture a new identified [`Event`]. Unless you have a distinct ID you can
    /// associate with a user, you probably want to use [`new_anon`] instead.
    pub fn new<S: Into<String>>(event: S, distinct_id: S) -> Self {
        Self {
            event: event.into(),
            distinct_id: distinct_id.into(),
            properties: HashMap::new(),
            groups: HashMap::new(),
            timestamp: None,
        }
    }

    /// Capture a new anonymous event.
    /// See https://posthog.com/docs/data/anonymous-vs-identified-events#how-to-capture-anonymous-events
    pub fn new_anon<S: Into<String>>(event: S) -> Self {
        let mut res = Self {
            event: event.into(),
            distinct_id: Uuid::now_v7().to_string(),
            properties: HashMap::new(),
            groups: HashMap::new(),
            timestamp: None,
        };
        res.insert_prop("$process_person_profile", false)
            .expect("bools are safe for serde");
        res
    }

    /// Add a property to the event
    ///
    /// Errors if `prop` fails to serialize
    pub fn insert_prop<K: Into<String>, P: Serialize>(
        &mut self,
        key: K,
        prop: P,
    ) -> Result<(), Error> {
        let as_json =
            serde_json::to_value(prop).map_err(|e| Error::Serialization(e.to_string()))?;
        let _ = self.properties.insert(key.into(), as_json);
        Ok(())
    }

    /// Capture this as a group event. See https://posthog.com/docs/product-analytics/group-analytics#how-to-capture-group-events
    /// Note that group events cannot be personless, and will be automatically upgraded to include person profile processing if
    /// they were anonymous. This might lead to "empty" person profiles being created.
    pub fn add_group(&mut self, group_name: &str, group_id: &str) {
        // You cannot disable person profile processing for groups
        self.insert_prop("$process_person_profile", true)
            .expect("bools are safe for serde");
        self.groups.insert(group_name.into(), group_id.into());
    }
}

// This exists so that the client doesn't have to specify the API key over and over
#[derive(Serialize)]
pub struct InnerEvent {
    api_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    uuid: Option<Uuid>,
    event: String,
    #[serde(rename = "$distinct_id")]
    distinct_id: String,
    properties: HashMap<String, serde_json::Value>,
    timestamp: Option<NaiveDateTime>,
}

impl InnerEvent {
    pub fn new(event: Event, api_key: String) -> Self {
        Self::new_with_uuid(event, api_key, None)
    }

    pub fn new_with_uuid(event: Event, api_key: String, uuid: Option<Uuid>) -> Self {
        let mut properties = event.properties;

        // Add $lib_name and $lib_version to the properties
        properties.insert(
            "$lib".into(),
            serde_json::Value::String("posthog-rs".into()),
        );

        let version_str = env!("CARGO_PKG_VERSION");
        properties.insert(
            "$lib_version".into(),
            serde_json::Value::String(version_str.into()),
        );

        if let Ok(version) = version_str.parse::<Version>() {
            properties.insert(
                "$lib_version__major".into(),
                serde_json::Value::Number(version.major.into()),
            );
            properties.insert(
                "$lib_version__minor".into(),
                serde_json::Value::Number(version.minor.into()),
            );
            properties.insert(
                "$lib_version__patch".into(),
                serde_json::Value::Number(version.patch.into()),
            );
        }

        if !event.groups.is_empty() {
            properties.insert(
                "$groups".into(),
                serde_json::Value::Object(
                    event
                        .groups
                        .into_iter()
                        .map(|(k, v)| (k, serde_json::Value::String(v)))
                        .collect(),
                ),
            );
        }

        Self {
            api_key,
            uuid,
            event: event.event,
            distinct_id: event.distinct_id,
            properties,
            timestamp: event.timestamp,
        }
    }
}

#[cfg(test)]
pub mod tests {
    use crate::{event::InnerEvent, Event};

    #[test]
    fn inner_event_adds_lib_properties_correctly() {
        // Arrange
        let mut event = Event::new("unit test event", "1234");
        event.insert_prop("key1", "value1").unwrap();
        let api_key = "test_api_key".to_string();

        // Act
        let inner_event = InnerEvent::new(event, api_key);

        // Assert
        let props = &inner_event.properties;
        assert_eq!(
            props.get("$lib"),
            Some(&serde_json::Value::String("posthog-rs".to_string()))
        );
    }
}
