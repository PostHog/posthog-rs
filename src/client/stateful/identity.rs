use std::collections::HashMap;
use std::sync::atomic::Ordering;

use serde_json::Value;
use sha1::{Digest, Sha1};

use super::producer::CaptureGuard;
use super::writer::UpdateError;
use super::{Capture, CaptureOutcome, Client, Snapshot};

/// Default person processing and permission to use identity/profile operations.
/// Per-event options and before-send hooks retain the ordinary Rust precedence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum PersonProfiles {
    /// Do not process profiles by default; ignore identity/profile/group operations.
    Never,
    /// Process after identification or explicit alias/profile/group operations.
    #[default]
    IdentifiedOnly,
    /// Process all events by default, including anonymous events.
    Always,
}

impl PersonProfiles {
    pub(super) fn processes(self, state: &Snapshot) -> bool {
        match self {
            Self::Never => false,
            Self::IdentifiedOnly => state.identified || state.person_processing,
            Self::Always => true,
        }
    }
}

/// Identification input. String shorthand supplies only the distinct ID.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Identify {
    /// Nonblank user ID. Switching an already-identified user requires reset first.
    pub distinct_id: String,
    /// Person properties to set, including null values.
    pub set: HashMap<String, Value>,
    /// Person properties to set only if absent.
    pub set_once: HashMap<String, Value>,
}

impl Identify {
    /// Identify this ID without additional person properties.
    pub fn new(distinct_id: impl Into<String>) -> Self {
        Self {
            distinct_id: distinct_id.into(),
            ..Self::default()
        }
    }
}

impl From<&str> for Identify {
    fn from(id: &str) -> Self {
        Self::new(id)
    }
}

impl From<String> for Identify {
    fn from(id: String) -> Self {
        Self::new(id)
    }
}

