use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Properties {
    pub distinct_id: String,
    pub props: HashMap<String, serde_json::Value>,
}

impl Properties {
    pub fn new<S: Into<String>>(distinct_id: S) -> Self {
        Self {
            distinct_id: distinct_id.into(),
            props: Default::default(),
        }
    }
}
