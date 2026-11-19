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
    /// Per-event properties. These take precedence over registered context,
    /// remembered groups, platform context and SDK defaults using Rust's fill rules.
    pub properties: HashMap<String, Value>,
    /// Explicit wire options, with the same precedence as the server Event API.
    /// With the default configuration, a fresh anonymous client sends
    /// process_person_profile=false. Set this option to true to enable processing
    /// for this event.
    pub options: HashMap<String, Value>,
    /// Typed groups override entries of the same type in properties.$groups.
    /// Group analytics requires person processing; typed groups alone do not
    /// enable it. When needed, set
    /// `capture.options.insert("process_person_profile".into(), true.into())`.
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

/// A `$screen` event input. Start with [`Screen::new`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Screen {
    /// Nonblank screen name, sent as `$screen_name`.
    pub name: String,
    /// Per-event properties. The name wins over a conflicting `$screen_name`.
    pub properties: HashMap<String, Value>,
    /// Explicit wire options, as on [`Capture::options`].
    pub options: HashMap<String, Value>,
}

impl Screen {
    /// Create a screen input with default optional fields.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}

impl From<String> for Screen {
    fn from(name: String) -> Self {
        Self::new(name)
    }
}

impl From<&str> for Screen {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}

/// The person-profiles mode fills `process_person_profile` only when the caller
/// set neither the option nor a `$process_person_profile` property, as the
/// mobile SDKs do: an explicit per-event choice always wins.
fn fill_person_profile_option(event: &mut crate::Event, process: bool) {
    if event
        .properties()
        .get("$process_person_profile")
        .is_some_and(|value| !value.is_null())
    {
        return;
    }
    event.insert_option_default(PROCESS_PERSON_PROFILE_OPT, process.into());
}

/// Local admission only: acceptance is not a persistence or ingestion acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CaptureOutcome {
    /// Admitted with this final event UUID; delivery and persistence are asynchronous.
    Accepted { uuid: Uuid },
    /// Configuration or local startup disabled the client.
    Disabled,
    /// Collection was opted out when the event's state snapshot was taken.
    OptedOut,
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
    pub(super) default_opt_out: bool,
    pub(super) session_clock: std::sync::Arc<dyn super::sessions::SessionClock>,
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
            default_opt_out: config.opt_out,
            session_clock: std::sync::Arc::new(super::sessions::SystemSessionClock),
        }
    }

    fn prepare(
        &self,
        input: Capture,
        snapshot: &Snapshot,
        now: DateTime<Utc>,
        session_id: Option<Uuid>,
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
        for (key, value) in &snapshot.registered {
            event.insert_prop_default(key.clone(), value.clone());
        }
        if !snapshot.groups.is_empty() {
            event.insert_prop_default(
                "$groups",
                Value::Object(
                    snapshot
                        .groups
                        .iter()
                        .map(|(kind, key)| (kind.clone(), key.clone().into()))
                        .collect(),
                ),
            );
        }
        for (key, value) in &self.context {
            event.insert_prop_default(key.clone(), value.clone());
        }
        if let Some(id) = session_id {
            event.insert_prop_default("$session_id", id.to_string().into());
        }
        // Fill $lib and $lib_version as a pair, as the server client does, so a
        // caller's own $lib never gets this SDK's version.
        let lib = event.properties().get("$lib");
        if !crate::event::names_a_library(lib) {
            event.set_prop("$lib", self.sdk_name.clone().into());
            event.set_prop("$lib_version", self.sdk_version.clone().into());
        } else if lib.and_then(serde_json::Value::as_str) == Some(self.sdk_name.as_str()) {
            event.insert_prop_default("$lib_version", self.sdk_version.clone().into());
        }
        event.insert_prop_default("$device_id", snapshot.device_id.clone().into());
        event.insert_prop_default("$is_identified", snapshot.identified.into());
        fill_person_profile_option(&mut event, self.person_profiles.processes(snapshot));
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
    /// Session activity is committed only when local admission succeeds.
    pub fn capture(&self, input: impl Into<Capture>) -> CaptureOutcome {
        if self.is_disabled() {
            return CaptureOutcome::Disabled;
        }
        if self.closed.load(Ordering::Acquire) {
            return CaptureOutcome::Closed;
        }
        if self.snapshot_opted_out(&self.state.as_ref().unwrap().snapshot()) {
            return CaptureOutcome::OptedOut;
        }
        let _guard = match CaptureGuard::enter(self) {
            Ok(guard) => guard,
            Err(outcome) => return outcome,
        };
        let input = input.into();
        if input.event.trim().is_empty() {
            return CaptureOutcome::Invalid;
        }
        let snapshot = self.state.as_ref().unwrap().mutate(|snapshot| {
            let opted_out = self.snapshot_opted_out(snapshot);
            let now = self.producer.as_ref().unwrap().session_clock.now();
            let session = (!opted_out).then(|| snapshot.sessions.prepare_admission(now));
            (snapshot.clone(), session)
        });
        let Ok((_, (snapshot, session))) = snapshot else {
            return if self.closed.load(Ordering::Acquire) {
                CaptureOutcome::Closed
            } else {
                CaptureOutcome::Full
            };
        };
        self.capture_with_session(input, &snapshot, session, true)
    }

    /// Passive identity/group events may create attribution, without touching idle time.
    /// The caller owns the recursion guard and the operation's identity snapshot.
    pub(super) fn capture_snapshot(&self, input: Capture, snapshot: &Snapshot) -> CaptureOutcome {
        let session = self.prepare_admission_session(snapshot.generation);
        self.capture_with_session(input, snapshot, session, false)
    }

    /// Select attribution for an event built from an earlier identity snapshot.
    /// A reset since then means the current session belongs to the next user,
    /// so the event is sent without a session instead.
    pub(super) fn prepare_admission_session(
        &self,
        generation: u64,
    ) -> Option<super::sessions::AdmissionSession> {
        let now = self.producer.as_ref().unwrap().session_clock.now();
        self.state
            .as_ref()
            .unwrap()
            .mutate(|snapshot| {
                (snapshot.generation == generation && !self.snapshot_opted_out(snapshot))
                    .then(|| snapshot.sessions.prepare_admission(now))
            })
            .ok()
            .and_then(|(_, token)| token)
    }

    /// Attribution was selected with the operation's snapshot. Do not recheck
    /// expiration here: crossing a boundary cannot erase the selected session.
    fn capture_with_session(
        &self,
        input: Capture,
        snapshot: &Snapshot,
        session: Option<super::sessions::AdmissionSession>,
        activity: bool,
    ) -> CaptureOutcome {
        let wire =
            match self.prepare_with_session(input, snapshot, session.and_then(|token| token.id)) {
                Ok(wire) => wire,
                Err(outcome) => return outcome,
            };
        self.admit_wire(wire, session, activity)
    }

    fn admit_wire(
        &self,
        wire: CaptureEvent,
        session: Option<super::sessions::AdmissionSession>,
        activity: bool,
    ) -> CaptureOutcome {
        let uuid = wire.uuid;
        let now = self.producer.as_ref().unwrap().session_clock.now();
        let mut admission = None;
        // Measure before taking the state lock, so large events do not extend it.
        let bytes = crate::client::event_store::record_size(&wire);
        self.state.as_ref().unwrap().admit_capture(
            session.map(|token| (token, now, activity)),
            None,
            || {
                let result = self
                    .transport
                    .as_ref()
                    .unwrap()
                    .enqueue_measured_deferred(wire, bytes);
                let accepted = result.admission == Admission::Accepted;
                admission = Some(result);
                accepted
            },
        );
        match admission.map(|result| result.report()) {
            Some(Admission::Accepted) => CaptureOutcome::Accepted { uuid },
            Some(Admission::Closed) => CaptureOutcome::Closed,
            _ if self.closed.load(Ordering::Acquire) => CaptureOutcome::Closed,
            _ => CaptureOutcome::Full,
        }
    }

    /// Prepare outside SDK locks; hooks can inspect, reset, or close the client.
    pub(super) fn prepare_snapshot(
        &self,
        input: Capture,
        snapshot: &Snapshot,
        session: Option<super::sessions::AdmissionSession>,
    ) -> Result<CaptureEvent, CaptureOutcome> {
        self.prepare_with_session(input, snapshot, session.and_then(|token| token.id))
    }

    fn prepare_with_session(
        &self,
        input: Capture,
        snapshot: &Snapshot,
        session_id: Option<Uuid>,
    ) -> Result<CaptureEvent, CaptureOutcome> {
        if self.closed.load(Ordering::Acquire) {
            return Err(CaptureOutcome::Closed);
        }
        if self.snapshot_opted_out(snapshot) {
            return Err(CaptureOutcome::OptedOut);
        }
        let wire =
            self.producer
                .as_ref()
                .unwrap()
                .prepare(input, snapshot, Utc::now(), session_id)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(CaptureOutcome::Closed);
        }
        Ok(wire)
    }

    /// Capture `$screen` through the ordinary event pipeline. The explicit name
    /// wins over a conflicting `$screen_name` property before hooks run.
    pub fn screen(&self, screen: impl Into<Screen>) -> CaptureOutcome {
        if self.is_disabled() {
            return CaptureOutcome::Disabled;
        }
        if self.closed.load(Ordering::Acquire) {
            return CaptureOutcome::Closed;
        }
        let Screen {
            name,
            mut properties,
            options,
        } = screen.into();
        if name.trim().is_empty() {
            return CaptureOutcome::Invalid;
        }
        properties.insert("$screen_name".into(), name.into());
        let mut event = Capture::new("$screen");
        event.properties = properties;
        event.options = options;
        self.capture(event)
    }
}
