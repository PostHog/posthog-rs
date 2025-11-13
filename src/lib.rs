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
pub use feature_flags::{FeatureFlag, FlagDetail, FlagMetadata, FlagReason, FlagValue};

// We expose a global capture function as a convenience, that uses a global client
pub use global::capture;
pub use global::disable as disable_global;
pub use global::init_global_client as init_global;
pub use global::is_disabled as global_is_disabled;
