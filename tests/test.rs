#[cfg(all(feature = "e2e-test", feature = "async-client"))]
#[tokio::test]
async fn get_client_async() {
    use dotenv::dotenv;
    dotenv().ok(); // Load the .env file
    println!("Loaded .env for tests");

    // see https://us.posthog.com/project/115809/ for the e2e project
    use posthog_rs::Event;
    use std::collections::HashMap;

    let api_key = std::env::var("POSTHOG_RS_E2E_TEST_API_KEY").unwrap();
    let client = posthog_rs::client(api_key.as_str()).await;

    let mut child_map = HashMap::new();
    child_map.insert("child_key1", "child_value1");

    let mut event = Event::new("e2e test event", "1234");
    event.insert_prop("key1", "value1").unwrap();
    event.insert_prop("key2", vec!["a", "b"]).unwrap();
    event.insert_prop("key3", child_map).unwrap();

    client.capture(event).await.unwrap();
}

#[cfg(all(feature = "e2e-test", not(feature = "async-client")))]
#[test]
fn get_client_blocking() {
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

// E2E Test for Feature Flag Events
#[cfg(all(feature = "e2e-test", feature = "async-client"))]
#[tokio::test]
async fn test_feature_flag_events_e2e_async() {
    use dotenv::dotenv;
    use std::collections::HashMap;
    dotenv().ok();

    let api_key = std::env::var("POSTHOG_RS_E2E_TEST_API_KEY").unwrap();
    let client = posthog_rs::client(api_key.as_str()).await;

    println!("Testing feature flag evaluation with real API...");

    // Test 1: Evaluate a feature flag (this should create a $feature_flag_called event)
    let mut properties = HashMap::new();
    properties.insert("email".to_string(), serde_json::json!("test@example.com"));

    let result = client
        .get_feature_flag(
            "test-flag",
            "e2e-test-user-123",
            None,
            Some(properties.clone()),
            None,
        )
        .await;

    println!("Feature flag result: {:?}", result);
    assert!(result.is_ok());

    // Test 2: Test is_feature_enabled (should also create event)
    let enabled_result = client
        .is_feature_enabled(
            "test-flag",
            "e2e-test-user-456",
            None,
            Some(properties.clone()),
            None,
        )
        .await;

    println!("Feature enabled result: {:?}", enabled_result);
    assert!(enabled_result.is_ok());

    // Test 3: Check same flag again for same user - should NOT create new event (deduplication)
    println!("\nTesting deduplication - calling same flag for user-123 again...");
    let duplicate_result = client
        .get_feature_flag(
            "test-flag",
            "e2e-test-user-123",
            None,
            Some(properties.clone()),
            None,
        )
        .await;

    println!("Duplicate call result: {:?}", duplicate_result);
    println!("Note: This should NOT create a new event (same user + flag + response)");

    // Give time for events to be sent
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    println!("\n=== E2E Test Summary ===");
    println!("✅ Total flag evaluations: 3");
    println!("✅ Expected events in PostHog: 2");
    println!("   - Event 1: user-123 + test-flag + false");
    println!("   - Event 2: user-456 + test-flag + false");
    println!("   - Event 3: DEDUPLICATED (same as Event 1)");
    println!("\nCheck PostHog for exactly 2 new $feature_flag_called events.");
}

#[cfg(all(feature = "e2e-test", not(feature = "async-client")))]
#[test]
fn test_feature_flag_events_e2e_blocking() {
    use dotenv::dotenv;
    use std::collections::HashMap;
    dotenv().ok();

    let api_key = std::env::var("POSTHOG_RS_E2E_TEST_API_KEY").unwrap();
    let client = posthog_rs::client(api_key.as_str());

    println!("Testing feature flag evaluation with real API (blocking)...");

    // Test 1: Evaluate a feature flag (this should create a $feature_flag_called event)
    let mut properties = HashMap::new();
    properties.insert("email".to_string(), serde_json::json!("test@example.com"));

    let result = client.get_feature_flag(
        "test-flag".to_string(),
        "e2e-test-user-123".to_string(),
        None,
        Some(properties.clone()),
        None,
    );

    println!("Feature flag result: {:?}", result);
    assert!(result.is_ok());

    // Test 2: Test is_feature_enabled (should also create event)
    let enabled_result = client.is_feature_enabled(
        "test-flag".to_string(),
        "e2e-test-user-456".to_string(),
        None,
        Some(properties.clone()),
        None,
    );

    println!("Feature enabled result: {:?}", enabled_result);
    assert!(enabled_result.is_ok());

    // Test 3: Check same flag again for same user - should NOT create new event (deduplication)
    println!("\nTesting deduplication - calling same flag for user-123 again...");
    let duplicate_result = client.get_feature_flag(
        "test-flag".to_string(),
        "e2e-test-user-123".to_string(),
        None,
        Some(properties.clone()),
        None,
    );

    println!("Duplicate call result: {:?}", duplicate_result);
    println!("Note: This should NOT create a new event (same user + flag + response)");

    // Give time for events to be sent
    std::thread::sleep(std::time::Duration::from_secs(2));

    println!("\n=== E2E Test Summary ===");
    println!("✅ Total flag evaluations: 3");
    println!("✅ Expected events in PostHog: 2");
    println!("   - Event 1: user-123 + test-flag + false");
    println!("   - Event 2: user-456 + test-flag + false");
    println!("   - Event 3: DEDUPLICATED (same as Event 1)");
    println!("\nCheck PostHog for exactly 2 new $feature_flag_called events.");
}
