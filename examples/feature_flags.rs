/// Feature Flags Example
/// 
/// Demonstrates how to use feature flags with the PostHog Rust SDK.
/// This example shows:
/// - Boolean feature flags (on/off)
/// - Multivariate flags (A/B testing with variants)
/// - Feature flag payloads
/// - Using person properties for targeting
/// 
/// To run with real PostHog:
///   export POSTHOG_API_TOKEN=your_api_key
///   cargo run --example feature_flags --features async-client
///
/// Without an API key, it will use mock data for demonstration.

use posthog_rs::{ClientOptionsBuilder, FlagValue};
use std::collections::HashMap;
use serde_json::json;

#[cfg(feature = "async-client")]
#[tokio::main]
async fn main() {
    // Try to get API key from environment, or use demo mode
    let api_key = std::env::var("POSTHOG_API_TOKEN")
        .unwrap_or_else(|_| {
            println!("No POSTHOG_API_TOKEN found. Running in demo mode with mock data.\n");
            "demo_api_key".to_string()
        });

    let is_demo = api_key == "demo_api_key";

    // Create client
    let client = if is_demo {
        create_demo_client().await
    } else {
        let options = ClientOptionsBuilder::default()
            .api_key(api_key)
            .build()
            .unwrap();
        posthog_rs::client(options).await
    };

    // Example 1: Simple boolean flag check
    println!("=== Example 1: Boolean Feature Flag ===");
    let user_id = "user-123";
    
    match client.is_feature_enabled(
        "new-dashboard".to_string(),
        user_id.to_string(),
        None,
        None,
        None,
    ).await {
        Ok(enabled) => {
            if enabled {
                println!("✅ New dashboard is enabled for {}", user_id);
            } else {
                println!("❌ New dashboard is disabled for {}", user_id);
            }
        }
        Err(e) => println!("Error checking flag: {}", e),
    }

    // Example 2: Multivariate flag (A/B testing)
    println!("\n=== Example 2: A/B Test Variant ===");
    
    match client.get_feature_flag(
        "checkout-flow".to_string(),
        user_id.to_string(),
        None,
        None,
        None,
    ).await {
        Ok(Some(FlagValue::String(variant))) => {
            println!("User {} gets checkout variant: {}", user_id, variant);
            match variant.as_str() {
                "control" => println!("  → Show original checkout flow"),
                "variant-a" => println!("  → Show streamlined checkout"),
                "variant-b" => println!("  → Show one-click checkout"),
                _ => println!("  → Unknown variant"),
            }
        }
        Ok(Some(FlagValue::Boolean(enabled))) => {
            println!("Checkout flow flag is a boolean: {}", enabled);
        }
        Ok(None) => {
            println!("Checkout flow flag not found or not evaluated");
        }
        Err(e) => println!("Error getting flag: {}", e),
    }

    // Example 3: Using person properties for targeting
    println!("\n=== Example 3: Property-based Targeting ===");
    
    let mut properties = HashMap::new();
    properties.insert("plan".to_string(), json!("premium"));
    properties.insert("country".to_string(), json!("US"));
    properties.insert("account_age_days".to_string(), json!(45));
    
    match client.get_feature_flag(
        "premium-features".to_string(),
        user_id.to_string(),
        None,
        Some(properties.clone()),
        None,
    ).await {
        Ok(Some(FlagValue::Boolean(true))) => {
            println!("✅ Premium features enabled (user matches targeting rules)");
        }
        Ok(Some(FlagValue::Boolean(false))) => {
            println!("❌ Premium features disabled (user doesn't match targeting rules)");
        }
        Ok(Some(FlagValue::String(v))) => {
            println!("Premium features variant: {}", v);
        }
        Ok(None) => {
            println!("Premium features flag not found");
        }
        Err(e) => println!("Error: {}", e),
    }

    // Example 4: Getting all flags at once
    println!("\n=== Example 4: Batch Flag Evaluation ===");
    
    match client.get_feature_flags(
        user_id.to_string(),
        None,
        Some(properties),
        None,
    ).await {
        Ok((flags, payloads)) => {
            println!("All flags for {}:", user_id);
            for (flag_key, flag_value) in flags {
                match flag_value {
                    FlagValue::Boolean(b) => println!("  {}: {}", flag_key, b),
                    FlagValue::String(s) => println!("  {}: \"{}\"", flag_key, s),
                }
            }
            
            if !payloads.is_empty() {
                println!("\nFlag payloads:");
                for (flag_key, payload) in payloads {
                    println!("  {}: {}", flag_key, payload);
                }
            }
        }
        Err(e) => println!("Error getting all flags: {}", e),
    }

    // Example 5: Feature flag with payload
    println!("\n=== Example 5: Feature Flag Payload ===");
    
    match client.get_feature_flag_payload(
        "onboarding-config".to_string(),
        user_id.to_string(),
    ).await {
        Ok(Some(payload)) => {
            println!("Onboarding configuration payload:");
            println!("{}", serde_json::to_string_pretty(&payload).unwrap());
            
            // Use payload data
            if let Some(steps) = payload.get("steps").and_then(|v| v.as_array()) {
                println!("\nOnboarding steps: {} steps total", steps.len());
            }
        }
        Ok(None) => {
            println!("No payload for onboarding-config flag");
        }
        Err(e) => println!("Error getting payload: {}", e),
    }
}

#[cfg(feature = "async-client")]
async fn create_demo_client() -> posthog_rs::Client {
    // In demo mode, create a client that will fail gracefully
    // In a real app, you might want to use a mock server or local evaluation
    let options = ClientOptionsBuilder::default()
        .host("https://demo.posthog.com")
        .api_key("demo_key".to_string())
        .build()
        .unwrap();
    
    let client = posthog_rs::client(options).await;
    
    // Note: API calls will fail in demo mode, but the example structure is shown
    println!("Note: Running in demo mode. API calls will fail but the code structure is demonstrated.\n");
    
    client
}

#[cfg(not(feature = "async-client"))]
fn main() {
    println!("This example requires the async-client feature.");
    println!("Run with: cargo run --example feature_flags --features async-client");
}