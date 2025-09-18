/// Local Evaluation Example
/// 
/// Demonstrates high-performance local feature flag evaluation.
/// Local evaluation:
/// - Fetches all flag definitions once at startup  
/// - Polls for updates periodically in the background
/// - Evaluates flags locally without API calls (100-1000x faster)
/// 
/// Requirements:
///   export POSTHOG_API_TOKEN=your_project_api_key
///   export POSTHOG_PERSONAL_API_TOKEN=your_personal_api_key
///   cargo run --example local_evaluation --features async-client
///
/// Personal API keys can be created at: https://app.posthog.com/me/settings

use posthog_rs::ClientOptionsBuilder;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use serde_json::json;

#[cfg(feature = "async-client")]
#[tokio::main]
async fn main() {
    // Get API keys from environment
    let api_key = match std::env::var("POSTHOG_API_TOKEN") {
        Ok(key) => key,
        Err(_) => {
            eprintln!("Error: POSTHOG_API_TOKEN environment variable not set");
            eprintln!("Please set it to your PostHog project API token");
            eprintln!("\nExample: export POSTHOG_API_TOKEN=phc_...");
            std::process::exit(1);
        }
    };

    let personal_key = match std::env::var("POSTHOG_PERSONAL_API_TOKEN") {
        Ok(key) => key,
        Err(_) => {
            eprintln!("Error: POSTHOG_PERSONAL_API_TOKEN environment variable not set");
            eprintln!("Please set it to your PostHog personal API token");
            eprintln!("\nTo create a personal API key:");
            eprintln!("1. Go to https://app.posthog.com/me/settings");
            eprintln!("2. Click 'Create personal API key'");
            eprintln!("3. Export it: export POSTHOG_PERSONAL_API_TOKEN=phx_...");
            std::process::exit(1);
        }
    };

    println!("=== Local Evaluation Performance Demo ===\n");

    // Create client WITH local evaluation
    let local_client = {
        let options = ClientOptionsBuilder::default()
            .api_key(api_key.clone())
            .personal_api_key(personal_key)
            .enable_local_evaluation(true)
            .poll_interval_seconds(30) // Poll for updates every 30 seconds
            .build()
            .unwrap();
        
        posthog_rs::client(options).await
    };

    // Create client WITHOUT local evaluation (for comparison)
    let api_client = {
        let options = ClientOptionsBuilder::default()
            .api_key(api_key)
            .build()
            .unwrap();
        
        posthog_rs::client(options).await
    };

    // Give local evaluation time to fetch initial flags
    println!("Fetching flag definitions for local evaluation...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Test data
    let user_id = "perf-test-user";
    let mut properties = HashMap::new();
    properties.insert("plan".to_string(), json!("enterprise"));
    properties.insert("country".to_string(), json!("US"));

    // Performance comparison
    println!("\n=== Performance Comparison ===");
    
    // Test API evaluation speed
    println!("\n1. API Evaluation (10 requests):");
    let start = Instant::now();
    for i in 0..10 {
        let _ = api_client.get_feature_flag(
            "using-feature-flags".to_string(),
            format!("{}-{}", user_id, i),
            None,
            Some(properties.clone()),
            None,
        ).await;
    }
    let api_duration = start.elapsed();
    println!("   Time: {:?} total, {:?} per request", 
             api_duration, 
             api_duration / 10);

    // Test local evaluation speed
    println!("\n2. Local Evaluation (10 requests):");
    let start = Instant::now();
    for i in 0..10 {
        let _ = local_client.get_feature_flag(
            "using-feature-flags".to_string(),
            format!("{}-{}", user_id, i),
            None,
            Some(properties.clone()),
            None,
        ).await;
    }
    let local_duration = start.elapsed();
    println!("   Time: {:?} total, {:?} per request", 
             local_duration,
             local_duration / 10);

    // Show speedup
    let speedup = api_duration.as_micros() as f64 / local_duration.as_micros().max(1) as f64;
    println!("\n📊 Local evaluation is {:.1}x faster!", speedup);

    // Demonstrate batch evaluation
    println!("\n=== Batch Evaluation Demo ===");
    
    let start = Instant::now();
    match local_client.get_feature_flags(
        user_id.to_string(),
        None,
        Some(properties),
        None,
    ).await {
        Ok((flags, _)) => {
            let duration = start.elapsed();
            println!("Evaluated {} flags in {:?}", flags.len(), duration);
            
            // Show some flag values
            println!("\nSample flags:");
            for (key, value) in flags.iter().take(5) {
                println!("  {}: {:?}", key, value);
            }
        }
        Err(e) => println!("Error: {}", e),
    }

    println!("\n✅ Local evaluation continues polling for updates in the background");
    println!("   Updates will be fetched every 30 seconds automatically");
}

#[cfg(not(feature = "async-client"))]
fn main() {
    println!("This example requires the async-client feature.");
    println!("Run with: cargo run --example local_evaluation --features async-client");
}