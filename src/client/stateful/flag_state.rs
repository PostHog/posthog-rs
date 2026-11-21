//! Persisted targeting and optional result data share the client-state owner.
//! Cached results carry their producing context, never an inferred current user.

use std::collections::{HashMap, HashSet};
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
    // Shared until a report changes it, so cloning a snapshot stays cheap.
    pub(super) reports: Arc<super::flag_report::ReportTracker>,
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

// Like `registered` and `groups`, a missing field or a value this build can't
// read or accept restores empty, so it never discards the identity and consent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FlagState {
    #[serde(default, deserialize_with = "lenient_person_properties")]
    pub(super) person_properties: HashMap<String, Value>,
    #[serde(default, deserialize_with = "lenient_group_properties")]
    pub(super) group_properties: HashMap<String, HashMap<String, Value>>,
    // Immutable evaluation data is shared by otherwise independent state snapshots.
    #[serde(default, with = "shared_cache")]
    pub(super) cache: Option<Arc<FlagCache>>,
}

impl FlagState {
    pub(super) fn is_valid(&self) -> bool {
        valid_person_properties(&self.person_properties)
            && valid_group_properties(&self.group_properties)
    }
}

fn valid_person_properties(properties: &HashMap<String, Value>) -> bool {
    super::within_depth(properties.values())
}

fn valid_group_properties(properties: &HashMap<String, HashMap<String, Value>>) -> bool {
    properties.keys().all(|key| !key.trim().is_empty())
        && super::within_depth(properties.values().flat_map(HashMap::values))
}

/// Saved person targeting, or none when it can't be read or is too deep.
fn lenient_person_properties<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<HashMap<String, Value>, D::Error> {
    let properties: HashMap<String, Value> =
        serde_json::from_value(Value::deserialize(deserializer)?).unwrap_or_default();
    Ok(if valid_person_properties(&properties) {
        properties
    } else {
        HashMap::new()
    })
}

/// Saved group targeting, or none when it can't be read, has a blank group
/// type or is too deep.
fn lenient_group_properties<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<HashMap<String, HashMap<String, Value>>, D::Error> {
    let properties: HashMap<String, HashMap<String, Value>> =
        serde_json::from_value(Value::deserialize(deserializer)?).unwrap_or_default();
    Ok(if valid_group_properties(&properties) {
        properties
    } else {
        HashMap::new()
    })
}

/// Saved flag state, or the default when it isn't an object this build can read.
pub(super) fn lenient_flags<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<FlagState, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(deserializer)?).unwrap_or_default())
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
    pub(super) context: Arc<FlagContext>,
    pub(super) flags: HashMap<String, CachedFlag>,
    /// Keys whose latest record failed to evaluate. A read with no value for
    /// one reports `evaluation_failed` instead of `flag_missing`.
    pub(super) failed: HashSet<String>,
    /// The latest response's `errorsWhileComputingFlags`. As in posthog-ios and
    /// posthog-android, every read reports it, whichever record it finds.
    pub(super) errors_while_computing: bool,
}

/// Reporting metadata belongs to each retained record: a partial response cannot
/// relabel older records with the new response's request ID or minimization gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CachedFlag {
    pub(super) context: Arc<FlagContext>,
    pub(super) partial: bool,
    pub(super) record: EvaluatedFlagRecord,
    pub(super) request_id: Option<String>,
    pub(super) evaluated_at: Option<i64>,
}

impl CachedFlag {
    /// Whether reads see this flag as enabled: an empty variant is not.
    pub(super) fn served(&self) -> bool {
        self.record.enabled && self.record.variant.as_deref() != Some("")
    }
}

// On disk each distinct evaluation context is written once. `contexts[0]` is
// the cache's own context; every flag refers to its producing context by index.
#[derive(Serialize)]
struct StoredCacheRef<'a> {
    contexts: Vec<&'a FlagContext>,
    flags: HashMap<&'a str, StoredFlagRef<'a>>,
    #[serde(skip_serializing_if = "HashSet::is_empty")]
    failed: &'a HashSet<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    errors_while_computing: bool,
}

#[derive(Serialize)]
struct StoredFlagRef<'a> {
    context: usize,
    partial: bool,
    record: &'a EvaluatedFlagRecord,
    request_id: &'a Option<String>,
    evaluated_at: Option<i64>,
}

#[derive(Deserialize)]
struct StoredCache {
    contexts: Vec<FlagContext>,
    flags: HashMap<String, StoredFlag>,
    #[serde(default)]
    failed: HashSet<String>,
    #[serde(default)]
    errors_while_computing: bool,
}

#[derive(Deserialize)]
struct StoredFlag {
    context: usize,
    partial: bool,
    record: EvaluatedFlagRecord,
    request_id: Option<String>,
    evaluated_at: Option<i64>,
}

impl Serialize for FlagCache {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut contexts = vec![&*self.context];
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
                        partial: flag.partial,
                        record: &flag.record,
                        request_id: &flag.request_id,
                        evaluated_at: flag.evaluated_at,
                    },
                )
            })
            .collect();
        StoredCacheRef {
            contexts,
            flags,
            failed: &self.failed,
            errors_while_computing: self.errors_while_computing,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FlagCache {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = StoredCache::deserialize(deserializer)?;
        let contexts: Vec<Arc<FlagContext>> = stored.contexts.into_iter().map(Arc::new).collect();
        let context = contexts
            .first()
            .cloned()
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
                        partial: flag.partial,
                        record: flag.record,
                        request_id: flag.request_id,
                        evaluated_at: flag.evaluated_at,
                    },
                ))
            })
            .collect::<Result<_, D::Error>>()?;
        Ok(Self {
            context,
            flags,
            failed: stored.failed,
            errors_while_computing: stored.errors_while_computing,
        })
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
        loaded: LoadedFlags,
    ) -> Self {
        let (mut flags, mut failed) = if loaded.partial {
            previous
                .map(|cache| (cache.flags.clone(), cache.failed.clone()))
                .unwrap_or_default()
        } else {
            Default::default()
        };
        // A partial response keeps an earlier value for a failed key, as
        // posthog-ios does: the failure is only a diagnostic.
        failed.extend(loaded.failed);
        for (key, record) in loaded.records {
            failed.remove(&key);
            flags.insert(
                key,
                CachedFlag {
                    context: context.clone(),
                    partial: loaded.partial,
                    record,
                    request_id: loaded.request_id.clone(),
                    evaluated_at: loaded.evaluated_at,
                },
            );
        }
        Self {
            context: context.clone(),
            flags,
            failed,
            errors_while_computing: loaded.partial,
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
                self.flag_load.reports = Arc::new(previous.flag_load.reports.cleared());
            }
            // The cached flags stay until the new identity's response
            // replaces them, as in posthog-ios, posthog-android and posthog-js.
            if identity_changed {
                self.flag_load.completed = None;
            }
        }
        Some(())
    }
}
