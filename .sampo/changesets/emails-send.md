---
cargo/posthog-rs: minor
---

Add `client.emails()` to send transactional email from a template made in PostHog (`TemplateEmail`) or from content your code rendered (`CodeEmail`), and to read an email's delivery status. New client options: `app_host`, `email_request_timeout_seconds`, `email_request_max_retries` and `migrated_from`.
