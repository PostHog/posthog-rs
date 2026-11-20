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
//! Dropping the client also shuts it down.
//!
//! [`Config::persistence`] selects where state and events are kept: the OS
//! local application-data directory by default, a directory you choose, or
//! memory only. See [`Persistence`].
//!
//! A complete program is in `examples/stateful.rs`.

mod config;
mod consent;
mod flag_bootstrap;
mod flag_listeners;
mod flag_reads;
mod flag_report;
mod flag_state;
mod flag_targeting;
mod flags;
mod identity;
#[cfg(feature = "async-client")]
mod initialization;
pub(super) mod producer;
mod properties;
mod remote_config;
#[cfg(feature = "session-replay")]
pub(super) mod replay;
mod runtime;
mod sessions;
mod storage;
mod writer;

#[cfg(feature = "session-replay")]
pub use crate::client::on_error::ReplayFailure;
pub use crate::client::on_error::{InitializationFailure, RemoteConfigFailure};
pub use crate::client::transport::flags::LoadError as FeatureFlagLoadError;
pub use config::Config;
pub use flag_listeners::{FeatureFlagsSubscription, FeatureFlagsUpdate};
pub use flag_reads::{FeatureFlagContext, FeatureFlagLookup, FeatureFlagResult};
pub use identity::{Alias, Identify, PersonProfiles, PersonProperties};
pub use producer::{Capture, CaptureOutcome, Screen};
pub use properties::Group;
#[cfg(feature = "session-replay")]
pub use replay::{
    FrameLayout, FrameUnavailable, PixelFormat, ReplayConfig, ReplayFrame, SessionReplay,
    SubmitOutcome,
};
pub use runtime::{Client, DeliveryOutcome, DrainReport, PersistenceOutcome};
pub use sessions::{SessionMode, SessionPersistence};
pub use storage::Persistence;

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::transport::remote_config::RemoteConfig;

/// Saved JSON must stay readable by serde_json's 128-level decoder, including
/// the few levels the state record adds around application values.
const MAX_VALUE_DEPTH: usize = 100;

/// Serde for an `Option<Arc<T>>` field, stored as the plain value: immutable
/// data shared by otherwise independent snapshots.
mod shared_arc {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;

    pub(super) fn serialize<T: Serialize, S: Serializer>(
        value: &Option<Arc<T>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_deref().serialize(serializer)
    }

    pub(super) fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Arc<T>>, D::Error> {
        Option::<T>::deserialize(deserializer).map(|value| value.map(Arc::new))
    }
}

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
    // Records written before registered properties and groups lack them.
    #[serde(default)]
    registered: HashMap<String, Value>,
    #[serde(default)]
    groups: HashMap<String, String>,
    // Records written before feature flags restore with an empty cache.
    #[serde(default)]
    flags: flag_state::FlagState,
    // Records written before remote configuration restore without one.
    #[serde(default, with = "shared_arc")]
    remote_config: Option<Arc<RemoteConfig>>,
    #[serde(skip)]
    remote_config_loading: bool,
    #[serde(skip)]
    flag_load: flag_state::FlagLoadState,
    // Records written before sessions lack this field and restore without one.
    #[serde(default)]
    session: Option<sessions::StoredSession>,
    #[serde(skip)]
    sessions: sessions::Sessions,
    // Process-local bookkeeping shares the same lock as identity, but is not persisted.
    #[serde(skip)]
    generation: u64,
    #[cfg(feature = "session-replay")]
    #[serde(skip)]
    replay_consent_generation: u64,
    #[serde(skip)]
    person_properties: Option<[u8; 20]>,
}

/// A boolean, or None for any other value, as the other SDKs read consent.
fn lenient_bool<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<bool>, D::Error> {
    Ok(serde_json::Value::deserialize(deserializer)?.as_bool())
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
            flags: flag_state::FlagState::default(),
            remote_config: None,
            remote_config_loading: false,
            flag_load: flag_state::FlagLoadState::default(),
            session: None,
            sessions: sessions::Sessions::default(),
            generation: 0,
            #[cfg(feature = "session-replay")]
            replay_consent_generation: 0,
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
            && self.flags == other.flags
            && self.remote_config == other.remote_config
            && self.session == other.session
    }

    fn is_valid(&self) -> bool {
        self.flags.is_valid()
            && within_depth(self.registered.values())
            && self
                .remote_config
                .as_ref()
                .map_or(true, |config| config.valid())
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
