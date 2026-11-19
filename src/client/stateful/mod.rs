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
#[cfg(feature = "async-client")]
mod initialization;
mod producer;
mod runtime;
mod storage;
mod writer;

pub use crate::client::on_error::InitializationFailure;
pub use config::Config;
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
}

impl Snapshot {
    fn anonymous(id: String) -> Self {
        Self {
            device_id: id.clone(),
            anonymous_id: id.clone(),
            distinct_id: id,
        }
    }

    fn is_valid(&self) -> bool {
        !self.device_id.trim().is_empty()
            && !self.anonymous_id.trim().is_empty()
            && !self.distinct_id.trim().is_empty()
    }
}
