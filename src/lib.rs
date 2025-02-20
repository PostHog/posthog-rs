mod client;
mod error;
mod event;
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

// We expose a global capture function as a convenience, that uses a global client
pub use global::capture;
pub use global::disable as disable_global;
pub use global::init_global_client as init_global;
pub use global::is_disabled as global_is_disabled;
