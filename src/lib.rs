mod client;
mod endpoints;
mod error;
mod event;
mod feature_flags;
mod global;
mod local_evaluation;

// Public interface - any change to this is breaking!
// Client
pub use client::client;
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

// Event
pub use event::Event;

// Feature Flags
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
pub use global::disable as disable_global;
pub use global::init_global_client as init_global;
pub use global::is_disabled as global_is_disabled;
