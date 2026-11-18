---
cargo/posthog: patch
---

Send transactional email through PostHog's key-scoped `/api/messaging/` routes, which take the project from the secret key, instead of `/api/projects/@current/messaging/`. A PostHog instance without these routes answers `404`.
