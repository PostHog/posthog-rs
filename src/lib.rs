mod client;
mod error;
mod event;

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
