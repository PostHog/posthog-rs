use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::Ordering;

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::{Client, Config, PersonProfiles, Snapshot};
use crate::capture_event::CaptureEvent;
use crate::client::common::apply_before_send_hooks;
use crate::client::transport::Admission;
use crate::constants::PROCESS_PERSON_PROFILE_OPT;
use crate::{BeforeSendHook, Event};

#[cfg(test)]
#[path = "producer_tests.rs"]
mod tests;

/// An event input whose identity comes from the stateful client.
/// Start with [`Capture::new`] and set optional fields before capture.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Capture {
    /// Nonblank event name.
    pub event: String,
    /// Per-event properties. These take precedence over platform and SDK defaults.
    pub properties: HashMap<String, Value>,
    /// Explicit wire options, with the same precedence as the server Event API.
    pub options: HashMap<String, Value>,
    /// Typed groups override entries of the same type in properties.$groups.
    pub groups: HashMap<String, String>,
    /// Capture-time override, normalized to UTC on the wire.
    pub timestamp: Option<DateTime<Utc>>,
    /// Deduplication override. Otherwise a UUIDv7 is generated for this capture.
    pub uuid: Option<Uuid>,
}

impl Capture {
    /// Create an event input with default optional fields.
    pub fn new(event: impl Into<String>) -> Self {
        Self {
            event: event.into(),
            ..Self::default()
        }
    }
}

impl From<String> for Capture {
    fn from(event: String) -> Self {
        Self::new(event)
    }
}

impl From<&str> for Capture {
    fn from(event: &str) -> Self {
        Self::new(event)
    }
}

/// Local admission only: acceptance is not a persistence or ingestion acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CaptureOutcome {
    /// Admitted with this final event UUID; delivery and persistence are asynchronous.
    Accepted { uuid: Uuid },
    /// Configuration or local startup disabled the client.
    Disabled,
    /// Client admission is closed or its delivery worker has exited.
    Closed,
    /// The input name or a hook-transformed required field is blank.
    Invalid,
    /// A hook dropped/failed, or guarded privacy processing was unavailable.
    Dropped,
    /// This thread is already capturing through the same client.
    Reentrant,
    /// The resident count or serialized-byte budget refused the event.
    Full,
}

pub(super) struct Producer {
    context: HashMap<String, Value>,
    sdk_name: String,
    sdk_version: String,
    disable_geoip: bool,
    hooks: Vec<BeforeSendHook>,
    pub(super) person_profiles: PersonProfiles,
}

impl Producer {
    /// Environment discovery belongs to initialization, not the capture path.
    pub(super) fn new(config: &Config) -> Self {
        let info = os_info::get();
        let mut context = config.platform_context.clone();
        context
            .entry("$os".into())
            .or_insert_with(|| info.os_type().to_string().into());
        context
            .entry("$os_version".into())
            .or_insert_with(|| info.version().to_string().into());
        Self {
            context,
            sdk_name: config.sdk_name.clone(),
            sdk_version: config.sdk_version.clone(),
            disable_geoip: config.disable_geoip,
            hooks: config.before_send.clone(),
            person_profiles: config.person_profiles,
        }
    }

    fn prepare(
        &self,
        input: Capture,
        snapshot: &Snapshot,
        now: DateTime<Utc>,
    ) -> Result<CaptureEvent, CaptureOutcome> {
        if input.event.trim().is_empty() {
            return Err(CaptureOutcome::Invalid);
        }
        let mut event = Event::from_parts(
            input.event,
            snapshot.distinct_id.clone(),
            input.properties,
            input.options,
            input.groups,
            input.timestamp.map(|time| time.naive_utc()),
            input.uuid,
        );
        event.ensure_timestamp(now);
        event.ensure_uuid();
        for (key, value) in &self.context {
            event.insert_prop_default(key.clone(), value.clone());
        }
        event.insert_prop_default("$lib", self.sdk_name.clone().into());
        event.insert_prop_default("$lib_version", self.sdk_version.clone().into());
        event.insert_prop_default("$device_id", snapshot.anonymous_id.clone().into());
        event.insert_prop_default("$is_identified", snapshot.identified.into());
        event.insert_option_default(
            PROCESS_PERSON_PROFILE_OPT,
            self.person_profiles.processes(snapshot).into(),
        );
        if self.disable_geoip {
            event.insert_prop_default("$geoip_disable", true.into());
        }
        event.merge_groups_into_properties();
        let event = apply_before_send_hooks(&self.hooks, event).ok_or(CaptureOutcome::Dropped)?;
        if event.event_name().trim().is_empty() || event.distinct_id().trim().is_empty() {
            return Err(CaptureOutcome::Invalid);
        }
        let mut wire = CaptureEvent::from_stateful_event_at(&event, now);
        normalize_custom_properties(&mut wire.properties);
        Ok(wire)
    }
}

