---
cargo/posthog: patch
---

Send transactional email through PostHog's key-scoped `/api/messaging/` routes, which take the project from the secret key, instead of `/api/projects/@current/messaging/`. Needs PostHog Cloud, or a self-hosted PostHog from 2026-11-18 or later. Older self-hosted versions answer these calls with a 404.
