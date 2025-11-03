use crate::Error;

const DEFAULT_HOST: &str = "https://us.i.posthog.com";
const SINGLE_EVENT_PATH: &str = "/i/v0/e/";
const BATCH_EVENT_PATH: &str = "/batch/";

/// Configuration options for the PostHog client.
#[derive(Debug)]
pub struct ClientOptions {
    api_endpoint: String,
    api_key: String,
    request_timeout_seconds: u64,
}

impl ClientOptions {
    /// Get the full endpoint URL for single event capture
    pub fn single_event_endpoint(&self) -> String {
        format!(
            "{}{}",
            self.api_endpoint.trim_end_matches('/'),
            SINGLE_EVENT_PATH
        )
    }

    /// Get the full endpoint URL for batch event capture
    pub fn batch_event_endpoint(&self) -> String {
        format!(
            "{}{}",
            self.api_endpoint.trim_end_matches('/'),
            BATCH_EVENT_PATH
        )
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn request_timeout_seconds(&self) -> u64 {
        self.request_timeout_seconds
    }
}

/// Builder for ClientOptions with validation.
pub struct ClientOptionsBuilder {
    api_endpoint: Option<String>,
    api_key: Option<String>,
    request_timeout_seconds: Option<u64>,
}

impl ClientOptionsBuilder {
    /// Create a new ClientOptionsBuilder with default values
    pub fn new() -> Self {
        Self {
            api_endpoint: None,
            api_key: None,
            request_timeout_seconds: None,
        }
    }

    /// Set the API key (required)
    pub fn api_key(mut self, api_key: String) -> Self {
        self.api_key = Some(api_key);
        self
    }

    /// Set the API endpoint. Accepts either:
    /// - A hostname like "https://us.posthog.com"
    /// - A full endpoint URL like "https://us.i.posthog.com/i/v0/e/" (for backward compatibility)
    ///
    /// The SDK will automatically append the appropriate paths (/i/v0/e/ or /batch/)
    /// based on the operation being performed.
    pub fn api_endpoint(mut self, endpoint: String) -> Self {
        self.api_endpoint = Some(endpoint);
        self
    }

    /// Set the request timeout in seconds (default: 30)
    pub fn request_timeout_seconds(mut self, seconds: u64) -> Self {
        self.request_timeout_seconds = Some(seconds);
        self
    }

    /// Build the ClientOptions, validating all fields
    pub fn build(self) -> Result<ClientOptions, Error> {
        let api_key = self
            .api_key
            .ok_or_else(|| Error::Serialization("API key is required".to_string()))?;

        let request_timeout_seconds = self.request_timeout_seconds.unwrap_or(30);

        // Process the api_endpoint
        let api_endpoint = if let Some(endpoint) = self.api_endpoint {
            normalize_endpoint(&endpoint)?
        } else {
            DEFAULT_HOST.to_string()
        };

        Ok(ClientOptions {
            api_endpoint,
            api_key,
            request_timeout_seconds,
        })
    }
}

impl Default for ClientOptionsBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Normalize an endpoint to a base URL.
/// Accepts both hostnames (https://us.posthog.com) and full endpoints (https://us.i.posthog.com/i/v0/e/)
fn normalize_endpoint(endpoint: &str) -> Result<String, Error> {
    let endpoint = endpoint.trim();

    // Basic validation - must start with http:// or https://
    if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
        return Err(Error::Serialization(
            "Endpoint must start with http:// or https://".to_string(),
        ));
    }

    // Parse as URL to validate
    let url = endpoint
        .parse::<url::Url>()
        .map_err(|e| Error::Serialization(format!("Invalid URL: {}", e)))?;

    // Extract scheme and host
    let scheme = url.scheme();
    let host = url
        .host_str()
        .ok_or_else(|| Error::Serialization("Missing host".to_string()))?;

    // Check if this looks like a full endpoint path (contains /i/v0/e or /batch)
    let path = url.path();
    if path.contains("/i/v0/e") || path.contains("/batch") {
        // Strip the path, keep only scheme://host:port
        let port = url.port().map(|p| format!(":{}", p)).unwrap_or_default();
        Ok(format!("{}://{}{}", scheme, host, port))
    } else {
        // Already a base URL, just reconstruct it cleanly
        let port = url.port().map(|p| format!(":{}", p)).unwrap_or_default();
        Ok(format!("{}://{}{}", scheme, host, port))
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