// Person/group payloads have their own mutation/type semantics. Wire options and
// session/window fields have already been lifted out of properties by conversion.
fn normalize_custom_properties(properties: &mut Value) {
    let Some(properties) = properties.as_object_mut() else {
        return;
    };
    let reserved = |key: &str| matches!(key, "$set" | "$set_once" | "$group_set" | "$groups");
    properties.retain(|key, value| reserved(key) || !value.is_null());
    let mut pending: Vec<_> = properties
        .iter_mut()
        .filter_map(|(key, value)| (!reserved(key)).then_some(value))
        .collect();
    while let Some(value) = pending.pop() {
        match value {
            Value::Object(object) => {
                object.retain(|_, value| !value.is_null());
                pending.extend(object.values_mut());
            }
            Value::Array(array) => pending.extend(array.iter_mut()),
            _ => {}
        }
    }
}

thread_local! {
    static CAPTURING: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

pub(super) struct CaptureGuard(usize);

impl CaptureGuard {
    pub(super) fn enter(client: &Client) -> Result<Self, CaptureOutcome> {
        let key = client as *const Client as usize;
        CAPTURING
            .try_with(|active| {
                let mut active = active.borrow_mut();
                if active.contains(&key) {
                    return Err(CaptureOutcome::Reentrant);
                }
                active.push(key);
                Ok(Self(key))
            })
            .unwrap_or(Err(CaptureOutcome::Dropped))
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        let _ = CAPTURING.try_with(|active| active.borrow_mut().retain(|key| *key != self.0));
    }
}

impl Client {
    pub(super) fn in_capture(&self) -> bool {
        let key = self as *const Client as usize;
        CAPTURING
            .try_with(|active| active.borrow().contains(&key))
            .unwrap_or(false)
    }

    /// Snapshot identity, enrich and run hooks synchronously, then attempt bounded
    /// admission. The SDK does no disk or network I/O on this path. Hooks run on
    /// the calling thread, outside client-state locks. Concurrent calls serialize
    /// each mutable hook; a hook must not wait for another capture of this client.
    /// Recursive same-thread capture is rejected rather than skipping hooks.
    pub fn capture(&self, input: impl Into<Capture>) -> CaptureOutcome {
        if self.is_disabled() {
            return CaptureOutcome::Disabled;
        }
        if self.closed.load(Ordering::Acquire) {
            return CaptureOutcome::Closed;
        }
        let _guard = match CaptureGuard::enter(self) {
            Ok(guard) => guard,
            Err(outcome) => return outcome,
        };
        let snapshot = self.state.as_ref().unwrap().snapshot();
        self.capture_snapshot(input.into(), &snapshot)
    }

    /// The caller owns the recursion guard and the operation's identity snapshot.
    pub(super) fn capture_snapshot(&self, input: Capture, snapshot: &Snapshot) -> CaptureOutcome {
        if self.closed.load(Ordering::Acquire) {
            return CaptureOutcome::Closed;
        }
        let wire = match self
            .producer
            .as_ref()
            .unwrap()
            .prepare(input, snapshot, Utc::now())
        {
            Ok(wire) => wire,
            Err(outcome) => return outcome,
        };
        // A hook can close the client. Its outer capture must not be admitted.
        if self.closed.load(Ordering::Acquire) {
            return CaptureOutcome::Closed;
        }
        let uuid = wire.uuid;
        match self.transport.as_ref().unwrap().enqueue_prepared(wire) {
            Admission::Accepted => CaptureOutcome::Accepted { uuid },
            Admission::Full => CaptureOutcome::Full,
            Admission::Closed => CaptureOutcome::Closed,
        }
    }

    /// Capture `$screen` through the ordinary event pipeline. The explicit name
    /// wins over a conflicting `$screen_name` property before hooks run.
    pub fn screen(
        &self,
        name: impl Into<String>,
        mut properties: HashMap<String, Value>,
    ) -> CaptureOutcome {
        if self.is_disabled() {
            return CaptureOutcome::Disabled;
        }
        if self.closed.load(Ordering::Acquire) {
            return CaptureOutcome::Closed;
        }
        let name = name.into();
        if name.trim().is_empty() {
            return CaptureOutcome::Invalid;
        }
        properties.insert("$screen_name".into(), name.into());
        let mut event = Capture::new("$screen");
        event.properties = properties;
        self.capture(event)
    }
}
