---
cargo/posthog-rs: minor
---

A `CodeEmail` now takes `.track_opens_and_clicks(false)` to send the email without PostHog's open pixel, so PostHog can't count its opens. PostHog doesn't track clicks on emails sent from code, so their links are never rewritten either way. It defaults to `true`, and it's independent of `.capture_engagement()`, which decides whether engagement events are captured on the recipient's person. A send that doesn't set it sends the same request as before.

The option needs PostHog's API support for it, which is rolling out. Until it reaches your PostHog region or self-hosted version, PostHog refuses a send that sets it.
