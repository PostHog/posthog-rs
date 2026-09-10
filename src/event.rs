use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDateTime, TimeZone, Utc};
use serde::Serialize;
use tracing::warn;
use uuid::Uuid;

use crate::feature_flag_evaluations::FeatureFlagEvaluations;
use crate::Error;

/// The only properties retained on a minimized `$feature_flag_called` event.
///
/// A fresh allowlist (rather than a denylist) so nothing added upstream — super
/// properties, context, user-supplied properties, SDK metadata, system context —
/// can leak past it. Applied as the final capture-pipeline step, after all
/// enrichment, so the resulting event carries exactly this set intersected with
/// what the full event would have had. Beyond the evaluation properties it keeps
/// cheap, static, low-cardinality identity useful for platform/runtime
/// breakdowns (`$lib`, `$lib_version`, `$os`, `$os_version`), processing-control
/// sentinels this SDK sets (`$geoip_disable`, `$process_person_profile`,
/// `$is_server`), correctness-required `$groups`, and linkage identifiers.
pub(crate) const MINIMAL_FLAG_CALLED_EVENT_PROPERTIES: &[&str] = &[
    // Identity
    "$feature_flag",
    "$feature_flag_response",
    "$feature_flag_has_experiment",
    // Evaluation debug
    "$feature_flag_id",
    "$feature_flag_version",
    "$feature_flag_reason",
    "$feature_flag_request_id",
    "$feature_flag_evaluated_at",
    "$feature_flag_error",
    "locally_evaluated",
    // Correctness-required / processing-control
    "$groups",
    "$process_person_profile",
    "$geoip_disable",
    // Linkage / SDK identity
    "$session_id",
    "$window_id",
    "$device_id",
    "$lib",
    "$lib_version",
    "$is_server",
    // Static platform/runtime identity
    "$os",
    "$os_version",
];

/// Whether `key` survives minimization to [`MINIMAL_FLAG_CALLED_EVENT_PROPERTIES`].
/// Applied by the capture pipeline (`capture::build_events_at`) so the
/// allowlist check has a single implementation independent of the properties
/// representation it is applied to.
pub(crate) fn is_minimal_flag_called_property(key: &str) -> bool {
    MINIMAL_FLAG_CALLED_EVENT_PROPERTIES.contains(&key)
}

/// An [`Event`] represents an interaction a user has with your app or
/// website. Examples include button clicks, pageviews, query completions, and signups.
/// See the [PostHog documentation](https://posthog.com/docs/data/events)
/// for a detailed explanation of PostHog Events.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Event {
    event: String,
    distinct_id: String,
    properties: HashMap<String, serde_json::Value>,
    groups: HashMap<String, String>,
    timestamp: Option<NaiveDateTime>,
    uuid: Uuid,
    /// When set, the capture pipeline trims this event's properties to
    /// [`MINIMAL_FLAG_CALLED_EVENT_PROPERTIES`] as its final enrichment step.
    /// Set only for minimized `$feature_flag_called` events; never serialized.
    #[serde(skip)]
    minimal_flag_called: bool,
}

impl Event {
    /// Create a new identified [`Event`]. Unless you have a distinct ID you can
    /// associate with a user, you probably want to use [`Event::new_anon`]
    /// instead.
    ///
    /// # Parameters
    ///
    /// - `event`: Event name, such as `"user_signed_up"`.
    /// - `distinct_id`: Stable user or account identifier. For backend events,
    ///   use the same distinct ID your frontend passes to `posthog.identify()`.
    pub fn new<S: Into<String>>(event: S, distinct_id: S) -> Self {
        Self {
            event: event.into(),
            distinct_id: distinct_id.into(),
            properties: HashMap::new(),
            groups: HashMap::new(),
            timestamp: None,
            uuid: Uuid::now_v7(),
            minimal_flag_called: false,
        }
    }

