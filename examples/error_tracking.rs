//! Manual Error Tracking capture.
//!
//! Run with:
//!   POSTHOG_API_KEY=phc_... cargo run --example error_tracking --features "async-client,error-tracking"

#[cfg(all(feature = "async-client", feature = "error-tracking"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use posthog_rs::{client, Exception};

    let api_key = std::env::var("POSTHOG_API_KEY")?;
    let host = std::env::var("POSTHOG_HOST").unwrap_or_else(|_| posthog_rs::DEFAULT_HOST.into());
    let client = client((api_key.as_str(), host.as_str())).await;

    let error = std::io::Error::other("checkout failed");

    // Happy path: capture a Rust error directly.
    client.capture_error(&error, "user-123").await?;

    // Custom context: convert to an event and use the standard Event API.
    let mut event = Exception::from_error(&error).into_event("user-123");
    event.insert_prop("route", "/checkout")?;
    client.capture(event).await?;

    Ok(())
}

#[cfg(not(all(feature = "async-client", feature = "error-tracking")))]
fn main() {
    println!("This example requires the async-client and error-tracking features.");
    println!(
        "Run with: cargo run --example error_tracking --features \"async-client,error-tracking\""
    );
}
