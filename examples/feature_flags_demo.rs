/// Feature Flags Demo Application
/// 
/// This is a complete demo showing how to use feature flags in a real application.
/// It simulates an e-commerce platform with various feature-flagged functionality.
/// 
/// To run: cargo run --example feature_flags_demo --all-features
/// 
/// Set POSTHOG_API_KEY environment variable or it will use a demo mode with local evaluation.

use posthog_rs::{client, ClientOptions, FlagValue};
use std::collections::HashMap;
use std::env;
use serde_json::json;

#[cfg(feature = "async-client")]
#[tokio::main]
async fn main() {
    run_async_demo().await;
}

#[cfg(not(feature = "async-client"))]
fn main() {
    run_sync_demo();
}

#[cfg(not(feature = "async-client"))]
fn run_sync_demo() {
    println!("🚀 PostHog Feature Flags Demo - E-Commerce Platform");
    println!("{}", "=".repeat(60));
    
    // Try to get real API key, fallback to demo mode
    let api_key = env::var("POSTHOG_API_KEY").unwrap_or_else(|_| {
        println!("⚠️  No POSTHOG_API_KEY found. Running in demo mode with mock data.\n");
        "phc_demo_key_for_testing".to_string()
    });
    
    let client = client(ClientOptions::from(api_key.as_str()));
    
    // Demo users with different properties
    let users = vec![
        ("user-001", "Alice", "US", "premium", 5000),
        ("user-002", "Bob", "UK", "basic", 100),
        ("user-003", "Charlie", "US", "enterprise", 50000),
        ("user-004", "Diana", "FR", "premium", 2000),
        ("user-005", "Eve", "US", "basic", 50),
    ];
    
    println!("Testing feature flags for different users:\n");
    
    for (user_id, name, country, plan, lifetime_value) in users {
        println!("👤 User: {} ({})", name, user_id);
        println!("   Properties: country={}, plan={}, LTV=${}", country, plan, lifetime_value);
        
        let mut person_properties = HashMap::new();
        person_properties.insert("country".to_string(), json!(country));
        person_properties.insert("plan".to_string(), json!(plan));
        person_properties.insert("lifetime_value".to_string(), json!(lifetime_value));
        person_properties.insert("name".to_string(), json!(name));
        
        // Test different feature flags
        test_new_checkout_flow(&client, user_id, &person_properties);
        test_ai_recommendations(&client, user_id, &person_properties);
        test_pricing_experiment(&client, user_id, &person_properties);
        test_holiday_theme(&client, user_id, &person_properties);
        
        println!();
    }
    
    // Interactive testing
    println!("{}", "=".repeat(60));
    println!("\n📝 Interactive Testing");
    println!("You can now test with custom user IDs and properties.\n");
    
    loop {
        print!("Enter user ID (or 'quit' to exit): ");
        use std::io::Write;
        std::io::stdout().flush().unwrap();
        
        let mut user_id = String::new();
        std::io::stdin().read_line(&mut user_id).unwrap();
        let user_id = user_id.trim();
        
        if user_id == "quit" {
            break;
        }
        
        print!("Enter country (e.g., US, UK, FR): ");
        use std::io::Write;
        std::io::stdout().flush().unwrap();
        
        let mut country = String::new();
        std::io::stdin().read_line(&mut country).unwrap();
        let country = country.trim();
        
        print!("Enter plan (basic, premium, enterprise): ");
        use std::io::Write;
        std::io::stdout().flush().unwrap();
        
        let mut plan = String::new();
        std::io::stdin().read_line(&mut plan).unwrap();
        let plan = plan.trim();
        
        let mut person_properties = HashMap::new();
        person_properties.insert("country".to_string(), json!(country));
        person_properties.insert("plan".to_string(), json!(plan));
        
        println!("\nResults for user '{}':", user_id);
        test_all_flags(&client, user_id, &person_properties);
        println!();
    }
    
    println!("\n✅ Demo completed!");
}

