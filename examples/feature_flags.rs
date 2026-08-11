//! Feature flags example.
//!
//! Shows boolean flags, multivariate flags, payloads, and property targeting
//! through the snapshot-based `evaluate_flags` API.
//!
//! Run with a real API key:
//!   export POSTHOG_API_TOKEN=phc_your_key
//!   cargo run --example feature_flags --features async-client

#[cfg(feature = "async-client")]
use posthog::{EvaluateFlagsOptions, FlagValue};
#[cfg(feature = "async-client")]
use serde_json::json;
#[cfg(feature = "async-client")]
use std::collections::HashMap;

#[cfg(feature = "async-client")]
#[tokio::main]
async fn main() {
    let api_key = std::env::var("POSTHOG_API_TOKEN").unwrap_or_else(|_| {
        println!("No POSTHOG_API_TOKEN found. Running in demo mode.\n");
        "demo_api_key".to_string()
    });

    let client = if api_key == "demo_api_key" {
        create_demo_client().await
    } else {
        posthog::client(api_key.as_str()).await
    };

    let user_id = "user-123";
    let mut person_properties = HashMap::new();
    person_properties.insert("plan".to_string(), json!("premium"));
    person_properties.insert("country".to_string(), json!("US"));
    person_properties.insert("account_age_days".to_string(), json!(45));

    let mut options = EvaluateFlagsOptions::default();
    options.person_properties = Some(person_properties);
    let flags = match client.evaluate_flags(user_id, options).await {
        Ok(flags) => flags,
        Err(error) => {
            eprintln!("Unable to evaluate feature flags: {error}");
            return;
        }
    };

    println!("=== Boolean feature flag ===");
    if flags.is_enabled("new-dashboard") {
        println!("New dashboard is enabled");
    } else {
        println!("New dashboard is disabled");
    }

    println!("\n=== Multivariate feature flag ===");
    match flags.get_flag("checkout-flow") {
        Some(FlagValue::String(variant)) => println!("Checkout variant: {variant}"),
        Some(FlagValue::Boolean(enabled)) => println!("Checkout flag is boolean: {enabled}"),
        None => println!("Checkout flag was not returned"),
    }

    println!("\n=== All evaluated flags ===");
    let mut keys = flags.keys();
    keys.sort();
    for key in keys {
        println!("  {key}: {:?}", flags.get_flag(&key));
    }

    println!("\n=== Feature flag payload ===");
    match flags.get_flag_payload("onboarding-config") {
        Some(payload) => println!("{}", serde_json::to_string_pretty(&payload).unwrap()),
        None => println!("No payload for onboarding-config"),
    }
}

#[cfg(feature = "async-client")]
async fn create_demo_client() -> posthog::Client {
    println!("API calls will fail in demo mode, but the example shows the API shape.\n");
    posthog::client(("demo_key", "https://demo.posthog.com")).await
}

#[cfg(not(feature = "async-client"))]
fn main() {
    println!("This example requires the async-client feature.");
    println!("Run with: cargo run --example feature_flags --features async-client");
}
