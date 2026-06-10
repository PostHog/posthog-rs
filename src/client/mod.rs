use std::sync::{Arc, Mutex};

use crate::endpoints::{EndpointManager, DEFAULT_HOST};
use crate::event::Event;
use derive_builder::Builder;
use tracing::warn;

mod common;

/// Request-body compression algorithm for the capture pipelines.
///
/// When set on [`ClientOptions`], capture requests are compressed and the
/// matching `Content-Encoding` header is sent. The variant string matches the
/// HTTP `Content-Encoding` token the server expects. The V0 pipeline supports
/// `Gzip` only; V1 supports all variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureCompression {
    Gzip,
    Deflate,
    Br,
    Zstd,
}

impl CaptureCompression {
    /// The HTTP `Content-Encoding` token for this algorithm.
    pub(crate) fn content_encoding(self) -> &'static str {
        match self {
            CaptureCompression::Gzip => "gzip",
            CaptureCompression::Deflate => "deflate",
            CaptureCompression::Br => "br",
            CaptureCompression::Zstd => "zstd",
        }
    }
}

#[cfg(not(feature = "async-client"))]
mod blocking;
mod retry;
#[cfg(not(feature = "capture-v1"))]
mod v0_capture;
#[cfg(feature = "capture-v1")]
mod v1_capture;
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

type BeforeSendFn = dyn FnMut(Event) -> Option<Event> + Send + 'static;
type SharedBeforeSendHook = Arc<Mutex<Box<BeforeSendFn>>>;

/// Hook that can modify or discard events before they are sent.
///
/// Hooks run before serialization. Return `Some(event)` to continue sending the
/// event, or `None` to drop it.
#[derive(Clone)]
pub struct BeforeSendHook(SharedBeforeSendHook);

impl BeforeSendHook {
    /// Create a new before-send hook.
    pub fn new<F>(hook: F) -> Self
    where
        F: FnMut(Event) -> Option<Event> + Send + 'static,
    {
        Self(Arc::new(Mutex::new(Box::new(hook))))
    }

    pub(crate) fn apply(&self, event: Event) -> Option<Event> {
        let mut hook = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (hook)(event)
    }
}

/// Configuration options for the PostHog client.
///
/// Use [`ClientOptionsBuilder`] to construct options with custom settings, or
/// create options directly from a project API key with
/// `ClientOptions::from("your-api-key")`.
///
/// # Example
///
/// ```ignore
/// use posthog_rs::ClientOptionsBuilder;
///
/// let options = ClientOptionsBuilder::default()
///     .api_key("your-project-api-key".to_string())
///     .host("https://eu.posthog.com")
///     .build()
///     .unwrap();
/// ```
#[derive(Builder, Clone)]
#[builder(build_fn(name = "build_unchecked", private))]
pub struct ClientOptions {
    /// Host URL for the PostHog API. Defaults to the US ingestion endpoint.
    /// App hosts such as `https://eu.posthog.com` are normalized to ingestion
    /// hosts before requests are sent.
    #[builder(setter(into, strip_option), default)]
    host: Option<String>,

    /// PostHog project API key (project token). If missing or blank, the client
    /// is disabled.
    #[builder(default)]
    api_key: String,

    /// Request timeout in seconds for capture, batch, and local evaluation
    /// definition requests. Defaults to `30`.
    #[builder(default = "30")]
    request_timeout_seconds: u64,

    /// Personal API key for fetching flag definitions. Required when
    /// `enable_local_evaluation` is `true`.
    #[builder(setter(into, strip_option), default)]
    personal_api_key: Option<String>,

    /// Enable local evaluation of feature flags using a background definitions
    /// poller.
    #[builder(default = "false")]
    enable_local_evaluation: bool,

    /// Interval for polling flag definitions, in seconds. Defaults to `30`.
    #[builder(default = "30")]
    poll_interval_seconds: u64,

    /// Disable tracking and remote flag requests. Useful for development and
    /// tests.
    #[builder(default = "false")]
    disabled: bool,

    /// Disable automatic GeoIP enrichment for capture and flag requests.
    #[builder(default = "false")]
    disable_geoip: bool,

    /// Whether events originate from a server-side runtime. Defaults to `true`,
    /// which stamps `$is_server: true` so PostHog won't attribute the host OS to
    /// the user. Set `false` for client/CLI use (the property is then omitted).
    #[builder(default = "true")]
    is_server: bool,

