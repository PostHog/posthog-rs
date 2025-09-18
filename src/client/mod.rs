use crate::API_ENDPOINT;
use derive_builder::Builder;

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

#[derive(Builder, Clone)]
pub struct ClientOptions {
    #[builder(default = "API_ENDPOINT.to_string()")]
    api_endpoint: String,
    api_key: String,

    #[builder(default = "30")]
    request_timeout_seconds: u64,
    
    /// Personal API key for fetching flag definitions (required for local evaluation)
    #[builder(setter(into, strip_option), default)]
    personal_api_key: Option<String>,
    
    /// Enable local evaluation of feature flags
    #[builder(default = "false")]
    enable_local_evaluation: bool,
    
    /// Interval for polling flag definitions (in seconds)
    #[builder(default = "30")]
    poll_interval_seconds: u64,
}

impl From<&str> for ClientOptions {
    fn from(api_key: &str) -> Self {
        ClientOptionsBuilder::default()
            .api_key(api_key.to_string())
            .build()
            .expect("We always set the API key, so this is infallible")
    }
}
