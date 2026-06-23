use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDateTime, TimeZone, Utc};
use semver::Version;
use serde::Serialize;
use uuid::Uuid;

use crate::client::CRATE_VERSION;
use crate::feature_flag_evaluations::FeatureFlagEvaluations;
use crate::Error;

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
            "$process_person_profile".into(),
            serde_json::Value::Bool(false),
        );
        Self {
            event: event.into(),
            distinct_id: Uuid::now_v7().to_string(),
            properties,
            groups: HashMap::new(),
            timestamp: None,
            uuid: Uuid::now_v7(),
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
            "$process_person_profile".into(),
            serde_json::Value::Bool(true),
        );
        self.groups.insert(group_name.into(), group_id.into());
    }

    /// Set the event timestamp, for events that happened in the past.
    ///
    /// # Parameters
    ///
    /// - `timestamp`: Timestamp to send with the event. It is converted to UTC
    ///   before serialization.
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
    #[cfg_attr(not(feature = "capture-v1"), allow(dead_code))]
    pub fn event_name(&self) -> &str {
        &self.event
    }

    /// Return the event distinct ID.
    #[cfg_attr(not(feature = "capture-v1"), allow(dead_code))]
    pub fn distinct_id(&self) -> &str {
        &self.distinct_id
    }

    #[cfg_attr(not(feature = "capture-v1"), allow(dead_code))]
    pub(crate) fn uuid(&self) -> Uuid {
        self.uuid
    }

    #[cfg_attr(not(feature = "capture-v1"), allow(dead_code))]
    pub(crate) fn timestamp(&self) -> Option<NaiveDateTime> {
        self.timestamp
    }

    /// Return the event properties.
    #[cfg_attr(not(feature = "capture-v1"), allow(dead_code))]
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

    #[cfg_attr(not(feature = "capture-v1"), allow(dead_code))]
    pub(crate) fn groups(&self) -> &HashMap<String, String> {
        &self.groups
    }

    /// Inject SDK metadata and `$groups` into V0 properties.
    /// Call before constructing [`InnerEvent`] so that the wire payload matches
    /// what the V0 `/capture` and `/batch` endpoints expect.
    ///
    /// `$process_person_profile` is already in `properties` when set by
    /// constructors (`new_anon`, `add_group`) or explicit `insert_prop`.
    #[cfg_attr(feature = "capture-v1", allow(dead_code))]
    pub(crate) fn prepare_for_v0(&mut self) {
        if !self.properties.contains_key("$lib") {
            self.properties.insert(
                "$lib".into(),
                serde_json::Value::String("posthog-rs".into()),
            );
        }

        let version_str = CRATE_VERSION;
        if !self.properties.contains_key("$lib_version") {
            self.properties.insert(
                "$lib_version".into(),
                serde_json::Value::String(version_str.into()),
            );
        }

        if !self.properties.contains_key("$lib_version__major") {
            if let Ok(version) = version_str.parse::<Version>() {
                self.properties.insert(
                    "$lib_version__major".into(),
                    serde_json::Value::Number(version.major.into()),
                );
                self.properties.insert(
                    "$lib_version__minor".into(),
                    serde_json::Value::Number(version.minor.into()),
                );
                self.properties.insert(
                    "$lib_version__patch".into(),
                    serde_json::Value::Number(version.patch.into()),
                );
            }
        }

        if !self.groups.is_empty() {
            self.properties.insert(
                "$groups".into(),
                serde_json::Value::Object(
                    self.groups
                        .iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                        .collect(),
                ),
            );
        }
    }
}

/// Wrapper for the `/batch/` endpoint that includes the API key and options
/// alongside the event array.
#[cfg(not(feature = "capture-v1"))]
#[derive(Serialize)]
pub struct BatchRequest {
    pub api_key: String,
    pub historical_migration: bool,
    pub batch: Vec<InnerEvent>,
}

// With `capture-v1` enabled nothing outside tests builds the V0 wire format.
#[cfg_attr(feature = "capture-v1", allow(dead_code))]
#[derive(Serialize)]
pub struct InnerEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    uuid: Uuid,
    event: String,
    distinct_id: String,
    properties: HashMap<String, serde_json::Value>,
    timestamp: Option<NaiveDateTime>,
}

impl InnerEvent {
    /// Construct a V0 single-event wire event. Expects that
    /// [`Event::prepare_for_v0`] has already been called so properties are fully
    /// decorated.
    #[cfg_attr(feature = "capture-v1", allow(dead_code))]
    pub fn new(event: Event, api_key: String) -> Self {
        Self::from_event(event, Some(api_key))
    }

    /// Construct a V0 batch wire event. The `/batch/` root `api_key` has
    /// precedence on the backend, so per-event keys are intentionally omitted.
    #[cfg(not(feature = "capture-v1"))]
    pub(crate) fn new_for_batch(event: Event) -> Self {
        Self::from_event(event, None)
    }

    fn from_event(event: Event, api_key: Option<String>) -> Self {
        Self {
            api_key,
            uuid: event.uuid,
            event: event.event,
            distinct_id: event.distinct_id,
            properties: event.properties,
            timestamp: event.timestamp,
        }
    }
}

#[cfg(test)]
pub mod tests {
    use uuid::Uuid;

    use crate::{event::InnerEvent, Event};

    /// Helper: prepares an event for V0 and constructs the InnerEvent.
    fn build_v0(mut event: Event) -> InnerEvent {
        event.prepare_for_v0();
        InnerEvent::new(event, "test_api_key".to_string())
    }

