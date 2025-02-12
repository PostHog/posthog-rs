#[cfg(test)]
mod tests {
    use crate::models::event::{build_feature_flag_called_event, build_identify_event, build_pageview_event, build_screen_view_event, build_survey_event, EventBuilder};

    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn test_custom_event_builder() {
        let event = EventBuilder::new("custom_event")
            .distinct_id("user123".to_string())
            .timestamp_now()
            .properties(json!({"custom_prop": "value"}))
            .build();

        let event_obj = event.as_object().unwrap();
        assert_eq!(event_obj["event"], "custom_event");
        assert_eq!(event_obj["distinct_id"], "user123");
        assert!(event_obj["timestamp"].as_str().unwrap().starts_with(&Utc::now().date_naive().to_string()));
        assert_eq!(event_obj["properties"]["custom_prop"], "value");
    }

    #[test]
    fn test_anonymous_event() {
        let event = EventBuilder::new("anon_event")
            .anonymous(true)
            .build();

        let event_obj = event.as_object().unwrap();
        assert_eq!(event_obj["properties"]["$process_person_profile"], false);
    }

    #[test]
    fn test_group_identify_event() {
        let event = EventBuilder::new("group_event")
            .group_identify(
                "company".to_string(),
                "company_123".to_string(),
                json!({"name": "Acme Inc"})
            )
            .build();

        let event_obj = event.as_object().unwrap();
        assert_eq!(event_obj["properties"]["$group_type"], "company");
        assert_eq!(event_obj["properties"]["$group_key"], "company_123");
        assert_eq!(event_obj["properties"]["$group_set"]["name"], "Acme Inc");
    }

    #[test]
    fn test_identify_event() {
        let values = json!({
            "email": "test@example.com",
            "name": "Test User"
        });
        let event = build_identify_event("user123".to_string(), values);

        let event_obj = event.as_object().unwrap();
        assert_eq!(event_obj["event"], "$identify");
        assert_eq!(event_obj["distinct_id"], "user123");
        assert_eq!(event_obj["properties"]["email"], "test@example.com");
        assert_eq!(event_obj["properties"]["name"], "Test User");
    }

    #[test]
    fn test_pageview_event() {
        let values = json!({
            "referrer": "https://example.com"
        });
        let event = build_pageview_event(
            "user123".to_string(),
            "https://app.example.com/dashboard".to_string(),
            Some(values)
        );

        let event_obj = event.as_object().unwrap();
        assert_eq!(event_obj["event"], "$pageview");
        assert_eq!(event_obj["distinct_id"], "user123");
        assert_eq!(event_obj["properties"]["$current_url"], "https://app.example.com/dashboard");
        assert_eq!(event_obj["properties"]["referrer"], "https://example.com");
    }

    #[test]
    fn test_screen_view_event() {
        let values = json!({
            "app_version": "1.0.0"
        });
        let event = build_screen_view_event(
            "user123".to_string(),
            "Dashboard".to_string(),
            Some(values)
        );

        let event_obj = event.as_object().unwrap();
        assert_eq!(event_obj["event"], "$screen");
        assert_eq!(event_obj["distinct_id"], "user123");
        assert_eq!(event_obj["properties"]["$screen_name"], "Dashboard");
        assert_eq!(event_obj["properties"]["app_version"], "1.0.0");
    }

    #[test]
    fn test_survey_event() {
        let values = json!({
            "user_type": "premium"
        });
        let event = build_survey_event(
            "user123".to_string(),
            "survey_456".to_string(),
            "Great service!".to_string(),
            Some(values)
        );

        let event_obj = event.as_object().unwrap();
        assert_eq!(event_obj["event"], "$survey");
        assert_eq!(event_obj["distinct_id"], "user123");
        assert_eq!(event_obj["properties"]["$survey_id"], "survey_456");
        assert_eq!(event_obj["properties"]["$survey_response"], "Great service!");
        assert_eq!(event_obj["properties"]["user_type"], "premium");
    }

    #[test]
    fn test_event_builder_failure_cases() {
        // Test missing required fields
        let event = EventBuilder::new("").build();
        assert_eq!(event["event"], "");
        assert!(event["distinct_id"].is_null());

        // Test invalid properties
        let event = EventBuilder::new("test")
            .properties(json!(null))
            .build();
        assert_eq!(event["properties"], json!(null));

        // Test invalid timestamp format
        let event = EventBuilder::new("test")
            .timestamp("invalid-timestamp".to_string())
            .build();
        assert_eq!(event["timestamp"], "invalid-timestamp");
    }

    #[test]
    fn test_standard_events_failure_cases() {
        // Test identify with empty values
        let event = build_identify_event("user123".to_string(), json!({}));
        assert!(event["properties"].as_object().unwrap().is_empty());

        // Test pageview with empty URL
        let event = build_pageview_event("user123".to_string(), "".to_string(), None);
        assert_eq!(event["properties"]["$current_url"], "");

        // Test screen view with empty name
        let event = build_screen_view_event("user123".to_string(), "".to_string(), None);
        assert_eq!(event["properties"]["$screen_name"], "");

        // Test survey with empty response
        let event = build_survey_event(
            "user123".to_string(),
            "survey_123".to_string(),
            "".to_string(),
            None
        );
        assert_eq!(event["properties"]["$survey_response"], "");
    }

    #[test]
    fn test_feature_flag_called_event() {
        let event = build_feature_flag_called_event(
            "user123".to_string(),
            "feature_flag_key".to_string(),
            "variant_name".to_string(),
            None,
        );
        assert_eq!(event["event"], "$feature_flag_called");
        assert_eq!(event["distinct_id"], "user123");
        assert_eq!(
            event["properties"],
            json!({ "$feature_flag": "feature_flag_key", "$feature_flag_response": "variant_name" })
        );
    }
}
