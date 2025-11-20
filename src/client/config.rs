use crate::endpoints::{normalize_endpoint, EndpointManager};
use crate::Error;

/// Configuration options for the PostHog client.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub(crate) host: Option<String>,
    pub(crate) api_key: String,
    pub(crate) request_timeout_seconds: u64,

    // Feature flags related fields
    pub(crate) personal_api_key: Option<String>,
    pub(crate) enable_local_evaluation: bool,
    pub(crate) poll_interval_seconds: u64,
    pub(crate) feature_flags_request_timeout_seconds: u64,
    pub(crate) send_feature_flag_events: bool,

    // Other configuration
    pub(crate) gzip: bool,
    pub(crate) disabled: bool,
    pub(crate) disable_geoip: bool,

    // Endpoint management
    pub(crate) endpoint_manager: EndpointManager,
}

impl ClientOptions {
    /// Get the full endpoint URL for single event capture
    #[cfg(test)]
    pub(crate) fn single_event_endpoint(&self) -> String {
        use crate::endpoints::Endpoint;
        self.endpoint_manager.build_url(Endpoint::Capture)
    }

    /// Get the full endpoint URL for batch event capture
    #[cfg(test)]
    pub(crate) fn batch_event_endpoint(&self) -> String {
        self.endpoint_manager.batch_event_endpoint()
    }

    /// Get the endpoint manager
    pub(crate) fn endpoints(&self) -> &EndpointManager {
        &self.endpoint_manager
    }

    /// Check if the client is disabled
    pub(crate) fn is_disabled(&self) -> bool {
        self.disabled
    }
}

/// Builder for ClientOptions with validation.
pub struct ClientOptionsBuilder {
    api_endpoint: Option<String>,
    host: Option<String>,
    api_key: Option<String>,
    request_timeout_seconds: Option<u64>,
    personal_api_key: Option<String>,
    enable_local_evaluation: Option<bool>,
    poll_interval_seconds: Option<u64>,
    feature_flags_request_timeout_seconds: Option<u64>,
    send_feature_flag_events: Option<bool>,
    gzip: Option<bool>,
    disabled: Option<bool>,
    disable_geoip: Option<bool>,
}

impl ClientOptionsBuilder {
    /// Create a new ClientOptionsBuilder with default values
    pub fn new() -> Self {
        Self {
            api_endpoint: None,
            host: None,
            api_key: None,
            request_timeout_seconds: None,
            personal_api_key: None,
            enable_local_evaluation: None,
            poll_interval_seconds: None,
            feature_flags_request_timeout_seconds: None,
            send_feature_flag_events: None,
            gzip: None,
            disabled: None,
            disable_geoip: None,
        }
    }

    /// Set the API key (required)
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Set the host URL (defaults to US ingestion endpoint)
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Set the API endpoint. Accepts either:
    /// - A hostname like "https://us.posthog.com"
    /// - A full endpoint URL like "https://us.i.posthog.com/i/v0/e/" (for backward compatibility)
    ///
    /// The SDK will automatically append the appropriate paths (/i/v0/e/ or /batch/)
    /// based on the operation being performed.
    pub fn api_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.api_endpoint = Some(endpoint.into());
        self
    }

    /// Set the request timeout in seconds (default: 30)
    pub fn request_timeout_seconds(mut self, seconds: u64) -> Self {
        self.request_timeout_seconds = Some(seconds);
        self
    }

    /// Set the personal API key for flag definitions (required for local evaluation)
    pub fn personal_api_key(mut self, key: impl Into<String>) -> Self {
        self.personal_api_key = Some(key.into());
        self
    }

    /// Enable local evaluation of feature flags
    pub fn enable_local_evaluation(mut self, enable: bool) -> Self {
        self.enable_local_evaluation = Some(enable);
        self
    }

    /// Set the poll interval for flag definitions (default: 30)
    pub fn poll_interval_seconds(mut self, seconds: u64) -> Self {
        self.poll_interval_seconds = Some(seconds);
        self
    }

    /// Set the feature flags request timeout (default: 3)
    pub fn feature_flags_request_timeout_seconds(mut self, seconds: u64) -> Self {
        self.feature_flags_request_timeout_seconds = Some(seconds);
        self
    }

    /// Enable automatic $feature_flag_called events (default: true)
    pub fn send_feature_flag_events(mut self, send: bool) -> Self {
        self.send_feature_flag_events = Some(send);
        self
    }

    /// Enable gzip compression for requests
    pub fn gzip(mut self, enable: bool) -> Self {
        self.gzip = Some(enable);
        self
    }

    /// Disable tracking (useful for development)
    pub fn disabled(mut self, disable: bool) -> Self {
        self.disabled = Some(disable);
        self
    }

    /// Disable automatic geoip enrichment
    pub fn disable_geoip(mut self, disable: bool) -> Self {
        self.disable_geoip = Some(disable);
        self
    }

    /// Build the ClientOptions, validating all fields
    pub fn build(self) -> Result<ClientOptions, Error> {
        #[allow(deprecated)]
        let api_key = self
            .api_key
            .ok_or_else(|| Error::Serialization("API key is required".to_string()))?;

        let request_timeout_seconds = self.request_timeout_seconds.unwrap_or(30);

        // Process the endpoint with correct priority: api_endpoint > host
        let endpoint_to_use = self.api_endpoint.or(self.host.clone());

        // Validate the endpoint if provided
        if let Some(ref endpoint) = endpoint_to_use {
            normalize_endpoint(endpoint)?;
        }

        // Initialize endpoint manager with the prioritized endpoint
        let endpoint_manager = EndpointManager::new(endpoint_to_use);

        Ok(ClientOptions {
            host: self.host,
            api_key,
            request_timeout_seconds,
            personal_api_key: self.personal_api_key,
            enable_local_evaluation: self.enable_local_evaluation.unwrap_or(false),
            poll_interval_seconds: self.poll_interval_seconds.unwrap_or(30),
            feature_flags_request_timeout_seconds: self
                .feature_flags_request_timeout_seconds
                .unwrap_or(3),
            send_feature_flag_events: self.send_feature_flag_events.unwrap_or(true),
            gzip: self.gzip.unwrap_or(false),
            disabled: self.disabled.unwrap_or(false),
            disable_geoip: self.disable_geoip.unwrap_or(false),
            endpoint_manager,
        })
    }
}

