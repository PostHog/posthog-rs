mod client;
mod error;
mod event;
mod llm;
mod integrations;
mod global;

const API_ENDPOINT: &str = "https://us.i.posthog.com/i/v0/e/";

// Public interface - any change to this is breaking!
// Client
pub use client::client;
pub use client::Client;
pub use client::ClientOptions;
pub use client::ClientOptionsBuilder;
pub use client::ClientOptionsBuilderError;

// Error
pub use error::Error;

// Event
pub use event::Event;

// LLM Analytics
pub use llm::generation::GenerationBuilder;
pub use llm::trace::{TraceBuilder, SpanBuilder};
pub use llm::embedding::EmbeddingBuilder;
pub use llm::privacy::PrivacyMode;

// Integrations
#[cfg(feature = "rig-integration")]
pub use integrations::rig::*;

// We expose a global capture function as a convenience, that uses a global client
pub use global::capture;
pub use global::disable as disable_global;
pub use global::init_global_client as init_global;
pub use global::is_disabled as global_is_disabled;
