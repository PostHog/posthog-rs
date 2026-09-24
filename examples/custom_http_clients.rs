use posthog_rs::{ClientOptionsBuilder, Event};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("POSTHOG_API_TOKEN").unwrap_or_default();
    // Blocking reqwest clients must be constructed and finally dropped outside
    // an async runtime, even when the SDK's public client is asynchronous.
    let blocking_http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut options = ClientOptionsBuilder::default();
    options
        .api_key(api_key)
        .request_timeout_seconds(10)
        .feature_flags_request_timeout_seconds(3)
        .blocking_http_client(blocking_http.clone());

    #[cfg(feature = "async-client")]
    {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        options.http_client(http);
        let options = options.build()?;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let client = posthog_rs::client(options).await;
                client.capture(Event::new("custom_http_example", "example-user"));
                client.shutdown().await;
            });
    }

    #[cfg(not(feature = "async-client"))]
    {
        let client = posthog_rs::client(options.build()?);
        client.capture(Event::new("custom_http_example", "example-user"));
        client.shutdown();
    }
    Ok(())
}
