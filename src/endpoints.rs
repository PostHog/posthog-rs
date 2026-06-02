use std::fmt;

/// US ingestion endpoint
pub const US_INGESTION_ENDPOINT: &str = "https://us.i.posthog.com";

/// EU ingestion endpoint
pub const EU_INGESTION_ENDPOINT: &str = "https://eu.i.posthog.com";

/// Default host (US by default)
pub const DEFAULT_HOST: &str = US_INGESTION_ENDPOINT;

/// API endpoints used by the SDK for different operations.
#[derive(Debug, Clone)]
pub enum Endpoint {
    /// Event capture endpoint
    Capture,
    /// Batch event capture endpoint
    Batch,
    /// Feature flags endpoint
    Flags,
    /// Local evaluation endpoint
    LocalEvaluation,
}

impl Endpoint {
    /// Get the URL path for this endpoint.
    pub fn path(&self) -> &str {
        match self {
            Endpoint::Capture => "/i/v0/e/",
            Endpoint::Batch => "/batch/",
            Endpoint::Flags => "/flags/?v=2",
            Endpoint::LocalEvaluation => "/flags/definitions/?send_cohorts",
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path())
    }
}

/// Manages PostHog API endpoints and host configuration.
///
/// This low-level helper normalizes app hosts such as
/// `https://us.posthog.com` to ingestion hosts such as
/// [`US_INGESTION_ENDPOINT`].
#[derive(Debug, Clone)]
pub struct EndpointManager {
    base_host: String,
    raw_host: String,
}

impl EndpointManager {
    /// Create a new endpoint manager for `host`.
    ///
    /// `host` may be an app host (for example `https://eu.posthog.com`), an
    /// ingestion host, or a custom reverse-proxy host.
    pub fn new(host: String) -> Self {
        let base_host = Self::determine_server_host(&host);

        Self {
            base_host,
            raw_host: host,
        }
    }

    /// Determine the ingestion host used for API calls.
    ///
    /// Maps PostHog app hosts to their ingestion equivalents and removes a
    /// trailing slash. Custom hosts are returned unchanged except for the
    /// trailing slash.
    pub fn determine_server_host(host: &str) -> String {
        let trimmed_host = host.trim_end_matches('/');

        match trimmed_host {
            "https://app.posthog.com" | "https://us.posthog.com" => {
                US_INGESTION_ENDPOINT.to_string()
            }
            "https://eu.posthog.com" => EU_INGESTION_ENDPOINT.to_string(),
            _ => trimmed_host.to_string(),
        }
    }

    /// Get the normalized base host URL used for API calls.
    pub fn base_host(&self) -> &str {
        &self.base_host
    }

    /// Get the raw host as provided by the user.
    pub fn raw_host(&self) -> &str {
        &self.raw_host
    }

    /// Build a full URL for a given SDK endpoint.
    pub fn build_url(&self, endpoint: Endpoint) -> String {
        format!(
            "{}{}",
            self.base_host.trim_end_matches('/'),
            endpoint.path()
        )
    }

    /// Build a URL with a custom path relative to the base host.
    ///
    /// The path may be passed with or without a leading `/`.
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

    /// Build the local evaluation definitions URL with a project token.
    ///
    /// The token is included as a query parameter, so avoid logging this URL in
    /// production diagnostics.
    pub fn build_local_eval_url(&self, token: &str) -> String {
        format!(
            "{}/flags/definitions/?token={}&send_cohorts",
            self.base_host.trim_end_matches('/'),
            token
        )
    }

    /// Get the base host for API operations without a trailing slash or path.
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
            EndpointManager::determine_server_host("https://app.posthog.com"),
            US_INGESTION_ENDPOINT
        );

        assert_eq!(
            EndpointManager::determine_server_host("https://us.posthog.com"),
            US_INGESTION_ENDPOINT
        );

        assert_eq!(
            EndpointManager::determine_server_host("https://eu.posthog.com"),
            EU_INGESTION_ENDPOINT
        );

        assert_eq!(
            EndpointManager::determine_server_host("https://custom.domain.com"),
            "https://custom.domain.com"
        );

        assert_eq!(
            EndpointManager::determine_server_host("https://eu.posthog.com/"),
            EU_INGESTION_ENDPOINT
        );
    }

    #[test]
    fn test_build_url() {
        let manager = EndpointManager::new(DEFAULT_HOST.to_string());

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
        let manager = EndpointManager::new("https://custom.com/".to_string());

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
