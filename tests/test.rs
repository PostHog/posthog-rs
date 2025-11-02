// CI runs: cargo test --features e2e-test --no-default-features
#[cfg(all(feature = "e2e-test", not(feature = "async-client")))]
#[test]
fn get_client() {
    use dotenv::dotenv;
    dotenv().ok(); // Load the .env file
    println!("Loaded .env for tests");

    // see https://us.posthog.com/project/115809/ for the e2e project
    use posthog_rs::Event;
    use std::collections::HashMap;

    let api_key = std::env::var("POSTHOG_RS_E2E_TEST_API_KEY").unwrap();
    let client = posthog_rs::client(api_key.as_str());

    let mut child_map = HashMap::new();
    child_map.insert("child_key1", "child_value1");

    let mut event = Event::new("e2e test event", "1234");
    event.insert_prop("key1", "value1").unwrap();
    event.insert_prop("key2", vec!["a", "b"]).unwrap();
    event.insert_prop("key3", child_map).unwrap();

    client.capture(event).unwrap();
}
