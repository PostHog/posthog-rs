//! Run with `cargo run --features stateful --example stateful`.
//! Set POSTHOG_PROJECT_TOKEN; POSTHOG_HOST, POSTHOG_DATA_DIR and
//! POSTHOG_DISTINCT_ID and POSTHOG_GROUP_KEY are optional.
use std::collections::HashMap;
use std::time::Duration;

use posthog::stateful::{Capture, Client, Config, Identify, Persistence};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::new(std::env::var("POSTHOG_PROJECT_TOKEN")?);
    if let Ok(host) = std::env::var("POSTHOG_HOST") {
        config.host = host;
    }
    if let Ok(root) = std::env::var("POSTHOG_DATA_DIR") {
        config.persistence = Persistence::Directory(root.into());
    }
    config.before_send(|mut event| {
        event.remove_prop("private_value");
        Some(event)
    });
    let client = Client::init_blocking(config);
    // Attempt previously queued work before the example records new events.
    println!("startup drain: {:?}", client.flush_blocking());
    if let Ok(id) = std::env::var("POSTHOG_DISTINCT_ID") {
        // Call client.reset() on logout before identifying a different user.
        let mut identity = Identify::new(id);
        identity.set.insert("example".into(), json!("rust"));
        client.identify(identity);
    }
    client.register(HashMap::from([("example_context".into(), json!("rust"))]));
    if let Ok(group) = std::env::var("POSTHOG_GROUP_KEY") {
        client.group("organization", group, None);
    }
    let mut event = Capture::new("example opened");
    event
        .properties
        .insert("source".into(), json!("rust-example"));
    println!("capture: {:?}", client.capture(event));
    println!("screen: {:?}", client.screen("Example", HashMap::new()));
    println!("flush: {:?}", client.flush_blocking());
    println!(
        "shutdown: {:?}",
        client.shutdown_blocking(Some(Duration::from_secs(5)))
    );
    Ok(())
}
