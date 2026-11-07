//! Run with `cargo run --features stateful --example stateful`.
//! Set POSTHOG_PROJECT_TOKEN; POSTHOG_HOST and POSTHOG_DATA_DIR are optional.
use std::collections::HashMap;
use std::time::Duration;

use posthog::stateful::{Capture, Client, Config, Persistence};
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
