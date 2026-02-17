use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDateTime, TimeZone, Utc};
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
    uuid: Option<Uuid>,
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
            uuid: None,
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
            uuid: None,
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

    /// Set the event timestamp, for events that happened in the past.
    ///
    /// Errors if the timestamp is in the future.
    pub fn set_timestamp<Tz>(&mut self, timestamp: DateTime<Tz>) -> Result<(), Error>
    where
        Tz: TimeZone,
    {
        if timestamp > Utc::now() + Duration::seconds(1) {
            return Err(Error::InvalidTimestamp(String::from(
                "Events cannot occur in the future",
            )));
        }
        self.timestamp = Some(timestamp.naive_utc());
        Ok(())
    }

    /// Set a specific UUID for this event, overriding the server-generated one.
    /// Useful for deduplication when re-importing historical data.
    pub fn set_uuid(&mut self, uuid: Uuid) {
        self.uuid = Some(uuid);
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
        let uuid = event.uuid;
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
    use uuid::Uuid;

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

    #[test]
    fn inner_event_preserves_uuid_from_event() {
        let uuid = Uuid::now_v7();
        let mut event = Event::new("test", "user1");
        event.set_uuid(uuid);

        let inner = InnerEvent::new(event, "key".to_string());
        let json = serde_json::to_value(&inner).unwrap();

        assert_eq!(json["uuid"], uuid.to_string());
    }

    #[test]
    fn inner_event_omits_uuid_when_not_set() {
        let event = Event::new("test", "user1");

        let inner = InnerEvent::new(event, "key".to_string());
        let json = serde_json::to_value(&inner).unwrap();

        assert!(json.get("uuid").is_none());
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use chrono::{DateTime, Utc};

    use super::Event;

    #[test]
    fn test_timestamp_is_correctly_set() {
        let mut event = Event::new_anon("test");
        let ts = DateTime::parse_from_rfc3339("2023-01-01T10:00:00+03:00").unwrap();
        event.set_timestamp(ts).expect("Date is not in the future");
        let expected = DateTime::parse_from_rfc3339("2023-01-01T07:00:00Z").unwrap();
        assert_eq!(event.timestamp.unwrap(), expected.naive_utc())
    }

    #[test]
    fn test_timestamp_is_correctly_set_with_future_date() {
        let mut event = Event::new_anon("test");
        let ts = Utc::now() + Duration::from_secs(60);
        event
            .set_timestamp(ts)
            .expect_err("Date is in the future, should be rejected");

        assert!(event.timestamp.is_none())
    }
}