impl Client {
    /// Whether local state is identified. Reads memory only; false when disabled.
    pub fn is_identified(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| state.snapshot().identified)
    }

    fn identity_guard(&self) -> Option<CaptureGuard> {
        if self.is_disabled()
            || self.closed.load(Ordering::Acquire)
            || self.producer.as_ref()?.person_profiles == PersonProfiles::Never
        {
            return None;
        }
        CaptureGuard::enter(self).ok()
    }

    /// Update identity synchronously, then attempt its event through ordinary capture.
    /// A different already-identified ID is ignored: call reset before switching users.
    /// Dropped/full events do not roll back local state or schedule linking-event replay.
    /// Properties also merge into local flag targeting before hooks, independently
    /// of analytics dedup/admission. Anonymous identity changes reload flags; same-ID
    /// promotion reloads only with properties. Identified property updates do not.
    /// Persistence and delivery are asynchronous; this is not an ingestion acknowledgment.
    pub fn identify(&self, input: impl Into<Identify>) {
        self.identify_with_reload(input, true);
    }

    fn identify_with_reload(&self, input: impl Into<Identify>, reload: bool) {
        let Some(_guard) = self.identity_guard() else {
            return;
        };
        let input = input.into();
        if input.distinct_id.trim().is_empty() {
            tracing::warn!("posthog-rs: identity update requires a nonblank distinct ID");
            return;
        }
        let fingerprint = properties_hash(&input.set, &input.set_once);
        let has_properties = !input.set.is_empty() || !input.set_once.is_empty();
        let mut reset_required = false;
        let result = self.state.as_ref().unwrap().mutate(|state| {
            if state.identified && state.distinct_id != input.distinct_id {
                reset_required = true;
                return None;
            }
            let was_anonymous = !state.identified;
            let changed = state.distinct_id != input.distinct_id;
            let old_id = state.distinct_id.clone();
            // Local targeting is independent of analytics admission and dedup.
            state.flags.mirror_person(&input.set, &input.set_once);
            if was_anonymous {
                state.distinct_id = input.distinct_id;
                if changed {
                    // Invalidate under the same lock, before bootstrap or ordinary
                    // identify hooks can observe the new identity.
                    state.flags.cache = None;
                }
                state.identified = true;
                state.person_processing = true;
                state.generation = state.generation.wrapping_add(1);
                state.person_properties = None;
            } else if !has_properties || state.person_properties == Some(fingerprint) {
                return None;
            }
            let mut event = person_event(
                if was_anonymous && changed {
                    "$identify"
                } else {
                    "$set"
                },
                input.set,
                input.set_once,
            );
            if was_anonymous && changed {
                event
                    .properties
                    .insert("$anon_distinct_id".into(), old_id.into());
            }
            Some((
                event,
                state.clone(),
                was_anonymous && (changed || has_properties),
            ))
        });
        if reset_required {
            tracing::warn!("posthog-rs: reset before identifying a different user");
        }
        match result {
            Ok((_, Some((event, snapshot, refresh)))) => {
                if reload && refresh {
                    self.reload_feature_flags();
                }
                self.capture_person(event, &snapshot, Some(fingerprint));
            }
            Err(UpdateError::Full) => {
                tracing::warn!("posthog-rs: identity update exceeds client-state capacity");
            }
            Err(UpdateError::Invalid) => {
                tracing::warn!("posthog-rs: identity update cannot be encoded in client state");
            }
            _ => {}
        }
    }

    /// Link an alias to the current identity without changing either local ID.
    pub fn alias(&self, alias: impl Into<String>) {
        let Some(_guard) = self.identity_guard() else {
            return;
        };
        let alias = alias.into();
        if alias.trim().is_empty() {
            return;
        }
        let result = self.state.as_ref().unwrap().mutate(|state| {
            if self.snapshot_opted_out(state) {
                return None;
            }
            state.person_processing = true;
            Some(state.clone())
        });
        if let Ok((_, Some(snapshot))) = result {
            let mut event = Capture::new("$create_alias");
            event.properties.insert("alias".into(), alias.into());
            event
                .properties
                .insert("distinct_id".into(), snapshot.distinct_id.clone().into());
            self.capture_person(event, &snapshot, None);
        }
    }

    /// Update the current person's properties without identifying. Empty maps are a no-op.
    /// Only accepted analytics updates are deduplicated. Local flag targeting merges
    /// set-once absent keys, then set values, even when analytics is deduplicated or
    /// rejected. It does not immediately reload flags. State enablement survives rejection.
    pub fn set_person_properties(
        &self,
        set: HashMap<String, Value>,
        set_once: HashMap<String, Value>,
    ) {
        let Some(_guard) = self.identity_guard() else {
            return;
        };
        if set.is_empty() && set_once.is_empty() {
            return;
        }
        let fingerprint = properties_hash(&set, &set_once);
        let result = self.state.as_ref().unwrap().mutate(|state| {
            state.flags.mirror_person(&set, &set_once);
            if state.person_properties == Some(fingerprint) {
                return None;
            }
            state.person_processing = true;
            Some(state.clone())
        });
        if let Ok((_, Some(snapshot))) = result {
            self.capture_person(
                person_event("$set", set, set_once),
                &snapshot,
                Some(fingerprint),
            );
        }
    }

    /// Enable person processing without identifying, attempting an empty `$set` once.
    /// Already-enabled processing is a no-op, even if its original event was dropped.
    pub fn create_person_profile(&self) {
        let Some(_guard) = self.identity_guard() else {
            return;
        };
        let mode = self.producer.as_ref().unwrap().person_profiles;
        let result = self.state.as_ref().unwrap().mutate(|state| {
            if self.snapshot_opted_out(state) || mode.processes(state) {
                return None;
            }
            state.person_processing = true;
            Some(state.clone())
        });
        if let Ok((_, Some(snapshot))) = result {
            self.capture_person(
                person_event("$set", HashMap::new(), HashMap::new()),
                &snapshot,
                None,
            );
        }
    }

    /// Start a fresh anonymous identity, including a fresh device ID. Emits no event
    /// and preserves queued events. Clears registered properties, groups and explicit
    /// consent, restoring Config::opt_out; with the normal default, reset re-enables
    /// collection. Setup bootstrap is not
    /// reapplied. A hook may reset:
    /// its already-snapshotted outer event retains the old identity.
    pub fn reset(&self) {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return;
        }
        let id = uuid::Uuid::now_v7().to_string();
        let result = self.state.as_ref().unwrap().mutate(|state| {
            let generation = state.generation.wrapping_add(1);
            let context_revision = state.context_revision.wrapping_add(1);
            let sessions = state.sessions.cleared();
            *state = Snapshot::anonymous(id);
            state.sessions = sessions;
            state.generation = generation;
            state.context_revision = context_revision;
        });
        if result.is_ok() {
            self.reload_feature_flags();
        }
    }

    fn capture_person(&self, event: Capture, snapshot: &Snapshot, fingerprint: Option<[u8; 20]>) {
        if matches!(
            self.capture_snapshot(event, snapshot),
            CaptureOutcome::Accepted { .. }
        ) {
            if let Some(fingerprint) = fingerprint {
                let _ = self.state.as_ref().unwrap().mutate(|state| {
                    if state.generation == snapshot.generation {
                        state.person_properties = Some(fingerprint);
                    }
                });
            }
        }
    }

    pub(super) fn reconcile_identified_bootstrap(&self, id: &str) {
        let current = self.state.as_ref().unwrap().snapshot();
        if current.identified {
            if current.distinct_id != id {
                tracing::warn!("posthog-rs: bootstrap differs from the identified user; reset before switching");
            }
        } else if current.distinct_id == id {
            let _ = self.state.as_ref().unwrap().mutate(|state| {
                state.identified = true;
                state.person_processing = true;
                state.generation = state.generation.wrapping_add(1);
                state.person_properties = None;
            });
        } else {
            // Reconcile setup identity without an intermediate flags request.
            self.identify_with_reload(id, false);
        }
    }
}

