//! Official Rust SDK for PostHog.
//!
//! Use [`client`] to construct a [`Client`], [`Event`] to capture analytics
//! events, and [`Client::evaluate_flags`] with [`EvaluateFlagsOptions`] for
//! feature flag evaluation.
//!
//! See the [PostHog Rust SDK documentation](https://posthog.com/docs/libraries/rust)
//! for installation, configuration, and more examples.
//!
//! # Getting started
//!
//! Add `posthog-rs` to your `Cargo.toml`, then initialize a client with your
//! project API key.
//!
//! ```no_run
//! use posthog_rs::{client, EvaluateFlagsOptions, Event};
//!
//! #[cfg(feature = "async-client")]
//! #[tokio::main]
//! async fn main() -> Result<(), posthog_rs::Error> {
//!     let api_key = std::env::var("POSTHOG_API_KEY")
//!         .expect("set POSTHOG_API_KEY to your PostHog project API key");
//!
//!     let posthog = client(api_key.as_str()).await;
//!     let distinct_id = "user-123";
//!
//!     // Capture an analytics event.
//!     let mut event = Event::new("signed_up", distinct_id);
//!     event.insert_prop("plan", "pro")?;
//!     posthog.capture(event).await?;
//!
//!     // Evaluate feature flags once, then read from the snapshot.
//!     let flags = posthog
//!         .evaluate_flags(distinct_id, EvaluateFlagsOptions::default())
//!         .await?;
//!
//!     if flags.is_enabled("new-onboarding") {
//!         let mut event = Event::new("onboarding_step_completed", distinct_id);
//!         event.with_flags(&flags.only_accessed());
//!         posthog.capture(event).await?;
//!     }
//!
//!     Ok(())
//! }
//!
//! #[cfg(not(feature = "async-client"))]
//! fn main() -> Result<(), posthog_rs::Error> {
//!     let api_key = std::env::var("POSTHOG_API_KEY")
//!         .expect("set POSTHOG_API_KEY to your PostHog project API key");
//!
//!     let posthog = client(api_key.as_str());
//!     let distinct_id = "user-123";
//!
//!     // Capture an analytics event.
//!     let mut event = Event::new("signed_up", distinct_id);
//!     event.insert_prop("plan", "pro")?;
//!     posthog.capture(event)?;
//!
//!     // Evaluate feature flags once, then read from the snapshot.
//!     let flags = posthog.evaluate_flags(distinct_id, EvaluateFlagsOptions::default())?;
//!
//!     if flags.is_enabled("new-onboarding") {
//!         let mut event = Event::new("onboarding_step_completed", distinct_id);
//!         event.with_flags(&flags.only_accessed());
//!         posthog.capture(event)?;
//!     }
//!
//!     Ok(())
//! }
//! ```
mod client;
mod compression;
mod endpoints;
mod error;
#[cfg(feature = "error-tracking")]
mod error_tracking;
mod event;
#[cfg(feature = "capture-v1")]
mod event_v1;
mod feature_flag_evaluations;
mod feature_flags;
mod global;
mod local_evaluation;

// Public interface - any change to this is breaking!
// Client
pub use client::client;
pub use client::get_default_user_agent;
pub use client::BeforeSendHook;
pub use client::CaptureCompression;
pub use client::Client;
pub use client::ClientOptions;
pub use client::ClientOptionsBuilder;
pub use client::ClientOptionsBuilderError;

// Endpoints
pub use endpoints::{
    Endpoint, EndpointManager, DEFAULT_HOST, EU_INGESTION_ENDPOINT, US_INGESTION_ENDPOINT,
};

// Error
pub use error::Error;

// Error Tracking
#[cfg(feature = "error-tracking")]
pub use error_tracking::{
    CaptureExceptionOptions, ErrorTrackingOptions, ErrorTrackingOptionsBuilder,
    ErrorTrackingOptionsBuilderError,
};

// Event
pub use event::Event;
#[cfg(feature = "capture-v1")]
pub use event::EventOptions;

// V1 Capture types
#[cfg(feature = "capture-v1")]
pub use event_v1::{CaptureResponse, EventResult, EventStatus};

// Feature Flags
pub use feature_flag_evaluations::{EvaluateFlagsOptions, FeatureFlagEvaluations};
pub use feature_flags::{
    match_feature_flag, match_feature_flag_with_context, match_property_with_context,
    CohortDefinition, EvaluationContext, FeatureFlag, FeatureFlagCondition, FeatureFlagFilters,
    FeatureFlagsResponse, FlagDetail, FlagMetadata, FlagReason, FlagValue, InconclusiveMatchError,
    MultivariateFilter, MultivariateVariant, Property,
};

// Local Evaluation
pub use local_evaluation::{
    Cohort, FlagCache, FlagPoller, LocalEvaluationConfig, LocalEvaluationResponse, LocalEvaluator,
};

#[cfg(feature = "async-client")]
pub use local_evaluation::AsyncFlagPoller;

// We expose a global capture function as a convenience, that uses a global client
pub use global::capture;
#[cfg(feature = "error-tracking")]
pub use global::capture_exception;
#[cfg(feature = "error-tracking")]
pub use global::capture_exception_with;
pub use global::disable as disable_global;
pub use global::init_global_client as init_global;
pub use global::is_disabled as global_is_disabled;