impl Default for ClientOptionsBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl From<&str> for ClientOptions {
    fn from(api_key: &str) -> Self {
        ClientOptionsBuilder::default()
            .api_key(api_key.to_string())
            .build()
            .expect("We always set the API key, so this is infallible")
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_options_builder_default_endpoint() {
        let options = ClientOptionsBuilder::new()
            .api_key("test_key")
            .build()
            .unwrap();

        assert_eq!(
            options.single_event_endpoint(),
            "https://us.i.posthog.com/i/v0/e/"
        );
        assert_eq!(
            options.batch_event_endpoint(),
            "https://us.i.posthog.com/batch/"
        );
    }

    #[test]
    fn test_client_options_builder_with_hostname() {
        let options = ClientOptionsBuilder::new()
            .api_key("test_key")
            .api_endpoint("https://eu.posthog.com")
            .build()
            .unwrap();

        // EU PostHog Cloud redirects to EU ingestion endpoint
        assert_eq!(
            options.single_event_endpoint(),
            "https://eu.i.posthog.com/i/v0/e/"
        );
        assert_eq!(
            options.batch_event_endpoint(),
            "https://eu.i.posthog.com/batch/"
        );
    }

    #[test]
    fn test_client_options_builder_with_full_endpoint_single() {
        // Backward compatibility: accept full endpoint and strip path
        let options = ClientOptionsBuilder::new()
            .api_key("test_key")
            .api_endpoint("https://us.i.posthog.com/i/v0/e/")
            .build()
            .unwrap();

        assert_eq!(
            options.single_event_endpoint(),
            "https://us.i.posthog.com/i/v0/e/"
        );
        assert_eq!(
            options.batch_event_endpoint(),
            "https://us.i.posthog.com/batch/"
        );
    }

    #[test]
    fn test_client_options_builder_with_full_endpoint_batch() {
        // Backward compatibility: accept batch endpoint and strip path
        let options = ClientOptionsBuilder::new()
            .api_key("test_key")
            .api_endpoint("https://us.i.posthog.com/batch/")
            .build()
            .unwrap();

        assert_eq!(
            options.single_event_endpoint(),
            "https://us.i.posthog.com/i/v0/e/"
        );
        assert_eq!(
            options.batch_event_endpoint(),
            "https://us.i.posthog.com/batch/"
        );
    }

    #[test]
    fn test_client_options_builder_with_port() {
        let options = ClientOptionsBuilder::new()
            .api_key("test_key")
            .api_endpoint("http://localhost:8000")
            .build()
            .unwrap();

        assert_eq!(
            options.single_event_endpoint(),
            "http://localhost:8000/i/v0/e/"
        );
        assert_eq!(
            options.batch_event_endpoint(),
            "http://localhost:8000/batch/"
        );
    }

    #[test]
    fn test_client_options_builder_with_trailing_slash() {
        let options = ClientOptionsBuilder::new()
            .api_key("test_key")
            .api_endpoint("https://eu.posthog.com/")
            .build()
            .unwrap();

        assert_eq!(
            options.single_event_endpoint(),
            "https://eu.i.posthog.com/i/v0/e/"
        );
        assert_eq!(
            options.batch_event_endpoint(),
            "https://eu.i.posthog.com/batch/"
        );
    }

    #[test]
    fn test_client_options_builder_invalid_endpoint_no_scheme() {
        let result = ClientOptionsBuilder::new()
            .api_key("test_key")
            .api_endpoint("posthog.com")
            .build();

        assert!(result.is_err());
        match result.unwrap_err() {
            #[allow(deprecated)]
            Error::Serialization(msg) => {
                assert!(msg.contains("Endpoint must start with http://"));
            }
            _ => panic!("Expected Serialization error"),
        }
    }

    #[test]
    fn test_client_options_builder_invalid_endpoint_malformed() {
        let result = ClientOptionsBuilder::new()
            .api_key("test_key")
            .api_endpoint("not a url")
            .build();

        assert!(result.is_err());
        match result.unwrap_err() {
            #[allow(deprecated)]
            Error::Serialization(msg) => {
                // Should contain error about scheme or being invalid
                assert!(msg.contains("http://") || msg.contains("https://"));
            }
            _ => panic!("Expected Serialization error"),
        }
    }

    #[test]
    fn test_client_options_builder_missing_api_key() {
        let result = ClientOptionsBuilder::new().build();

        assert!(result.is_err());
        match result.unwrap_err() {
            #[allow(deprecated)]
            Error::Serialization(msg) => {
                assert!(msg.contains("API key"));
            }
            _ => panic!("Expected Serialization error"),
        }
    }
}
