use posthog_rs::{client, ClientOptions, FlagValue};
use std::collections::HashMap;
use serde_json::json;

#[cfg(not(feature = "async-client"))]
fn main() {
    // Initialize the PostHog client with your API key
    let api_key = std::env::var("POSTHOG_API_KEY")
        .expect("POSTHOG_API_KEY environment variable not set");
    
    let client = client(ClientOptions::from(api_key.as_str()));
    
    // Example 1: Check if a feature flag is enabled for a user
    match client.is_feature_enabled(
        "new-dashboard".to_string(),
        "user-123".to_string(),
        None,
        None,
        None,
    ) {
        Ok(enabled) => {
            if enabled {
                println!("New dashboard feature is enabled for user-123");
            } else {
                println!("New dashboard feature is disabled for user-123");
            }
        }
        Err(e) => println!("Error checking feature flag: {}", e),
    }
    
    // Example 2: Get feature flag value (could be boolean or string variant)
    match client.get_feature_flag(
        "experiment-variant".to_string(),
        "user-456".to_string(),
        None,
        None,
        None,
    ) {
        Ok(Some(FlagValue::String(variant))) => {
            println!("User is in variant: {}", variant);
        }
        Ok(Some(FlagValue::Boolean(enabled))) => {
            println!("Feature flag is: {}", if enabled { "enabled" } else { "disabled" });
        }
        Ok(None) => {
            println!("Feature flag not found or disabled");
        }
        Err(e) => println!("Error getting feature flag: {}", e),
    }
    
    // Example 3: Get feature flags with person properties
    let mut person_properties = HashMap::new();
    person_properties.insert("plan".to_string(), json!("enterprise"));
    person_properties.insert("country".to_string(), json!("US"));
    
    match client.get_feature_flags(
        "user-789".to_string(),
        None,
        Some(person_properties),
        None,
    ) {
        Ok(response) => {
            println!("Feature flags for user-789:");
            for (flag_key, flag_value) in response.feature_flags {
                match flag_value {
                    FlagValue::Boolean(b) => println!("  {}: {}", flag_key, b),
                    FlagValue::String(s) => println!("  {}: {}", flag_key, s),
                }
            }
            
            if !response.feature_flag_payloads.is_empty() {
                println!("Feature flag payloads:");
                for (flag_key, payload) in response.feature_flag_payloads {
                    println!("  {}: {}", flag_key, payload);
                }
            }
        }
        Err(e) => println!("Error getting feature flags: {}", e),
    }
    
    // Example 4: Get feature flags with groups (for B2B apps)
    let mut groups = HashMap::new();
    groups.insert("company".to_string(), "company-abc".to_string());
    
    let mut group_properties = HashMap::new();
    let mut company_props = HashMap::new();
    company_props.insert("industry".to_string(), json!("technology"));
    company_props.insert("size".to_string(), json!(500));
    group_properties.insert("company".to_string(), company_props);
    
    match client.get_feature_flag(
        "b2b-feature".to_string(),
        "user-in-company".to_string(),
        Some(groups),
        None,
        Some(group_properties),
    ) {
        Ok(Some(value)) => {
            println!("B2B feature flag value: {:?}", value);
        }
        Ok(None) => {
            println!("B2B feature flag is disabled");
        }
        Err(e) => println!("Error getting B2B feature flag: {}", e),
    }
    
    // Example 5: Get feature flag payload (additional data)
    match client.get_feature_flag_payload(
        "onboarding-flow".to_string(),
        "new-user".to_string(),
    ) {
        Ok(Some(payload)) => {
            println!("Onboarding flow payload: {}", payload);
            // You can parse the payload to get specific configuration
            if let Some(steps) = payload.get("steps") {
                println!("Onboarding steps: {}", steps);
            }
        }
        Ok(None) => {
            println!("No payload for onboarding flow");
        }
        Err(e) => println!("Error getting feature flag payload: {}", e),
    }
}

#[cfg(feature = "async-client")]
#[tokio::main]
async fn main() {
    // Initialize the PostHog client with your API key
    let api_key = std::env::var("POSTHOG_API_KEY")
        .expect("POSTHOG_API_KEY environment variable not set");
    
    let client = client(ClientOptions::from(api_key.as_str())).await;
    
    // Example 1: Check if a feature flag is enabled for a user
    match client.is_feature_enabled(
        "new-dashboard".to_string(),
        "user-123".to_string(),
        None,
        None,
        None,
    ).await {
        Ok(enabled) => {
            if enabled {
                println!("New dashboard feature is enabled for user-123");
            } else {
                println!("New dashboard feature is disabled for user-123");
            }
        }
        Err(e) => println!("Error checking feature flag: {}", e),
    }
    
    // Example 2: Get feature flag value (could be boolean or string variant)
    match client.get_feature_flag(
        "experiment-variant".to_string(),
        "user-456".to_string(),
        None,
        None,
        None,
    ).await {
        Ok(Some(FlagValue::String(variant))) => {
            println!("User is in variant: {}", variant);
        }
        Ok(Some(FlagValue::Boolean(enabled))) => {
            println!("Feature flag is: {}", if enabled { "enabled" } else { "disabled" });
        }
        Ok(None) => {
            println!("Feature flag not found or disabled");
        }
        Err(e) => println!("Error getting feature flag: {}", e),
    }
    
    // Example 3: Get feature flags with person properties
    let mut person_properties = HashMap::new();
    person_properties.insert("plan".to_string(), json!("enterprise"));
    person_properties.insert("country".to_string(), json!("US"));
    
    match client.get_feature_flags(
        "user-789".to_string(),
        None,
        Some(person_properties),
        None,
    ).await {
        Ok(response) => {
            println!("Feature flags for user-789:");
            for (flag_key, flag_value) in response.feature_flags {
                match flag_value {
                    FlagValue::Boolean(b) => println!("  {}: {}", flag_key, b),
                    FlagValue::String(s) => println!("  {}: {}", flag_key, s),
                }
            }
            
            if !response.feature_flag_payloads.is_empty() {
                println!("Feature flag payloads:");
                for (flag_key, payload) in response.feature_flag_payloads {
                    println!("  {}: {}", flag_key, payload);
                }
            }
        }
        Err(e) => println!("Error getting feature flags: {}", e),
    }
}