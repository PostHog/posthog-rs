/// Feature Flags Example
///
/// Shows all feature flag patterns: boolean flags, A/B tests, payloads, and targeting.
///
/// Run with real API:
///   export POSTHOG_API_TOKEN=phc_your_key
///   cargo run --example feature_flags --features async-client
use posthog_rs::FlagValue;
use serde_json::json;
use std::collections::HashMap;

#[cfg(feature = "async-client")]
#[tokio::main]
async fn main() {
    // Try to get API key from environment, or use demo mode
    let api_key = std::env::var("POSTHOG_API_TOKEN").unwrap_or_else(|_| {
        println!("No POSTHOG_API_TOKEN found. Running in demo mode with mock data.\n");
        "demo_api_key".to_string()
    });

    let is_demo = api_key == "demo_api_key";

    // Create client
    let client = if is_demo {
        create_demo_client().await
    } else {
        posthog_rs::client(api_key.as_str()).await
    };

    // Example 1: Simple boolean flag check
    println!("=== Example 1: Boolean Feature Flag ===");
    let user_id = "user-123";

    match client
        .is_feature_enabled(
            "new-dashboard".to_string(),
            user_id.to_string(),
            None,
            None,
            None,
        )
        .await
    {
        Ok(enabled) => {
            if enabled {
                println!("New dashboard is ENABLED for current user");
            } else {
                println!("New dashboard is DISABLED for current user");
            }
        }
        Err(e) => println!("Error checking flag: {}", e),
    }

    // Example 2: Multivariate flag (A/B testing)
    println!("\n=== Example 2: A/B Test Variant ===");

    match client
        .get_feature_flag(
            "checkout-flow".to_string(),
            user_id.to_string(),
            None,
            None,
            None,
        )
        .await
    {
        Ok(Some(FlagValue::String(variant))) => {
            println!("Current user gets checkout variant: {}", variant);
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

    match client
        .get_feature_flag(
            "premium-features".to_string(),
            user_id.to_string(),
            None,
            Some(properties.clone()),
            None,
        )
        .await
    {
        Ok(Some(FlagValue::Boolean(true))) => {
            println!("Premium features ENABLED (user matches targeting rules)");
        }
        Ok(Some(FlagValue::Boolean(false))) => {
            println!("Premium features DISABLED (user doesn't match targeting rules)");
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

    match client
        .get_feature_flags(user_id.to_string(), None, Some(properties), None)
        .await
    {
        Ok((flags, payloads)) => {
            println!("All flags for current user:");
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

    match client
        .get_feature_flag_payload("onboarding-config".to_string(), user_id.to_string())
        .await
    {
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
    println!("Note: Running in demo mode. API calls will fail but code structure is shown.\n");
    posthog_rs::client(("demo_key", "https://demo.posthog.com")).await
}

#[cfg(not(feature = "async-client"))]
fn main() {
    println!("This example requires the async-client feature.");
    println!("Run with: cargo run --example feature_flags --features async-client");
}
