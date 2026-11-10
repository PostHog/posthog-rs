//! Run with `cargo run --features stateful --example stateful`.
//! Set POSTHOG_PROJECT_TOKEN; POSTHOG_HOST, POSTHOG_DATA_DIR and
//! POSTHOG_DISTINCT_ID, POSTHOG_GROUP_KEY, POSTHOG_FLAG_KEY and
//! POSTHOG_OPT_OUT (true/false) are optional.
use std::collections::HashMap;
use std::time::Duration;

use posthog::stateful::{Capture, Client, Config, Identify, Persistence, SessionPersistence};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::new(std::env::var("POSTHOG_PROJECT_TOKEN")?);
    // Repeated CLI invocations can continue an unexpired session in this scope.
    config.session_persistence = SessionPersistence::Persistent;
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
    if let Ok(opt_out) = std::env::var("POSTHOG_OPT_OUT") {
        if opt_out.parse::<bool>()? {
            client.opt_out();
        } else {
            client.opt_in();
        }
    }
    println!("opted out: {}", client.is_opt_out());
    let subscription = client.on_feature_flags(|update| {
        println!(
            "flags available: {}, known flags: {}, loading errors: {}",
            update.flags.is_some(),
            update.flags.as_ref().map_or(0, HashMap::len),
            update.errors_loading
        );
    });
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
    // Analytics registration is not implicit flag targeting. Combine explicit
    // targeting with the final identity/group context before requesting a refresh.
    client.set_person_properties_for_flags(
        HashMap::from([("application".into(), json!("rust-example"))]),
        Some(false),
    );
    client.reload_feature_flags();
    let mut event = Capture::new("example opened");
    event
        .properties
        .insert("source".into(), json!("rust-example"));
    println!("capture: {:?}", client.capture(event));
    println!("session: {:?}", client.peek_session_id());
    println!("screen: {:?}", client.screen("Example", HashMap::new()));
    println!("flush: {:?}", client.flush_blocking());
    if let Ok(key) = std::env::var("POSTHOG_FLAG_KEY") {
        // Cached read only: None is unavailable, not false. A known value reports
        // access unless consent disallows it. Shutdown drains that event too.
        println!(
            "cached flag {key}: {:?}",
            client.get_feature_flag(&key, None)
        );
        println!("payload {key}: {:?}", client.get_feature_flag_payload(&key));
    }
    subscription.unsubscribe();
    println!(
        "shutdown: {:?}",
        client.shutdown_blocking(Some(Duration::from_secs(5)))
    );
    Ok(())
}
