use crate::Error;
use std::fmt;

/// US ingestion endpoint
pub const US_INGESTION_ENDPOINT: &str = "https://us.i.posthog.com";

/// EU ingestion endpoint  
pub const EU_INGESTION_ENDPOINT: &str = "https://eu.i.posthog.com";

/// Default host (US by default)
pub const DEFAULT_HOST: &str = US_INGESTION_ENDPOINT;

/// API endpoints for different operations
#[derive(Debug, Clone)]
pub enum Endpoint {
    /// Event capture endpoint
    Capture,
    /// Feature flags endpoint
    Flags,
    /// Local evaluation endpoint
    LocalEvaluation,
}

impl Endpoint {
    /// Get the path for this endpoint
    pub fn path(&self) -> &str {
        match self {
            Endpoint::Capture => "/i/v0/e/",
            Endpoint::Flags => "/flags/?v=2",
            Endpoint::LocalEvaluation => "/api/feature_flag/local_evaluation/?send_cohorts",
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path())
    }
}

/// Normalize an endpoint to a base URL.
/// Accepts both hostnames (https://us.posthog.com) and full endpoints (https://us.i.posthog.com/i/v0/e/)
pub fn normalize_endpoint(endpoint: &str) -> Result<String, Error> {
    let endpoint = endpoint.trim();

    // Basic validation - must start with http:// or https://
    if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
        #[allow(deprecated)]
        return Err(Error::Serialization(
            "Endpoint must start with http:// or https://".to_string(),
        ));
    }

    // Parse as URL to validate
    #[allow(deprecated)]
    let url = endpoint
        .parse::<url::Url>()
        .map_err(|e| Error::Serialization(format!("Invalid URL: {}", e)))?;

    // Extract scheme and host
    let scheme = url.scheme();
    #[allow(deprecated)]
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

/// Manages PostHog API endpoints and host configuration
#[derive(Debug, Clone)]
pub struct EndpointManager {
    base_host: String,
    raw_host: String,
}

impl EndpointManager {
    /// Create a new endpoint manager with the given host
    pub fn new(host: Option<String>) -> Self {
        // Normalize the host if provided (strips paths from full endpoint URLs)
        let normalized_host = host.and_then(|h| normalize_endpoint(&h).ok());

        let raw_host = normalized_host
            .clone()
            .unwrap_or_else(|| DEFAULT_HOST.to_string());
        let base_host = Self::determine_server_host(normalized_host);

        Self {
            base_host,
            raw_host,
        }
    }

    /// Determine the actual server host based on the provided host
    pub fn determine_server_host(host: Option<String>) -> String {
        let host_or_default = host.unwrap_or_else(|| DEFAULT_HOST.to_string());
        let trimmed_host = host_or_default.trim_end_matches('/');

        match trimmed_host {
            "https://app.posthog.com" | "https://us.posthog.com" => {
                US_INGESTION_ENDPOINT.to_string()
            }
            "https://eu.posthog.com" => EU_INGESTION_ENDPOINT.to_string(),
            _ => host_or_default,
        }
    }

    /// Get the base host URL (for constructing endpoints)
    pub fn base_host(&self) -> &str {
        &self.base_host
    }

    /// Get the raw host (as provided by the user, used for session replay URLs)
    pub fn raw_host(&self) -> &str {
        &self.raw_host
    }

    /// Build a full URL for a given endpoint
    pub fn build_url(&self, endpoint: Endpoint) -> String {
        format!(
            "{}{}",
            self.base_host.trim_end_matches('/'),
            endpoint.path()
        )
    }

    /// Build a URL with a custom path
    pub fn build_custom_url(&self, path: &str) -> String {
        let normalized_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        format!(
            "{}{}",
            self.base_host.trim_end_matches('/'),
            normalized_path
        )
    }

    /// Build the local evaluation URL with a token
    pub fn build_local_eval_url(&self, token: &str) -> String {
        format!(
            "{}/api/feature_flag/local_evaluation/?token={}&send_cohorts",
            self.base_host.trim_end_matches('/'),
            token
        )
    }

    /// Get the base host for API operations (without the path)
    pub fn api_host(&self) -> String {
        self.base_host.trim_end_matches('/').to_string()
    }

    /// Get the single event capture endpoint URL
    pub fn single_event_endpoint(&self) -> String {
        self.build_url(Endpoint::Capture)
    }

    /// Get the batch event capture endpoint URL (legacy, uses same endpoint as single)
    pub fn batch_event_endpoint(&self) -> String {
        self.build_custom_url("/batch/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_determine_server_host() {
        assert_eq!(
            EndpointManager::determine_server_host(None),
            US_INGESTION_ENDPOINT
        );

        assert_eq!(
            EndpointManager::determine_server_host(Some("https://app.posthog.com".to_string())),
            US_INGESTION_ENDPOINT
        );

        assert_eq!(
            EndpointManager::determine_server_host(Some("https://us.posthog.com".to_string())),
            US_INGESTION_ENDPOINT
        );

        assert_eq!(
            EndpointManager::determine_server_host(Some("https://eu.posthog.com".to_string())),
            EU_INGESTION_ENDPOINT
        );

        assert_eq!(
            EndpointManager::determine_server_host(Some("https://custom.domain.com".to_string())),
            "https://custom.domain.com"
        );
    }

    #[test]
    fn test_build_url() {
        let manager = EndpointManager::new(None);

        assert_eq!(
            manager.build_url(Endpoint::Capture),
            format!("{}/i/v0/e/", US_INGESTION_ENDPOINT)
        );

        assert_eq!(
            manager.build_url(Endpoint::Flags),
            format!("{}/flags/?v=2", US_INGESTION_ENDPOINT)
        );
    }

    #[test]
    fn test_build_custom_url() {
        let manager = EndpointManager::new(Some("https://custom.com/".to_string()));

        assert_eq!(
            manager.build_custom_url("/api/test"),
            "https://custom.com/api/test"
        );

        assert_eq!(
            manager.build_custom_url("api/test"),
            "https://custom.com/api/test"
        );
    }
}
