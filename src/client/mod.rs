use derive_builder::Builder;

/// PostHog host configuration for different regions and custom endpoints.
#[derive(Debug, Clone)]
pub enum Host {
    /// US PostHog cloud (<https://us.i.posthog.com/i/v0/e/>).
    US,
    /// EU PostHog cloud (<https://eu.i.posthog.com/i/v0/e/>).
    EU,
    /// Custom PostHog endpoint.
    Custom(String),
}

impl Host {
    const US_ENDPOINT: &'static str = "https://us.i.posthog.com/i/v0/e/";
    const EU_ENDPOINT: &'static str = "https://eu.i.posthog.com/i/v0/e/";
}

impl AsRef<str> for Host {
    fn as_ref(&self) -> &str {
        match self {
            Host::US => Self::US_ENDPOINT,
            Host::EU => Self::EU_ENDPOINT,
            Host::Custom(url) => url,
        }
    }
}

impl std::fmt::Display for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Host::US => write!(f, "{}", Self::US_ENDPOINT),
            Host::EU => write!(f, "{}", Self::EU_ENDPOINT),
            Host::Custom(url) => write!(f, "{url}",),
        }
    }
}

#[cfg(not(feature = "async-client"))]
mod blocking;
#[cfg(not(feature = "async-client"))]
pub use blocking::client;
#[cfg(not(feature = "async-client"))]
pub use blocking::Client;

#[cfg(feature = "async-client")]
mod async_client;
#[cfg(feature = "async-client")]
pub use async_client::client;
#[cfg(feature = "async-client")]
pub use async_client::Client;

#[derive(Builder)]
pub struct ClientOptions {
    #[builder(default = "Host::US")]
    host: Host,
    api_key: String,

    #[builder(default = "30")]
    request_timeout_seconds: u64,
}

impl From<&str> for ClientOptions {
    fn from(api_key: &str) -> Self {
        ClientOptionsBuilder::default()
            .api_key(api_key.to_string())
            .build()
            .expect("We always set the API key, so this is infallible")
    }
}
