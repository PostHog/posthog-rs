/// Snapshot-based feature flag evaluation
///
/// Demonstrates the `evaluate_flags()` API: one round-trip to PostHog produces a
/// `FeatureFlagEvaluations` cache that you can read many times without further
/// network traffic. Reads through `is_enabled` / `get_flag` fire a deduplicated
/// `$feature_flag_called` event with full metadata. Pass the snapshot to
/// `Event::with_flags(&snapshot)` so a captured event inherits `$feature/<key>`
/// and `$active_feature_flags` without a second `/flags` round-trip.
///
/// Run:
///   export POSTHOG_API_TOKEN=phc_your_key
///   cargo run --example evaluate_flags --features async-client
use posthog_rs::{EvaluateFlagsOptions, Event};

#[cfg(feature = "async-client")]
#[tokio::main]
async fn main() {
    let api_key = std::env::var("POSTHOG_API_TOKEN").unwrap_or_else(|_| {
        println!("No POSTHOG_API_TOKEN found. Demo mode — calls will fail without a key.\n");
        "demo_api_key".to_string()
    });

    let client = posthog_rs::client(api_key.as_str()).await;

    let user_id = "user-123";

    let snapshot = match client
        .evaluate_flags(user_id, EvaluateFlagsOptions::default())
        .await
    {
        Ok(s) => s,
        Err(e) => {
            println!("evaluate_flags failed: {e}");
            return;
        }
    };

    println!("Loaded {} flag(s) in one request:", snapshot.keys().len());
    for key in snapshot.keys() {
        println!("  - {key}");
    }

    if snapshot.is_enabled("new-dashboard") {
        println!("\nnew-dashboard is enabled — render the new layout.");
    }

    if let Some(payload) = snapshot.get_flag_payload("onboarding-config") {
        println!("\nonboarding-config payload (no event fired): {payload}");
    }

    // Capture an event that inherits the snapshot's flag context. No second
    // /flags round-trip happens here.
    let mut event = Event::new("checkout-started", user_id);
    event.with_flags(&snapshot);
    client.capture(event);

    // Optional: only attach the flags actually consulted on this request path.
    let mut narrow = Event::new("checkout-completed", user_id);
    narrow.with_flags(&snapshot.only_accessed());
    client.capture(narrow);
}

#[cfg(not(feature = "async-client"))]
fn main() {
    println!("This example requires the async-client feature.");
    println!("Run with: cargo run --example evaluate_flags --features async-client");
}
