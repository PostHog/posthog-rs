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
    /// Per-capture fallback properties, filled after registered properties,
    /// platform context and SDK metadata using the ordinary Rust fill rules.
    /// Hooks see the assembled result and can change or remove these values.
    /// These properties apply only to this capture; they are not registered.
    pub fallback_properties: HashMap<String, Value>,
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

#[derive(Clone, Copy)]
enum CapturePolicy {
    Standard,
    FlagCalled { minimal: bool },
}

#[derive(Clone)]
pub(crate) struct Producer {
    capture_key: std::sync::Arc<()>,
    context: HashMap<String, Value>,
    sdk_name: String,
    sdk_version: String,
    disable_geoip: bool,
    hooks: Vec<BeforeSendHook>,
    pub(super) person_profiles: PersonProfiles,
    pub(super) default_opt_out: bool,
    pub(super) send_feature_flag_event: bool,
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
            capture_key: std::sync::Arc::new(()),
            context,
            sdk_name: config.sdk_name.clone(),
            sdk_version: config.sdk_version.clone(),
            disable_geoip: config.disable_geoip,
            hooks: config.before_send.clone(),
            person_profiles: config.person_profiles,
            default_opt_out: config.opt_out,
            send_feature_flag_event: config.send_feature_flag_event,
            session_clock: std::sync::Arc::new(super::sessions::SystemSessionClock),
        }
    }

    #[cfg(test)]
    fn prepare(
        &self,
        input: Capture,
        snapshot: &Snapshot,
        now: DateTime<Utc>,
        session_id: Option<Uuid>,
    ) -> Result<CaptureEvent, CaptureOutcome> {
        self.prepare_with_policy(input, snapshot, now, session_id, CapturePolicy::Standard)
    }

    fn prepare_with_policy(
        &self,
        input: Capture,
        snapshot: &Snapshot,
        now: DateTime<Utc>,
        session_id: Option<Uuid>,
        policy: CapturePolicy,
    ) -> Result<CaptureEvent, CaptureOutcome> {
        let mut event = self.assemble(input, snapshot, now, session_id)?;
        if matches!(policy, CapturePolicy::FlagCalled { minimal: true }) {
            event.mark_minimal_flag_called();
            event.apply_minimal_flag_called_allowlist();
        }
        let mut wire = CaptureEvent::from_stateful_event_at(&event, now);
        normalize_custom_properties(
            &mut wire.properties,
            matches!(policy, CapturePolicy::FlagCalled { .. }),
        );
        Ok(wire)
    }

    pub(super) fn capture_key(&self) -> usize {
        std::sync::Arc::as_ptr(&self.capture_key) as usize
    }

    /// Assemble a native event before selecting its wire protocol.
    pub(super) fn assemble(
        &self,
        input: Capture,
        snapshot: &Snapshot,
        now: DateTime<Utc>,
        session_id: Option<Uuid>,
    ) -> Result<Event, CaptureOutcome> {
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
        for (key, value) in input.fallback_properties {
            event.insert_prop_default(key, value);
        }
        let event = apply_before_send_hooks(&self.hooks, event).ok_or(CaptureOutcome::Dropped)?;
        if event.event_name().trim().is_empty() || event.distinct_id().trim().is_empty() {
            return Err(CaptureOutcome::Invalid);
        }
        Ok(event)
    }
}

