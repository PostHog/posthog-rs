//! A client for applications: it keeps a local anonymous identity and queues
//! events, and both survive restarts unless you choose memory-only storage.
//! [`Client::identify`] links it to a known user, [`Client::alias`] links
//! another ID, and [`Client::reset`] starts a new anonymous identity.
//!
//! Create a [`Config`] with your project token and pass it to
//! [`Client::init_blocking`], or to `Client::init` with the `async-client`
//! feature. Initialization restores the saved identity and queued events and
//! never waits for the network. [`Client::capture`] and [`Client::screen`]
//! return a [`CaptureOutcome`] at once; background workers write to disk and
//! deliver events in batches. Call [`Client::flush_blocking`] to request
//! delivery now, and [`Client::shutdown_blocking`] before the app exits.
//! Dropping the client also shuts it down: it does not wait for delivery, but
//! waits up to about 1 s to write pending state and captured events to disk.
//!
//! [`Client::opt_out`] stops new events and [`Client::opt_in`] resumes them;
//! the choice is saved with the identity. [`Config::opt_out`] sets the default
//! when no choice is saved, including after [`Client::reset`].
//!
//! [`Client::register`] adds properties to every later event, and
//! [`Client::group`] adds a group to every later event's `$groups` and turns on
//! person processing. Both are saved with the identity; [`Client::reset`]
//! clears them.
//!
//! [`Config::persistence`] selects where state and events are kept: the OS
//! local application-data directory by default, a directory you choose, or
//! memory only. See [`Persistence`].
//!
//! A complete program is in `examples/stateful.rs`.

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
pub use identity::{Alias, Identify, PersonProfiles, PersonProperties};
pub use producer::{Capture, CaptureOutcome, Screen};
pub use properties::Group;
pub use runtime::{Client, DeliveryOutcome, DrainReport, PersistenceOutcome};
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
    // Records written before identity support lack these fields and restore as anonymous.
    #[serde(default)]
    identified: bool,
    #[serde(default)]
    person_processing: bool,
    // None follows the configured default, including after reset. A missing
    // or unreadable value is None, so it never discards the identity.
    #[serde(default, deserialize_with = "lenient_bool")]
    opt_out: Option<bool>,
    // Records written before registered properties and groups lack them. A
    // value this build can't read or accept restores empty, so, like
    // `opt_out`, it never discards the identity.
    #[serde(default, deserialize_with = "lenient_registered")]
    registered: HashMap<String, Value>,
    #[serde(default, deserialize_with = "lenient_groups")]
    groups: HashMap<String, String>,
    // Process-local bookkeeping shares the same lock as identity, but is not persisted.
    #[serde(skip)]
    generation: u64,
    #[serde(skip)]
    person_properties: Option<[u8; 20]>,
}

/// A boolean, or None for any other value, as the other SDKs read consent.
fn lenient_bool<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<bool>, D::Error> {
    Ok(serde_json::Value::deserialize(deserializer)?.as_bool())
}

/// Saved registered properties, or none when they can't be read or are too deep.
fn lenient_registered<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<HashMap<String, Value>, D::Error> {
    let registered: HashMap<String, Value> =
        serde_json::from_value(Value::deserialize(deserializer)?).unwrap_or_default();
    Ok(if within_depth(registered.values()) {
        registered
    } else {
        HashMap::new()
    })
}

/// Saved groups, or none when they can't be read or hold a blank type or key.
fn lenient_groups<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<HashMap<String, String>, D::Error> {
    let groups: HashMap<String, String> =
        serde_json::from_value(Value::deserialize(deserializer)?).unwrap_or_default();
    Ok(if valid_groups(&groups) {
        groups
    } else {
        HashMap::new()
    })
}

fn valid_groups(groups: &HashMap<String, String>) -> bool {
    groups
        .iter()
        .all(|(kind, key)| !kind.trim().is_empty() && !key.trim().is_empty())
}

impl Snapshot {
    /// Become the identified person `id`, with person processing on.
    fn identify_as(&mut self, id: String) {
        self.distinct_id = id;
        self.identified = true;
        self.person_processing = true;
        self.generation = self.generation.wrapping_add(1);
        self.person_properties = None;
    }

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
            generation: 0,
            person_properties: None,
        }
    }

    /// Compares only the persisted fields. The exhaustive destructure makes a new
    /// field fail to compile until it is classified as persisted or not.
    fn same_persisted_state(&self, other: &Self) -> bool {
        let Self {
            device_id,
            anonymous_id,
            distinct_id,
            identified,
            person_processing,
            opt_out,
            registered,
            groups,
            generation: _,
            person_properties: _,
        } = self;
        *device_id == other.device_id
            && *anonymous_id == other.anonymous_id
            && *distinct_id == other.distinct_id
            && *identified == other.identified
            && *person_processing == other.person_processing
            && *opt_out == other.opt_out
            && *registered == other.registered
            && *groups == other.groups
    }

    fn is_valid(&self) -> bool {
        within_depth(self.registered.values())
            && !self.device_id.trim().is_empty()
            && !self.anonymous_id.trim().is_empty()
            && !self.distinct_id.trim().is_empty()
            && valid_groups(&self.groups)
    }
}
