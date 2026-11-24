---
cargo/posthog: minor
---

Send the same transactional email to many recipients with `emails().batch()`, one email each, from a `TemplateBatch` with `template_data` per recipient or from a `CodeBatch` with content your code rendered. The client sends at most 100 recipients and 4 MiB per request, one request after another, gives each recipient an idempotency key that every retry reuses, and returns a result per recipient: `Queued`, `Failed`, `Unconfirmed` or `NotAttempted`. A request that fails as a whole returns an `EmailError` whose `batch_result()` holds every recipient's result.
