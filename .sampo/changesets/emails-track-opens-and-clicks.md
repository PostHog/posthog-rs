---
cargo/posthog-rs: minor
---

A `CodeEmail` now takes `.track_opens_and_clicks(false)` to send the email without open and click tracking: no open pixel, and no links rewritten to count clicks. PostHog then can't count that email's opens and clicks. It defaults to `true`, and it's independent of `.capture_engagement()`, which decides whether engagement events are captured on the recipient's person. A send that doesn't set it sends the same request as before.

The option needs PostHog's API support for it, which is rolling out. Until it reaches your PostHog region or self-hosted version, PostHog refuses a send that sets it.