// Person/group payloads have their own mutation/type semantics. Wire options and
// session/window fields have already been lifted out of properties by conversion.
fn normalize_custom_properties(properties: &mut Value, flag_called: bool) {
    let Some(properties) = properties.as_object_mut() else {
        return;
    };
    let reserved = |key: &str| {
        matches!(key, "$set" | "$set_once" | "$group_set" | "$groups")
            || (flag_called
                && matches!(
                    key,
                    "$feature_flag_payload" | "$feature_flag_bootstrapped_payload"
                ))
    };
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
    #[cfg(feature = "session-replay")]
    pub(super) fn active(key: usize) -> bool {
        CAPTURING
            .try_with(|active| active.borrow().contains(&key))
            .unwrap_or(true)
    }

    pub(super) fn enter(client: &Client) -> Result<Self, CaptureOutcome> {
        Self::enter_key(client.producer.as_ref().unwrap().capture_key())
    }

    pub(super) fn enter_key(key: usize) -> Result<Self, CaptureOutcome> {
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
        let Some(producer) = &self.producer else {
            return false;
        };
        let key = producer.capture_key();
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
            snapshot.session_operation(super::sessions::Operation::Activity, now, opted_out);
            let session_id = snapshot.sessions.active_id(now.elapsed);
            (snapshot.clone(), session_id)
        });
        let Ok((_, (snapshot, session_id))) = snapshot else {
            return if self.closed.load(Ordering::Acquire) {
                CaptureOutcome::Closed
            } else {
                CaptureOutcome::Full
            };
        };
        self.capture_with_session(input, &snapshot, session_id, CapturePolicy::Standard)
    }

    /// Passive SDK events inspect attribution without maintaining the session.
    /// The caller owns the recursion guard and the operation's identity snapshot.
    pub(super) fn capture_snapshot(&self, input: Capture, snapshot: &Snapshot) -> CaptureOutcome {
        self.capture_snapshot_with_policy(input, snapshot, CapturePolicy::Standard)
    }

    pub(super) fn capture_flag_snapshot(
        &self,
        input: Capture,
        snapshot: &Snapshot,
        minimal: bool,
    ) -> CaptureOutcome {
        self.capture_snapshot_with_policy(input, snapshot, CapturePolicy::FlagCalled { minimal })
    }

    fn capture_snapshot_with_policy(
        &self,
        input: Capture,
        snapshot: &Snapshot,
        policy: CapturePolicy,
    ) -> CaptureOutcome {
        let session_id = snapshot
            .sessions
            .active_id(self.producer.as_ref().unwrap().session_clock.now().elapsed);
        self.capture_with_session(input, snapshot, session_id, policy)
    }

    /// Attribution was selected with the operation's snapshot. Do not recheck
    /// expiration here: crossing a boundary cannot erase the selected session.
    fn capture_with_session(
        &self,
        input: Capture,
        snapshot: &Snapshot,
        session_id: Option<Uuid>,
        policy: CapturePolicy,
    ) -> CaptureOutcome {
        let wire = match self.prepare_with_session(input, snapshot, session_id, policy) {
            Ok(wire) => wire,
            Err(outcome) => return outcome,
        };
        let uuid = wire.uuid;
        match self.transport.as_ref().unwrap().enqueue_prepared(wire) {
            Admission::Accepted => CaptureOutcome::Accepted { uuid },
            Admission::Full => CaptureOutcome::Full,
            Admission::Closed => CaptureOutcome::Closed,
        }
    }

    /// Prepare outside SDK locks; hooks can inspect, reset, or close the client.
    pub(super) fn prepare_snapshot(
        &self,
        input: Capture,
        snapshot: &Snapshot,
    ) -> Result<CaptureEvent, CaptureOutcome> {
        let session_id = snapshot
            .sessions
            .active_id(self.producer.as_ref().unwrap().session_clock.now().elapsed);
        self.prepare_with_session(input, snapshot, session_id, CapturePolicy::Standard)
    }

    fn prepare_with_session(
        &self,
        input: Capture,
        snapshot: &Snapshot,
        session_id: Option<Uuid>,
        policy: CapturePolicy,
    ) -> Result<CaptureEvent, CaptureOutcome> {
        if self.closed.load(Ordering::Acquire) {
            return Err(CaptureOutcome::Closed);
        }
        if self.snapshot_opted_out(snapshot) {
            return Err(CaptureOutcome::OptedOut);
        }
        let wire = self.producer.as_ref().unwrap().prepare_with_policy(
            input,
            snapshot,
            Utc::now(),
            session_id,
            policy,
        )?;
        if self.closed.load(Ordering::Acquire) {
            return Err(CaptureOutcome::Closed);
        }
        Ok(wire)
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
