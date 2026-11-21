use std::collections::HashMap;
use std::time::Duration;

use super::{Persistence, PersonProfiles};
use crate::client::event_store::StoreLimits;
use crate::client::transport::MAX_SHUTDOWN_TIMEOUT;
use crate::client::{ClientOptions, OnErrorHook, PostHogError};
use crate::{BeforeSendHook, CaptureCompression, EndpointManager, Event, DEFAULT_HOST};

/// A client SDK waits 1 second before its first resend, doubling to the
/// 30-second ceiling, where a server SDK starts at 100 ms.
const RETRY_INITIAL_BACKOFF_MS: u64 = 1_000;

/// The request timeout of the iOS, Android and posthog-js clients.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Configuration for a stateful client. Start with [`Config::new`].
///
/// Out-of-range settings are clamped and an invalid host falls back to the
/// default host, as in the other PostHog SDKs. A missing or invalid project
/// token reports an initialization diagnostic and disables the client. Storage
/// failures instead preserve enabled, memory-backed operation.
#[non_exhaustive]
pub struct Config {
    /// Project token, trimmed during initialization. Never use a personal API key.
    pub project_token: String,
    /// Ingestion host or reverse proxy. PostHog app hosts are normalized.
    pub host: String,
    /// Explicitly disable this client. Defaults to false.
    pub disabled: bool,
    /// Storage selection. Defaults to the OS local application-data directory.
    pub persistence: Persistence,
    /// Separates the storage of clients that use the same project token. The
    /// storage namespace is the project token plus this name; the host does not
    /// change it. Default: "default".
    /// Each persistent namespace has one live owner; see [`Persistence`] for
    /// sharing a client or selecting independent identities.
    pub instance_name: String,
    /// Seed a fresh scope's anonymous identity. Persisted identity takes precedence.
    pub bootstrap_anonymous_id: Option<String>,
    /// Identified setup seed. Mutually exclusive with bootstrap_anonymous_id.
    /// Reconciles restored anonymous state; never replaces a different identified user.
    pub bootstrap_identified_id: Option<String>,
    /// Default person processing and identity-operation policy. Default: IdentifiedOnly.
    /// Explicit capture options and hooks retain their normal precedence.
    pub person_profiles: PersonProfiles,
    /// Events that trigger an automatic flush. Default: 20.
    pub flush_at: usize,
    /// Maximum events per batch. Default: 100.
    pub max_batch_size: usize,
    /// Interval between automatic flushes. Default: 10 seconds.
    pub flush_interval: Duration,
    /// Maximum events queued for delivery: on disk, or in memory with
    /// [`Persistence::Memory`]. Default: 1,000. At this limit the oldest events
    /// are dropped to keep the newest; capture still returns
    /// [`CaptureOutcome::Accepted`](super::CaptureOutcome::Accepted).
    pub max_queue_size: usize,
    /// Serialized event bytes held in memory: the queue with
    /// [`Persistence::Memory`], otherwise events not yet written to disk.
    pub(crate) max_queue_bytes: usize,
    /// Serialized event bytes stored on disk.
    pub(crate) max_stored_bytes: u64,
    /// How long to wait after a state change before writing client state to
    /// disk, so changes made close together are written once.
    pub(crate) state_write_delay: Duration,
    /// Timeout per network request. Default: 10 seconds. Sub-second precision is
    /// rounded up to whole seconds.
    pub request_timeout: Duration,
    /// Default SDK-controlled shutdown waiting budget. Default: 30 seconds.
    /// The first shutdown selects one deadline; later calls cannot extend it.
    /// Dropping the client ends that deadline at once and does not wait for
    /// delivery; it waits up to about 1 s for local writes. Already-started work owns its
    /// resources until completion and may retain them indefinitely if it never
    /// returns. Arbitrary destructors of application objects captured by
    /// callbacks may block outside this bound.
    pub shutdown_timeout: Duration,
    /// Request compression. Default: gzip; None disables compression.
    pub capture_compression: Option<CaptureCompression>,
    /// Disable server-side GeoIP enrichment. Default: false.
    pub disable_geoip: bool,
    /// Platform properties cached at initialization. Per-event properties win.
    /// Missing $os/$os_version are discovered once during initialization.
    pub platform_context: HashMap<String, serde_json::Value>,
    /// SDK name for events' `$lib` and for the `PostHog-Sdk-Info` and
    /// `User-Agent` headers of capture requests. Defaults to posthog-rs; a
    /// wrapper SDK sets its own. A blank name or version uses its default. If
    /// the name or version is not visible ASCII, the client uses both
    /// defaults.
    pub sdk_name: String,
    /// SDK version for events' `$lib_version` and for the same headers.
    /// Defaults to this crate's version. Like the name, it must be visible
    /// ASCII.
    pub sdk_version: String,
    pub(super) before_send: Vec<BeforeSendHook>,
    pub(super) on_error: Vec<OnErrorHook>,
}

