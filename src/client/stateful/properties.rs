use std::collections::HashMap;
use std::sync::atomic::Ordering;

use serde_json::Value;

use super::producer::CaptureGuard;
use super::{Capture, Client, PersonProfiles};

impl Client {
    /// Register owned context for future events without emitting an event. Each
    /// supplied top-level value replaces the stored value; null is not removal.
    /// Per-event properties win using the ordinary Rust property-fill rules.
    /// Available while opted out. Updates exceeding state byte or codec limits
    /// are rejected as a whole.
    /// Memory changes immediately; persistence is asynchronous.
    pub fn register(&self, properties: HashMap<String, Value>) {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return;
        }
        let _ = self.state.as_ref().unwrap().mutate(|state| {
            if properties
                .iter()
                .any(|(key, value)| state.registered.get(key) != Some(value))
            {
                state.registered.extend(properties);
                state.context_revision = state.context_revision.wrapping_add(1);
            }
        });
    }

    /// Remove one registered property locally, including while opted out.
    /// Already captured events are unchanged; a missing key is a no-op.
    pub fn unregister(&self, key: &str) {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return;
        }
        let _ = self.state.as_ref().unwrap().mutate(|state| {
            if state.registered.remove(key).is_some() {
                state.context_revision = state.context_revision.wrapping_add(1);
            }
        });
    }

    /// Remember one group membership and attempt `$groupidentify` on every valid
    /// call, even for the same key without properties. Enables person processing
    /// without identifying the user. Opted-out/Never/disabled/closed clients no-op.
    /// Membership is visible before hooks run; rejected events do not roll it back
    /// or schedule replay. Persistence and delivery remain asynchronous.
    pub fn group(
        &self,
        group_type: impl Into<String>,
        group_key: impl Into<String>,
        properties: Option<HashMap<String, Value>>,
    ) {
        self.capture_group(group_type, group_key, properties.unwrap_or_default(), true);
    }

    /// Update a group's profile without changing remembered membership. Uses the
    /// current client identity and nests properties under `$group_set`. Like group,
    /// every valid call enables processing and attempts an event unless opted out,
    /// Never, disabled or closed. Rejection does not roll back processing or replay.
    pub fn group_identify(
        &self,
        group_type: impl Into<String>,
        group_key: impl Into<String>,
        properties: HashMap<String, Value>,
    ) {
        self.capture_group(group_type, group_key, properties, false);
    }

    /// Remove remembered membership locally without emitting an event or disabling
    /// person processing. Available while opted out or configured with Never.
    /// Disabled/closed clients no-op; already captured events are unchanged.
    pub fn remove_group(&self, group_type: &str) {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return;
        }
        let _ = self.state.as_ref().unwrap().mutate(|state| {
            if state.groups.remove(group_type).is_some() {
                state.context_revision = state.context_revision.wrapping_add(1);
            }
        });
    }

    fn capture_group(
        &self,
        group_type: impl Into<String>,
        group_key: impl Into<String>,
        properties: HashMap<String, Value>,
        remember: bool,
    ) {
        if self.is_disabled()
            || self.closed.load(Ordering::Acquire)
            || self.producer.as_ref().unwrap().person_profiles == PersonProfiles::Never
        {
            return;
        }
        let Ok(_guard) = CaptureGuard::enter(self) else {
            return;
        };
        let group_type = group_type.into();
        let group_key = group_key.into();
        if group_type.trim().is_empty() || group_key.trim().is_empty() {
            tracing::warn!("posthog-rs: group type and key must be nonblank");
            return;
        }
        let result = self.state.as_ref().unwrap().mutate(|state| {
            if self.snapshot_opted_out(state) {
                return None;
            }
            state.person_processing = true;
            if remember && state.groups.get(&group_type) != Some(&group_key) {
                state.groups.insert(group_type.clone(), group_key.clone());
                state.context_revision = state.context_revision.wrapping_add(1);
            }
            Some(state.clone())
        });
        if let Ok((_, Some(snapshot))) = result {
            let mut event = Capture::new("$groupidentify");
            event
                .properties
                .insert("$group_type".into(), group_type.into());
            event
                .properties
                .insert("$group_key".into(), group_key.into());
            event.properties.insert(
                "$group_set".into(),
                Value::Object(properties.into_iter().collect()),
            );
            self.capture_snapshot(event, &snapshot);
        }
    }
}
