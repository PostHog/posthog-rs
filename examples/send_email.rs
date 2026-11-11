//! Send one transactional email, then read its status.
//!
//! Run with a template made transactional in PostHog:
//!   POSTHOG_API_KEY=phc_... POSTHOG_SECRET_KEY=phs_... EMAIL_TO=you@example.com \
//!   POSTHOG_EMAIL_TEMPLATE_ID=<template UUID> cargo run --example send_email
//!
//! Without `POSTHOG_EMAIL_TEMPLATE_ID`, it sends HTML that the example renders
//! itself, named by the key `rust-example`. Set `POSTHOG_HOST` to your region,
//! such as `https://eu.i.posthog.com`, and `POSTHOG_APP_HOST` when the host is
//! not PostHog Cloud.

#[cfg(feature = "async-client")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use posthog::{ClientOptionsBuilder, CodeEmail, SendEmail, TemplateEmail};
    use serde_json::json;

    let to = std::env::var("EMAIL_TO")?;
    let mut options = ClientOptionsBuilder::default();
    options
        .api_key(std::env::var("POSTHOG_API_KEY")?)
        .secret_key(std::env::var("POSTHOG_SECRET_KEY")?)
        .host(std::env::var("POSTHOG_HOST").unwrap_or_else(|_| posthog::DEFAULT_HOST.into()));
    if let Ok(app_host) = std::env::var("POSTHOG_APP_HOST") {
        options.app_host(app_host);
    }
    let posthog = posthog::client(options.build()?).await;

    let email: SendEmail = match std::env::var("POSTHOG_EMAIL_TEMPLATE_ID") {
        Ok(template_id) => TemplateEmail::new(&to, template_id)
            .template_data(json!({ "name": "Ada" }))
            .distinct_id("rust-example-user")
            .into(),
        // Render the email in your app, for example with a template engine,
        // and pass the HTML.
        Err(_) => CodeEmail::new(&to, "rust-example", "Hello from posthog-rs")
            .html("<p>Hello from <strong>posthog-rs</strong>.</p>")
            .distinct_id("rust-example-user")
            .into(),
    };

    let sent = posthog.emails().send(email).await?;
    println!("queued {} (replayed: {})", sent.id, sent.replayed);

    // The status read answers not_found for a few seconds after the send.
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    match posthog.emails().get(&sent.id).await {
        Ok(status) => println!("status {} at {}", status.status.as_str(), status.status_at),
        Err(error) => println!("status not readable yet: {error}"),
    }

    posthog.shutdown().await;
    Ok(())
}

#[cfg(not(feature = "async-client"))]
fn main() {
    println!("This example needs the async-client feature.");
}