    #[cfg(not(feature = "capture-v1"))]
    fn build_v0_batch_event(mut event: Event) -> InnerEvent {
        event.prepare_for_v0();
        InnerEvent::new_for_batch(event)
    }

    #[test]
    fn v0_adds_lib_properties() {
        let mut event = Event::new("unit test event", "1234");
        event.insert_prop("key1", "value1").unwrap();

        let inner = build_v0(event);
        assert_eq!(
            inner.properties.get("$lib"),
            Some(&serde_json::Value::String("posthog-rs".to_string()))
        );
    }

    #[test]
    fn v0_serializes_distinct_id_at_root() {
        let inner = build_v0(Event::new("test", "user1"));
        let json = serde_json::to_value(&inner).unwrap();

        // Canonical field at the event root; the legacy `$distinct_id` spelling
        // (only tolerated by capture via a serde alias) must not be emitted.
        assert_eq!(json["distinct_id"], "user1");
        assert!(json.get("$distinct_id").is_none());
    }

    #[cfg(not(feature = "capture-v1"))]
    #[test]
    fn v0_batch_serializes_distinct_id_at_root() {
        use crate::event::BatchRequest;

        let batch = BatchRequest {
            api_key: "test_api_key".to_string(),
            historical_migration: false,
            batch: vec![
                build_v0_batch_event(Event::new("e1", "user1")),
                build_v0_batch_event(Event::new("e2", "user2")),
            ],
        };
        let json = serde_json::to_value(&batch).unwrap();

        assert_eq!(json["api_key"], "test_api_key");

        let events = json["batch"].as_array().expect("batch is an array");
        for (event, expected_id) in events.iter().zip(["user1", "user2"]) {
            assert_eq!(event["distinct_id"], expected_id);
            assert!(event.get("$distinct_id").is_none());
            assert!(event.get("api_key").is_none());
        }
    }

    #[test]
    fn v0_includes_auto_generated_uuid() {
        let event = Event::new("test", "user1");
        let inner = build_v0(event);
        let json = serde_json::to_value(&inner).unwrap();

        let uuid_str = json["uuid"].as_str().expect("uuid should be present");
        Uuid::parse_str(uuid_str).expect("uuid should be valid");
    }

    #[test]
    fn v0_preserves_overridden_uuid() {
        let uuid = Uuid::now_v7();
        let mut event = Event::new("test", "user1");
        event.set_uuid(uuid);

        let inner = build_v0(event);
        let json = serde_json::to_value(&inner).unwrap();
        assert_eq!(json["uuid"], uuid.to_string());
    }

    #[test]
    fn v0_preserves_existing_lib_properties() {
        let mut event = Event::new("forwarded event", "user1");
        event.insert_prop("$lib", "posthog-js").unwrap();
        event.insert_prop("$lib_version", "1.42.0").unwrap();
        event.insert_prop("$lib_version__major", 1u64).unwrap();

        let inner = build_v0(event);
        let props = &inner.properties;

        assert_eq!(
            props.get("$lib"),
            Some(&serde_json::Value::String("posthog-js".to_string()))
        );
        assert_eq!(
            props.get("$lib_version"),
            Some(&serde_json::Value::String("1.42.0".to_string()))
        );
        assert_eq!(
            props.get("$lib_version__major"),
            Some(&serde_json::Value::Number(1u64.into()))
        );
    }

    #[test]
    fn v0_injects_process_person_profile_for_anon() {
        let event = Event::new_anon("anon_test");
        let inner = build_v0(event);
        assert_eq!(
            inner.properties.get("$process_person_profile"),
            Some(&serde_json::Value::Bool(false))
        );
    }

    #[test]
    fn v0_injects_process_person_profile_for_group() {
        let mut event = Event::new("test", "user1");
        event.add_group("company", "acme");
        let inner = build_v0(event);
        assert_eq!(
            inner.properties.get("$process_person_profile"),
            Some(&serde_json::Value::Bool(true))
        );
    }

    #[test]
    fn v0_no_process_person_profile_when_unset() {
        let event = Event::new("test", "user1");
        let inner = build_v0(event);
        assert!(!inner.properties.contains_key("$process_person_profile"));
    }

    #[test]
    fn v0_user_property_wins_over_constructor_default() {
        let mut event = Event::new_anon("test");
        // new_anon sets $process_person_profile=false; explicit insert overwrites.
        event.insert_prop("$process_person_profile", true).unwrap();
        let inner = build_v0(event);
        assert_eq!(
            inner.properties.get("$process_person_profile"),
            Some(&serde_json::Value::Bool(true)),
        );
    }

    #[test]
    fn v0_identified_event_with_explicit_personless() {
        let mut event = Event::new("test", "user1");
        event.insert_prop("$process_person_profile", false).unwrap();
        let inner = build_v0(event);
        assert_eq!(
            inner.properties.get("$process_person_profile"),
            Some(&serde_json::Value::Bool(false)),
        );
    }

    #[test]
    fn v0_add_group_overrides_anon_person_profile() {
        let mut event = Event::new_anon("test");
        // new_anon sets $process_person_profile=false; add_group forces true.
        event.add_group("company", "acme");
        let inner = build_v0(event);
        assert_eq!(
            inner.properties.get("$process_person_profile"),
            Some(&serde_json::Value::Bool(true)),
        );
        let groups = inner
            .properties
            .get("$groups")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(groups.get("company").unwrap().as_str().unwrap(), "acme");
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use chrono::{DateTime, Utc};

    use super::Event;

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
}
