//! Local targeting shares the validated state transaction with identity and groups.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use serde_json::Value;

use super::flag_state::FlagState;
use super::Client;

impl FlagState {
    pub(super) fn mirror_person(
        &mut self,
        set: &HashMap<String, Value>,
        set_once: &HashMap<String, Value>,
    ) {
        for (key, value) in set_once {
            self.person_properties
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        self.person_properties.extend(set.clone());
    }
}

impl Client {
    /// Merge local person targeting without emitting analytics. Null is a value,
    /// not removal. Empty input is ignored. `None` reloads flags, as does
    /// `Some(true)`; `Some(false)` keeps the last-known values until a later reload.
    /// Available while opted out or person profiles are disabled.
    /// Memory changes synchronously; bounded persistence is asynchronous.
    pub fn set_person_properties_for_flags(
        &self,
        properties: HashMap<String, Value>,
        reload: Option<bool>,
    ) {
        if properties.is_empty() {
            return;
        }
        self.mutate_flag_targeting(|flags| flags.person_properties.extend(properties), reload);
    }

    /// Clear all local person targeting. Does not change the analytics profile.
    /// `None` or `Some(true)` reloads even when already empty; `Some(false)` does not.
    pub fn reset_person_properties_for_flags(&self, reload: Option<bool>) {
        self.mutate_flag_targeting(|flags| flags.person_properties.clear(), reload);
    }

    /// Merge local targeting for one nonblank group type without changing group
    /// membership or emitting analytics. Empty input is ignored. `None` or
    /// `Some(true)` reloads; `Some(false)` retains last-known flags until refreshed.
    /// Available while opted out or person profiles are disabled. Updates exceeding
    /// mandatory state limits are rejected as a whole, without a reload.
    pub fn set_group_properties_for_flags(
        &self,
        group_type: impl Into<String>,
        properties: HashMap<String, Value>,
        reload: Option<bool>,
    ) {
        let group_type = group_type.into();
        if group_type.trim().is_empty() || properties.is_empty() {
            return;
        }
        self.mutate_flag_targeting(
            |flags| {
                flags
                    .group_properties
                    .entry(group_type)
                    .or_default()
                    .extend(properties)
            },
            reload,
        );
    }

    /// Clear one group's local targeting, or all groups when `group_type` is
    /// `None`. Membership is unchanged. Blank types are ignored. `None` or
    /// `Some(true)` for `reload` refreshes flags, including an already-empty scope.
    pub fn reset_group_properties_for_flags(&self, group_type: Option<&str>, reload: Option<bool>) {
        if group_type.is_some_and(|kind| kind.trim().is_empty()) {
            return;
        }
        self.mutate_flag_targeting(
            |flags| match group_type {
                Some(kind) => {
                    flags.group_properties.remove(kind);
                }
                None => flags.group_properties.clear(),
            },
            reload,
        );
    }

    fn mutate_flag_targeting(&self, update: impl FnOnce(&mut FlagState), reload: Option<bool>) {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return;
        }
        if self
            .state
            .as_ref()
            .unwrap()
            .mutate(|state| update(&mut state.flags))
            .is_ok()
            && reload.unwrap_or(true)
        {
            self.reload_feature_flags();
        }
    }
}