    /// Create a new anonymous event.
    ///
    /// See <https://posthog.com/docs/data/anonymous-vs-identified-events#how-to-capture-anonymous-events>.
    ///
    /// # Parameters
    ///
    /// - `event`: Event name.
    ///
    /// # Remarks
    ///
    /// Generates a random distinct ID and sets `$process_person_profile` to
    /// `false` so PostHog does not create a person profile for the event.
    pub fn new_anon<S: Into<String>>(event: S) -> Self {
        let mut properties = HashMap::new();
        properties.insert(
            crate::constants::PROCESS_PERSON_PROFILE_PROP.into(),
            serde_json::Value::Bool(false),
        );
        Self {
            event: event.into(),
            distinct_id: Uuid::now_v7().to_string(),
            properties,
            groups: HashMap::new(),
            timestamp: None,
            uuid: Uuid::now_v7(),
            minimal_flag_called: false,
        }
    }

    /// Add a property to the event.
    ///
    /// # Parameters
    ///
    /// - `key`: Property name.
    /// - `prop`: Any value that can be serialized to JSON.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Serialization`] if `prop` cannot be serialized.
    pub fn insert_prop<K: Into<String>, P: Serialize>(
        &mut self,
        key: K,
        prop: P,
    ) -> Result<(), Error> {
        let as_json =
            serde_json::to_value(prop).map_err(|e| Error::Serialization(e.to_string()))?;
        let _ = self.properties.insert(key.into(), as_json);
        Ok(())
    }

    /// Remove a property from the event and return its previous value, if any.
    pub fn remove_prop(&mut self, key: &str) -> Option<serde_json::Value> {
        self.properties.remove(key)
    }

    /// Capture this as a group event.
    ///
    /// See <https://posthog.com/docs/product-analytics/group-analytics#how-to-capture-group-events>.
    ///
    /// # Parameters
    ///
    /// - `group_name`: Group type, such as `"company"`.
    /// - `group_id`: Stable identifier for the group.
    ///
    /// # Remarks
    ///
    /// Group events cannot be personless, and will be automatically upgraded to
    /// include person profile processing if they were anonymous. This might lead
    /// to "empty" person profiles being created.
    pub fn add_group(&mut self, group_name: &str, group_id: &str) {
        self.properties.insert(
            crate::constants::PROCESS_PERSON_PROFILE_PROP.into(),
            serde_json::Value::Bool(true),
        );
        self.groups.insert(group_name.into(), group_id.into());
    }

    /// Set the event timestamp, for events that happened in the past.
    ///
    /// # Parameters
    ///
    /// - `timestamp`: Timestamp to send with the event. UTC input is preferred;
    ///   non-UTC input is converted to the equivalent UTC instant before serialization.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidTimestamp`] if the timestamp is in the future.
    pub fn set_timestamp<Tz>(&mut self, timestamp: DateTime<Tz>) -> Result<(), Error>
    where
        Tz: TimeZone,
    {
        if timestamp > Utc::now() + Duration::seconds(1) {
            return Err(Error::InvalidTimestamp(String::from(
                "Events cannot occur in the future",
            )));
        }
        self.timestamp = Some(timestamp.naive_utc());
        Ok(())
    }

    /// Build the `$create_alias` event backing [`crate::Client::alias`].
    ///
    /// The event is attributed to `previous_id`, which is also mirrored into
    /// `properties.distinct_id` alongside the merge target in `properties.alias`
    /// — the shape posthog-python, posthog-js-lite, and posthog-php all send.
    ///
    /// Returns `None` when either ID is blank. A merge needs both sides, so a
    /// blank one can only produce a malformed event; the SDKs above drop it with
    /// a warning rather than sending it, and so do we.
    ///
    /// Properties are built directly rather than through `insert_prop` so
    /// construction is infallible: both values are strings, which cannot fail to
    /// serialize, leaving no always-`Ok` `Result` for callers to discard.
    pub(crate) fn alias(previous_id: String, distinct_id: String) -> Option<Self> {
        if previous_id.trim().is_empty() || distinct_id.trim().is_empty() {
            warn!("alias() called with a blank id, dropping the $create_alias event");
            return None;
        }

        let mut properties = HashMap::new();
        properties.insert(
            "distinct_id".to_string(),
            serde_json::Value::String(previous_id.clone()),
        );
        properties.insert("alias".to_string(), serde_json::Value::String(distinct_id));

        Some(Self {
            event: "$create_alias".to_string(),
            distinct_id: previous_id,
            properties,
            groups: HashMap::new(),
            timestamp: None,
            uuid: Uuid::now_v7(),
            minimal_flag_called: false,
        })
    }

