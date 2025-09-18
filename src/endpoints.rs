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

/// Manages PostHog API endpoints and host configuration
#[derive(Debug, Clone)]
pub struct EndpointManager {
    base_host: String,
    raw_host: String,
}

impl EndpointManager {
    /// Create a new endpoint manager with the given host
    pub fn new(host: Option<String>) -> Self {
        let raw_host = host.clone().unwrap_or_else(|| DEFAULT_HOST.to_string());
        let base_host = Self::determine_server_host(host);

        Self {
            base_host,
            raw_host,
        }
    }

    /// Determine the actual server host based on the provided host
    /// Similar to posthog-python's determine_server_host function
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
            format!("/{}", path)
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
