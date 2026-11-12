use std::collections::HashMap;
use std::time::Duration;

use super::storage::snapshot_capacity;
use super::{Persistence, PersonProfiles, SessionMode, SessionPersistence, Snapshot};
use crate::client::event_store::StoreLimits;
use crate::client::transport::MAX_SHUTDOWN_TIMEOUT;
use crate::client::{ClientOptions, OnErrorHook, PostHogError};
use crate::{BeforeSendHook, CaptureCompression, EndpointManager, Event, DEFAULT_HOST};

/// Configuration for a stateful client. Start with [`Config::new`].
///
/// Invalid configuration reports an initialization diagnostic and disables the
/// client. Storage failures instead preserve enabled, memory-backed operation.
pub struct Config {
    /// Project token, trimmed during initialization. Never use a personal API key.
    pub project_token: String,
    /// Ingestion host or reverse proxy. PostHog app hosts are normalized.
    pub host: String,
    /// Explicitly disable this client. Defaults to false.
    pub disabled: bool,
    /// Default collection choice when no explicit consent can be restored,
    /// including after reset or state-storage open/read failure. False by default.
    /// Set true when unavailable consent must require opt-in. A successfully
    /// restored explicit opt-in/out choice takes precedence.
    pub opt_out: bool,
    /// Storage selection. Defaults to the OS local application-data directory.
    pub persistence: Persistence,
    /// Session boundary ownership. Default: Automatic (30-minute idle, 24-hour max).
    pub session_mode: SessionMode,
    /// Session restart continuity. Default: Memory. Persistent uses the configured
    /// state store; it does not override Persistence::Memory.
    pub session_persistence: SessionPersistence,
    /// Isolates storage for instances using the same host and project. Default: "default".
    /// Each persistent namespace has one live owner; see [`Persistence`] for
    /// sharing a client or selecting independent identities.
    pub instance_name: String,
    /// Seed a fresh scope's anonymous identity. Persisted identity takes precedence.
    pub bootstrap_anonymous_id: Option<String>,
    /// Identified setup seed. Mutually exclusive with bootstrap_anonymous_id.
    /// Reconciles restored anonymous state; never replaces a different identified user.
    pub bootstrap_identified_id: Option<String>,
    /// Default person processing and identity/group-operation policy. Default: IdentifiedOnly.
    /// Explicit capture options and hooks retain their normal precedence.
    pub person_profiles: PersonProfiles,
    /// Preload current-user flags after the asynchronous project settings request.
    /// Default: true. A successful project response with hasFeatureFlags=false
    /// skips preload; unavailable settings fall back to loading flags. Readiness
    /// does not wait for either response. False suppresses only startup preload,
    /// not project settings or manual/identity/group/targeting-triggered refreshes.
    pub preload_feature_flags: bool,
    /// Report completed single-flag lookups through `$feature_flag_called` by
    /// default. Individual lookups can override this; consent still applies.
    pub send_feature_flag_event: bool,
    /// Optional initial flags snapshot, replacing restored values before preload.
    /// Only true and nonempty variants are served. None preserves restored values;
    /// Some(empty) installs an empty snapshot. Reset does not reapply this seed.
    /// Oversized snapshots are ignored and reported through `on_error`.
    pub bootstrap_feature_flags: Option<HashMap<String, crate::FlagValue>>,
    /// Already-decoded JSON payloads for enabled bootstrap flags only. Strings
    /// are not parsed again; JSON null is a present payload. Ignored without flags.
    pub bootstrap_feature_flag_payloads: Option<HashMap<String, serde_json::Value>>,
    /// Events that trigger an automatic flush. Default: 20.
    pub flush_at: usize,
    /// Maximum events per batch. Default: 100.
    pub max_batch_size: usize,
    /// Interval between automatic flushes. Default: 10 seconds.
    pub flush_interval: Duration,
    /// Maximum resident events, including buffered, in-flight and retry work. Default: 1,000.
    pub max_queue_size: usize,
    /// Maximum resident serialized event bytes, not allocator memory. Default: 8 MiB.
    pub max_queue_bytes: usize,
    /// Maximum persisted event records. Default: 10,000.
    pub max_stored_events: usize,
    /// Maximum persisted event bytes. Default: 64 MiB.
    pub max_stored_bytes: u64,
    /// Maximum persisted event record size. Default: 1 MiB.
    pub max_record_bytes: u64,
    /// Maximum mandatory state record size. Default: 1 MiB.
    pub max_state_bytes: usize,
    /// State-write coalescing window. Default: 250 milliseconds.
    pub state_coalesce: Duration,
    /// Timeout per delivery request, in whole seconds. Default: 30.
    pub request_timeout_seconds: u64,
    /// Default best-effort shutdown budget, including Drop. Explicit shutdown
    /// may override it. In-flight work and joins may exceed it. Default: 30 seconds.
    pub shutdown_timeout: Duration,
    /// Attempts per active delivery sequence, including the first. Default: 4.
    pub max_capture_attempts: u32,
    /// Request compression. Default: gzip; None disables compression.
    pub capture_compression: Option<CaptureCompression>,
    /// Disable server-side GeoIP enrichment. Default: false.
    pub disable_geoip: bool,
    /// Platform properties cached at initialization. Per-event and registered properties win.
    /// Missing $os/$os_version are discovered once during initialization.
    pub platform_context: HashMap<String, serde_json::Value>,
    /// Capture-time SDK name. Defaults to posthog-rs; embedding clients may override it.
    pub sdk_name: String,
    /// Capture-time SDK version. Defaults to this crate's version.
    pub sdk_version: String,
    pub(super) before_send: Vec<BeforeSendHook>,
    pub(super) on_error: Vec<OnErrorHook>,
    pub(super) on_feature_flags: Vec<super::flag_listeners::FlagsHook>,
}

