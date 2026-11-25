use std::collections::HashMap;
use std::sync::atomic::Ordering;

use serde_json::Value;

use super::{Capture, Client};

/// Group input for [`Client::group`] and [`Client::group_identify`]. Start with
/// [`Group::new`], or pass a `(type, key)` pair.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Group {
    /// Nonblank group type, such as `"company"`.
    pub group_type: String,
    /// Nonblank group key.
    pub group_key: String,
    /// Group properties, sent under `$group_set`.
    pub properties: HashMap<String, Value>,
    /// Explicit wire options for the `$groupidentify` event, as on [`Capture::options`].
    pub options: HashMap<String, Value>,
}

impl Group {
    /// A group without properties or options.
    pub fn new(group_type: impl Into<String>, group_key: impl Into<String>) -> Self {
        Self {
            group_type: group_type.into(),
            group_key: group_key.into(),
            ..Self::default()
        }
    }
}

impl<T: Into<String>, K: Into<String>> From<(T, K)> for Group {
    fn from((group_type, group_key): (T, K)) -> Self {
        Self::new(group_type, group_key)
    }
}

impl Client {
    /// Register owned context for future events without emitting an event. Each
    /// supplied top-level value replaces the stored value; null is not removal.
    /// Per-event properties win using the ordinary Rust property-fill rules.
    /// Available while opted out. An update containing a value nested more than
    /// 100 levels deep is rejected as a whole.
    /// Memory changes immediately; persistence is asynchronous.
    pub fn register(&self, properties: HashMap<String, Value>) {
        self.change_context(|state| state.registered.extend(properties));
    }

    /// Remove one registered property locally, including while opted out.
    /// Already captured events are unchanged; a missing key is a no-op.
    pub fn unregister(&self, key: &str) {
        self.change_context(|state| {
            state.registered.remove(key);
        });
    }

    /// Remember one group membership and attempt `$groupidentify` on every valid
    /// call, even for the same key without properties. Enables person processing
    /// without identifying the user. Opted-out/Never/disabled/closed clients no-op.
    /// Changed membership replaces scoped flag targeting and reloads flags. Supplied
    /// properties for the same membership merge without an immediate reload, leaving
    /// out of flag targeting any value nested more than 100 levels deep; a load
    /// already in flight continues with the new targeting.
    /// Membership is visible before hooks run; rejected events do not roll it back
    /// or schedule replay. Persistence and delivery remain asynchronous.
    pub fn group(&self, group: impl Into<Group>) {
        self.capture_group(group, true);
    }

    /// Update a group's profile without changing remembered membership. Uses the
    /// current client identity and nests properties under `$group_set`. Like group,
    /// every valid call enables processing and attempts an event unless opted out,
    /// Never, disabled or closed. Rejection does not roll back processing or replay.
    /// Properties mirror into flag targeting only for the remembered key of this
    /// type, without an immediate reload; a load already in flight continues with
    /// the new targeting.
    pub fn group_identify(&self, group: impl Into<Group>) {
        self.capture_group(group, false);
    }

    /// Remove remembered membership locally without emitting an event or disabling
    /// person processing. Available while opted out or configured with Never.
    /// Clears this type's flag targeting and reloads when membership was removed.
    /// Disabled/closed clients no-op; already captured events are unchanged.
    pub fn remove_group(&self, group_type: &str) {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return;
        }
        let result = self.state.as_ref().unwrap().mutate(|state| {
            state.flags.group_properties.remove(group_type);
            state.groups.remove(group_type).is_some()
        });
        if matches!(result, Ok((_, true))) {
            self.reload_feature_flags();
        }
    }

    /// Change registered properties or groups locally. An unchanged state is
    /// not written again.
    fn change_context(&self, change: impl FnOnce(&mut super::Snapshot)) {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return;
        }
        // Only `register` can make the state invalid, through a value nested too deep.
        if let Err(super::writer::UpdateError::Invalid) =
            self.state.as_ref().unwrap().mutate(change)
        {
            tracing::warn!(
                "posthog-rs: register update nested more than 100 levels deep was ignored"
            );
        }
    }

    fn capture_group(&self, group: impl Into<Group>, remember: bool) {
        let Some(_guard) = self.identity_guard(if remember { "group" } else { "group_identify" })
        else {
            return;
        };
        let Group {
            group_type,
            group_key,
            properties,
            options,
        } = group.into();
        if group_type.trim().is_empty() || group_key.trim().is_empty() {
            tracing::warn!("posthog-rs: group type and key must be nonblank");
            return;
        }
        let result = self.state.as_ref().unwrap().mutate(|state| {
            if self.snapshot_opted_out(state) {
                return None;
            }
            state.person_processing = true;
            let changed = remember && state.groups.get(&group_type) != Some(&group_key);
            if changed {
                state.flags.group_properties.remove(&group_type);
                state.groups.insert(group_type.clone(), group_key.clone());
            }
            // A direct profile update for another group must not target the
            // currently remembered group of the same type.
            if state.groups.get(&group_type) == Some(&group_key) && !properties.is_empty() {
                state.flags.mirror_group(&group_type, &properties);
                state.retarget_flags();
            }
            Some((state.clone(), changed))
        });
        if let Ok((_, Some((snapshot, changed)))) = result {
            if changed {
                self.reload_feature_flags();
            }
            let mut event = Capture::new("$groupidentify");
            event.options = options;
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
