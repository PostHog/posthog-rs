use async_posthog::Event;
use std::collections::HashMap;

fn build_client() -> async_posthog::Client {
    async_posthog::client(env!("POSTHOG_API_KEY"))
}

#[tokio::test]
async fn capture() {
    let client = build_client();

    let mut child_map = HashMap::new();
    child_map.insert("child_key1", "child_value1");

    let mut event = Event::new("test async capture", "1234");
    event.insert_prop("key1", "value1").unwrap();
    event.insert_prop("key2", vec!["a", "b"]).unwrap();
    event.insert_prop("key3", child_map).unwrap();

    client.capture(event).await.unwrap();
}

#[tokio::test]
async fn capture_batch() {
    let client = build_client();

    let events = (0..16)
        .map(|_| {
            let mut child_map = HashMap::new();
            child_map.insert("child_key1", "child_value1");

            let mut event = Event::new("test async capture batch", "1234");
            event.insert_prop("key1", "value1").unwrap();
            event.insert_prop("key2", vec!["a", "b"]).unwrap();
            event.insert_prop("key3", child_map).unwrap();

            event
        })
        .collect::<Vec<_>>();

    client.capture_batch(events).await.unwrap();
}
