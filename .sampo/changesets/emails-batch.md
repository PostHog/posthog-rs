---
cargo/posthog: minor
---

Send the same transactional email to up to 100 recipients with `emails().batch()`, one email each, from a `TemplateBatch` with `template_data` per recipient or from a `CodeBatch` with content your code rendered. A batch takes 1 to 100 recipients and goes out in one request: the client gives each recipient an idempotency key that every retry reuses, and returns a result per recipient: `Queued`, `Failed` or `Unconfirmed`. A batch PostHog finds too large comes back as a `content_too_large` error. With status `413`, split the recipients into smaller batches. With status `400`, the content or template data sent to every recipient is too large for a batch: send those emails one at a time. The client never reads an email by its key: send an `Unconfirmed` recipient again with the same key within 24 hours of the first request that used it, so it goes out once. A request that fails returns an `EmailError` whose `batch_result()` holds every recipient's result.