#[cfg(feature = "async-client")]
async fn run_async_demo() {
    println!("🚀 PostHog Feature Flags Demo - E-Commerce Platform (Async)");
    println!("{}", "=".repeat(60));
    
    // Try to get real API key, fallback to demo mode
    let api_key = env::var("POSTHOG_API_KEY").unwrap_or_else(|_| {
        println!("⚠️  No POSTHOG_API_KEY found. Running in demo mode with mock data.\n");
        "phc_demo_key_for_testing".to_string()
    });
    
    let client = client(ClientOptions::from(api_key.as_str())).await;
    
    // Demo users with different properties
    let users = vec![
        ("user-001", "Alice", "US", "premium", 5000),
        ("user-002", "Bob", "UK", "basic", 100),
        ("user-003", "Charlie", "US", "enterprise", 50000),
        ("user-004", "Diana", "FR", "premium", 2000),
        ("user-005", "Eve", "US", "basic", 50),
    ];
    
    println!("Testing feature flags for different users:\n");
    
    for (user_id, name, country, plan, lifetime_value) in users {
        println!("👤 User: {} ({})", name, user_id);
        println!("   Properties: country={}, plan={}, LTV=${}", country, plan, lifetime_value);
        
        let mut person_properties = HashMap::new();
        person_properties.insert("country".to_string(), json!(country));
        person_properties.insert("plan".to_string(), json!(plan));
        person_properties.insert("lifetime_value".to_string(), json!(lifetime_value));
        person_properties.insert("name".to_string(), json!(name));
        
        // Test different feature flags
        test_new_checkout_flow_async(&client, user_id, &person_properties).await;
        test_ai_recommendations_async(&client, user_id, &person_properties).await;
        test_pricing_experiment_async(&client, user_id, &person_properties).await;
        test_holiday_theme_async(&client, user_id, &person_properties).await;
        
        println!();
    }
    
    println!("\n✅ Demo completed!");
}

// Synchronous test functions
#[cfg(not(feature = "async-client"))]
fn test_new_checkout_flow(client: &posthog_rs::Client, user_id: &str, props: &HashMap<String, serde_json::Value>) {
    match client.is_feature_enabled(
        "new-checkout-flow".to_string(),
        user_id.to_string(),
        None,
        Some(props.clone()),
        None,
    ) {
        Ok(enabled) => {
            if enabled {
                println!("   ✅ New checkout flow: ENABLED");
            } else {
                println!("   ❌ New checkout flow: DISABLED");
            }
        }
        Err(_) => {
            // Fallback to local evaluation for demo
            let enabled = props.get("plan").and_then(|p| p.as_str()) 
                .map(|p| p == "enterprise" || p == "premium")
                .unwrap_or(false);
            if enabled {
                println!("   ✅ New checkout flow: ENABLED (local eval)");
            } else {
                println!("   ❌ New checkout flow: DISABLED (local eval)");
            }
        }
    }
}

#[cfg(not(feature = "async-client"))]
fn test_ai_recommendations(client: &posthog_rs::Client, user_id: &str, props: &HashMap<String, serde_json::Value>) {
    match client.get_feature_flag(
        "ai-recommendations".to_string(),
        user_id.to_string(),
        None,
        Some(props.clone()),
        None,
    ) {
        Ok(Some(FlagValue::String(variant))) => {
            println!("   🤖 AI Recommendations: {} variant", variant);
        }
        Ok(Some(FlagValue::Boolean(enabled))) => {
            if enabled {
                println!("   🤖 AI Recommendations: ENABLED");
            } else {
                println!("   🤖 AI Recommendations: DISABLED");
            }
        }
        _ => {
            // Fallback for demo
            let variants = vec!["gpt-4", "gpt-3.5", "claude", "local-model"];
            let hash = user_id.bytes().fold(0u8, |acc, b| acc.wrapping_add(b));
            let variant = variants[hash as usize % variants.len()];
            println!("   🤖 AI Recommendations: {} variant (local eval)", variant);
        }
    }
}

#[cfg(not(feature = "async-client"))]
fn test_pricing_experiment(client: &posthog_rs::Client, user_id: &str, props: &HashMap<String, serde_json::Value>) {
    match client.get_feature_flag(
        "pricing-experiment".to_string(),
        user_id.to_string(),
        None,
        Some(props.clone()),
        None,
    ) {
        Ok(Some(FlagValue::String(variant))) => {
            let price = match variant.as_str() {
                "control" => "$99",
                "test-10-off" => "$89",
                "test-20-off" => "$79",
                _ => "$99",
            };
            println!("   💰 Pricing Experiment: {} (Price: {})", variant, price);
        }
        _ => {
            // Fallback for demo
            let ltv = props.get("lifetime_value")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let price = if ltv > 10000 { "$79" } else if ltv > 1000 { "$89" } else { "$99" };
            println!("   💰 Pricing Experiment: Price {} (based on LTV)", price);
        }
    }
}

#[cfg(not(feature = "async-client"))]
fn test_holiday_theme(client: &posthog_rs::Client, user_id: &str, props: &HashMap<String, serde_json::Value>) {
    match client.is_feature_enabled(
        "holiday-theme".to_string(),
        user_id.to_string(),
        None,
        Some(props.clone()),
        None,
    ) {
        Ok(enabled) => {
            if enabled {
                println!("   🎄 Holiday Theme: ACTIVE");
            } else {
                println!("   🎄 Holiday Theme: INACTIVE");
            }
        }
        Err(_) => {
            // Simple rollout for demo
            let country = props.get("country").and_then(|c| c.as_str()).unwrap_or("");
            let enabled = country == "US" || country == "UK";
            if enabled {
                println!("   🎄 Holiday Theme: ACTIVE (local eval)");
            } else {
                println!("   🎄 Holiday Theme: INACTIVE (local eval)");
            }
        }
    }
}

