//! Memory-first client state. Filesystem access belongs to initialization and
//! the state-persistence worker, never ordinary capture or state access.

mod config;
#[cfg(feature = "async-client")]
mod initialization;
mod producer;
mod runtime;
mod storage;
mod writer;

pub use crate::client::on_error::InitializationFailure;
pub use config::Config;
pub use producer::{Capture, CaptureOutcome};
pub use runtime::{Client, DeliveryOutcome, DrainReport, PersistenceOutcome};
pub use storage::Persistence;

use serde::{Deserialize, Serialize};

/// The complete mandatory state persisted as one versioned snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    anonymous_id: String,
    distinct_id: String,
}

impl Snapshot {
    fn anonymous(id: String) -> Self {
        Self {
            anonymous_id: id.clone(),
            distinct_id: id,
        }
    }

    fn is_valid(&self) -> bool {
        !self.anonymous_id.trim().is_empty() && !self.distinct_id.trim().is_empty()
    }
}
