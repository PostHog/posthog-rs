mod client;
mod error;
mod event;

use std::time::Duration;

const API_ENDPOINT: &str = "https://us.i.posthog.com/capture/";
const TIMEOUT: &Duration = &Duration::from_millis(800); // This should be specified by the user

// Public interface - any change to this is breaking!
// Client
pub use client::client;
pub use client::Client;
pub use client::ClientOptions;

// Error
pub use error::Error;

// Event
pub use event::Event;
