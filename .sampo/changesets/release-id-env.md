---
cargo/posthog-rs: minor
---

Report a `$release_id` on `$exception` events when `POSTHOG_RELEASE_ID` is set. This is the native, deploy-time counterpart to injecting `$release_id` into a web bundle: a build tool runs `posthog-cli release resolve` to create the release and print its id, launches the app with that id in `POSTHOG_RELEASE_ID`, and the SDK stamps it on exceptions so the server resolves each one's release by a direct id lookup. Only exception events carry it — that is where a release is resolved. The variable is read once; an unset or blank value changes nothing, and a `before_send` hook can still drop the property.
