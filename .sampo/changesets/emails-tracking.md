---
cargo/posthog-rs: minor
---

A `CodeEmail` now takes `.sensitive()` and `.tracking()`.

`.sensitive()` marks an email that carries a login link, a reset token or a code. PostHog keeps a sensitive email's links unchanged, leaves its subject off engagement events and keeps its content out of logs. Without it, a code send takes the setting of the saved template whose key matches its `template_key`, or is sensitive when no template has that key, as before.

`.tracking()` says what PostHog tracks for the email: `true` or `EmailTracking::On`, `false` or `EmailTracking::Off`, or `EmailTrackingOptions::new()` with `.opens()`, `.clicks()` and `.engagement()` to set each kind on its own. Without it, PostHog counts opens, captures engagement events, and counts clicks only when the email isn't sensitive. Turn clicks on only when the email's links are safe to send through a redirect. Click tracking needs open tracking.

`.capture_engagement()` is deprecated. Use `.tracking(EmailTrackingOptions::new().engagement(...))` instead. It still works, but PostHog refuses a send that sets both. The SDK now leaves `capture_engagement` out of the request unless you set it, which PostHog reads the same way.

`.sensitive()` and `.tracking()` need PostHog's API support, which is rolling out. Until it reaches your PostHog region or self-hosted version, PostHog refuses a send that sets either.
