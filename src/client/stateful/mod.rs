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
pub use identity::{Identify, PersonProfiles};
pub use producer::{Capture, CaptureOutcome};
#[cfg(feature = "session-replay")]
pub use replay::{
    FrameLayout, PixelFormat, ReplayConfig, ReplayFrame, SessionReplay, SubmitOutcome,
};
pub use runtime::{Client, DeliveryOutcome, DrainReport, PersistenceOutcome};
pub use sessions::{SessionMode, SessionPersistence};
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
    flags: flag_state::FlagState,
    #[serde(with = "remote_config::cache_serde")]
    project_config: Option<std::sync::Arc<crate::client::transport::remote_config::ProjectConfig>>,
    #[serde(skip)]
    project_config_loading: bool,
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
            anonymous_id: id.clone(),
            distinct_id: id,
            identified: false,
            person_processing: false,
            opt_out: None,
            registered: HashMap::new(),
            groups: HashMap::new(),
            flags: flag_state::FlagState::default(),
            project_config: None,
            project_config_loading: false,
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
            && self.distinct_id == other.distinct_id
            && self.identified == other.identified
            && self.person_processing == other.person_processing
            && self.opt_out == other.opt_out
            && self.registered == other.registered
            && self.groups == other.groups
            && self.flags == other.flags
            && self.project_config == other.project_config
            && self.session == other.session
    }

    fn is_valid(&self) -> bool {
        self.flags.is_valid()
            && self
                .project_config
                .as_ref()
                .map_or(true, |config| config.valid())
            && self
                .session
                .as_ref()
                .map_or(true, sessions::StoredSession::is_valid)
            && !self.anonymous_id.trim().is_empty()
            && !self.distinct_id.trim().is_empty()
            && self
                .groups
                .iter()
                .all(|(kind, key)| !kind.trim().is_empty() && !key.trim().is_empty())
    }
}
