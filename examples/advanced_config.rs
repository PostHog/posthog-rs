/// SDK Configuration Examples
/// 
/// Shows different ways to configure the PostHog Rust SDK for various use cases.
/// 
/// Run with: cargo run --example advanced_config --features async-client

use posthog_rs::{ClientOptionsBuilder, Event, EU_INGESTION_ENDPOINT};
use std::collections::HashMap;
use serde_json::json;

#[cfg(feature = "async-client")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== PostHog SDK Configuration Examples ===\n");

    // 1. SIMPLEST: Just an API key (uses US endpoint by default)
    println!("1. Basic client (US region):");
    let _basic = posthog_rs::client("phc_test_api_key").await;
    println!("   → Created with default settings\n");

    // 2. REGIONAL: EU data residency
    println!("2. EU region client:");
    let _eu = posthog_rs::client(("phc_test_api_key", EU_INGESTION_ENDPOINT)).await;
    println!("   → Data stays in EU (GDPR compliant)\n");

    // 3. SELF-HOSTED: Your own PostHog instance
    println!("3. Self-hosted instance:");
    let _custom = posthog_rs::client(("phc_test_api_key", "https://analytics.mycompany.com")).await;
    println!("   → Uses your private PostHog deployment\n");

    // 4. PRODUCTION: Common production settings
    println!("4. Production configuration:");
    let production_config = ClientOptionsBuilder::default()
        .api_key("phc_production_key".to_string())
        .host("https://eu.posthog.com")  // Auto-detects and uses EU ingestion
        .gzip(true)                       // Compress requests
        .flush_at(100)                     // Batch 100 events
        .flush_interval_seconds(10)        // Or flush every 10 seconds
        .max_retries(3)                    // Retry on failure
        .request_timeout_seconds(30)       // 30s timeout
        .build()?;
    
    let _prod = posthog_rs::client(production_config).await;
    println!("   → Optimized for production workloads\n");

    // 5. HIGH-PERFORMANCE: Local flag evaluation
    println!("5. High-performance with local evaluation:");
    let performance_config = ClientOptionsBuilder::default()
        .api_key("phc_project_key".to_string())
        .personal_api_key("phx_personal_key")  // Required for local eval
        .enable_local_evaluation(true)         // Cache flags locally
        .poll_interval_seconds(30)             // Update cache every 30s
        .feature_flags_request_timeout_seconds(3)
        .build()?;
    
    let _perf = posthog_rs::client(performance_config).await;
    println!("   → Evaluates flags locally (100x faster)\n");

    // 6. DEVELOPMENT: Debugging and testing
    println!("6. Development/debug configuration:");
    let debug_config = ClientOptionsBuilder::default()
        .api_key("phc_test_key".to_string())
        .debug(true)           // Enable debug logs
        .disabled(true)        // Disable actual sending (for testing)
        .disable_geoip(true)   // Skip geoip lookup
        .build()?;
    
    let debug_client = posthog_rs::client(debug_config).await;
    println!("   → Safe for development (tracking disabled)\n");

    // 7. SHARED CONTEXT: Super properties on all events
    println!("7. Adding context to all events:");
    let mut context = HashMap::new();
    context.insert("app_version".to_string(), json!("2.0.0"));
    context.insert("environment".to_string(), json!("staging"));
    context.insert("server_region".to_string(), json!("us-west-2"));
    
    let context_config = ClientOptionsBuilder::default()
        .api_key("phc_test_key".to_string())
        .super_properties(context)
        .build()?;
    
    let _context_client = posthog_rs::client(context_config).await;
    println!("   → Automatically includes version, env, region on all events\n");

    // Example event (won't actually send with test keys)
    let event = Event::new("config_example_completed", "demo-user");
    let _ = debug_client.capture(event).await;  // This won't send (disabled=true)

    println!("✅ Configuration examples complete!");
    println!("\nTip: Check out 'feature_flags' example for flag usage");
    println!("     and 'local_evaluation' for performance optimization.");
    
    Ok(())
}

#[cfg(not(feature = "async-client"))]
fn main() {
    println!("This example requires the async-client feature.");
    println!("Run with: cargo run --example advanced_config --features async-client");
}