impl Config {
    /// Create client-oriented defaults with a project token.
    pub fn new(project_token: impl Into<String>) -> Self {
        Self {
            project_token: project_token.into(),
            host: DEFAULT_HOST.into(),
            disabled: false,
            opt_out: false,
            persistence: Persistence::Automatic,
            session_mode: SessionMode::Automatic,
            session_persistence: SessionPersistence::Memory,
            instance_name: "default".into(),
            bootstrap_anonymous_id: None,
            bootstrap_identified_id: None,
            person_profiles: PersonProfiles::IdentifiedOnly,
            preload_feature_flags: true,
            send_feature_flag_event: true,
            bootstrap_feature_flags: None,
            bootstrap_feature_flag_payloads: None,
            flush_at: 20,
            max_batch_size: 100,
            flush_interval: Duration::from_secs(10),
            max_queue_size: 1_000,
            max_queue_bytes: 8 * 1024 * 1024,
            max_stored_events: 10_000,
            max_stored_bytes: 64 * 1024 * 1024,
            max_record_bytes: 1024 * 1024,
            max_state_bytes: 1024 * 1024,
            state_coalesce: Duration::from_millis(250),
            request_timeout_seconds: 30,
            shutdown_timeout: Duration::from_secs(30),
            max_capture_attempts: 4,
            capture_compression: Some(CaptureCompression::Gzip),
            disable_geoip: false,
            platform_context: HashMap::new(),
            sdk_name: crate::client::SDK_USERAGENT_NAME.into(),
            sdk_version: crate::client::CRATE_VERSION.into(),
            before_send: Vec::new(),
            on_error: Vec::new(),
            on_feature_flags: Vec::new(),
        }
    }

    /// Observe bootstrap/restored flags before initialization returns, and later
    /// remote completions. Runs in registration order outside SDK locks; panics
    /// are isolated. Remote calls run on the delivery worker, not a UI thread.
    /// The callback remains registered until this client's terminal shutdown.
    pub fn on_feature_flags<F>(&mut self, callback: F) -> &mut Self
    where
        F: Fn(&super::FeatureFlagsUpdate) + Send + Sync + 'static,
    {
        self.on_feature_flags.push(std::sync::Arc::new(callback));
        self
    }

    /// Observe initialization, storage, project settings, and delivery failures. Hooks run in
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
        if self.max_queue_bytes == 0 || self.max_stored_events == 0 || self.max_record_bytes == 0 {
            return Some("event byte and storage count limits must be positive");
        }
        if self.max_record_bytes > self.max_queue_bytes as u64
            || self.max_record_bytes > self.max_stored_bytes
        {
            return Some("max_record_bytes must fit max_queue_bytes and max_stored_bytes");
        }
        if self.flush_interval.as_millis() == 0
            || self.flush_interval > MAX_SHUTDOWN_TIMEOUT
            || self.state_coalesce > MAX_SHUTDOWN_TIMEOUT
            || self.shutdown_timeout > MAX_SHUTDOWN_TIMEOUT
        {
            return Some("flush_interval must be at least 1 ms; scheduling durations must not exceed one day");
        }
        if self.request_timeout_seconds == 0
            || self.request_timeout_seconds > MAX_SHUTDOWN_TIMEOUT.as_secs()
            || self.max_capture_attempts == 0
        {
            return Some(
                "request timeout must be 1..=86400 seconds and capture attempts must be positive",
            );
        }
        let generated = Snapshot::anonymous(uuid::Uuid::nil().to_string());
        if self.max_state_bytes < snapshot_capacity(&generated) {
            return Some("max_state_bytes must hold a generated anonymous identity record");
        }
        if self.bootstrap_anonymous_id.is_some() && self.bootstrap_identified_id.is_some() {
            return Some("configure only one bootstrap identity");
        }
        if let Some(id) = &self.bootstrap_identified_id {
            let mut snapshot = generated;
            snapshot.distinct_id = id.clone();
            snapshot.identified = true;
            snapshot.person_processing = true;
            if !snapshot.is_valid() || snapshot_capacity(&snapshot) > self.max_state_bytes {
                return Some("bootstrap_identified_id must be nonblank and fit max_state_bytes");
            }
        }
        if let Some(id) = &self.bootstrap_anonymous_id {
            let snapshot = Snapshot::anonymous(id.clone());
            if !snapshot.is_valid() || snapshot_capacity(&snapshot) > self.max_state_bytes {
                return Some("bootstrap_anonymous_id must be nonblank and fit max_state_bytes");
            }
        }
        None
    }

    pub(super) fn event_limits(&self) -> StoreLimits {
        StoreLimits {
            max_events: self.max_stored_events,
            max_bytes: self.max_stored_bytes,
            max_record_bytes: self.max_record_bytes,
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
        options.request_timeout_seconds = self.request_timeout_seconds;
        options.shutdown_timeout_ms = self.shutdown_timeout.as_millis() as u64;
        options.max_capture_attempts = self.max_capture_attempts;
        options.capture_compression = self.capture_compression;
        options.on_error = self.on_error.clone();
        options
    }
}
