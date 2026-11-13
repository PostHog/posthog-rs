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
mod storage;
mod writer;

pub use crate::client::on_error::InitializationFailure;
pub use config::Config;
pub use identity::{Identify, PersonProfiles};
pub use producer::{Capture, CaptureOutcome};
pub use runtime::{Client, DeliveryOutcome, DrainReport, PersistenceOutcome};
pub use storage::Persistence;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The complete mandatory state persisted as one versioned snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    anonymous_id: String,
    distinct_id: String,
    identified: bool,
    person_processing: bool,
    // None follows the configured default, including after reset.
    #[serde(deserialize_with = "Option::<bool>::deserialize")]
    opt_out: Option<bool>,
    registered: HashMap<String, Value>,
    groups: HashMap<String, String>,
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
            anonymous_id: id.clone(),
            distinct_id: id,
            identified: false,
            person_processing: false,
            opt_out: None,
            registered: HashMap::new(),
            groups: HashMap::new(),
            context_revision: 0,
            generation: 0,
            person_properties: None,
        }
    }

    fn same_persisted_state(&self, other: &Self) -> bool {
        self.anonymous_id == other.anonymous_id
            && self.distinct_id == other.distinct_id
            && self.identified == other.identified
            && self.person_processing == other.person_processing
            && self.opt_out == other.opt_out
            && self.registered == other.registered
            && self.groups == other.groups
    }

    fn is_valid(&self) -> bool {
        !self.anonymous_id.trim().is_empty()
            && !self.distinct_id.trim().is_empty()
            && self
                .groups
                .iter()
                .all(|(kind, key)| !kind.trim().is_empty() && !key.trim().is_empty())
    }
}
