use crate::errors::Error;
use crate::properties::Properties;
use chrono::NaiveDateTime;
use serde::Serialize;

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Event {
    pub event: String,
    pub properties: Properties,
    pub timestamp: Option<NaiveDateTime>,
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
