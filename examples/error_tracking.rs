//! Manual Error Tracking capture.
//!
//! Run with:
//!   POSTHOG_API_KEY=phc_... cargo run --example error_tracking

#[cfg(all(feature = "async-client", feature = "error-tracking"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use posthog_rs::{client, CaptureExceptionOptions};

    let api_key = std::env::var("POSTHOG_API_KEY")?;
    let host = std::env::var("POSTHOG_HOST").unwrap_or_else(|_| posthog_rs::DEFAULT_HOST.into());
    let client = client((api_key.as_str(), host.as_str())).await;

    let error = std::io::Error::other("checkout failed");

    // Associate the error with a person and attach context.
    client
        .capture_exception_with(
            &error,
            CaptureExceptionOptions::new()
                .distinct_id("user-123")
                .property("route", "/checkout")?,
        )
        .await?;

    // Personless capture.
    client.capture_exception(&error).await?;

    // Capturing an `eyre::Report`
    let result: eyre::Result<()> = do_work();
    if let Err(err) = result {
        // `eyre::Report` implements `AsRef` for both `dyn Error` and
        // `dyn Error + Send + Sync`, so annotate which one we want.
        let source: &dyn std::error::Error = err.as_ref();
        client.capture_exception(source).await?;
    }

    Ok(())
}

/// Dummy for real application logic that fails with an `eyre::Report`.
#[cfg(all(feature = "async-client", feature = "error-tracking"))]
fn do_work() -> eyre::Result<()> {
    use eyre::WrapErr;

    std::fs::read_to_string("/nonexistent/config.toml")
        .wrap_err("while attempting to load the checkout service config")?;
    Ok(())
}

#[cfg(not(all(feature = "async-client", feature = "error-tracking")))]
fn main() {
    println!("This example requires the async-client and error-tracking features (both default).");
    println!("Run with: cargo run --example error_tracking");
}
