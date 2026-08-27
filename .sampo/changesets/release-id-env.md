---
cargo/posthog-rs: minor
---

Report a `$release_id` on `$exception` events, from an explicit `release_id` client option or the `POSTHOG_RELEASE_ID` environment variable. This is the native counterpart to injecting `$release_id` into a web bundle: `posthog-cli release resolve` creates the release and prints its id, and that id reaches the SDK one of two ways. Set the `release_id` option — typically `option_env!("POSTHOG_RELEASE_ID")` — to bake it into the binary at build time, so a shipped binary self-identifies with nothing to set at runtime. Or leave it unset and let the SDK read `POSTHOG_RELEASE_ID` from the environment at runtime, so a deploy supplies it without a rebuild. An explicit option wins over the environment. Either way the SDK stamps it only on exceptions — that is where the server resolves a release — the value is read once, an unset or blank value changes nothing, and a `before_send` hook can still drop the property.
