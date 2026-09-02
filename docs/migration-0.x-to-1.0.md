# Migrating from posthog-rs 0.x to 1.0

Version 1.0 makes the V1 analytics endpoint the SDK's only capture path and removes APIs that were already deprecated in 0.x. This guide describes the changes currently staged on the `v1` branch.

## Cargo features

Remove `capture-v1` from your dependency features. Capture no longer needs a feature flag.

```toml
# 0.x while opting into V1 capture
posthog-rs = { version = "0.25", features = ["capture-v1"] }

# 1.0
posthog-rs = "1"
```

The default features are the async client, error tracking, and TLS. Use `default-features = false` for the blocking client and explicitly enable a TLS feature when sending to an HTTPS endpoint.

```toml
# Async client with error tracking and TLS
posthog-rs = "1"

# Blocking client with TLS
posthog-rs = { version = "1", default-features = false, features = ["tls"] }
```

TLS was previously enabled even with `default-features = false`. In 1.0, choose `tls` to use reqwest's default Rustls provider, or choose `tls-no-provider` when the application installs a process-level Rustls `CryptoProvider`. The latter avoids pulling in `aws-lc-rs` from this SDK, but constructing a client before installing a provider will panic. Cargo features are additive, so `tls-no-provider` only avoids the built-in provider when no dependency enables `tls`.

The V0 capture implementation and its `/i/v0/e/` and batch plumbing have been removed. `Endpoint::Batch` no longer exists, and `Endpoint::Capture` now resolves to `/i/v1/analytics/events`.

## Capture behavior

The regular `capture` and `capture_batch` methods remain fire-and-forget. They enqueue events for the background worker, which batches and retries delivery. `capture_immediate` and `capture_batch_immediate` still bypass the queue and return a `CaptureSummary` after reaching a terminal result.

All event-producing SDK paths now use the same capture endpoint, including error tracking, `$feature_flag_called` events, historical migration, `before_send`, and terminal failures reported through `on_error`.

### Compression

`CaptureCompression::Gzip`, `Deflate`, `Br`, and `Zstd` now all apply their corresponding `Content-Encoding`. In older default V0 builds, only gzip was supported and selecting another variant could send an uncompressed body. Check any proxy or WAF in front of PostHog before enabling Brotli or Zstandard.

### Retry and persistence results

The V1 endpoint returns a result for each event. The SDK retries transient request failures and only the events with retryable results from a partial response.

`CaptureSummary::not_persisted()` and `CaptureSummary::all_persisted()` now use those per-event results. In the V0 path they reported a successful `2xx` as fully persisted without per-event confirmation. Applications that advance durable state after `capture_immediate` should check both that `submitted()` equals the number of intended events and that `all_persisted()` is true, because disabled clients and fully `before_send`-filtered batches submit no events but still report `all_persisted()` as true.

HTTP 429 is not a retryable V1 capture status. The V1 service uses HTTP 402 for billing limits and per-event `drop` or `warning` results in successful responses. `Retry-After` is still honored for retryable failures and retry results.

### Renamed capture response type

Rename `V1ErrorResponse` to `CaptureErrorResponse`:

```rust
// 0.x with capture-v1
let response: Option<&posthog_rs::V1ErrorResponse> = failure.error_response();

// 1.0
let response: Option<&posthog_rs::CaptureErrorResponse> = failure.error_response();
```

## Feature flags

The deprecated single-flag methods have been removed:

- `is_feature_enabled`
- `get_feature_flag`
- `get_feature_flag_payload`
- `get_feature_flags`

Call `evaluate_flags` once and read from the returned snapshot instead:

```rust
use posthog_rs::EvaluateFlagsOptions;

let flags = client
    .evaluate_flags("user-123", EvaluateFlagsOptions::default())
    .await?;

if flags.is_enabled("new-checkout") {
    // Use the enabled feature.
}

let variant = flags.get_flag("checkout-variant");
let payload = flags.get_flag_payload("checkout-variant");
```

The blocking client uses the same calls without `.await`.

`EvaluateFlagsOptions` is now non-exhaustive. Construct it with `Default` and assign the fields you need instead of using a struct literal:

```rust
let mut options = EvaluateFlagsOptions::default();
options.disable_geoip = Some(true);
options.flag_keys = Some(vec!["new-checkout".to_string()]);
```

## Local evaluation credentials

Use `secret_key` terminology throughout configuration. The builder's deprecated `personal_api_key` alias has been removed, and `LocalEvaluationConfig::personal_api_key` is now `secret_key`.

```rust
let options = posthog_rs::ClientOptionsBuilder::default()
    .api_key("phc_project_token")
    .secret_key("phs_project_secret")
    .enable_local_evaluation(true)
    .build()?;
```

`secret_key` accepts either a project secret key (`phs_...`) or a personal API key (`phx_...`). Do not send this key as an event property.
