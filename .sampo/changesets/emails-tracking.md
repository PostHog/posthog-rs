---
cargo/posthog-rs: minor
---

A `CodeEmail` now takes `.tracking()`, which says what PostHog tracks for the email: `true` or `EmailTracking::On`, `false` or `EmailTracking::Off`, or `EmailTrackingOptions::new()` with `.opens()`, `.clicks()` and `.engagement()` to set each kind on its own. Without it, PostHog counts opens and captures engagement events, but doesn't rewrite links to count clicks, because a link in an email sent from code can carry a login or password reset token. Turn clicks on only when the email's links are safe to send through a redirect. Click tracking needs open tracking.

`.capture_engagement()` is deprecated. Use `.tracking(EmailTrackingOptions::new().engagement(...))` instead. It still works, but PostHog refuses a send that sets both. The SDK now leaves `capture_engagement` out of the request unless you set it, which PostHog reads the same way.

`.tracking()` needs PostHog's API support, which is rolling out. Until it reaches your PostHog region or self-hosted version, PostHog refuses a send that sets it.
