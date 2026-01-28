use crate::endpoints::EndpointManager;
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
    /// Host URL for the PostHog API (defaults to US ingestion endpoint)
    #[builder(setter(into, strip_option), default)]
    host: Option<String>,

    /// Project API key (required)
    api_key: String,

    /// Request timeout in seconds
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

    /// Enable gzip compression for requests
    #[builder(default = "false")]
    gzip: bool,

    /// Disable tracking (useful for development)
    #[builder(default = "false")]
    disabled: bool,

    /// Disable automatic geoip enrichment
    #[builder(default = "false")]
    disable_geoip: bool,

    /// Feature flags request timeout in seconds
    #[builder(default = "3")]
    feature_flags_request_timeout_seconds: u64,

    #[builder(setter(skip))]
    #[builder(default = "EndpointManager::new(None)")]
    endpoint_manager: EndpointManager,
}

impl ClientOptions {
    /// Get the endpoint manager
    pub(crate) fn endpoints(&self) -> &EndpointManager {
        &self.endpoint_manager
    }

    /// Check if the client is disabled
    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Create ClientOptions with properly initialized endpoint_manager
    fn with_endpoint_manager(mut self) -> Self {
        self.endpoint_manager = EndpointManager::new(self.host.clone());
        self
    }
}

impl From<&str> for ClientOptions {
    fn from(api_key: &str) -> Self {
        ClientOptionsBuilder::default()
            .api_key(api_key.to_string())
            .build()
            .expect("We always set the API key, so this is infallible")
            .with_endpoint_manager()
    }
}

impl From<(&str, &str)> for ClientOptions {
    /// Create options from API key and host
    fn from((api_key, host): (&str, &str)) -> Self {
        ClientOptionsBuilder::default()
            .api_key(api_key.to_string())
            .host(host.to_string())
            .build()
            .expect("We always set the API key, so this is infallible")
            .with_endpoint_manager()
    }
}