    /// Timeout in seconds for remote `/flags` requests. Defaults to `3`.
    #[builder(default = "3")]
    feature_flags_request_timeout_seconds: u64,

    /// When true, never fall back to the remote API for flag evaluation. If local
    /// evaluation is inconclusive (flag not cached or missing properties), the SDK
    /// returns `Ok(None)` instead of making a network call. Only meaningful when
    /// `enable_local_evaluation` is also true.
    #[builder(default = "false")]
    local_evaluation_only: bool,

    /// Maximum number of attempts for V1 capture requests (default: 3).
    /// Includes the initial attempt, so `3` means 1 initial + 2 retries.
    #[builder(default = "3")]
    #[cfg_attr(not(feature = "capture-v1"), allow(dead_code))]
    pub(crate) max_capture_attempts: u32,

    /// Initial retry backoff duration in milliseconds (default: 200)
    #[builder(default = "200")]
    #[cfg_attr(not(feature = "capture-v1"), allow(dead_code))]
    pub(crate) retry_initial_backoff_ms: u64,

    /// Maximum retry backoff duration in milliseconds (default: 30000)
    #[builder(default = "30000")]
    #[cfg_attr(not(feature = "capture-v1"), allow(dead_code))]
    pub(crate) retry_max_backoff_ms: u64,

    /// Optional request-body compression. When `None` (default), bodies are
    /// sent uncompressed. The V0 pipeline supports `Gzip` only; V1 supports all
    /// variants.
    #[builder(default, setter(strip_option))]
    pub(crate) capture_compression: Option<CaptureCompression>,

    /// Hooks to modify, filter, or sample events before they are sent.
    #[builder(default, setter(custom))]
    pub(crate) before_send: Vec<BeforeSendHook>,

    /// Extra HTTP headers injected into every outbound capture request.
    /// Used by the SDK test harness adapter to attach `X-Test-Id` for
    /// parallel test isolation.
    #[cfg(feature = "test-harness")]
    #[builder(default, setter(strip_option))]
    #[allow(dead_code)]
    pub(crate) extra_capture_headers: Option<std::collections::HashMap<String, String>>,

    #[builder(setter(skip))]
    #[builder(default = "EndpointManager::new(DEFAULT_HOST.to_string())")]
    endpoint_manager: EndpointManager,
}

/// Resolved client-level default properties for capture requests.
///
/// Built once from [`ClientOptions`] and threaded through all event-producing
/// paths (V0 capture, V0 flag-called host, V1 capture) so each default is
/// applied in exactly one place with caller-wins (`entry().or_insert`)
/// semantics.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CaptureDefaults {
    pub(crate) disable_geoip: bool,
    pub(crate) is_server: bool,
}

impl ClientOptions {
    /// Build the resolved capture defaults for this client configuration.
    pub(crate) fn capture_defaults(&self) -> CaptureDefaults {
        CaptureDefaults {
            disable_geoip: self.disable_geoip,
            is_server: self.is_server,
        }
    }

    /// Get the endpoint manager
    pub(crate) fn endpoints(&self) -> &EndpointManager {
        &self.endpoint_manager
    }

    /// Check whether the client is disabled.
    ///
    /// A client is disabled when configured with `disabled(true)` or when the
    /// project API key is missing or blank after trimming.
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
    /// Add a hook that can modify or discard events before they are sent.
    pub fn before_send<F>(&mut self, hook: F) -> &mut Self
    where
        F: FnMut(Event) -> Option<Event> + Send + 'static,
    {
        self.before_send
            .get_or_insert_with(Vec::new)
            .push(BeforeSendHook::new(hook));
        self
    }

    /// Build sanitized [`ClientOptions`].
    ///
    /// Missing or whitespace-only API keys are allowed and disable the client so
    /// SDK initialization remains infallible while avoiding requests with an
    /// empty API key.
    ///
    /// # Errors
    ///
    /// Returns [`ClientOptionsBuilderError`] if a required builder value is
    /// invalid according to the generated builder.
    pub fn build(&self) -> Result<ClientOptions, ClientOptionsBuilderError> {
        Ok(self.build_unchecked()?.sanitize())
    }
}

impl From<&str> for ClientOptions {
    /// Create options from a PostHog project API key.
    fn from(api_key: &str) -> Self {
        ClientOptionsBuilder::default()
            .api_key(api_key.to_string())
            .build()
            .expect("We always set the API key, so this is infallible")
    }
}

impl From<(&str, &str)> for ClientOptions {
    /// Create options from a PostHog project API key and host URL.
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
