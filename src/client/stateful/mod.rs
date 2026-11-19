//! Memory-first client state. Filesystem access belongs to initialization and
//! the state-persistence worker, never ordinary capture or state access.

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
    // None follows the configured default. Reset keeps an explicit choice.
    #[serde(deserialize_with = "Option::<bool>::deserialize")]
    opt_out: Option<bool>,
    registered: HashMap<String, Value>,
    groups: HashMap<String, String>,
    flags: flag_state::FlagState,
    #[serde(with = "remote_config::cache_serde")]
    remote_config: Option<Arc<RemoteConfig>>,
    #[serde(skip)]
    remote_config_loading: bool,
    #[serde(skip)]
    flag_load: flag_state::FlagLoadState,
    #[serde(deserialize_with = "Option::<sessions::StoredSession>::deserialize")]
    session: Option<sessions::StoredSession>,
    #[serde(skip)]
    sessions: sessions::Sessions,
    #[serde(skip)]
    context_revision: u64,
    // Process-local bookkeeping shares the same lock as identity, but is not persisted.
    #[serde(skip)]
    generation: u64,
    #[cfg(feature = "session-replay")]
    #[serde(skip)]
    replay_consent_generation: u64,
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
            flags: flag_state::FlagState::default(),
            remote_config: None,
            remote_config_loading: false,
            flag_load: flag_state::FlagLoadState::default(),
            session: None,
            sessions: sessions::Sessions::default(),
            context_revision: 0,
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
