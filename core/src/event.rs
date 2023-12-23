use chrono::NaiveDateTime;
use serde::Serialize;
use std::collections::HashMap;

use crate::error::Error;

#[derive(Serialize)]
pub struct InnerEvent {
    api_key: String,
    #[serde(flatten)]
    event: Event,
}

impl InnerEvent {
    pub fn new(event: Event, api_key: String) -> Self {
        Self { api_key, event }
    }
}

#[derive(Serialize)]
pub struct InnerEventBatch {
    api_key: String,
    batch: Vec<Event>,
}

impl InnerEventBatch {
    pub fn new(batch: Vec<Event>, api_key: String) -> Self {
        Self { api_key, batch }
    }
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Event {
    pub(crate) event: String,
    pub(crate) properties: Properties,
    pub(crate) timestamp: Option<NaiveDateTime>,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Properties {
    pub(crate) distinct_id: String,
    #[serde(rename = "$groups", skip_serializing_if = "Option::is_none")]
    pub(crate) groups: Option<HashMap<String, String>>,
    #[serde(flatten)]
    pub(crate) props: HashMap<String, serde_json::Value>,
}

impl Properties {
    fn new<S: Into<String>>(distinct_id: S) -> Self {
        Self {
            distinct_id: distinct_id.into(),
            props: Default::default(),
            groups: None,
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

    pub fn with_timestamp<S: Into<String>>(
        event: S,
        distinct_id: S,
        timestamp: NaiveDateTime,
    ) -> Self {
        Self {
            event: event.into(),
            properties: Properties::new(distinct_id),
            timestamp: Some(timestamp),
        }
    }

    /// Errors if `prop` fails to serialize
    pub fn insert_prop<K: Into<String>, P: Serialize>(
        &mut self,
        key: K,
        prop: P,
    ) -> Result<(), Error> {
        let as_json =
            serde_json::to_value(prop).map_err(|source| Error::Serialization { source })?;
        let _ = self.properties.props.insert(key.into(), as_json);
        Ok(())
    }

    pub fn insert_group<K: Into<String>, P: Into<String>>(
        &mut self,
        group_type: K,
        group_key: P,
    ) -> () {
        let groups = self.properties.groups.get_or_insert_with(HashMap::new);
        groups.insert(group_type.into(), group_key.into());
    }
}
