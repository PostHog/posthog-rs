//! Memory-only reads of one immutable cache snapshot per call.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::Value;

use super::flag_state::CachedFlag;
use super::{Client, Snapshot};
use crate::feature_flag_evaluations::flag_value_for;
use crate::FlagValue;

/// Inputs that produced a cached flag, not necessarily the latest targeting.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FeatureFlagContext {
    pub distinct_id: String,
    pub anonymous_id: String,
    pub groups: HashMap<String, String>,
    pub person_properties: HashMap<String, Value>,
    pub group_properties: HashMap<String, HashMap<String, Value>>,
    pub disable_geoip: bool,
}

/// An owned evaluation snapshot. False is a known result, not unavailability.
/// Payloads and metadata belong to this record's producing response, including
/// records retained across a partial refresh.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FeatureFlagResult {
    pub key: String,
    pub enabled: bool,
    pub variant: Option<String>,
    pub payload: Option<Value>,
    pub context: FeatureFlagContext,
    /// Producing process-local targeting revision; not a server/global version.
    pub evaluation_revision: u64,
    pub request_id: Option<String>,
    pub evaluated_at: Option<i64>,
    pub partial: bool,
    pub id: Option<u64>,
    pub version: Option<u32>,
    pub reason: Option<String>,
}

impl FeatureFlagContext {
    pub(super) fn from_context(context: &super::flag_state::FlagContext) -> Self {
        Self {
            distinct_id: context.distinct_id.clone(),
            anonymous_id: context.anonymous_id.clone(),
            groups: context.groups.clone(),
            person_properties: context.person_properties.clone(),
            group_properties: context.group_properties.clone(),
            disable_geoip: context.disable_geoip,
        }
    }
}

impl FeatureFlagResult {
    pub(super) fn from_cached(key: &str, flag: &CachedFlag) -> Self {
        Self {
            key: key.into(),
            enabled: flag.record.enabled,
            variant: if flag.record.enabled {
                flag.record.variant.clone()
            } else {
                None
            },
            payload: flag.record.payload.clone(),
            context: FeatureFlagContext::from_context(&flag.context),
            evaluation_revision: flag.revision,
            request_id: flag.request_id.clone(),
            evaluated_at: flag.evaluated_at,
            partial: flag.partial,
            id: flag.record.id,
            version: flag.record.version,
            reason: flag.record.reason.clone(),
        }
    }
}

impl Client {
    /// Read a cached value without fetching. None means unavailable/unknown.
    /// Reports access by default; None inherits Config::send_feature_flag_event.
    /// Reporting is synchronous through hooks and bounded admission, never HTTP.
    /// Its success does not change the returned value. Consent still applies.
    #[must_use]
    pub fn get_feature_flag(
        &self,
        key: &str,
        send_feature_flag_event: Option<bool>,
    ) -> Option<FlagValue> {
        self.read_flag(key, send_feature_flag_event, |flag| {
            flag_value_for(&flag.record)
        })
    }

    /// Read cached enablement: Some(false) is known disabled, None unavailable.
    /// Uses the same event override and deduplication as get_feature_flag.
    #[must_use]
    pub fn is_feature_enabled(
        &self,
        key: &str,
        send_feature_flag_event: Option<bool>,
    ) -> Option<bool> {
        self.read_flag(key, send_feature_flag_event, |flag| flag.record.enabled)
    }

    /// Read value, decoded payload and producing context as one owned result.
    /// A disabled result has no variant. Property-only targeting updates can
    /// retain an earlier result; context describes that earlier evaluation.
    /// Uses the same event override and deduplication as get_feature_flag.
    #[must_use]
    pub fn get_feature_flag_result(
        &self,
        key: &str,
        send_feature_flag_event: Option<bool>,
    ) -> Option<FeatureFlagResult> {
        self.read_flag(key, send_feature_flag_event, |flag| {
            FeatureFlagResult::from_cached(key, flag)
        })
    }

    fn read_flag<T>(
        &self,
        key: &str,
        send: Option<bool>,
        read: impl FnOnce(&CachedFlag) -> T,
    ) -> Option<T> {
        let snapshot = self.flag_snapshot()?;
        let flag = snapshot.flags.cache.as_ref()?.flags.get(key)?;
        let result = read(flag);
        if send.unwrap_or(self.producer.as_ref()?.send_feature_flag_event) {
            self.report_flag_access(key, &snapshot, flag);
        }
        Some(result)
    }

    /// Return the cached JSON payload without fetching flags or emitting analytics.
    /// None means unavailable, unknown, or no payload. Some(Value::Null) is a
    /// present JSON null. Already-decoded strings are not parsed again.
    #[must_use]
    pub fn get_feature_flag_payload(&self, key: &str) -> Option<Value> {
        let snapshot = self.flag_snapshot()?;
        snapshot
            .flags
            .cache
            .as_ref()?
            .flags
            .get(key)?
            .record
            .payload
            .clone()
    }

    /// Export all cached flag values without fetching flags or emitting analytics.
    /// None means unavailable (including disabled/closed); Some(empty) is a known
    /// empty snapshot. Known false values are included, missing keys are omitted.
    ///
    /// Values reflect their last evaluation. Property-only targeting changes may
    /// retain old values until reload. Each call reads one coherent snapshot;
    /// separate calls can observe different snapshots during a refresh.
    #[must_use]
    pub fn get_feature_flags(&self) -> Option<HashMap<String, FlagValue>> {
        let snapshot = self.flag_snapshot()?;
        Some(
            snapshot
                .flags
                .cache
                .as_ref()?
                .flags
                .iter()
                .map(|(key, flag)| (key.clone(), flag_value_for(&flag.record)))
                .collect(),
        )
    }

    /// Export the present cached payloads, including JSON nulls. Known flags
    /// without a payload are omitted. None means no available cache; Some(empty)
    /// means the available cache has no payloads. Performs no I/O or analytics.
    ///
    /// As with get_feature_flags, a call reads one snapshot and returns owned
    /// values. Mutating the returned map never changes the client's cache.
    #[must_use]
    pub fn get_feature_flag_payloads(&self) -> Option<HashMap<String, Value>> {
        let snapshot = self.flag_snapshot()?;
        Some(
            snapshot
                .flags
                .cache
                .as_ref()?
                .flags
                .iter()
                .filter_map(|(key, flag)| {
                    flag.record
                        .payload
                        .clone()
                        .map(|payload| (key.clone(), payload))
                })
                .collect(),
        )
    }

    pub(super) fn flag_snapshot(&self) -> Option<Arc<Snapshot>> {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return None;
        }
        Some(self.state.as_ref()?.snapshot())
    }
}
