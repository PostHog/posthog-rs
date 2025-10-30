/// Feature Flag Events Example
///
/// Demonstrates automatic `$feature_flag_called` event capture when evaluating feature flags.
/// These events help track which flags are being used and their values.
///
/// Setup:
///   export POSTHOG_API_TOKEN=phc_your_project_key
///   cargo run --example feature_flag_events --features async-client
///
/// Then check your PostHog dashboard for `$feature_flag_called` events!
use posthog_rs::{ClientOptionsBuilder, Event};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;

#[cfg(feature = "async-client")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get API key from environment
    let api_key = match std::env::var("POSTHOG_API_TOKEN") {
        Ok(key) => key,
        Err(_) => {
            eprintln!("Error: POSTHOG_API_TOKEN environment variable not set");
            eprintln!("Please set it to your PostHog project API token");
            eprintln!("\nExample: export POSTHOG_API_TOKEN=phc_...");
            std::process::exit(1);
        }
    };

    println!("=== Feature Flag Events Demo ===\n");
    println!("This example shows how PostHog automatically captures");
    println!("`$feature_flag_called` events when you evaluate feature flags.\n");

    // Create client with feature flag events ENABLED (default)
    let client_with_events = {
        let options = ClientOptionsBuilder::default()
            .api_key(api_key.clone())
            .send_feature_flag_events(true) // This is the default
            .build()?;

        posthog_rs::client(options).await
    };

    // Create client with feature flag events DISABLED (for comparison)
    let client_without_events = {
        let options = ClientOptionsBuilder::default()
            .api_key(api_key)
            .send_feature_flag_events(false) // Disable automatic events
            .build()?;

        posthog_rs::client(options).await
    };

    let user_id = "example-user-123";
    let mut properties = HashMap::new();
    properties.insert("email".to_string(), json!("user@example.com"));
    properties.insert("plan".to_string(), json!("premium"));

    // Example 1: Automatic event capture (default behavior)
    println!("=== Example 1: Automatic Event Capture ===");
    println!("Checking feature flag 'new-dashboard'...");

    match client_with_events
        .is_feature_enabled(
            "new-dashboard",
            user_id,
            None,
            Some(properties.clone()),
            None,
        )
        .await
    {
        Ok(enabled) => {
            println!("✅ Flag evaluated: {}", enabled);
            println!("📊 A `$feature_flag_called` event was automatically sent to PostHog!");
            println!("   Event properties include:");
            println!("   - $feature_flag: 'new-dashboard'");
            println!("   - $feature_flag_response: {}", enabled);
            println!("   - distinct_id: '{}'", user_id);
        }
        Err(e) => println!("❌ Error: {}", e),
    }

    // Example 2: Multiple flag evaluations with deduplication
    println!("\n=== Example 2: Deduplication ===");
    println!("Checking the same flag multiple times...");

    for i in 1..=3 {
        println!("\nAttempt {}:", i);
        match client_with_events
            .get_feature_flag(
                "new-dashboard",
                user_id,
                None,
                Some(properties.clone()),
                None,
            )
            .await
        {
            Ok(value) => {
                println!("   Flag value: {:?}", value);
                if i == 1 {
                    println!("   → Event sent (first time for this user + flag + value)");
                } else {
                    println!("   → Event NOT sent (deduplicated - same user/flag/value)");
                }
            }
            Err(e) => println!("   Error: {}", e),
        }
    }

    // Example 3: Different users get separate events
    println!("\n=== Example 3: Different Users ===");
    println!("Checking flag for different users...");

    for user_num in 1..=3 {
        let user = format!("user-{}", user_num);
        match client_with_events
            .is_feature_enabled("new-dashboard", &user, None, Some(properties.clone()), None)
            .await
        {
            Ok(enabled) => {
                println!(
                    "✅ User {}: {} → Event sent (different user)",
                    user_num, enabled
                );
            }
            Err(e) => println!("❌ User {}: Error - {}", user_num, e),
        }
    }

    // Example 4: Multivariate flags with variants
    println!("\n=== Example 4: Multivariate Flags ===");
    println!("Checking multivariate flag 'checkout-flow'...");

    match client_with_events
        .get_feature_flag(
            "checkout-flow",
            user_id,
            None,
            Some(properties.clone()),
            None,
        )
        .await
    {
        Ok(Some(variant)) => {
            println!("✅ User got variant: {:?}", variant);
            println!("📊 Event captured with:");
            println!("   - $feature_flag: 'checkout-flow'");
            println!("   - $feature_flag_response: {:?}", variant);
            println!("   - Includes variant information");
        }
        Ok(None) => println!("Flag not found or not evaluated"),
        Err(e) => println!("Error: {}", e),
    }

    // Example 5: Disabling automatic events
    println!("\n=== Example 5: Disabled Events ===");
    println!("Using client with send_feature_flag_events=false...");

    match client_without_events
        .is_feature_enabled(
            "new-dashboard",
            "another-user",
            None,
            Some(properties.clone()),
            None,
        )
        .await
    {
        Ok(enabled) => {
            println!("✅ Flag evaluated: {}", enabled);
            println!("📊 NO event was sent (events disabled for this client)");
        }
        Err(e) => println!("Error: {}", e),
    }

    // Example 6: Manual event capture with flag context
    println!("\n=== Example 6: Manual Event with Flag Context ===");
    println!("Manually capturing an event that includes feature flag info...");

    // Evaluate flag first
    let flag_enabled = client_with_events
        .is_feature_enabled(
            "new-dashboard",
            user_id,
            None,
            Some(properties.clone()),
            None,
        )
        .await
        .unwrap_or(false);

    // Manually capture a business event with flag context
    let mut event = Event::new("button_clicked", user_id);
    event
        .insert_prop("button_id", json!("checkout-button"))
        .ok();
    event
        .insert_prop("new_dashboard_enabled", json!(flag_enabled))
        .ok();

    match client_with_events.capture(event).await {
        Ok(_) => {
            println!("✅ Manual event captured with flag context");
            println!("   This gives you flag state alongside your business events!");
        }
        Err(e) => println!("Error: {}", e),
    }

    // Give events time to flush
    println!("\n⏳ Waiting for events to flush to PostHog...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    println!("\n=== Summary ===");
    println!("✅ Feature flag events are automatically captured by default");
    println!("✅ Events are deduplicated per user+flag+value combination");
    println!("✅ Up to 50,000 unique combinations are tracked");
    println!("✅ Events include rich metadata (locally_evaluated, payloads, etc.)");
    println!("\n📊 Check your PostHog dashboard for `$feature_flag_called` events!");
    println!("   Filter by: Event name = '$feature_flag_called'");

    Ok(())
}

#[cfg(not(feature = "async-client"))]
fn main() {
    println!("This example requires the async-client feature.");
    println!("Run with: cargo run --example feature_flag_events --features async-client");
}
