mod client;
mod error;
mod event;
mod global;

const API_ENDPOINT: &str = "https://us.i.posthog.com/capture/";

// Public interface - any change to this is breaking!
// Client
pub use client::client;
pub use client::Client;
pub use client::ClientOptions;

// Error
pub use error::Error;

// Event
pub use event::Event;

// We expose a global capture function as a convenience, that uses a global client
pub use global::capture;
pub use global::init_global_client as init_global;
