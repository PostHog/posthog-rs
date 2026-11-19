use std::collections::HashMap;
use std::time::Duration;

use super::{Persistence, Snapshot};
use crate::client::event_store::StoreLimits;
use crate::client::transport::MAX_SHUTDOWN_TIMEOUT;
use crate::client::{ClientOptions, OnErrorHook, PostHogError};
use crate::{BeforeSendHook, CaptureCompression, EndpointManager, Event, DEFAULT_HOST};

/// Largest serialized event the client accepts, roughly the capture endpoint's
/// own per-event limit. Larger events are refused at capture with
/// `CaptureOutcome::Full`.
const MAX_EVENT_BYTES: u64 = 1024 * 1024;

/// A client SDK waits 1 second before its first resend, doubling to the
/// 30-second ceiling, where a server SDK starts at 100 ms.
const RETRY_INITIAL_BACKOFF_MS: u64 = 1_000;

/// Configuration for a stateful client. Start with [`Config::new`].
///
/// Invalid configuration reports an initialization diagnostic and disables the
/// client. Storage failures instead preserve enabled, memory-backed operation.
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
    /// Events that trigger an automatic flush. Default: 20.
    pub flush_at: usize,
    /// Maximum events per batch. Default: 100.
    pub max_batch_size: usize,
    /// Interval between automatic flushes. Default: 10 seconds.
    pub flush_interval: Duration,
    /// Maximum events held in memory, waiting for delivery or for a disk write.
    /// Default: 1,000.
    ///
    /// At this limit or `max_queue_bytes`, the oldest events in memory are
    /// dropped to keep the newest. Capture still returns
    /// [`CaptureOutcome::Accepted`](super::CaptureOutcome::Accepted).
    pub max_queue_size: usize,
    /// Maximum serialized event bytes held in memory. Default: 8 MiB. This
    /// does not cap the process's memory use.
    ///
    /// At this limit, the oldest events in memory are dropped to keep the
    /// newest. Capture returns [`CaptureOutcome::Full`](super::CaptureOutcome::Full)
    /// for any single event over 1 MiB, or over this limit if it is smaller.
    pub max_queue_bytes: usize,
    /// Maximum events stored on disk. Default: 10,000.
    ///
    /// At this limit or `max_stored_bytes`, the oldest stored events are dropped
    /// to keep the newest. Files the client cannot delete keep counting against
    /// these limits; while they fill them, new events stay in memory under the
    /// memory limits.
    pub max_stored_events: usize,
    /// Maximum serialized event bytes stored on disk. Default: 64 MiB.
    ///
    /// At this limit or `max_stored_events`, the oldest stored events are
    /// dropped to keep the newest. Files the client cannot delete keep counting
    /// against these limits; while they fill them, new events stay in memory
    /// under the memory limits.
    pub max_stored_bytes: u64,
    /// How long to wait after a state change before writing client state to
    /// disk. Changes made within the window are written together. Lower values
    /// lose less on a crash; higher values write less often. Default: 250 ms.
    pub state_write_delay: Duration,
    /// Timeout per network request. Default: 30 seconds. Sub-second precision is
    /// rounded up to whole seconds.
    pub request_timeout: Duration,
    /// Default SDK-controlled shutdown/Drop waiting budget. Default: 30 seconds.
    /// The first shutdown selects one deadline; later calls and Drop cannot
    /// extend it. Already-started work owns its resources until completion and
    /// may retain them indefinitely if it never returns. Arbitrary destructors
    /// of application objects captured by callbacks may block outside this bound.
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
    /// wrapper SDK sets its own.
    pub sdk_name: String,
    /// SDK version for events' `$lib_version` and for the same headers.
    /// Defaults to this crate's version.
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
            flush_at: 20,
            max_batch_size: 100,
            flush_interval: Duration::from_secs(10),
            max_queue_size: 1_000,
            max_queue_bytes: 8 * 1024 * 1024,
            max_stored_events: 10_000,
            max_stored_bytes: 64 * 1024 * 1024,
            state_write_delay: Duration::from_millis(250),
            request_timeout: Duration::from_secs(30),
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

    pub(super) fn normalize(&mut self) {
        self.project_token = self.project_token.trim().into();
        self.sdk_name = self.sdk_name.trim().into();
        self.sdk_version = self.sdk_version.trim().into();
        self.host = self.host.trim().into();
        if self.host.is_empty() {
            self.host = DEFAULT_HOST.into();
        }
        self.host = EndpointManager::determine_server_host(&self.host);
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
        if !reqwest::Url::parse(&self.host).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
        }) {
            return Some("host must be an HTTP(S) URL without credentials, query or fragment");
        }
        if self.sdk_name.is_empty()
            || self.sdk_version.is_empty()
            || reqwest::header::HeaderValue::from_str(&format!(
                "{}/{}",
                self.sdk_name, self.sdk_version
            ))
            .is_err()
        {
            return Some("SDK name and version must be nonblank and valid header text");
        }
        if self.flush_at == 0 || self.max_batch_size == 0 || self.max_queue_size == 0 {
            return Some("flush_at, max_batch_size and max_queue_size must be positive");
        }
        if self.max_queue_bytes == 0 || self.max_stored_events == 0 || self.max_stored_bytes == 0 {
            return Some("event byte and storage count limits must be positive");
        }
        if self.flush_interval.as_millis() == 0
            || self.flush_interval > MAX_SHUTDOWN_TIMEOUT
            || self.state_write_delay > MAX_SHUTDOWN_TIMEOUT
            || self.shutdown_timeout > MAX_SHUTDOWN_TIMEOUT
        {
            return Some("flush_interval must be at least 1 ms; scheduling durations must not exceed one day");
        }
        if self.request_timeout.is_zero() || self.request_timeout > MAX_SHUTDOWN_TIMEOUT {
            return Some("request_timeout must be positive and at most one day");
        }
        if let Some(id) = &self.bootstrap_anonymous_id {
            let snapshot = Snapshot::anonymous(id.clone());
            if !snapshot.is_valid() {
                return Some("bootstrap_anonymous_id must be nonblank");
            }
        }
        None
    }

    pub(super) fn request_timeout_seconds(&self) -> u64 {
        let seconds = self.request_timeout.as_secs();
        if self.request_timeout.subsec_nanos() == 0 {
            seconds
        } else {
            seconds + 1
        }
    }

    /// Largest serialized event capture admits, never larger than the queue.
    pub(super) fn max_event_bytes(&self) -> usize {
        (MAX_EVENT_BYTES as usize).min(self.max_queue_bytes)
    }

    pub(super) fn event_limits(&self) -> StoreLimits {
        StoreLimits {
            max_events: self.max_stored_events,
            max_bytes: self.max_stored_bytes,
            // Never larger than either budget that has to hold the event.
            max_record_bytes: (self.max_event_bytes() as u64).min(self.max_stored_bytes),
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
        options.sdk_info = Some(format!("{}/{}", self.sdk_name, self.sdk_version));
        options.on_error = self.on_error.clone();
        options
    }
}
