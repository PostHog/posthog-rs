#[cfg(test)]
pub mod tests {
    use super::*;

    // see https://us.posthog.com/project/115809/ for the e2e project

    #[test]
    fn inner_event_adds_lib_properties_correctly() {
        // Arrange
        let mut event = Event::new("unit test event", "1234");
        event.insert_prop("key1", "value1").unwrap();
        let api_key = "test_api_key".to_string();

        // Act
        let inner_event = InnerEvent::new(event, api_key);

        // Assert
        let props = &inner_event.properties.props;
        assert_eq!(
            props.get("$lib_name"),
            Some(&serde_json::Value::String("posthog-rs".to_string()))
        );
    }

    #[cfg(feature = "e2e-test")]
    #[test]
    fn get_client() {
        use std::collections::HashMap;

        let api_key = std::env::var("POSTHOG_RS_E2E_TEST_API_KEY").unwrap();
        let client = crate::client(api_key.as_str());

        let mut child_map = HashMap::new();
        child_map.insert("child_key1", "child_value1");

        let mut event = Event::new("e2e test event", "1234");
        event.insert_prop("key1", "value1").unwrap();
        event.insert_prop("key2", vec!["a", "b"]).unwrap();
        event.insert_prop("key3", child_map).unwrap();

        client.capture(event).unwrap();
    }
}
