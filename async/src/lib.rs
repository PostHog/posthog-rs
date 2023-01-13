mod client;
mod client_options;
mod error;

pub use client::Client;
pub use client_options::ClientOptions;
pub use error::Error;

pub use posthog_core::event::{Event, Properties};

pub fn client<C: Into<ClientOptions>>(options: C) -> Client {
    options.into().build()
}
