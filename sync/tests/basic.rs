use posthog_rs::{Event, GroupIdentify};
use std::collections::HashMap;

fn build_client() -> posthog_rs::Client {
    posthog_rs::client(env!("POSTHOG_API_KEY"))
}

#[test]
fn capture() {
    let client = build_client();

    let mut child_map = HashMap::new();
    child_map.insert("child_key1", "child_value1");

    let mut event = Event::new("test sync capture", "1234");
    event.insert_prop("key1", "value1").unwrap();
    event.insert_prop("key2", vec!["a", "b"]).unwrap();
    event.insert_prop("key3", child_map).unwrap();

    event.insert_group("company", "company_key");

    client.capture(event).unwrap();
}

#[test]
fn capture_batch() {
    let client = build_client();

    let events = (0..16)
        .map(|_| {
            let mut child_map = HashMap::new();
            child_map.insert("child_key1", "child_value1");

            let mut event = Event::new("test sync capture batch", "1234");
            event.insert_prop("key1", "value1").unwrap();
            event.insert_prop("key2", vec!["a", "b"]).unwrap();
            event.insert_prop("key3", child_map).unwrap();

            event
        })
        .collect::<Vec<_>>();

    client.capture_batch(events).unwrap();
}

#[test]
fn group_identify() {
    let client = build_client();

    let mut event = GroupIdentify::new("organisation", "some_id");
    event.insert_prop("status", "active").unwrap();

    client.group_identify(event).unwrap();
}
