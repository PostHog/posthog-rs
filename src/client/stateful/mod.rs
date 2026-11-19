//! Memory-first client state. Filesystem access belongs to initialization and
//! the state-persistence worker, never ordinary capture or state access.

mod config;
mod consent;
mod identity;
#[cfg(feature = "async-client")]
mod initialization;
mod producer;
mod properties;
mod runtime;
mod sessions;
mod storage;
mod writer;

pub use crate::client::on_error::InitializationFailure;
pub use config::Config;
pub use identity::{Alias, Identify, PersonProfiles, PersonProperties};
pub use producer::{Capture, CaptureOutcome, Screen};
pub use properties::Group;
pub use runtime::{Client, DeliveryOutcome, DrainReport, PersistenceOutcome};
pub use sessions::{SessionMode, SessionPersistence};
pub use storage::Persistence;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Saved JSON must stay readable by serde_json's 128-level decoder, including
/// the few levels the state record adds around application values.
const MAX_VALUE_DEPTH: usize = 100;

pub(super) fn within_depth<'a>(values: impl IntoIterator<Item = &'a Value>) -> bool {
    fn check(value: &Value, remaining: usize) -> bool {
        match value {
            Value::Array(items) => remaining > 0 && items.iter().all(|v| check(v, remaining - 1)),
            Value::Object(map) => remaining > 0 && map.values().all(|v| check(v, remaining - 1)),
            _ => true,
        }
    }
    values
        .into_iter()
        .all(|value| check(value, MAX_VALUE_DEPTH))
}

/// The complete mandatory state persisted as one versioned snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Snapshot {
    device_id: String,
    anonymous_id: String,
    distinct_id: String,
    identified: bool,
    person_processing: bool,
    // None follows the configured default, including after reset.
    #[serde(deserialize_with = "Option::<bool>::deserialize")]
    opt_out: Option<bool>,
    registered: HashMap<String, Value>,
    groups: HashMap<String, String>,
    #[serde(deserialize_with = "Option::<sessions::StoredSession>::deserialize")]
    session: Option<sessions::StoredSession>,
    #[serde(skip)]
    sessions: sessions::Sessions,
    #[serde(skip)]
    context_revision: u64,
    // Process-local bookkeeping shares the same lock as identity, but is not persisted.
    #[serde(skip)]
    generation: u64,
    #[serde(skip)]
    person_properties: Option<[u8; 20]>,
}

impl Snapshot {
    fn anonymous(id: String) -> Self {
        Self {
            device_id: id.clone(),
            anonymous_id: id.clone(),
            distinct_id: id,
            identified: false,
            person_processing: false,
            opt_out: None,
            registered: HashMap::new(),
            groups: HashMap::new(),
            session: None,
            sessions: sessions::Sessions::default(),
            context_revision: 0,
            generation: 0,
            person_properties: None,
        }
    }

    fn same_persisted_state(&self, other: &Self) -> bool {
        self.anonymous_id == other.anonymous_id
            && self.device_id == other.device_id
            && self.distinct_id == other.distinct_id
            && self.identified == other.identified
            && self.person_processing == other.person_processing
            && self.opt_out == other.opt_out
            && self.registered == other.registered
            && self.groups == other.groups
            && self.session == other.session
    }

    fn is_valid(&self) -> bool {
        within_depth(self.registered.values())
            && self
                .session
                .as_ref()
                .map_or(true, sessions::StoredSession::is_valid)
            && !self.device_id.trim().is_empty()
            && !self.anonymous_id.trim().is_empty()
            && !self.distinct_id.trim().is_empty()
            && self
                .groups
                .iter()
                .all(|(kind, key)| !kind.trim().is_empty() && !key.trim().is_empty())
    }
}
