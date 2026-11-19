//! A client for applications: it keeps a local anonymous identity and queues
//! events, and both survive restarts unless you choose memory-only storage.
//!
//! Create a [`Config`] with your project token and pass it to
//! [`Client::init_blocking`], or to `Client::init` with the `async-client`
//! feature. Initialization restores the saved identity and queued events and
//! never waits for the network. [`Client::capture`] and [`Client::screen`]
//! return a [`CaptureOutcome`] at once; background workers write to disk and
//! deliver events in batches. Call [`Client::flush_blocking`] to request
//! delivery now, and [`Client::shutdown_blocking`] before the app exits.
//! Dropping the client also shuts it down.
//!
//! [`Config::persistence`] selects where state and events are kept: the OS
//! local application-data directory by default, a directory you choose, or
//! memory only. See [`Persistence`].
//!
//! A complete program is in `examples/stateful.rs`.

mod config;
mod identity;
#[cfg(feature = "async-client")]
mod initialization;
mod producer;
mod runtime;
mod storage;
mod writer;

pub use crate::client::on_error::InitializationFailure;
pub use config::Config;
pub use identity::{Alias, Identify, PersonProfiles, PersonProperties};
pub use producer::{Capture, CaptureOutcome, Screen};
pub use runtime::{Client, DeliveryOutcome, DrainReport, PersistenceOutcome};
pub use storage::Persistence;

use serde::{Deserialize, Serialize};

/// The complete mandatory state persisted as one versioned snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Snapshot {
    device_id: String,
    anonymous_id: String,
    distinct_id: String,
    identified: bool,
    person_processing: bool,
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
    }

    fn is_valid(&self) -> bool {
        !self.device_id.trim().is_empty()
            && !self.anonymous_id.trim().is_empty()
            && !self.distinct_id.trim().is_empty()
    }
}
