use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct EventBuilder {
    properties: Value,
    event: String,
    distinct_id: Option<String>,
    timestamp: Option<String>,
}

impl EventBuilder {
    // ! Base Properties
    pub fn new(name: &str) -> Self {
        Self {
            event: name.to_string(),
            properties: json!({}),
            ..Default::default()
        }
    }

    pub fn distinct_id(mut self, distinct_id: String) -> Self {
        self.distinct_id = Some(distinct_id);
        self
    }

    pub fn timestamp(mut self, timestamp: String) -> Self {
        self.timestamp = Some(timestamp);
        self
    }

    pub fn timestamp_now(mut self) -> Self {
        self.timestamp = Some(Utc::now().to_rfc3339());
        self
    }

    /// Sets event properties, this will overwrite any existing properties
    /// Call it first
    pub fn properties(mut self, properties: Value) -> Self {
        self.properties = properties;
        self
    }

    // ! Build
    pub fn build(self) -> Value {
        json!(self)
    }

    // ! Optional Fields
    /// Anonymous event capture
    pub fn anonymous(mut self, anonymous: bool) -> Self {
        self.properties["$process_person_profile"] = (!anonymous).into();
        self
    }

    /// Alias
    pub fn alias(mut self, alias: String) -> Self {
        self.properties["alias"] = alias.into();
        self
    }
    /// Group identify
    pub fn group_identify(
        mut self,
        group_type: String,
        group_key: String,
        group_set: Value,
    ) -> Self {
        self.properties["$group_type"] = group_type.into();
        self.properties["$group_key"] = group_key.into();
        self.properties["$group_set"] = group_set;

        self
    }

    /// Groups
    pub fn groups(mut self, groups: Value) -> Self {
        self.properties["$groups"] = groups;
        self
    }

    // ! Special Events

    /// Identify, Refer to https://posthog.com/docs/api/capture#identify
    pub fn identify_event(distinct_id: String, values: Value) -> Value {
        EventBuilder::new("$identify")
            .properties(values)
            .distinct_id(distinct_id)
            .timestamp_now()
            .build()
    }

    /// Pageview, Refer to https://posthog.com/docs/api/capture#pageview
    pub fn pageview_event(
        distinct_id: String,
        url: String,
        values: impl Into<Option<Value>>,
    ) -> Value {
        let mut values = match values.into() {
            Some(values) => values,
            None => json!({}),
        };
        values["$current_url"] = url.into();

        EventBuilder::new("$pageview")
            .properties(values)
            .distinct_id(distinct_id)
            .timestamp_now()
            .build()
    }

    /// Screen view, Refer to https://posthog.com/docs/api/capture#screen
    pub fn screen_view_event(
        distinct_id: String,
        name: String,
        values: impl Into<Option<Value>>,
    ) -> Value {
        let mut values = match values.into() {
            Some(values) => values,
            None => json!({}),
        };
        values["$screen_name"] = name.into();

        EventBuilder::new("$screen")
            .properties(values)
            .distinct_id(distinct_id)
            .timestamp_now()
            .build()
    }

    /// Survey, Refer to https://posthog.com/docs/api/capture#survey
    pub fn survey_event(
        distinct_id: String,
        survey_id: String,
        survey_response: String,
        values: impl Into<Option<Value>>,
    ) -> Value {
        let mut values = match values.into() {
            Some(values) => values,
            None => json!({}),
        };

        values["$survey_id"] = survey_id.into();
        values["$survey_response"] = survey_response.into();

        EventBuilder::new("$survey")
            .distinct_id(distinct_id)
            .properties(values)
            .timestamp_now()
            .build()
    }

    /// Feature Flag Called
    pub fn feature_flag_called_event(
        distinct_id: String,
        feature_flag: String,
        feature_flag_response: String,
        values: impl Into<Option<Value>>,
    ) -> Value {
        let mut values = match values.into() {
            Some(values) => values,
            None => json!({}),
        };
        values["$feature_flag"] = feature_flag.into();
        values["$feature_flag_response"] = feature_flag_response.into();

        EventBuilder::new("$feature_flag_called")
            .distinct_id(distinct_id)
            .properties(values)
            .timestamp_now()
            .build()
    }
}
