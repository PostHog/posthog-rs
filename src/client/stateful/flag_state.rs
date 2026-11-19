//! Persisted targeting and optional result data share the client-state owner.
//! Cached results carry their producing context, never an inferred current user.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Snapshot;
use crate::client::transport::flags::{LoadError, LoadedFlags};
use crate::feature_flag_evaluations::EvaluatedFlagRecord;

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
            && super::within_depth(self.person_properties.values())
            && super::within_depth(self.group_properties.values().flat_map(HashMap::values))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FlagContext {
    pub(super) distinct_id: String,
    pub(super) anonymous_id: String,
    pub(super) device_id: String,
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
            device_id: snapshot.device_id.clone(),
            groups: snapshot.groups.clone(),
            person_properties: snapshot.flags.person_properties.clone(),
            group_properties: snapshot.flags.group_properties.clone(),
            disable_geoip,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FlagCache {
    pub(super) context: FlagContext,
    pub(super) flags: HashMap<String, CachedFlag>,
}

/// Reporting metadata belongs to each retained record: a partial response cannot
/// relabel older records with the new response's request ID or minimization gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CachedFlag {
    pub(super) context: Arc<FlagContext>,
    pub(super) revision: u64,
    pub(super) partial: bool,
    pub(super) record: EvaluatedFlagRecord,
    pub(super) request_id: Option<String>,
    pub(super) evaluated_at: Option<i64>,
}

// On disk each distinct evaluation context is written once. `contexts[0]` is
// the cache's own context; every flag refers to its producing context by index.
#[derive(Serialize)]
struct StoredCacheRef<'a> {
    contexts: Vec<&'a FlagContext>,
    flags: HashMap<&'a str, StoredFlagRef<'a>>,
}

#[derive(Serialize)]
struct StoredFlagRef<'a> {
    context: usize,
    revision: u64,
    partial: bool,
    record: &'a EvaluatedFlagRecord,
    request_id: &'a Option<String>,
    evaluated_at: Option<i64>,
}

#[derive(Deserialize)]
struct StoredCache {
    contexts: Vec<FlagContext>,
    flags: HashMap<String, StoredFlag>,
}

#[derive(Deserialize)]
struct StoredFlag {
    context: usize,
    revision: u64,
    partial: bool,
    record: EvaluatedFlagRecord,
    request_id: Option<String>,
    evaluated_at: Option<i64>,
}

impl Serialize for FlagCache {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut contexts = vec![&self.context];
        // Flags from one response share an Arc, so each distinct context is
        // compared by value at most once.
        let mut seen: Vec<(*const FlagContext, usize)> = Vec::new();
        let flags = self
            .flags
            .iter()
            .map(|(key, flag)| {
                let pointer = Arc::as_ptr(&flag.context);
                let index = match seen.iter().find(|(seen, _)| *seen == pointer) {
                    Some((_, index)) => *index,
                    None => {
                        let index = contexts
                            .iter()
                            .position(|context| **context == *flag.context)
                            .unwrap_or_else(|| {
                                contexts.push(&flag.context);
                                contexts.len() - 1
                            });
                        seen.push((pointer, index));
                        index
                    }
                };
                (
                    key.as_str(),
                    StoredFlagRef {
                        context: index,
                        revision: flag.revision,
                        partial: flag.partial,
                        record: &flag.record,
                        request_id: &flag.request_id,
                        evaluated_at: flag.evaluated_at,
                    },
                )
            })
            .collect();
        StoredCacheRef { contexts, flags }.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FlagCache {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = StoredCache::deserialize(deserializer)?;
        let contexts: Vec<Arc<FlagContext>> = stored.contexts.into_iter().map(Arc::new).collect();
        let context = contexts
            .first()
            .map(|context| (**context).clone())
            .ok_or_else(|| serde::de::Error::custom("missing flag cache context"))?;
        let flags = stored
            .flags
            .into_iter()
            .map(|(key, flag)| {
                let context = contexts
                    .get(flag.context)
                    .cloned()
                    .ok_or_else(|| serde::de::Error::custom("unknown flag context"))?;
                Ok((
                    key,
                    CachedFlag {
                        context,
                        revision: flag.revision,
                        partial: flag.partial,
                        record: flag.record,
                        request_id: flag.request_id,
                        evaluated_at: flag.evaluated_at,
                    },
                ))
            })
            .collect::<Result<_, D::Error>>()?;
        Ok(Self { context, flags })
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
    pub(super) fn updated(
        previous: Option<&Self>,
        context: Arc<FlagContext>,
        revision: u64,
        loaded: LoadedFlags,
    ) -> Self {
        let mut flags = if loaded.partial {
            previous
                .map(|cache| cache.flags.clone())
                .unwrap_or_default()
        } else {
            HashMap::new()
        };
        for (key, record) in loaded.records {
            flags.insert(
                key,
                CachedFlag {
                    context: context.clone(),
                    revision,
                    partial: loaded.partial,
                    record,
                    request_id: loaded.request_id.clone(),
                    evaluated_at: loaded.evaluated_at,
                },
            );
        }
        Self {
            context: (*context).clone(),
            flags,
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
            if self.distinct_id != previous.distinct_id {
                // Reset may have replaced the snapshot; never rewind reservation tickets.
                self.flag_load.reports = previous.flag_load.reports.cleared();
            }
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
        if cache.context != context {
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