    /// Build the `$groupidentify` event backing [`crate::Client::group_identify`].
    ///
    /// Sets `$group_type`, `$group_key`, and `$group_set` in `properties`.
    /// The event is attributed to `format!("${group_type}_{group_key}")`.
    ///
    /// Returns `Ok(None)` when either `group_type` or `group_key` is blank (a
    /// warning is logged and the event is dropped).
    ///
    /// Returns [`Error::Serialization`] if `properties` fails to serialize to
    /// JSON, or if it does not serialize to a JSON object — PostHog ingestion
    /// requires `$group_set` to be an object and silently drops the event
    /// otherwise.
    pub(crate) fn group_identify<P: Serialize>(
        group_type: String,
        group_key: String,
        properties: P,
    ) -> Result<Option<Self>, Error> {
        if group_type.trim().is_empty() {
            warn!("group_identify() called with a blank group_type, dropping the $groupidentify event");
            return Ok(None);
        }
        if group_key.trim().is_empty() {
            warn!(
                "group_identify() called with a blank group_key, dropping the $groupidentify event"
            );
            return Ok(None);
        }

        let group_set =
            serde_json::to_value(properties).map_err(|e| Error::Serialization(e.to_string()))?;
        if !group_set.is_object() {
            return Err(Error::Serialization(format!(
                "group_identify() properties must serialize to a JSON object, got {group_set}"
            )));
        }

        let distinct_id = format!("${group_type}_{group_key}");
        let mut props = HashMap::new();
        props.insert(
            "$group_type".to_string(),
            serde_json::Value::String(group_type),
        );
        props.insert(
            "$group_key".to_string(),
            serde_json::Value::String(group_key),
        );
        props.insert("$group_set".to_string(), group_set);

        Ok(Some(Self {
            event: "$groupidentify".to_string(),
            distinct_id,
            properties: props,
            groups: HashMap::new(),
            timestamp: None,
            uuid: Uuid::now_v7(),
            minimal_flag_called: false,
        }))
    }

    /// Stamp the capture (enqueue) time when the caller hasn't set an explicit
    /// timestamp. Done on the producer side before the event is queued, so a
    /// batched or retried event records when it *occurred*, not when the worker
    /// finally sent it.
    pub(crate) fn ensure_timestamp(&mut self, now: DateTime<Utc>) {
        if self.timestamp.is_none() {
            self.timestamp = Some(now.naive_utc());
        }
    }

    /// Override the auto-generated UUID for this event.
    ///
    /// Useful for deduplication when re-importing historical data.
    pub fn set_uuid(&mut self, uuid: Uuid) {
        self.uuid = uuid;
    }

    /// Attach the flag state captured by a [`FeatureFlagEvaluations`] snapshot
    /// to this event.
    ///
    /// Adds `$feature/<key>` for every evaluated flag plus a sorted
    /// `$active_feature_flags` list of enabled keys, mirroring what
    /// `send_feature_flags` would otherwise fetch — but without making an
    /// extra `/flags` request.
    ///
    /// # Returns
    ///
    /// Returns `self` so calls can be chained before capture.
    pub fn with_flags(&mut self, flags: &FeatureFlagEvaluations) -> &mut Self {
        for (key, value) in flags.event_properties() {
            self.properties.insert(key, value);
        }
        self
    }

