use std::collections::HashMap;
use std::sync::atomic::Ordering;

use serde_json::Value;
use sha1::{Digest, Sha1};

use super::producer::CaptureGuard;
use super::writer::UpdateError;
use super::{Capture, Client, Snapshot};
use crate::client::transport::Admission;
use crate::constants::PROCESS_PERSON_PROFILE_OPT;

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;

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
    /// Explicit wire options for the identify event, as on [`Capture::options`].
    pub options: HashMap<String, Value>,
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

/// Alias input. String shorthand supplies only the alias.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Alias {
    /// Nonblank ID to link to the current identity.
    pub alias: String,
    /// Explicit wire options for the `$create_alias` event, as on [`Capture::options`].
    pub options: HashMap<String, Value>,
}

impl Alias {
    /// Link this ID without options.
    pub fn new(alias: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
            ..Self::default()
        }
    }
}

impl From<&str> for Alias {
    fn from(alias: &str) -> Self {
        Self::new(alias)
    }
}

impl From<String> for Alias {
    fn from(alias: String) -> Self {
        Self::new(alias)
    }
}

/// Person property update. Start with [`PersonProperties::new`], or pass a map
/// of properties to set.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct PersonProperties {
    /// Person properties to set, including null values.
    pub set: HashMap<String, Value>,
    /// Person properties to set only if absent.
    pub set_once: HashMap<String, Value>,
    /// Explicit wire options for the `$set` event, as on [`Capture::options`].
    pub options: HashMap<String, Value>,
}

impl PersonProperties {
    /// An empty update; fill `set` and `set_once` before passing it.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Map shorthand supplies only the properties to set.
impl From<HashMap<String, Value>> for PersonProperties {
    fn from(set: HashMap<String, Value>) -> Self {
        Self {
            set,
            ..Self::default()
        }
    }
}

impl Client {
    /// Whether local state is identified. Reads memory only; false when disabled.
    #[cfg(test)]
    pub(crate) fn is_identified(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| state.snapshot().identified)
    }

