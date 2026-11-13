use std::collections::hash_map::Entry;
use std::collections::HashMap;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::constants::{PROCESS_PERSON_PROFILE_OPT, PROCESS_PERSON_PROFILE_PROP};
use crate::feature_flag_evaluations::FeatureFlagEvaluations;
use crate::request_context::current_distinct_id;
use crate::Error;

/// Events that merge persons or update person or group properties, which PostHog skips or
/// drops when person processing is off, so they default to `process_person_profile: true`
/// (see [`Event::apply_identify_style_person_default`]).
pub(crate) const IDENTIFY_STYLE_EVENTS: [&str; 6] = [
    "$identify",
    "$create_alias",
    "$merge_dangerously",
    "$groupidentify",
    "$set",
    "$set_once",
];

/// The only properties retained on a minimized `$feature_flag_called` event.
///
/// A fresh allowlist (rather than a denylist) so nothing added upstream — super
/// properties, context, user-supplied properties, SDK metadata, system context —
/// can leak past it. Applied after every SDK enrichment and before `before_send`,
/// so the event carries this set intersected with what the full event would have
/// had, plus whatever a hook adds. Beyond the evaluation properties it keeps
/// cheap, static, low-cardinality identity useful for platform/runtime
/// breakdowns (`$lib`, `$lib_version`, `$os`, `$os_version`), processing-control
/// sentinels (`$geoip_disable`, `$process_person_profile`, `$is_server`),
/// correctness-required `$groups`, and linkage identifiers.
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
    // Option sources: capture moves these legacy properties into `options`
    // after the allowlist runs, so dropping them would drop the option.
    "$cookieless_mode",
    "$ignore_sent_at",
    "$product_tour_id",
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
pub(crate) fn is_minimal_flag_called_property(key: &str) -> bool {
    MINIMAL_FLAG_CALLED_EVENT_PROPERTIES.contains(&key)
}

