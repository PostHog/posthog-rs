use crate::endpoints::{EndpointManager, DEFAULT_HOST};
use derive_builder::Builder;
use tracing::warn;

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

/// Configuration options for the PostHog client.
///
/// Use [`ClientOptionsBuilder`] to construct options with custom settings,
/// or create directly from an API key using `ClientOptions::from("your-api-key")`.
///
/// # Example
///
/// ```ignore
/// use posthog_rs::ClientOptionsBuilder;
///
/// let options = ClientOptionsBuilder::default()
///     .api_key("your-api-key".to_string())
///     .host("https://eu.posthog.com")
///     .build()
///     .unwrap();
/// ```
#[derive(Builder, Clone)]
#[builder(build_fn(name = "build_unchecked", private))]
pub struct ClientOptions {
    /// Host URL for the PostHog API (defaults to US ingestion endpoint)
    #[builder(setter(into, strip_option), default)]
    host: Option<String>,

    /// Project API key. If missing or blank, the client is disabled.
    #[builder(default)]
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

    /// Disable tracking (useful for development)
    #[builder(default = "false")]
    disabled: bool,

    /// Disable automatic geoip enrichment
    #[builder(default = "false")]
    disable_geoip: bool,

    /// Whether events originate from a server-side environment.
    ///
    /// Defaults to `true`. When `true`, the SDK adds `$is_server: true` to every
    /// captured event so PostHog can identify server-side events. Set this to
    /// `false` when running posthog-rs as a client/CLI (for example a desktop or
    /// command-line tool) so the event's device OS is attributed normally and
    /// `$is_server` is omitted. A caller-set `$is_server` property on an
    /// individual event always takes precedence over this option.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use posthog_rs::ClientOptionsBuilder;
    ///
    /// let options = ClientOptionsBuilder::default()
    ///     .api_key("your-api-key".to_string())
    ///     .is_server(false) // running as a CLI/client
    ///     .build()
    ///     .unwrap();
    /// ```
    #[builder(default = "true")]
    is_server: bool,

    /// Feature flags request timeout in seconds
    #[builder(default = "3")]
    feature_flags_request_timeout_seconds: u64,

    /// When true, never fall back to the remote API for flag evaluation. If local
    /// evaluation is inconclusive (flag not cached or missing properties), the SDK
    /// returns `Ok(None)` instead of making a network call. Only meaningful when
    /// `enable_local_evaluation` is also true.
    #[builder(default = "false")]
    local_evaluation_only: bool,

    #[builder(setter(skip))]
    #[builder(default = "EndpointManager::new(DEFAULT_HOST.to_string())")]
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

    fn sanitize(mut self) -> Self {
        self.api_key = self.api_key.trim().to_string();
        if self.api_key.is_empty() {
            warn!("api_key is empty after trimming whitespace; disabling PostHog client");
            self.disabled = true;
        }
        self.host = Some(match self.host {
            Some(host) => {
                let normalized = host.trim().to_string();
                if normalized.is_empty() {
                    DEFAULT_HOST.to_string()
                } else {
                    normalized
                }
            }
            None => DEFAULT_HOST.to_string(),
        });
        self.personal_api_key = self.personal_api_key.and_then(|personal_api_key| {
            let normalized = personal_api_key.trim().to_string();
            if normalized.is_empty() {
                None
            } else {
                Some(normalized)
            }
        });
        self.endpoint_manager = EndpointManager::new(
            self.host
                .clone()
                .expect("host is always normalized in sanitize"),
        );
        self
    }
}

impl ClientOptionsBuilder {
    /// Build sanitized [`ClientOptions`].
    ///
    /// Missing or whitespace-only API keys are allowed and disable the client so
    /// SDK initialization remains infallible while avoiding requests with an
    /// empty API key.
    pub fn build(&self) -> Result<ClientOptions, ClientOptionsBuilderError> {
        Ok(self.build_unchecked()?.sanitize())
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
    use super::ClientOptionsBuilder;
    use crate::endpoints::{EU_INGESTION_ENDPOINT, US_INGESTION_ENDPOINT};

    #[test]
    fn trims_whitespace_sensitive_options() {
        let options = ClientOptionsBuilder::default()
            .api_key(" \n test-api-key\t ".to_string())
            .host(" \nhttps://eu.posthog.com/\t ")
            .personal_api_key(" \n\t ")
            .build()
            .unwrap();

        assert_eq!(options.api_key, "test-api-key");
        assert_eq!(options.host.as_deref(), Some("https://eu.posthog.com/"));
        assert_eq!(options.personal_api_key, None);
        assert_eq!(options.endpoints().api_host(), EU_INGESTION_ENDPOINT);
    }

    #[test]
    fn defaults_blank_host_after_trimming_whitespace() {
        let options = ClientOptionsBuilder::default()
            .api_key("test-api-key".to_string())
            .host(" \n\t ")
            .build()
            .unwrap();

        assert_eq!(options.host.as_deref(), Some(US_INGESTION_ENDPOINT));
        assert_eq!(options.endpoints().api_host(), US_INGESTION_ENDPOINT);
    }

    #[test]
    fn builder_allows_missing_api_key_and_disables_client() {
        let options = ClientOptionsBuilder::default().build().unwrap();

        assert_eq!(options.api_key, "");
        assert!(options.is_disabled());
    }

    #[test]
    fn builder_disables_client_for_trim_empty_api_key() {
        let options = ClientOptionsBuilder::default()
            .api_key(" \n\t ".to_string())
            .build()
            .unwrap();

        assert_eq!(options.api_key, "");
        assert!(options.is_disabled());
    }
}
