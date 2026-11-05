---
cargo/posthog-rs: minor
---

The new `client.emails()` sends transactional email from your Rust code and reads each email's delivery status.

```rust
use posthog_rs::{ClientOptionsBuilder, CodeEmail};

let options = ClientOptionsBuilder::default()
    .api_key("phc_...".to_string())
    .secret_key("phs_...")
    .build()?;
let posthog = posthog_rs::client(options).await;

let sent = posthog
    .emails()
    .send(
        CodeEmail::new("user@example.com", "password-reset", "Reset your password")
            .html(rendered_html)
            .distinct_id("user_123"),
    )
    .await?;

let status = posthog.emails().get(&sent.id).await?;
println!("{}", status.status.as_str()); // "delivered", "bounced", ...
```

You can also send a template made in PostHog with `TemplateEmail::new` and its `template_id`.

You need a project secret API key, the kind that starts with `phs_`. Create one under **Project secret API keys** in your project settings. Set its **Transactional email** scope to Write to send emails, or to Read for a key that only reads statuses. Set it with `secret_key` on `ClientOptionsBuilder`. Set `app_host` to your PostHog app URL when you self-host PostHog or send through a reverse proxy.

PostHog records each email's delivery, opens and clicks on the person with that `distinct_id`. It groups every email with the same `template_key`, so you see their metrics and logs in one place. The SDK retries a request that hits a network error, a rate limit or an outage twice by default. Every send carries an idempotency key, so a retry never sends the email twice. Pass your own `idempotency_key`, such as `welcome:user_123`, and your job runner's retries are safe too.

A `CodeEmail` also takes `.from()`, `.reply_to()` and `.capture_engagement(false)`. Set `migrated_from` on `ClientOptionsBuilder` to tell PostHog which email provider you moved from. A failed call returns an `EmailError` with PostHog's `code()` and the HTTP `status()`.

Docs: [Send transactional emails with Rust](https://posthog.com/docs/workflows/transactional-emails/rust) and the [transactional email overview](https://posthog.com/docs/workflows/transactional-emails).
