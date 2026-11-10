# PostHog Rust SDK Examples

This directory contains example applications demonstrating how to use the PostHog Rust SDK, including feature flags, local evaluation, configuration, and manual error tracking.

## Stateful application client

Set `POSTHOG_PROJECT_TOKEN` and optionally `POSTHOG_HOST` and
`POSTHOG_DATA_DIR` (a persistence root override), then run:

```bash
cargo run --features stateful --example stateful
# Identify a user (omit POSTHOG_DISTINCT_ID to remain anonymous):
POSTHOG_DISTINCT_ID=user-123 cargo run --features stateful --example stateful
# Without the async client feature:
cargo run --no-default-features --features stateful,tls --example stateful
```

This blocking example uses automatic local persistence, anonymous or identified
identity, registered context, explicit capture/screen, a native privacy hook, and
separate drain reports. It opts into persistent sessions so repeated invocations
in the same scope can share an unexpired session; the SDK default keeps sessions
in memory. Optionally set `POSTHOG_GROUP_KEY` to associate an organization
(enabling person processing). Identity persists between runs. Call `client.reset()`
before identifying a different user in the same storage scope.
Set `POSTHOG_OPT_OUT=true` or `false` to explicitly change consent; leaving it unset
preserves the stored choice. Opt-out suppresses new analytics, not delivery of
previously admitted events or flag loading.

The example observes flag-load notifications, sets explicit person targeting, and
requests a refresh after identity/group changes. `POSTHOG_FLAG_KEY` optionally reads
a cached flag and payload. A known flag read reports access when consent allows;
`None` means unavailable, not disabled. Initialization and successful drains do not
promise remote flag success. The listener reports load errors and is explicitly
unsubscribed before shutdown. Notifications can run on the caller or shared worker.
