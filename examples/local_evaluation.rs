//! Local evaluation performance demo.
//!
//! Shows how cached flag definitions avoid a remote `/flags` request.
//!
//! Setup:
//!   export POSTHOG_API_TOKEN=phc_your_project_key
//!   export POSTHOG_SECRET_KEY=phs_your_project_secret_or_phx_personal_key
//!   cargo run --example local_evaluation --features async-client

#[cfg(feature = "async-client")]
use posthog::{ClientOptionsBuilder, EvaluateFlagsOptions};
#[cfg(feature = "async-client")]
use serde_json::json;
#[cfg(feature = "async-client")]
use std::collections::HashMap;
#[cfg(feature = "async-client")]
use std::time::{Duration, Instant};

#[cfg(feature = "async-client")]
#[tokio::main]
async fn main() {
    let api_key = required_env("POSTHOG_API_TOKEN");
    let secret_key = required_env("POSTHOG_SECRET_KEY");

    println!("=== Local evaluation performance demo ===\n");

    let local_client = {
        let options = ClientOptionsBuilder::default()
            .api_key(api_key.clone())
            .secret_key(secret_key)
            .enable_local_evaluation(true)
            .poll_interval_seconds(30)
            .build()
            .unwrap();
        posthog::client(options).await
    };

    let api_client = {
        let options = ClientOptionsBuilder::default()
            .api_key(api_key)
            .build()
            .unwrap();
        posthog::client(options).await
    };

    println!("Fetching flag definitions for local evaluation...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let user_id = "perf-test-user";
    let flag_key = "using-feature-flags";
    let mut person_properties = HashMap::new();
    person_properties.insert("plan".to_string(), json!("enterprise"));
    person_properties.insert("country".to_string(), json!("US"));

    println!("\n1. Remote evaluation (10 requests):");
    let start = Instant::now();
    for i in 0..10 {
        let mut options = EvaluateFlagsOptions::default();
        options.person_properties = Some(person_properties.clone());
        options.flag_keys = Some(vec![flag_key.to_string()]);
        let _ = api_client
            .evaluate_flags(format!("{user_id}-{i}"), options)
            .await;
    }
    let api_duration = start.elapsed();
    println!(
        "   Time: {:?} total, {:?} per request",
        api_duration,
        api_duration / 10
    );

    println!("\n2. Local evaluation (10 requests):");
    let start = Instant::now();
    for i in 0..10 {
        let mut options = EvaluateFlagsOptions::default();
        options.person_properties = Some(person_properties.clone());
        options.only_evaluate_locally = true;
        options.flag_keys = Some(vec![flag_key.to_string()]);
        let _ = local_client
            .evaluate_flags(format!("{user_id}-{i}"), options)
            .await;
    }
    let local_duration = start.elapsed();
    println!(
        "   Time: {:?} total, {:?} per request",
        local_duration,
        local_duration / 10
    );

    let speedup = api_duration.as_micros() as f64 / local_duration.as_micros().max(1) as f64;
    println!("\nLocal evaluation is {speedup:.1}x faster!");

    let start = Instant::now();
    let mut options = EvaluateFlagsOptions::default();
    options.person_properties = Some(person_properties);
    options.only_evaluate_locally = true;
    match local_client.evaluate_flags(user_id, options).await {
        Ok(flags) => println!(
            "Evaluated {} flags in {:?}",
            flags.keys().len(),
            start.elapsed()
        ),
        Err(error) => println!("Error: {error}"),
    }

    println!("\nDefinitions continue refreshing every 30 seconds.");
}

#[cfg(feature = "async-client")]
fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        eprintln!("Error: {name} environment variable not set");
        std::process::exit(1);
    })
}

#[cfg(not(feature = "async-client"))]
fn main() {
    println!("This example requires the async-client feature.");
    println!("Run with: cargo run --example local_evaluation --features async-client");
}
