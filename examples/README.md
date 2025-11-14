# PostHog Rust SDK Examples

This directory contains example applications demonstrating how to use the PostHog Rust SDK

## Available Examples

### 1. Feature Flags (`feature_flags.rs`)
Comprehensive feature flag operations and patterns.

```bash
# With a PostHog API key
export POSTHOG_API_TOKEN=phc_your_key
cargo run --example feature_flags --features async-client

# Without API key (demo mode - shows code structure)
cargo run --example feature_flags --features async-client
```

**Demonstrates:**
- **Example 1**: Simple boolean flag checks using `is_feature_enabled`
- **Example 2**: Multivariate flags (A/B testing) with `get_feature_flag` returning variants (control, variant-a, variant-b)
- **Example 3**: Property-based targeting with person properties (plan, country, account_age_days)
- **Example 4**: Groups (B2B) - Organization and team-level features with group properties:
  - Company properties: name, plan, employees, industry
  - Team properties: name, size
- **Example 5**: Batch flag evaluation with `get_feature_flags` - getting all flags and payloads at once
- **Example 6**: Feature flag payloads with `get_feature_flag_payload` - JSON configuration data

### 2. Local Evaluation (`local_evaluation.rs`)
Performance optimization through local flag caching.

```bash
export POSTHOG_API_TOKEN=phc_your_project_key
export POSTHOG_PERSONAL_API_TOKEN=phx_your_personal_key
cargo run --example local_evaluation --features async-client
```

**Requirements:**
- Project API token (`phc_...`)
- Personal API token (`phx_...`) - [Create one](https://app.posthog.com/me/settings)

**Demonstrates:**
- Creating two clients for comparison: one with local evaluation, one without
- Performance benchmarking: 10 API requests vs 10 local evaluation requests
- Real-time speedup calculation showing 100x-1000x improvement
- Automatic background polling for flag definition updates (configurable with `poll_interval_seconds`)
- Batch flag evaluation performance with `get_feature_flags`
- Using `ClientOptionsBuilder` with `enable_local_evaluation(true)` and `personal_api_key`

### 3. Feature Flag Events (`feature_flag_events.rs`)
Automatic `$feature_flag_called` event tracking.

```bash
export POSTHOG_API_TOKEN=phc_your_project_key
cargo run --example feature_flag_events --features async-client
```

**Demonstrates:**
- **Example 1**: Automatic `$feature_flag_called` event capture when evaluating flags (enabled by default)
- **Example 2**: Event deduplication - same user + flag + value combination only sends event once
- **Example 3**: Events captured for different users (user-1, user-2, user-3) - each gets separate events
- **Example 4**: Multivariate flag events with variant information

### 4. Advanced Configuration (`advanced_config.rs`)
SDK configuration patterns for different use cases.

```bash
export POSTHOG_API_TOKEN=phc_your_key
export POSTHOG_PERSONAL_API_TOKEN=phx_your_personal_key
cargo run --example advanced_config --features async-client
```

**Shows 5 Configuration Patterns:**
1. **Basic client**: `posthog_rs::client("phc_test_api_key")` - US region by default
2. **EU data residency**: `posthog_rs::client(("phc_key", EU_INGESTION_ENDPOINT))` - GDPR compliant
3. **Self-hosted PostHog**: `posthog_rs::client(("phc_key", "https://analytics.mycompany.com"))`
4. **Production-optimized**: ClientOptionsBuilder with:
   - `gzip(true)` - compress requests
   - `request_timeout_seconds(30)` - 30s timeout
5. **High-performance**: Local evaluation with:
   - `enable_local_evaluation(true)` - cache flags locally
   - `poll_interval_seconds(30)` - update cache every 30s
   - `feature_flags_request_timeout_seconds(3)` - faster timeouts

## Quick Start

The simplest way to get started:

```bash
# Try feature flags without an API key (demo mode - shows code structure)
cargo run --example feature_flags --features async-client

# Or with your PostHog account
export POSTHOG_API_TOKEN=phc_your_key
cargo run --example feature_flags --features async-client
```

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

1. **Remote Evaluation**: Calls PostHog API for the latest flag values (default)
2. **Local Evaluation**: Caches flag definitions locally for 100-1000x faster evaluation (requires personal API key)

### Properties

- **Person Properties**: User attributes (country, plan, account_age_days, email, etc.)
- **Group Properties**: Organization/team attributes for B2B apps (company plan, employees, team size, etc.)

### Automatic Event Tracking

When you evaluate feature flags, the SDK automatically sends `$feature_flag_called` events to PostHog (enabled by default). These events include:
- Which flags were checked (`$feature_flag`)
- What value was returned (`$feature_flag_response`)
- User identifier (`distinct_id`)
- User properties used for evaluation

Events are deduplicated per user + flag + value combination to avoid duplicate tracking.

## Common Use Cases

1. **Feature Rollouts**: Gradually release features to users
2. **A/B Testing**: Test different variants (control, variant-a, variant-b) to measure impact
3. **User Targeting**: Enable features for specific user segments using person properties
4. **B2B Group Targeting**: Target entire organizations or teams using group properties
5. **Kill Switches**: Quickly disable problematic features
6. **Beta Programs**: Give early access to specific users
7. **Performance**: Use local evaluation for high-throughput applications (100-1000x faster)