/// Merge `groups` into the `$groups` property, key by key, so typed groups win
/// over a `$groups` value the caller set.
pub(crate) fn merge_groups_into(
    properties: &mut HashMap<String, serde_json::Value>,
    groups: &HashMap<String, String>,
) {
    if groups.is_empty() {
        return;
    }
    let mut merged = match properties.remove("$groups") {
        Some(serde_json::Value::Object(existing)) => existing,
        _ => serde_json::Map::new(),
    };
    for (k, v) in groups {
        merged.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    properties.insert("$groups".into(), serde_json::Value::Object(merged));
}

/// Properties whose object values fill one level deep instead of as a whole.
const NESTED_FILL_PROPERTIES: [&str; 4] = ["$set", "$set_once", "$groups", "$group_set"];

/// Fill `key` with a default only where the caller left it unset. For the
/// [`NESTED_FILL_PROPERTIES`], when both values are objects, the default adds
/// only the inner keys the caller's object lacks.
pub(crate) fn fill_property(
    properties: &mut HashMap<String, serde_json::Value>,
    key: String,
    value: serde_json::Value,
) {
    let nested = NESTED_FILL_PROPERTIES.contains(&key.as_str());
    match properties.entry(key) {
        Entry::Vacant(slot) => {
            slot.insert(value);
        }
        Entry::Occupied(mut slot) => {
            if !nested {
                return;
            }
            if let (serde_json::Value::Object(existing), serde_json::Value::Object(defaults)) =
                (slot.get_mut(), value)
            {
                for (inner_key, inner_value) in defaults {
                    existing.entry(inner_key).or_insert(inner_value);
                }
            }
        }
    }
}

/// Whether a `$lib` or `$lib_version` value counts as set: only a non-empty
/// string does. A missing key, `null`, `""` and non-strings are all unset.
pub(crate) fn names_a_library(value: Option<&serde_json::Value>) -> bool {
    matches!(value, Some(serde_json::Value::String(text)) if !text.is_empty())
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
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    options: HashMap<String, serde_json::Value>,
    groups: HashMap<String, String>,
    timestamp: Option<NaiveDateTime>,
    /// Assigned once when the client takes the event, unless set with
    /// [`Event::set_uuid`], so a reused or cloned `Event` never resends a uuid.
    uuid: Option<Uuid>,
    /// When set, the capture pipeline trims this event's properties to
    /// [`MINIMAL_FLAG_CALLED_EVENT_PROPERTIES`] after the SDK fills its values
    /// and before `before_send`. Set only for minimized `$feature_flag_called`
    /// events; never serialized.
    #[serde(skip)]
    minimal_flag_called: bool,
    #[serde(skip)]
    pub(crate) request_context_applied: bool,
    /// True while `process_person_profile` holds the personless default from
    /// [`Event::new_anon`], so a request context option can still replace it.
    #[serde(skip)]
    personless_default: bool,
    /// True for an event built by [`Event::new_anon`]: its distinct ID is generated, so the
    /// SDK fills `process_person_profile: false` before `before_send` while the option is unset.
    #[serde(skip)]
    personless: bool,
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
    ///   An empty or whitespace-only ID counts as missing: the event becomes
    ///   anonymous, as from [`Event::new_anon`].
    pub fn new<S: Into<String>>(event: S, distinct_id: S) -> Self {
        let distinct_id = distinct_id.into();
        if distinct_id.trim().is_empty() {
            return Self::new_anon(event);
        }
        Self {
            event: event.into(),
            distinct_id,
            properties: HashMap::new(),
            options: HashMap::new(),
            groups: HashMap::new(),
            timestamp: None,
            uuid: None,
            minimal_flag_called: false,
            request_context_applied: false,
            personless_default: false,
            personless: false,
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
    /// Generates a random distinct ID and sets the `process_person_profile`
    /// option to `false` so PostHog does not create a person profile for the
    /// event. To process one anyway, set the option to `true` with
    /// [`Event::insert_option`]. A `null` or removed option counts as unset,
    /// so the SDK sets `false` again before `before_send`.
    pub fn new_anon<S: Into<String>>(event: S) -> Self {
        let mut options = HashMap::new();
        options.insert(
            PROCESS_PERSON_PROFILE_OPT.into(),
            serde_json::Value::Bool(false),
        );
        Self {
            event: event.into(),
            distinct_id: Uuid::now_v7().to_string(),
            properties: HashMap::new(),
            options,
            groups: HashMap::new(),
            timestamp: None,
            uuid: None,
            minimal_flag_called: false,
            request_context_applied: false,
            personless_default: true,
            personless: true,
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

    /// Set a capture option that tells PostHog how to process the event, such
    /// as `"process_person_profile"`.
    ///
    /// # Parameters
    ///
    /// - `key`: Option name.
    /// - `value`: Any value that can be serialized to JSON.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Serialization`] if `value` cannot be serialized.
    ///
    /// # Remarks
    ///
    /// Sent unchecked: PostHog ignores unknown keys and drops the event if a
    /// known key has an unusable value. Legacy option properties still work,
    /// but an option overrides its legacy property. Use options.
    pub fn insert_option<K: Into<String>, V: Serialize>(
        &mut self,
        key: K,
        value: V,
    ) -> Result<(), Error> {
        let as_json =
            serde_json::to_value(value).map_err(|e| Error::Serialization(e.to_string()))?;
        let key = key.into();
        if key == PROCESS_PERSON_PROFILE_OPT {
            self.personless_default = false;
        }
        let _ = self.options.insert(key, as_json);
        Ok(())
    }

    /// Remove a capture option from the event and return its previous value, if any.
    pub fn remove_option(&mut self, key: &str) -> Option<serde_json::Value> {
        if key == PROCESS_PERSON_PROFILE_OPT {
            self.personless_default = false;
        }
        self.options.remove(key)
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
    /// Adding a group does not change person processing. PostHog attaches
    /// groups for group analytics only to events that process person profiles,
    /// so an event from [`Event::new_anon`] needs
    /// `insert_option("process_person_profile", true)` to count for its groups.
    pub fn add_group(&mut self, group_name: &str, group_id: &str) {
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
    /// Never returns an error; PostHog clamps timestamps far in the future.
    /// The `Result` stays so existing callers keep compiling.
    pub fn set_timestamp<Tz>(&mut self, timestamp: DateTime<Tz>) -> Result<(), Error>
    where
        Tz: TimeZone,
    {
        self.timestamp = Some(timestamp.naive_utc());
        Ok(())
    }

    /// Build the `$create_alias` event that [`crate::Client::alias`] sends, so you
    /// can set options on it before you capture it.
    ///
    /// # Parameters
    ///
    /// - `previous_id`: ID already known to PostHog, such as an anonymous ID.
    /// - `distinct_id`: ID it should be merged into, such as a logged-in user ID.
    ///
    /// # Returns
    ///
    /// The event, attributed to `previous_id`, which is also mirrored into
    /// `properties.distinct_id` alongside the merge target in `properties.alias`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidArgument`] when either ID is blank, because a merge
    /// needs both sides.
    ///
    /// # Remarks
    ///
    /// The SDK sends `process_person_profile: true` on this event unless you set
    /// the option, so a request context cannot turn person processing off for it.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn send(posthog: &posthog::Client) -> Result<(), posthog::Error> {
    /// let mut event = posthog::Event::alias("anon-abc123", "user-42")?;
    /// event.insert_option("disable_skew_correction", true)?;
    /// posthog.capture(event);
    /// # Ok(())
    /// # }
    /// ```
    pub fn alias<P: Into<String>, D: Into<String>>(
        previous_id: P,
        distinct_id: D,
    ) -> Result<Self, Error> {
        let (previous_id, distinct_id) = (previous_id.into(), distinct_id.into());
        if previous_id.trim().is_empty() || distinct_id.trim().is_empty() {
            return Err(Error::InvalidArgument(
                "alias needs a previous_id and a distinct_id that are not blank".to_string(),
            ));
        }

        // Built directly rather than through `insert_prop`, so construction cannot fail.
        let mut properties = HashMap::new();
        properties.insert(
            "distinct_id".to_string(),
            serde_json::Value::String(previous_id.clone()),
        );
        properties.insert("alias".to_string(), serde_json::Value::String(distinct_id));

        Ok(Self {
            event: "$create_alias".to_string(),
            distinct_id: previous_id,
            properties,
            options: HashMap::new(),
            groups: HashMap::new(),
            timestamp: None,
            uuid: None,
            minimal_flag_called: false,
            request_context_applied: false,
            personless_default: false,
            personless: false,
        })
    }

    /// Build the `$groupidentify` event that [`crate::Client::group_identify`]
    /// sends, so you can set options on it before you capture it.
    ///
    /// # Parameters
    ///
    /// - `group_type`: Group type, such as `"company"`.
    /// - `group_key`: Unique identifier for the group, such as an ID in your database.
    /// - `properties`: Any serializable object or JSON map of group properties.
    ///
    /// # Returns
    ///
    /// The event, with `$group_type`, `$group_key`, and `$group_set` in its
    /// properties. Its distinct ID is the request context's distinct ID when one
    /// is set, else `$<group_type>_<group_key>`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidArgument`] when `group_type` or `group_key` is
    /// blank. Returns [`Error::Serialization`] if `properties` fails to serialize
    /// to JSON, or if it does not serialize to a JSON object. PostHog requires
    /// `$group_set` to be an object and silently drops the event otherwise.
    ///
    /// # Remarks
    ///
    /// The SDK sends `process_person_profile: true` on this event unless you set
    /// the option, so a request context cannot turn person processing off for it.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn send(posthog: &posthog::Client) -> Result<(), posthog::Error> {
    /// let properties = serde_json::json!({ "name": "Awesome Inc." });
    /// let mut event = posthog::Event::group_identify("company", "acme_123", properties)?;
    /// event.insert_option("disable_skew_correction", true)?;
    /// posthog.capture(event);
    /// # Ok(())
    /// # }
    /// ```
    pub fn group_identify<T: Into<String>, K: Into<String>, P: Serialize>(
        group_type: T,
        group_key: K,
        properties: P,
    ) -> Result<Self, Error> {
        let (group_type, group_key) = (group_type.into(), group_key.into());
        if group_type.trim().is_empty() || group_key.trim().is_empty() {
            return Err(Error::InvalidArgument(
                "group_identify needs a group_type and a group_key that are not blank".to_string(),
            ));
        }

        let group_set =
            serde_json::to_value(properties).map_err(|e| Error::Serialization(e.to_string()))?;
        if !group_set.is_object() {
            return Err(Error::Serialization(format!(
                "group_identify() properties must serialize to a JSON object, got {group_set}"
            )));
        }

        let distinct_id =
            current_distinct_id().unwrap_or_else(|| format!("${group_type}_{group_key}"));
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

        Ok(Self {
            event: "$groupidentify".to_string(),
            distinct_id,
            properties: props,
            options: HashMap::new(),
            groups: HashMap::new(),
            timestamp: None,
            uuid: None,
            minimal_flag_called: false,
            request_context_applied: false,
            personless_default: false,
            personless: false,
        })
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
    /// Useful for deduplication when re-importing historical data. Every
    /// capture of an event with this uuid sends the same uuid. When one request
    /// contains a uuid twice, PostHog keeps the first event and drops the later
    /// copies, which the SDK reports with the reason `duplicate_event_uuid`.
    pub fn set_uuid(&mut self, uuid: Uuid) {
        self.uuid = Some(uuid);
    }

    /// Assign a v7 uuid if the event has none; the client calls this once when
    /// it takes the event.
    pub(crate) fn ensure_uuid(&mut self) {
        if self.uuid.is_none() {
            self.uuid = Some(Uuid::now_v7());
        }
    }

    /// Attach the flag state captured by a [`FeatureFlagEvaluations`] snapshot
    /// to this event.
    ///
    /// Adds `$feature/<key>` for every evaluated flag plus a sorted
    /// `$active_feature_flags` list of enabled keys, mirroring what
    /// `send_feature_flags` would otherwise fetch — but without making an
    /// extra `/flags` request.
    ///
    /// A property already on the event keeps its value, so the caller can
    /// override a `$feature/<key>` or `$active_feature_flags` value whether it
    /// is set before or after this call.
    ///
    /// # Returns
    ///
    /// Returns `self` so calls can be chained before capture.
    pub fn with_flags(&mut self, flags: &FeatureFlagEvaluations) -> &mut Self {
        for (key, value) in flags.event_properties() {
            self.properties.entry(key).or_insert(value);
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

    pub(crate) fn uuid(&self) -> Option<Uuid> {
        self.uuid
    }

    pub(crate) fn timestamp(&self) -> Option<NaiveDateTime> {
        self.timestamp
    }

    /// Return the event properties.
    pub fn properties(&self) -> &HashMap<String, serde_json::Value> {
        &self.properties
    }

    /// Return the event's capture options, as set by [`Event::insert_option`].
    pub fn options(&self) -> &HashMap<String, serde_json::Value> {
        &self.options
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
        fill_property(&mut self.properties, key.into(), value);
    }

    /// Set a property, replacing any value it had.
    pub(crate) fn set_prop<K: Into<String>>(&mut self, key: K, value: serde_json::Value) {
        self.properties.insert(key.into(), value);
    }

    /// Insert a default option only if the caller left it unset (missing or `null`).
    /// The personless default from [`Event::new_anon`] counts as unset.
    pub(crate) fn insert_option_default<K: Into<String>>(
        &mut self,
        key: K,
        value: serde_json::Value,
    ) {
        if value.is_null() {
            return;
        }
        let key = key.into();
        if key == PROCESS_PERSON_PROFILE_OPT && self.personless_default {
            self.personless_default = false;
            self.options.insert(key, value);
            return;
        }
        let slot = self.options.entry(key).or_insert(serde_json::Value::Null);
        if slot.is_null() {
            *slot = value;
        }
    }

    /// Fill the personless `process_person_profile: false` while the option is unset (missing
    /// or `null`), on an event built by [`Event::new_anon`]. Runs with the other SDK values,
    /// before `before_send`.
    pub(crate) fn fill_personless_default(&mut self) {
        if !self.personless {
            return;
        }
        let slot = self
            .options
            .entry(PROCESS_PERSON_PROFILE_OPT.to_string())
            .or_insert(serde_json::Value::Null);
        if slot.is_null() {
            *slot = serde_json::Value::Bool(false);
            self.personless_default = true;
        }
    }

    /// On an [`IDENTIFY_STYLE_EVENTS`] event, resolve `process_person_profile` from the
    /// caller's own values before any layer fills it, so no layer can turn person processing
    /// off for it.
    ///
    /// A caller option stays. Else a caller `$process_person_profile` property moves into
    /// the option, so a layer cannot fill the option first. Else the option is set to
    /// `true`, unless the SDK generated the distinct ID, which keeps the personless default.
    /// A `null` option or property counts as unset; a `null` property stays for capture
    /// to remove. Idempotent while no layer has filled the event.
    pub(crate) fn apply_identify_style_person_default(&mut self) {
        if !self.is_identify_style() {
            return;
        }
        // The personless default from `new_anon` is the SDK's, not the caller's.
        let caller_option = !self.personless_default
            && self
                .options
                .get(PROCESS_PERSON_PROFILE_OPT)
                .is_some_and(|value| !value.is_null());
        if caller_option {
            return;
        }
        let legacy = self
            .properties
            .get(PROCESS_PERSON_PROFILE_PROP)
            .filter(|value| !value.is_null())
            .cloned();
        let value = match legacy {
            Some(legacy) => {
                self.properties.remove(PROCESS_PERSON_PROFILE_PROP);
                legacy
            }
            None if self.personless => return,
            None => serde_json::Value::Bool(true),
        };
        self.personless_default = false;
        self.options
            .insert(PROCESS_PERSON_PROFILE_OPT.into(), value);
    }

    /// On an [`IDENTIFY_STYLE_EVENTS`] event whose `process_person_profile` option is set,
    /// remove the `$process_person_profile` property that capture would ignore. Runs right
    /// before `before_send`, so the hook sees one value, and a hook that removes the option
    /// cannot bring back a layered property value.
    pub(crate) fn remove_shadowed_legacy_person_property(&mut self) {
        let option_set = self
            .options
            .get(PROCESS_PERSON_PROFILE_OPT)
            .is_some_and(|value| !value.is_null());
        if option_set && self.is_identify_style() {
            self.properties.remove(PROCESS_PERSON_PROFILE_PROP);
        }
    }

    fn is_identify_style(&self) -> bool {
        IDENTIFY_STYLE_EVENTS.contains(&self.event.as_str())
    }

    pub(crate) fn groups(&self) -> &HashMap<String, String> {
        &self.groups
    }

    /// Mark this event as a minimized `$feature_flag_called` event. The capture
    /// pipeline then trims its properties to
    /// [`MINIMAL_FLAG_CALLED_EVENT_PROPERTIES`] before `before_send`.
    pub(crate) fn mark_minimal_flag_called(&mut self) {
        self.minimal_flag_called = true;
    }

    /// Move groups added with [`Event::add_group`] into the `$groups` property,
    /// so `before_send` sees them and can change or remove them.
    pub(crate) fn merge_groups_into_properties(&mut self) {
        merge_groups_into(&mut self.properties, &self.groups);
        self.groups.clear();
    }

    /// Trim a minimized `$feature_flag_called` event to
    /// [`MINIMAL_FLAG_CALLED_EVENT_PROPERTIES`]; other events are unchanged.
    pub(crate) fn apply_minimal_flag_called_allowlist(&mut self) {
        if self.minimal_flag_called {
            self.properties
                .retain(|key, _| is_minimal_flag_called_property(key));
        }
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use chrono::{DateTime, Utc};

    use uuid::Uuid;

    use super::Event;
    use crate::constants::PROCESS_PERSON_PROFILE_OPT;
    use crate::Error;

    #[test]
    fn test_timestamp_is_correctly_set() {
        let mut event = Event::new_anon("test");
        let ts = DateTime::parse_from_rfc3339("2023-01-01T10:00:00+03:00").unwrap();
        event.set_timestamp(ts).unwrap();
        let expected = DateTime::parse_from_rfc3339("2023-01-01T07:00:00Z").unwrap();
        assert_eq!(event.timestamp.unwrap(), expected.naive_utc())
    }

    #[test]
    fn future_timestamp_is_kept_for_posthog_to_handle() {
        let mut event = Event::new_anon("test");
        let ts = Utc::now() + Duration::from_secs(60);
        event.set_timestamp(ts).unwrap();
        assert_eq!(event.timestamp, Some(ts.naive_utc()));
    }

    #[test]
    fn blank_distinct_id_makes_an_anonymous_event() {
        for blank in ["", " ", " \n\t "] {
            let event = Event::new("test", blank);
            let generated = Uuid::parse_str(event.distinct_id()).unwrap();
            assert_eq!(generated.get_version_num(), 7);
            assert_eq!(
                event.options().get(PROCESS_PERSON_PROFILE_OPT),
                Some(&serde_json::json!(false))
            );
        }
        let event = Event::new("test", " user-1 ");
        assert_eq!(event.distinct_id(), " user-1 ");
        assert!(event.options().is_empty());
    }

    #[test]
    fn uuid_is_assigned_once_per_event_unless_set() {
        let template = Event::new("test", "user1");
        assert_eq!(template.uuid(), None);

        // Each clone handed to the client gets its own uuid, kept on reassignment.
        let mut first = template.clone();
        let mut second = template.clone();
        first.ensure_uuid();
        second.ensure_uuid();
        let assigned = first.uuid().unwrap();
        assert_eq!(assigned.get_version_num(), 7);
        assert_ne!(second.uuid(), Some(assigned));
        first.ensure_uuid();
        assert_eq!(first.uuid(), Some(assigned));

        // A caller's uuid is never replaced.
        let caller = Uuid::now_v7();
        let mut event = template;
        event.set_uuid(caller);
        event.ensure_uuid();
        assert_eq!(event.uuid(), Some(caller));
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
    fn options_insert_replace_and_remove() {
        let mut event = Event::new("test", "user1");
        event.insert_option("process_person_profile", true).unwrap();
        event
            .insert_option("process_person_profile", false)
            .unwrap();
        event.insert_option("future_option", vec![1, 2]).unwrap();
        assert_eq!(
            event.options().get("process_person_profile"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            event.remove_option("future_option"),
            Some(serde_json::json!([1, 2]))
        );
        assert_eq!(event.remove_option("future_option"), None);
        assert_eq!(event.options().len(), 1);
        // Options never leak into properties.
        assert!(event.properties().is_empty());

        // JSON object keys must be strings, so a tuple-keyed map cannot serialize.
        let unserializable: std::collections::HashMap<(u8, u8), u8> = [((1, 2), 3)].into();
        let err = event
            .insert_option("bad", unserializable)
            .expect_err("a map with non-string keys cannot serialize");
        assert!(matches!(err, Error::Serialization(_)));
        assert!(!event.options().contains_key("bad"));
    }

    #[test]
    fn alias_rejects_blank_ids() {
        for (previous_id, distinct_id) in
            [("", "user"), ("   ", "user"), ("anon", ""), ("anon", "   ")]
        {
            let err =
                Event::alias(previous_id, distinct_id).expect_err("a blank ID should be rejected");
            assert!(matches!(err, Error::InvalidArgument(_)));
        }
    }

    #[test]
    fn group_identify_rejects_blank_keys() {
        for (group_type, group_key) in [("", "key"), ("   ", "key"), ("type", ""), ("type", "   ")]
        {
            let err = Event::group_identify(group_type, group_key, serde_json::json!({}))
                .expect_err("a blank group_type or group_key should be rejected");
            assert!(matches!(err, Error::InvalidArgument(_)));
        }
    }

    #[test]
    fn group_identify_rejects_non_object_properties() {
        for properties in [
            serde_json::json!(42),
            serde_json::json!(["a", "b"]),
            serde_json::Value::Null,
        ] {
            let err = Event::group_identify("company", "acme_123", properties)
                .expect_err("non-object properties should be rejected");
            assert!(matches!(err, Error::Serialization(_)));
        }
    }
}