    /// Return the event name.
    pub fn event_name(&self) -> &str {
        &self.event
    }

    /// Return the event distinct ID.
    pub fn distinct_id(&self) -> &str {
        &self.distinct_id
    }

    pub(crate) fn uuid(&self) -> Uuid {
        self.uuid
    }

    pub(crate) fn timestamp(&self) -> Option<NaiveDateTime> {
        self.timestamp
    }

    /// Return the event properties.
    pub fn properties(&self) -> &HashMap<String, serde_json::Value> {
        &self.properties
    }

    /// Insert a default property only if the caller hasn't already set it.
    ///
    /// This gives caller-wins semantics: SDK-level defaults (like `$is_server`)
    /// are injected without overriding an explicit value the user placed on the
    /// event before calling `capture()`.
    pub(crate) fn insert_prop_default<K: Into<String>>(
        &mut self,
        key: K,
        value: serde_json::Value,
    ) {
        self.properties.entry(key.into()).or_insert(value);
    }

    pub(crate) fn groups(&self) -> &HashMap<String, String> {
        &self.groups
    }

    /// Mark this event as a minimized `$feature_flag_called` event. The capture
    /// pipeline then trims its properties to
    /// [`MINIMAL_FLAG_CALLED_EVENT_PROPERTIES`] after all enrichment.
    pub(crate) fn mark_minimal_flag_called(&mut self) {
        self.minimal_flag_called = true;
    }

    /// Whether this event is a minimized `$feature_flag_called` event.
    pub(crate) fn is_minimal_flag_called(&self) -> bool {
        self.minimal_flag_called
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use chrono::{DateTime, Utc};

    use super::Event;
    use crate::Error;

    #[test]
    fn test_timestamp_is_correctly_set() {
        let mut event = Event::new_anon("test");
        let ts = DateTime::parse_from_rfc3339("2023-01-01T10:00:00+03:00").unwrap();
        event.set_timestamp(ts).expect("Date is not in the future");
        let expected = DateTime::parse_from_rfc3339("2023-01-01T07:00:00Z").unwrap();
        assert_eq!(event.timestamp.unwrap(), expected.naive_utc())
    }

    #[test]
    fn test_timestamp_is_correctly_set_with_future_date() {
        let mut event = Event::new_anon("test");
        let ts = Utc::now() + Duration::from_secs(60);
        event
            .set_timestamp(ts)
            .expect_err("Date is in the future, should be rejected");

        assert!(event.timestamp.is_none())
    }

    #[test]
    fn ensure_timestamp_stamps_only_when_unset() {
        let now = DateTime::parse_from_rfc3339("2026-06-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // Unset -> stamped with the provided capture time.
        let mut event = Event::new("test", "user1");
        event.ensure_timestamp(now);
        assert_eq!(event.timestamp, Some(now.naive_utc()));

        // Caller's explicit timestamp wins; ensure is a no-op.
        let mut event = Event::new("test", "user1");
        let caller = DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        event.set_timestamp(caller).unwrap();
        event.ensure_timestamp(now);
        assert_eq!(event.timestamp, Some(caller.naive_utc()));
    }

    #[test]
    fn group_identify_rejects_blank_keys() {
        for (group_type, group_key) in [("", "key"), ("   ", "key"), ("type", ""), ("type", "   ")]
        {
            assert!(Event::group_identify(
                group_type.to_string(),
                group_key.to_string(),
                serde_json::json!({}),
            )
            .unwrap()
            .is_none());
        }
    }

    #[test]
    fn group_identify_rejects_non_object_properties() {
        for properties in [
            serde_json::json!(42),
            serde_json::json!(["a", "b"]),
            serde_json::Value::Null,
        ] {
            let err =
                Event::group_identify("company".to_string(), "acme_123".to_string(), properties)
                    .expect_err("non-object properties should be rejected");
            assert!(matches!(err, Error::Serialization(_)));
        }
    }
}
