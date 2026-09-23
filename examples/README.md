# PostHog Rust SDK Examples

This directory contains example applications demonstrating how to use the PostHog Rust SDK, including feature flags, local evaluation, configuration, and manual error tracking.

## Running the Examples

### 1. Feature Flags Example

Basic examples of all feature flag operations:

```bash
# With a real PostHog API key
export POSTHOG_API_TOKEN=phc_your_key
cargo run --example feature_flags --features async-client

# Without API key (runs in demo mode)
cargo run --example feature_flags --features async-client
```

Shows:
- Boolean feature flag checks
- A/B test variants (multivariate flags)
- Property-based targeting
- Batch flag evaluation
- Feature flag payloads

### 2. Local Evaluation Example

Demonstrates local flag evaluation for high-performance use cases:

```bash
export POSTHOG_API_TOKEN=phc_your_project_key
export POSTHOG_PERSONAL_API_TOKEN=phx_your_personal_key
cargo run --example local_evaluation --features async-client
```

Shows:
- Performance comparison (API vs local evaluation)
- Setting up local evaluation with polling
- Batch flag evaluation

Get a personal API key at: https://app.posthog.com/me/settings

### 3. Advanced Configuration Example

Shows different ways to configure the SDK:

```bash
cargo run --example advanced_config --features async-client
```

Shows:
- Basic client setup (US region)
- EU region configuration (GDPR compliance)
- Self-hosted instance configuration
- Production settings with timeouts and geoip configuration
- High-performance local evaluation setup
- Observability with `on_error` (terminal capture/flags/poller failures)

### 4. Error Tracking Example

Demonstrates manual error tracking capture:

```bash
export POSTHOG_API_KEY=phc_your_project_key
cargo run --example error_tracking
```

Shows:
- Capturing a Rust error as a PostHog Error Tracking event
- Attaching a distinct ID
- Adding custom exception properties

### 5. Custom HTTP Clients

```bash
export POSTHOG_API_TOKEN=phc_your_project_key
cargo run --example custom_http_clients
# Blocking SDK:
cargo run --example custom_http_clients --no-default-features
```

Use `ClientOptionsBuilder::http_client(reqwest::Client)` for async immediate
capture, remote flags, and local-evaluation polling. Use
`blocking_http_client(reqwest::blocking::Client)` for the blocking SDK and
background capture in **both** SDK modes. The async setter is only available
with `async-client`. Either option can be omitted; that path keeps SDK-created
clients and existing defaults.

Supplied clients retain their connection pools, TLS configuration, proxies, and
other client settings. Pass clones to share pools with your application. Use a
compatible version of the SDK's reqwest dependency (currently `0.13`); other HTTP
libraries and incompatible reqwest versions are not accepted.

Only supply clients whose default headers and cookies are safe to send to PostHog.
Do not reuse clients carrying `Authorization`, API keys, or other credentials for
unrelated services. Reqwest does not expose those defaults for the SDK to inspect
or filter. Use a separate client with safe defaults when necessary.

Configure bounded timeouts on supplied clients. `request_timeout_seconds` does
not override them. Remote `/flags` requests still apply
`feature_flags_request_timeout_seconds` (default: 3 seconds), replacing the
client's request timeout even if it is shorter. During background
shutdown draining, the SDK sets a per-request timeout to the remaining shutdown
deadline; this replaces the supplied client's request timeout for that request.
An already-running request still uses the supplied client's timeout and can delay
shutdown. With no client timeout, capture or local-evaluation initialization can
wait indefinitely, and an already-running background request can prevent
`shutdown()` or `Drop` from completing. SDK authentication, request headers,
batching, and retries remain in place; account for any additional retries
configured by your application.

Construct and finally drop application-owned blocking clients (including copies
held in options/builders) outside an async runtime, or in `spawn_blocking`. The
SDK releases its own blocking-client handles off the async runtime. Shutting down
the SDK does not invalidate client clones retained by the application.

Custom HTTP clients are separate from TLS crypto-provider selection. The
`tls-no-provider` feature proposed in #245 is on `v1`, not this `main` branch;
changing the provider does not change the reqwest client types.

## Key Concepts

### Feature Flag Types

1. **Boolean Flags**: Simple on/off toggles
   ```rust
   FlagValue::Boolean(true)  // enabled
   FlagValue::Boolean(false) // disabled
   ```

2. **Multivariate Flags**: Multiple variants for A/B/n testing
   ```rust
   FlagValue::String("control")
   FlagValue::String("variant-a")
   FlagValue::String("variant-b")
   ```

### Evaluation Methods

1. **Remote Evaluation**: Calls PostHog API for the latest flag values
2. **Local Evaluation**: Uses cached flag definitions for faster evaluation (requires personal API key)

### Properties

- **Person Properties**: User attributes (country, plan, age, etc.)
- **Group Properties**: Organization/team attributes for B2B apps

## Common Use Cases

1. **Feature Rollouts**: Gradually release features to users
2. **A/B Testing**: Test different variants to measure impact
3. **User Targeting**: Enable features for specific user segments
4. **Kill Switches**: Quickly disable problematic features
5. **Beta Programs**: Give early access to beta users