fn person_event(
    name: &str,
    set: HashMap<String, Value>,
    set_once: HashMap<String, Value>,
) -> Capture {
    let mut event = Capture::new(name);
    event
        .properties
        .insert("$set".into(), Value::Object(set.into_iter().collect()));
    event.properties.insert(
        "$set_once".into(),
        Value::Object(set_once.into_iter().collect()),
    );
    event
}

/// Stable, constant-size fingerprint. Traverse iteratively, sorting object keys;
/// retain array order and explicit nulls. No full serialized property copy is kept.
fn properties_hash(set: &HashMap<String, Value>, set_once: &HashMap<String, Value>) -> [u8; 20] {
    enum Part<'a> {
        Value(&'a Value),
        Key(&'a String),
    }
    let mut hash = Sha1::new();
    for map in [set, set_once] {
        hash.update((map.len() as u64).to_le_bytes());
        let mut keys: Vec<_> = map.keys().collect();
        keys.sort_unstable();
        let mut pending = Vec::new();
        for key in keys.into_iter().rev() {
            pending.push(Part::Value(&map[key]));
            pending.push(Part::Key(key));
        }
        while let Some(part) = pending.pop() {
            match part {
                Part::Key(text) | Part::Value(Value::String(text)) => {
                    hash.update(b"s");
                    hash.update((text.len() as u64).to_le_bytes());
                    hash.update(text.as_bytes());
                }
                Part::Value(Value::Object(object)) => {
                    hash.update(b"o");
                    hash.update((object.len() as u64).to_le_bytes());
                    let mut keys: Vec<_> = object.keys().collect();
                    keys.sort_unstable();
                    for key in keys.into_iter().rev() {
                        pending.push(Part::Value(&object[key]));
                        pending.push(Part::Key(key));
                    }
                }
                Part::Value(Value::Array(array)) => {
                    hash.update(b"a");
                    hash.update((array.len() as u64).to_le_bytes());
                    pending.extend(array.iter().rev().map(Part::Value));
                }
                Part::Value(value) => {
                    let text = value.to_string();
                    hash.update(b"v");
                    hash.update((text.len() as u64).to_le_bytes());
                    hash.update(text.as_bytes());
                }
            }
        }
    }
    hash.finalize().into()
}
