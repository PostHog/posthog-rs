//!  Posthog API Client
//!
//!  Allows for communication with posthog API (both public and private)

mod client;
mod config;
mod errors;
mod event;
mod feature_flags;
mod properties;
mod public_api;
/// Data types related to the API
pub mod types;

pub use crate::client::Client;
pub use crate::client::ClientOptionsBuilder;
pub use crate::feature_flags::FeatureFlagsAPI;
pub use crate::types::APIResult;
pub use types::*;