impl Config {
    /// Create client-oriented defaults with a project token.
    pub fn new(project_token: impl Into<String>) -> Self {
        Self {
            project_token: project_token.into(),
            host: DEFAULT_HOST.into(),
            disabled: false,
            persistence: Persistence::Automatic,
            instance_name: "default".into(),
            bootstrap_anonymous_id: None,
            bootstrap_identified_id: None,
            person_profiles: PersonProfiles::IdentifiedOnly,
            flush_at: 20,
            max_batch_size: 100,
            flush_interval: Duration::from_secs(10),
            max_queue_size: 1_000,
            max_queue_bytes: 8 * 1024 * 1024,
            max_stored_bytes: 64 * 1024 * 1024,
            state_write_delay: Duration::from_millis(250),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            shutdown_timeout: Duration::from_secs(30),
            capture_compression: Some(CaptureCompression::Gzip),
            disable_geoip: false,
            platform_context: HashMap::new(),
            sdk_name: crate::client::SDK_USERAGENT_NAME.into(),
            sdk_version: crate::client::CRATE_VERSION.into(),
            before_send: Vec::new(),
            on_error: Vec::new(),
        }
    }

    /// Observe initialization, storage, and delivery failures. Hooks run in
    /// registration order, outside state locks; panics are caught. The callback
    /// may run concurrently on initialization and worker threads. Do not emit
    /// analytics from it, which can create a delivery-failure feedback loop.
    pub fn on_error<F>(&mut self, hook: F) -> &mut Self
    where
        F: Fn(&PostHogError<'_>) + Send + Sync + 'static,
    {
        self.on_error.push(OnErrorHook::new(hook));
        self
    }

    /// Run a native event hook synchronously before admission. Hooks run in
    /// registration order, after enrichment and outside client-state locks.
    /// Returning None or panicking drops the event. Each mutable hook is
    /// serialized across captures; do not wait for another capture from a hook.
    ///
    /// A hook that returns a new [`Event`] instead of the one it received should
    /// copy that event's options, such as `process_person_profile`; the new
    /// event does not inherit them. Change capture settings with
    /// [`Event::insert_option`], not with legacy `$` properties such as
    /// `$process_person_profile`.
    pub fn before_send<F>(&mut self, hook: F) -> &mut Self
    where
        F: FnMut(Event) -> Option<Event> + Send + 'static,
    {
        self.before_send.push(BeforeSendHook::new(hook));
        self
    }

    /// Trim and clamp settings as the other PostHog SDKs do, rather than
    /// disabling the client for them.
    pub(super) fn normalize(&mut self) {
        self.project_token = self.project_token.trim().into();
        self.sdk_name = self.sdk_name.trim().into();
        if self.sdk_name.is_empty() {
            self.sdk_name = crate::client::SDK_USERAGENT_NAME.into();
        }
        self.sdk_version = self.sdk_version.trim().into();
        if self.sdk_version.is_empty() {
            self.sdk_version = crate::client::CRATE_VERSION.into();
        }
        // Capture rejects header values that are not visible ASCII, which
        // `HeaderValue::from_str` would otherwise accept.
        let readable = reqwest::header::HeaderValue::from_str(&self.sdk_identity().user_agent())
            .is_ok_and(|value| value.to_str().is_ok());
        if !readable {
            tracing::warn!(
                "posthog-rs: sdk_name or sdk_version is not visible ASCII; using the defaults"
            );
            self.sdk_name = crate::client::SDK_USERAGENT_NAME.into();
            self.sdk_version = crate::client::CRATE_VERSION.into();
        }
        self.host = self.host.trim().into();
        if !valid_host(&self.host) {
            if !self.host.is_empty() {
                tracing::warn!("posthog-rs: invalid host; using {DEFAULT_HOST}");
            }
            self.host = DEFAULT_HOST.into();
        }
        self.host = EndpointManager::determine_server_host(&self.host);
        // A blank bootstrap ID is ignored, as in the other PostHog SDKs.
        for id in [
            &mut self.bootstrap_anonymous_id,
            &mut self.bootstrap_identified_id,
        ] {
            if id.as_deref().is_some_and(|id| id.trim().is_empty()) {
                *id = None;
            }
        }
        self.flush_at = self.flush_at.max(1);
        self.max_batch_size = self.max_batch_size.max(1);
        self.max_queue_size = self.max_queue_size.max(1);
        self.flush_interval = self
            .flush_interval
            .clamp(Duration::from_millis(1), MAX_SHUTDOWN_TIMEOUT);
        self.shutdown_timeout = self.shutdown_timeout.min(MAX_SHUTDOWN_TIMEOUT);
        if self.request_timeout.is_zero() {
            self.request_timeout = DEFAULT_REQUEST_TIMEOUT;
        }
        self.request_timeout = self.request_timeout.min(MAX_SHUTDOWN_TIMEOUT);
    }

    pub(super) fn invalid_reason(&self) -> Option<&'static str> {
        if self.project_token.is_empty() {
            return Some("project_token is empty; supply a project token to enable the client");
        }
        if self.project_token.starts_with("phx_") {
            return Some("project_token is a personal API key; supply a project token instead");
        }
        if reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.project_token))
            .is_err()
        {
            return Some("project_token contains invalid header characters");
        }
        if self.bootstrap_anonymous_id.is_some() && self.bootstrap_identified_id.is_some() {
            return Some("configure only one bootstrap identity");
        }
        None
    }

    /// The SDK the requests name in the `PostHog-Sdk-Info` and `User-Agent` headers.
    fn sdk_identity(&self) -> crate::client::SdkIdentity {
        crate::client::SdkIdentity {
            name: self.sdk_name.clone().into(),
            version: self.sdk_version.clone().into(),
        }
    }

    pub(super) fn request_timeout_seconds(&self) -> u64 {
        let seconds = self.request_timeout.as_secs();
        if self.request_timeout.subsec_nanos() == 0 {
            seconds
        } else {
            seconds + 1
        }
    }

    pub(super) fn event_limits(&self) -> StoreLimits {
        StoreLimits {
            max_events: self.max_queue_size,
            max_bytes: self.max_stored_bytes,
        }
    }

    pub(super) fn transport_options(&self) -> ClientOptions {
        // The new public configuration maps into the existing delivery engine.
        let mut options = ClientOptions::from(self.project_token.as_str());
        options.host = Some(self.host.clone());
        options.endpoint_manager = EndpointManager::new(self.host.clone());
        options.is_server = false;
        options.disable_geoip = self.disable_geoip;
        options.flush_at = self.flush_at;
        options.max_batch_size = self.max_batch_size;
        options.max_queue_size = self.max_queue_size;
        options.flush_interval_ms = self.flush_interval.as_millis() as u64;
        options.request_timeout_seconds = self.request_timeout_seconds();
        options.shutdown_timeout_ms = self.shutdown_timeout.as_millis() as u64;
        options.retry_initial_backoff_ms = RETRY_INITIAL_BACKOFF_MS;
        options.capture_compression = self.capture_compression;
        options.sdk_identity = self.sdk_identity();
        options.on_error = self.on_error.clone();
        options
    }
}

/// An HTTP(S) URL with a host and no credentials, query or fragment.
fn valid_host(host: &str) -> bool {
    reqwest::Url::parse(host).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}
