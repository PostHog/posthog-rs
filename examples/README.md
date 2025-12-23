# PostHog Rust SDK Examples

This directory contains example applications demonstrating how to use the PostHog Rust SDK, particularly the feature flags functionality.

## Running the Examples

### 1. Feature Flags with Mock Data (No API Key Required)
This example demonstrates feature flag evaluation using local mock data:

```bash
cargo run --example feature_flags_with_mock
```

This example shows:
- Simple percentage rollouts
- Property-based targeting (country, plan, age)
- Multivariate experiments with variants
- Local evaluation without API calls

### 2. Feature Flags Demo Application
A complete e-commerce demo with interactive testing:

```bash
# With a real PostHog API key
POSTHOG_API_KEY=your_api_key cargo run --example feature_flags_demo --all-features

# Without API key (uses local fallbacks)
cargo run --example feature_flags_demo --all-features
```

Features demonstrated:
- New checkout flow (premium/enterprise plans)
- AI recommendations (multivariate test)
- Pricing experiments (based on lifetime value)
- Holiday themes (geographic targeting)
- Interactive testing mode

### 3. Basic Feature Flags Example
Simple examples of all feature flag operations:

```bash
POSTHOG_API_KEY=your_api_key cargo run --example feature_flags
```

Shows:
- Checking if flags are enabled
- Getting flag values and variants
- Using person and group properties
- Getting feature flag payloads

## Testing Without a PostHog Account

The `feature_flags_with_mock` example is perfect for testing the SDK without needing a PostHog account. It demonstrates:

1. **Percentage Rollouts**: Flags enabled for a percentage of users
2. **Property Matching**: Target users based on properties like country, plan, age
3. **Multivariate Testing**: Split users into different variants
4. **Complex Conditions**: Combine multiple conditions with AND logic

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
2. **Local Evaluation**: Uses cached flag definitions for offline evaluation

### Properties

- **Person Properties**: User attributes (country, plan, age, etc.)
- **Group Properties**: Organization/team attributes for B2B apps

## Common Use Cases

1. **Feature Rollouts**: Gradually release features to users
2. **A/B Testing**: Test different variants to measure impact
3. **User Targeting**: Enable features for specific user segments
4. **Kill Switches**: Quickly disable problematic features
5. **Beta Programs**: Give early access to beta users