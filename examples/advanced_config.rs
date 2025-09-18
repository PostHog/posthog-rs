use posthog_rs::{ClientOptionsBuilder, EU_INGESTION_ENDPOINT, Event};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Example 1: Basic configuration with US endpoint (default)
    let basic_client = posthog_rs::client("phc_test_api_key").await;
    
    // Example 2: Using EU endpoint
    let eu_client = posthog_rs::client(("phc_test_api_key", EU_INGESTION_ENDPOINT)).await;
    
    // Example 3: Using custom endpoint
    let custom_client = posthog_rs::client(("phc_test_api_key", "https://my.posthog.instance.com")).await;
    
    // Example 4: Advanced configuration with all options
    let advanced_options = ClientOptionsBuilder::default()
        .api_key("phc_test_api_key".to_string())
        .host("https://eu.posthog.com")  // Will automatically use EU ingestion endpoint
        .request_timeout_seconds(60)
        .personal_api_key("phx_personal_key")  // For local evaluation
        .enable_local_evaluation(true)
        .poll_interval_seconds(60)  // Poll for flag updates every minute
        .flush_at(200)  // Batch up to 200 events
        .flush_interval_seconds(5)  // Flush every 5 seconds
        .gzip(true)  // Enable compression
        .max_retries(5)  // Retry failed requests up to 5 times
        .disabled(false)  // Enable tracking
        .disable_geoip(false)  // Enable geoip enrichment
        .feature_flags_request_timeout_seconds(10)  // Timeout for flag requests
        .debug(true)  // Enable debug logging
        .max_queue_size(20000)  // Larger queue for high-volume apps
        .build()?;
    
    let advanced_client = posthog_rs::client(advanced_options).await;
    
    // Example 5: Configuration with super properties
    let mut super_props = HashMap::new();
    super_props.insert("app_version".to_string(), serde_json::json!("1.2.3"));
    super_props.insert("environment".to_string(), serde_json::json!("production"));
    
    let options_with_super_props = ClientOptionsBuilder::default()
        .api_key("phc_test_api_key".to_string())
        .super_properties(super_props)
        .build()?;
    
    let client_with_props = posthog_rs::client(options_with_super_props).await;
    
    // Test event capture with different clients
    let event = Event::new("test_event", "user123");
    
    // These would actually send events if the API key was valid
    // basic_client.capture(event.clone()).await?;
    // eu_client.capture(event.clone()).await?;
    // custom_client.capture(event.clone()).await?;
    // advanced_client.capture(event.clone()).await?;
    
    println!("Advanced configuration examples completed!");
    
    Ok(())
}