    pub(super) fn identity_guard(&self, operation: &str) -> Option<CaptureGuard> {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return None;
        }
        if self.producer.as_ref()?.person_profiles == PersonProfiles::Never {
            tracing::warn!(
                "posthog-rs: {} is ignored because person profiles are disabled",
                operation
            );
            return None;
        }
        CaptureGuard::enter(self).ok()
    }

    /// Update identity synchronously, then attempt its event through ordinary capture.
    /// A different already-identified ID is ignored: call reset before switching users.
    /// Dropped/full events do not roll back local state or schedule linking-event replay.
    /// Persistence and delivery are asynchronous; this is not an ingestion acknowledgment.
    pub fn identify(&self, input: impl Into<Identify>) {
        let Some(_guard) = self.identity_guard("identify") else {
            return;
        };
        let input = input.into();
        if input.distinct_id.trim().is_empty() {
            tracing::warn!("posthog-rs: identity update requires a nonblank distinct ID");
            return;
        }
        let fingerprint = properties_hash(&input.set, &input.set_once);
        let has_properties = !input.set.is_empty() || !input.set_once.is_empty();
        let deduplicable = surely_processes_person(input.options.get(PROCESS_PERSON_PROFILE_OPT));
        let mut reset_required = false;
        let mut already_identified = false;
        let result = self.state.as_ref().unwrap().mutate(|state| {
            if state.identified && state.distinct_id != input.distinct_id {
                reset_required = true;
                return None;
            }
            let was_anonymous = !state.identified;
            // Set only when an anonymous user becomes a different person.
            let anon_distinct_id = (was_anonymous && state.distinct_id != input.distinct_id)
                .then(|| state.distinct_id.clone());
            if was_anonymous {
                state.identify_as(input.distinct_id);
            } else if !has_properties {
                already_identified = true;
                return None;
            } else if deduplicable && state.person_properties == Some(fingerprint) {
                return None;
            }
            let mut event = person_event(
                if anon_distinct_id.is_some() {
                    "$identify"
                } else {
                    "$set"
                },
                input.set,
                input.set_once,
            );
            event.options = input.options;
            if let Some(old_id) = anon_distinct_id {
                event
                    .properties
                    .insert("$anon_distinct_id".into(), old_id.into());
            }
            Some((event, state.clone(), !was_anonymous))
        });
        if reset_required {
            tracing::warn!("posthog-rs: reset before identifying a different user");
        }
        if already_identified {
            tracing::debug!(
                "posthog-rs: already identified with this distinct ID; nothing to send"
            );
        }
        match result {
            Ok((_, Some((event, snapshot, deduplicate)))) => {
                self.capture_person(event, &snapshot, Some(fingerprint), deduplicate);
            }
            Err(UpdateError::Invalid) => {
                tracing::warn!("posthog-rs: identity update cannot be encoded in client state");
            }
            _ => {}
        }
    }

    /// Link an alias to the current identity without changing either local ID.
    /// A blank alias, or one equal to the current distinct ID, is ignored with a
    /// warning.
    pub fn alias(&self, input: impl Into<Alias>) {
        let Some(_guard) = self.identity_guard("alias") else {
            return;
        };
        let Alias { alias, options } = input.into();
        if alias.trim().is_empty() {
            tracing::warn!("posthog-rs: alias requires a nonblank alias");
            return;
        }
        let mut aliases_self = false;
        let result = self.state.as_ref().unwrap().mutate(|state| {
            if self.snapshot_opted_out(state) {
                return None;
            }
            if state.distinct_id == alias {
                aliases_self = true;
                return None;
            }
            state.person_processing = true;
            Some(state.clone())
        });
        if aliases_self {
            tracing::warn!("posthog-rs: alias equals the current distinct ID; nothing to link");
            return;
        }
        if let Ok((_, Some(snapshot))) = result {
            let mut event = Capture::new("$create_alias");
            event.options = options;
            event.properties.insert("alias".into(), alias.into());
            event
                .properties
                .insert("distinct_id".into(), snapshot.distinct_id.clone().into());
            self.capture_person(event, &snapshot, None, false);
        }
    }

    /// Update the current person's properties without identifying. Empty maps are a no-op.
    /// Only accepted updates are deduplicated. State enablement survives capture rejection.
    pub fn set_person_properties(&self, input: impl Into<PersonProperties>) {
        let Some(_guard) = self.identity_guard("set_person_properties") else {
            return;
        };
        let PersonProperties {
            set,
            set_once,
            options,
        } = input.into();
        if set.is_empty() && set_once.is_empty() {
            return;
        }
        let fingerprint = properties_hash(&set, &set_once);
        let deduplicable = surely_processes_person(options.get(PROCESS_PERSON_PROFILE_OPT));
        let result = self.state.as_ref().unwrap().mutate(|state| {
            if deduplicable && state.person_properties == Some(fingerprint) {
                return None;
            }
            state.person_processing = true;
            Some(state.clone())
        });
        if let Ok((_, Some(snapshot))) = result {
            let mut event = person_event("$set", set, set_once);
            event.options = options;
            self.capture_person(event, &snapshot, Some(fingerprint), true);
        }
    }

    /// Enable person processing without identifying, attempting an empty `$set` once.
    /// Already-enabled processing is a no-op, even if its original event was dropped.
    /// While opted out, processing is still enabled but no event is sent, as in
    /// posthog-js.
    pub fn create_person_profile(&self) {
        let Some(_guard) = self.identity_guard("create_person_profile") else {
            return;
        };
        let mode = self.producer.as_ref().unwrap().person_profiles;
        let result = self.state.as_ref().unwrap().mutate(|state| {
            if mode.processes(state) {
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
                false,
            );
        }
    }

    /// Start a fresh anonymous identity, preserving the device ID. Clears the
    /// identified and person-processing state, and the last screen name, so
    /// `$screen_name` is no longer added until the next `screen`. Emits no event
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
        let _ = self.state.as_ref().unwrap().mutate(|state| {
            let generation = state.generation.wrapping_add(1);
            let device_id = state.device_id.clone();
            *state = Snapshot::anonymous(id);
            state.device_id = device_id;
            state.generation = generation;
        });
        self.producer.as_ref().unwrap().clear_last_screen();
    }

    fn capture_person(
        &self,
        event: Capture,
        snapshot: &Snapshot,
        fingerprint: Option<[u8; 20]>,
        deduplicate: bool,
    ) {
        let Some(fingerprint) = fingerprint else {
            self.capture_snapshot(event, snapshot);
            return;
        };
        let Ok(wire) = self.prepare_snapshot(event, snapshot) else {
            return;
        };
        // Read the setting as sent, after hooks: an update that may be
        // personless neither checks nor records the hash, and clears it.
        let fingerprint = surely_processes_person(wire.options.get(PROCESS_PERSON_PROFILE_OPT))
            .then_some(fingerprint);
        // Admission runs no user code, so it may run under the state lock.
        self.state.as_ref().unwrap().admit_person(
            snapshot.generation,
            &snapshot.distinct_id,
            fingerprint,
            deduplicate,
            || {
                !self.closed.load(Ordering::Acquire)
                    && self.transport.as_ref().unwrap().enqueue_prepared(wire)
                        == Admission::Accepted
            },
        );
    }

    pub(super) fn reconcile_identified_bootstrap(&self, id: &str) {
        let current = self.state.as_ref().unwrap().snapshot();
        if current.identified {
            if current.distinct_id != id {
                tracing::warn!("posthog-rs: bootstrap differs from the identified user; reset before switching");
            }
        } else if current.distinct_id == id {
            let _ = self
                .state
                .as_ref()
                .unwrap()
                .mutate(|state| state.identify_as(id.into()));
        } else {
            // As in posthog-ios, posthog-android and posthog-js: identify does
            // nothing with person profiles off, so the anonymous user stays.
            self.identify(id);
        }
    }
}

/// Whether an update surely processes the person: its `process_person_profile`
/// option is missing, null or `true`. Only such updates check or record the
/// person-properties hash. Any other value is left for PostHog to read.
fn surely_processes_person(setting: Option<&Value>) -> bool {
    matches!(setting, None | Some(Value::Null | Value::Bool(true)))
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
