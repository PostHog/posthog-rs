use std::collections::HashMap;

use chrono::NaiveDateTime;
use semver::Version;
use serde::Serialize;

use crate::Error;

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Event {
    event: String,
    properties: Properties,
    timestamp: Option<NaiveDateTime>,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Properties {
    distinct_id: String,
    props: HashMap<String, serde_json::Value>,
}

impl Properties {
    fn new<S: Into<String>>(distinct_id: S) -> Self {
        Self {
            distinct_id: distinct_id.into(),
            props: Default::default(),
        }
    }
}

impl Event {
    pub fn new<S: Into<String>>(event: S, distinct_id: S) -> Self {
        Self {
            event: event.into(),
            properties: Properties::new(distinct_id),
            timestamp: None,
        }
    }

    /// Errors if `prop` fails to serialize
    pub fn insert_prop<K: Into<String>, P: Serialize>(
        &mut self,
        key: K,
        prop: P,
    ) -> Result<(), Error> {
        let as_json =
            serde_json::to_value(prop).map_err(|e| Error::Serialization(e.to_string()))?;
        let _ = self.properties.props.insert(key.into(), as_json);
        Ok(())
    }
}

// This exists so that the client doesn't have to specify the API key over and over
#[derive(Serialize)]
pub struct InnerEvent {
    api_key: String,
    event: String,
    properties: Properties,
    timestamp: Option<NaiveDateTime>,
}

impl InnerEvent {
    pub fn new(event: Event, api_key: String) -> Self {
        let mut properties = event.properties;

        // Add $lib_name and $lib_version to the properties
        properties.props.insert(
            "$lib_name".into(),
            serde_json::Value::String("posthog-rs".into()),
        );

        let version_str = env!("CARGO_PKG_VERSION");
        properties.props.insert(
            "$lib_version".into(),
            serde_json::Value::String(version_str.into()),
        );

        if let Ok(version) = version_str.parse::<Version>() {
            properties.props.insert(
                "$lib_version__major".into(),
                serde_json::Value::Number(version.major.into()),
            );
            properties.props.insert(
                "$lib_version__minor".into(),
                serde_json::Value::Number(version.minor.into()),
            );
            properties.props.insert(
                "$lib_version__patch".into(),
                serde_json::Value::Number(version.patch.into()),
            );
        }

        Self {
            api_key,
            event: event.event,
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
        let props = &inner_event.properties.props;
        assert_eq!(
            props.get("$lib_name"),
            Some(&serde_json::Value::String("posthog-rs".to_string()))
        );
    }
}
