use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// The person a request acts for, resolved by the identify callback. Without
/// one, events are anonymous and create no person profile.
///
/// Never put a raw credential, such as an `Authorization` header, into
/// `properties`: person properties are not redacted, and they persist on the
/// person profile.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Identity {
    pub distinct_id: String,
    /// Sent as `$set`.
    pub properties: Map<String, Value>,
    /// Group type to group key, sent as `$groups`.
    pub groups: BTreeMap<String, String>,
}

impl Identity {
    pub fn new(distinct_id: impl Into<String>) -> Self {
        Self {
            distinct_id: distinct_id.into(),
            ..Self::default()
        }
    }

    pub fn with_property(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }

    pub fn with_group(
        mut self,
        group_type: impl Into<String>,
        group_key: impl Into<String>,
    ) -> Self {
        self.groups.insert(group_type.into(), group_key.into());
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.distinct_id.trim().is_empty()
    }
}
