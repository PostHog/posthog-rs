///  Run this example (blocking/sync client only):
///  cargo run --example config_migration --no-default-features

#[cfg(feature = "async-client")]
fn main() {
    eprintln!("ERROR: This example only works with the blocking/sync client.");
    eprintln!("Run with: cargo run --example config_migration --no-default-features");
}

#[cfg(not(feature = "async-client"))]
fn main() {
    use posthog_rs::{ClientOptionsBuilder, Event};
    //--------------------------------
    // BEFORE MIGRATION
    //--------------------------------

    println!("\nExample 0: Before Migration");

    // Before for single event capture
    let options = ClientOptionsBuilder::new()
        .api_key("phc_demo")
        .api_endpoint("https://eu.posthog.com/i/v0/e/")
        .build()
        .unwrap();

    println!("Single event: {}", options.single_event_endpoint());

    let client = posthog_rs::client(options);

    let event = Event::new("user_signed_up", "distinct_id_of_the_user");
    client.capture(event).unwrap();

    // Before for batch event capture
    let options = ClientOptionsBuilder::new()
        .api_key("phc_demo")
        .api_endpoint("https://eu.posthog.com/batch/")
        .build()
        .unwrap();

    println!("Batch event: {}", options.batch_event_endpoint());

    let client = posthog_rs::client(options);

    let events = vec![
        Event::new("user_signed_up", "distinct_id_of_the_user"),
        Event::new("user_signed_up", "distinct_id_2_of_the_user"),
    ];
    client.capture_batch(events).unwrap();

    //--------------------------------
    // AFTER MIGRATION
    //--------------------------------

    // These changes are internal to the SDK and you don't need to do anything.
    // We're just showing you what happens internally.
    // In this example, we're using the hostname "https://eu.posthog.com"
    // which will be normalized to "https://eu.posthog.com/i/v0/e/" for single event capture
    // and "https://eu.posthog.com/batch/" for batch event capture.

    // Example 1: Default hostname (without endpoint url)
    println!("\nExample 1: Default hostname - (without endpoint url)");
    let options = ClientOptionsBuilder::new()
        .api_key("phc_demo")
        .build()
        .unwrap();

    // both are internally smarted assigned
    println!("Single event: {}", options.single_event_endpoint());
    println!("Batch event:  {}", options.batch_event_endpoint());

    let _client = posthog_rs::client(options);

    // or

    // let client = posthog_rs::client(env!("POSTHOG_API_KEY"));

    let event = Event::new("user_signed_up", "distinct_id_of_the_user");
    client.capture(event).unwrap();

    let events = vec![
        Event::new("user_signed_up", "distinct_id_of_the_user"),
        Event::new("user_signed_up", "distinct_id_2_of_the_user"),
    ];
    client.capture_batch(events).unwrap();

    // Example 2: EU region with hostname
    println!("\nExample 1: just hostname");
    let options = ClientOptionsBuilder::new()
        .api_key("phc_demo")
        .api_endpoint("https://eu.posthog.com")
        .build()
        .unwrap();

    // both are internally smarted assigned
    println!("Single event: {}", options.single_event_endpoint());
    println!("Batch event:  {}", options.batch_event_endpoint());

    let client = posthog_rs::client(options);

    let event = Event::new("user_signed_up", "distinct_id_of_the_user");
    client.capture(event).unwrap();

    let events = vec![
        Event::new("user_signed_up", "distinct_id_of_the_user"),
        Event::new("user_signed_up", "distinct_id_2_of_the_user"),
    ];
    client.capture_batch(events).unwrap();

    // Example 3: Backward compatibility
    println!("\nExample 3: Backward compatible (old full URL format still works)");
    let options = ClientOptionsBuilder::new()
        .api_key("phc_demo")
        .api_endpoint("https://eu.posthog.com/i/v0/e/")
        .build()
        .unwrap();

    println!("Input:        https://eu.posthog.com/i/v0/e/ or https://eu.posthog.com/batch/");
    println!("Single event: {}", options.single_event_endpoint());
    println!("Batch event:  {}", options.batch_event_endpoint());

    let client = posthog_rs::client(options);

    let event = Event::new("user_signed_up", "distinct_id_of_the_user");
    client.capture(event).unwrap();

    let events = vec![
        Event::new("user_signed_up", "distinct_id_of_the_user"),
        Event::new("user_signed_up", "distinct_id_2_of_the_user"),
    ];
    client.capture_batch(events).unwrap();
}
