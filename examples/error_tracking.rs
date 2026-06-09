//! Manual Error Tracking capture.
//!
//! Run with:
//!   POSTHOG_API_KEY=phc_... cargo run --example error_tracking --features "async-client,error-tracking"

#[cfg(all(feature = "async-client", feature = "error-tracking"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use posthog_rs::{client, ExceptionCapture};

    let api_key = std::env::var("POSTHOG_API_KEY")?;
    let host = std::env::var("POSTHOG_HOST").unwrap_or_else(|_| posthog_rs::DEFAULT_HOST.into());
    let client = client((api_key.as_str(), host.as_str())).await;

    let error = std::io::Error::other("checkout failed");
    let exception = ExceptionCapture::from_error(&error)
        .with_distinct_id("user-123")
        .with_prop("route", "/checkout")?;

    client.capture_exception(exception).await?;
    Ok(())
}

#[cfg(not(all(feature = "async-client", feature = "error-tracking")))]
fn main() {
    println!("This example requires the async-client and error-tracking features.");
    println!(
        "Run with: cargo run --example error_tracking --features \"async-client,error-tracking\""
    );
}
