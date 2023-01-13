use posthog_rs::Event;
use std::collections::HashMap;

#[test]
fn get_client() {
    let client = posthog_rs::client(env!("POSTHOG_API_KEY"));

    let mut child_map = HashMap::new();
    child_map.insert("child_key1", "child_value1");

    let mut event = Event::new("test", "1234");
    event.insert_prop("key1", "value1").unwrap();
    event.insert_prop("key2", vec!["a", "b"]).unwrap();
    event.insert_prop("key3", child_map).unwrap();

    client.capture(event).unwrap();
}
