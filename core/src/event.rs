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
    event: String,
    properties: Properties,
    timestamp: Option<NaiveDateTime>,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Properties {
    distinct_id: String,
    #[serde(flatten)]
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
            serde_json::to_value(prop).map_err(|source| Error::Serialization { source })?;
        let _ = self.properties.props.insert(key.into(), as_json);
        Ok(())
    }
}
