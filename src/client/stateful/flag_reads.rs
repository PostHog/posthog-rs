//! Memory-only reads of one immutable cache snapshot per call.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::Value;

use super::flag_state::CachedFlag;
use super::{Client, Snapshot};

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
            enabled: flag.record.enabled && flag.record.variant.as_deref() != Some(""),
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

/// A lazy cached-flag lookup created by [`Client::feature_flag`].
/// No cache read or access reporting occurs until [`Self::get`] is called.
#[must_use = "call get() to read the feature flag"]
pub struct FeatureFlagLookup<'a> {
    client: &'a Client,
    key: &'a str,
    send_event: Option<bool>,
}

impl FeatureFlagLookup<'_> {
    /// Override access reporting for this lookup. Without an override, `get()`
    /// uses [`Config::send_feature_flag_event`](super::Config::send_feature_flag_event).
    /// Consent still applies. Set false for a silent payload read.
    pub fn send_event(mut self, send: bool) -> Self {
        self.send_event = Some(send);
        self
    }

    /// Read the current cached value, decoded payload and producing context as
    /// one owned result, without fetching. None means unavailable/unknown,
    /// including disabled or closed clients; a known false flag is Some(result)
    /// with enabled=false. A boolean-disabled result has no variant; an empty
    /// variant is retained and reads as disabled.
    ///
    /// Property-only targeting updates can retain an earlier result; its context
    /// describes that earlier evaluation. A present JSON null payload remains
    /// Some(Value::Null), and decoded strings are not parsed again.
    ///
    /// Access reporting runs synchronously through hooks and bounded admission,
    /// never HTTP. Reporting success does not change the returned result.
    #[must_use]
    pub fn get(self) -> Option<FeatureFlagResult> {
        let snapshot = self.client.flag_snapshot()?;
        let flag = snapshot.flags.cache.as_ref()?.flags.get(self.key)?;
        let result = FeatureFlagResult::from_cached(self.key, flag);
        if self
            .send_event
            .unwrap_or(self.client.producer.as_ref()?.send_feature_flag_event)
        {
            self.client.report_flag_access(self.key, &snapshot, flag);
        }
        Some(result)
    }
}

impl Client {
    /// Build a cached-flag lookup. The cache is read only when `get()` is called.
    /// Access reporting defaults to Config::send_feature_flag_event.
    ///
    /// ```
    /// # fn example(client: &posthog::stateful::Client) {
    /// let enabled = client.feature_flag("checkout").get()
    ///     .map_or(false, |flag| flag.enabled);
    /// let variant = client.feature_flag("button-color").get()
    ///     .and_then(|flag| flag.variant)
    ///     .unwrap_or_else(|| "control".into());
    /// let payload = client.feature_flag("checkout").send_event(false).get()
    ///     .and_then(|flag| flag.payload);
    /// # }
    /// ```
    pub fn feature_flag<'a>(&'a self, key: &'a str) -> FeatureFlagLookup<'a> {
        FeatureFlagLookup {
            client: self,
            key,
            send_event: None,
        }
    }

    /// Export all cached rich results without fetching flags or emitting analytics.
    /// None means unavailable (including disabled/closed); Some(empty) is a known
    /// empty snapshot. Known false values are included, missing keys are omitted.
    ///
    /// Each result includes its decoded payload and producing context. Property-only
    /// targeting changes may retain old results until reload. Each call reads one
    /// coherent snapshot and returns owned values; mutating the returned map or
    /// results never changes the client's cache.
    #[must_use]
    pub fn get_feature_flags(&self) -> Option<HashMap<String, FeatureFlagResult>> {
        let snapshot = self.flag_snapshot()?;
        Some(
            snapshot
                .flags
                .cache
                .as_ref()?
                .flags
                .iter()
                .map(|(key, flag)| (key.clone(), FeatureFlagResult::from_cached(key, flag)))
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
