# PostHog Rust SDK Examples

This directory contains example applications demonstrating how to use the PostHog Rust SDK, particularly the feature flags functionality.

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
