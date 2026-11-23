---
cargo/posthog: minor
---

Schedule a transactional email up to 7 days ahead on PostHog Cloud with `scheduled_at` on `TemplateEmail` and `CodeEmail`. The send's result carries the confirmed `scheduled_at` and a `status()` method, `EmailDeliveryStatus::Scheduled` reads a waiting email, and `EmailError::message_id` names an email PostHog accepted without confirming its time.
