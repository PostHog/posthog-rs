//! Persisted targeting and optional result data share the client-state owner.
//! Cached results carry their producing context, never an inferred current user.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Snapshot;
use crate::client::event_store::RecordBuffer;
use crate::client::transport::flags::{LoadError, LoadedFlags, MAX_FLAGS};
use crate::feature_flag_evaluations::EvaluatedFlagRecord;

/// Serialized cache data, including producing contexts; not allocator/RSS usage.
pub(super) const MAX_CACHE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct FlagLoadState {
    pub(super) revision: u64,
    pub(super) request: u64,
    pub(super) completed: Option<FlagCompletion>,
    // Reporting context is setup-local, never restored from persisted cache.
    pub(super) bootstrap: Option<Arc<FlagCache>>,
    pub(super) remote_received: bool,
    pub(super) reports: super::flag_report::ReportTracker,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FlagCompletion {
    pub(super) context: Arc<FlagContext>,
    pub(super) revision: u64,
    pub(super) request: u64,
    pub(super) error: Option<LoadError>,
    pub(super) partial: bool,
    pub(super) invalid_payloads: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FlagState {
    pub(super) person_properties: HashMap<String, Value>,
    pub(super) group_properties: HashMap<String, HashMap<String, Value>>,
    // Immutable evaluation data is shared by otherwise independent state snapshots.
    #[serde(with = "shared_cache")]
    pub(super) cache: Option<Arc<FlagCache>>,
}

impl FlagState {
    pub(super) fn is_valid(&self) -> bool {
        self.group_properties
            .keys()
            .all(|key| !key.trim().is_empty())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FlagContext {
    pub(super) distinct_id: String,
    pub(super) anonymous_id: String,
    pub(super) groups: HashMap<String, String>,
    pub(super) person_properties: HashMap<String, Value>,
    pub(super) group_properties: HashMap<String, HashMap<String, Value>>,
    pub(super) disable_geoip: bool,
}

impl FlagContext {
    pub(super) fn current(snapshot: &Snapshot, disable_geoip: bool) -> Self {
        Self {
            distinct_id: snapshot.distinct_id.clone(),
            anonymous_id: snapshot.anonymous_id.clone(),
            groups: snapshot.groups.clone(),
            person_properties: snapshot.flags.person_properties.clone(),
            group_properties: snapshot.flags.group_properties.clone(),
            disable_geoip,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FlagCache {
    pub(super) context: FlagContext,
    pub(super) flags: HashMap<String, CachedFlag>,
}

/// Reporting metadata belongs to each retained record: a partial response cannot
/// relabel older records with the new response's request ID or minimization gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CachedFlag {
    #[serde(with = "shared_context")]
    pub(super) context: Arc<FlagContext>,
    pub(super) revision: u64,
    pub(super) partial: bool,
    pub(super) record: EvaluatedFlagRecord,
    pub(super) request_id: Option<String>,
    pub(super) evaluated_at: Option<i64>,
}

// Share a response's context in memory without changing the default/server serde
// feature graph. On disk each record remains self-contained.
mod shared_context {
    use super::*;
    pub(super) fn serialize<S: serde::Serializer>(
        context: &Arc<FlagContext>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        context.as_ref().serialize(serializer)
    }
    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<FlagContext>, D::Error> {
        FlagContext::deserialize(deserializer).map(Arc::new)
    }
}

mod shared_cache {
    use super::*;

    pub(super) fn serialize<S: serde::Serializer>(
        cache: &Option<Arc<FlagCache>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        cache.as_deref().serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Arc<FlagCache>>, D::Error> {
        Option::<FlagCache>::deserialize(deserializer).map(|cache| cache.map(Arc::new))
    }
}

impl FlagCache {
    pub(super) fn within_limits(&self) -> bool {
        self.flags.len() <= MAX_FLAGS
            && serde_json::to_writer(
                &mut RecordBuffer {
                    bytes: Vec::new(),
                    limit: MAX_CACHE_BYTES as u64,
                },
                self,
            )
            .is_ok()
    }

    pub(super) fn updated(
        previous: Option<&Self>,
        context: Arc<FlagContext>,
        revision: u64,
        loaded: LoadedFlags,
    ) -> Result<Self, LoadError> {
        let mut flags = if loaded.partial {
            previous
                .map(|cache| cache.flags.clone())
                .unwrap_or_default()
        } else {
            HashMap::new()
        };
        // Remove every recomputed entry before accounting. A growing replacement
        // must not be charged alongside a shrinking entry it will also replace.
        flags.retain(|key, _| !loaded.records.contains_key(key));
        // Stop while assembling, not only after it: a response-level request ID
        // can otherwise expand into an unbounded number of owned per-key copies.
        let entry_size = |key: &str, flag: &CachedFlag| {
            let mut buffer = RecordBuffer {
                bytes: Vec::new(),
                limit: MAX_CACHE_BYTES as u64,
            };
            serde_json::to_writer(&mut buffer, &(key, flag))
                .map(|_| buffer.bytes.len() - 2)
                .map_err(|_| LoadError::CacheTooLarge)
        };
        let mut bytes = 0usize;
        for (key, flag) in &flags {
            bytes += entry_size(key, flag)?;
        }
        for (key, record) in loaded.records {
            let flag = CachedFlag {
                context: context.clone(),
                revision,
                partial: loaded.partial,
                record,
                request_id: loaded.request_id.clone(),
                evaluated_at: loaded.evaluated_at,
            };
            bytes += entry_size(&key, &flag)?;
            if bytes > MAX_CACHE_BYTES || flags.len() >= MAX_FLAGS {
                return Err(LoadError::CacheTooLarge);
            }
            flags.insert(key, flag);
        }
        let cache = Self {
            context: (*context).clone(),
            flags,
        };
        if cache.within_limits() {
            Ok(cache)
        } else {
            Err(LoadError::CacheTooLarge)
        }
    }
}

impl Snapshot {
    /// All validated mutations participate, including reset and suppressed reloads.
    /// Session activity, consent and analytics-only properties are not flag context.
    pub(super) fn reconcile_flag_revision(&mut self, previous: &Self) -> Option<()> {
        let identity_changed = self.distinct_id != previous.distinct_id
            || self.anonymous_id != previous.anonymous_id
            || self.groups != previous.groups;
        if identity_changed
            || self.flags.person_properties != previous.flags.person_properties
            || self.flags.group_properties != previous.flags.group_properties
        {
            self.flag_load.revision = previous.flag_load.revision.checked_add(1)?;
            self.flag_load.reports.clear();
            if identity_changed {
                self.flags.cache = None;
                self.flag_load.completed = None;
            }
        }
        Some(())
    }

    /// Initialization restores only exact request-context matches. In-process
    /// property-only stale reads are a separate policy from restart hydration.
    pub(super) fn reconcile_flag_cache(&mut self, disable_geoip: bool) -> bool {
        let context = FlagContext::current(self, disable_geoip);
        let Some(cache) = self.flags.cache.as_mut() else {
            return false;
        };
        if cache.context != context || !cache.within_limits() {
            self.flags.cache = None;
            return true;
        }
        // A partial response can retain records evaluated with older targeting.
        // They may be served in-process, but never hydrated as current on restart.
        if cache.flags.values().all(|flag| *flag.context == context) {
            return false;
        }
        Arc::make_mut(cache)
            .flags
            .retain(|_, flag| *flag.context == context);
        true
    }
}