#[cfg(not(feature = "async-client"))]
fn test_all_flags(client: &posthog_rs::Client, user_id: &str, props: &HashMap<String, serde_json::Value>) {
    test_new_checkout_flow(client, user_id, props);
    test_ai_recommendations(client, user_id, props);
    test_pricing_experiment(client, user_id, props);
    test_holiday_theme(client, user_id, props);
}

// Async test functions
#[cfg(feature = "async-client")]
async fn test_new_checkout_flow_async(client: &posthog_rs::Client, user_id: &str, props: &HashMap<String, serde_json::Value>) {
    match client.is_feature_enabled(
        "new-checkout-flow".to_string(),
        user_id.to_string(),
        None,
        Some(props.clone()),
        None,
    ).await {
        Ok(enabled) => {
            if enabled {
                println!("   ✅ New checkout flow: ENABLED");
            } else {
                println!("   ❌ New checkout flow: DISABLED");
            }
        }
        Err(_) => {
            // Fallback to local evaluation for demo
            let enabled = props.get("plan").and_then(|p| p.as_str()) 
                .map(|p| p == "enterprise" || p == "premium")
                .unwrap_or(false);
            if enabled {
                println!("   ✅ New checkout flow: ENABLED (local eval)");
            } else {
                println!("   ❌ New checkout flow: DISABLED (local eval)");
            }
        }
    }
}

#[cfg(feature = "async-client")]
async fn test_ai_recommendations_async(client: &posthog_rs::Client, user_id: &str, props: &HashMap<String, serde_json::Value>) {
    match client.get_feature_flag(
        "ai-recommendations".to_string(),
        user_id.to_string(),
        None,
        Some(props.clone()),
        None,
    ).await {
        Ok(Some(FlagValue::String(variant))) => {
            println!("   🤖 AI Recommendations: {} variant", variant);
        }
        Ok(Some(FlagValue::Boolean(enabled))) => {
            if enabled {
                println!("   🤖 AI Recommendations: ENABLED");
            } else {
                println!("   🤖 AI Recommendations: DISABLED");
            }
        }
        _ => {
            // Fallback for demo
            let variants = vec!["gpt-4", "gpt-3.5", "claude", "local-model"];
            let hash = user_id.bytes().fold(0u8, |acc, b| acc.wrapping_add(b));
            let variant = variants[hash as usize % variants.len()];
            println!("   🤖 AI Recommendations: {} variant (local eval)", variant);
        }
    }
}

#[cfg(feature = "async-client")]
async fn test_pricing_experiment_async(client: &posthog_rs::Client, user_id: &str, props: &HashMap<String, serde_json::Value>) {
    match client.get_feature_flag(
        "pricing-experiment".to_string(),
        user_id.to_string(),
        None,
        Some(props.clone()),
        None,
    ).await {
        Ok(Some(FlagValue::String(variant))) => {
            let price = match variant.as_str() {
                "control" => "$99",
                "test-10-off" => "$89",
                "test-20-off" => "$79",
                _ => "$99",
            };
            println!("   💰 Pricing Experiment: {} (Price: {})", variant, price);
        }
        _ => {
            // Fallback for demo
            let ltv = props.get("lifetime_value")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let price = if ltv > 10000 { "$79" } else if ltv > 1000 { "$89" } else { "$99" };
            println!("   💰 Pricing Experiment: Price {} (based on LTV)", price);
        }
    }
}

#[cfg(feature = "async-client")]
async fn test_holiday_theme_async(client: &posthog_rs::Client, user_id: &str, props: &HashMap<String, serde_json::Value>) {
    match client.is_feature_enabled(
        "holiday-theme".to_string(),
        user_id.to_string(),
        None,
        Some(props.clone()),
        None,
    ).await {
        Ok(enabled) => {
            if enabled {
                println!("   🎄 Holiday Theme: ACTIVE");
            } else {
                println!("   🎄 Holiday Theme: INACTIVE");
            }
        }
        Err(_) => {
            // Simple rollout for demo
            let country = props.get("country").and_then(|c| c.as_str()).unwrap_or("");
            let enabled = country == "US" || country == "UK";
            if enabled {
                println!("   🎄 Holiday Theme: ACTIVE (local eval)");
            } else {
                println!("   🎄 Holiday Theme: INACTIVE (local eval)");
            }
        }
    }
}