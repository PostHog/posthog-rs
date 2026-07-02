use crate::client::{apply_on_error_hooks, get_default_user_agent, OnErrorHook};
use crate::feature_flags::{
    match_feature_flag, match_feature_flag_with_context, CohortDefinition, EvaluationContext,
    FeatureFlag, FlagValue, InconclusiveMatchError,
};
use crate::{Error, LocalEvaluationFailure, PostHogError};
use reqwest::header::{HeaderMap, ETAG, IF_NONE_MATCH, USER_AGENT};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::{debug, error, info, instrument, trace, warn};

/// Extract the ETag header value from a response's headers.
/// Returns None if the header is missing, invalid UTF-8, or empty.
fn extract_etag(headers: &HeaderMap) -> Option<String> {
    headers
        .get(ETAG)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Sleep up to `duration`, waking early when `stop_signal` is set. Returns
/// `true` if a stop was requested (either already pending or observed while
/// waiting). Polling in short steps keeps shutdown latency bounded even when
/// the poll interval is large — a plain `sleep(poll_interval)` would make
/// `stop`/`Drop` block for the remainder of the current interval.
fn sleep_until_stop(stop_signal: &AtomicBool, duration: Duration) -> bool {
    const STEP: Duration = Duration::from_millis(200);
    let mut remaining = duration;
    while !remaining.is_zero() {
        if stop_signal.load(Ordering::Relaxed) {
            return true;
        }
        let step = remaining.min(STEP);
        std::thread::sleep(step);
        remaining -= step;
    }
    stop_signal.load(Ordering::Relaxed)
}

/// Fire the `on_error` hooks for a failed definitions poll. The personal API
/// key is never included — only the cause and HTTP status are surfaced.
fn report_local_eval_error(hooks: &[OnErrorHook], status: Option<u16>, error: &Error) {
    if hooks.is_empty() {
        return;
    }
    let failure = PostHogError::LocalEvaluation(LocalEvaluationFailure { error, status });
    apply_on_error_hooks(hooks, &failure);
}

/// Response from the PostHog local evaluation API.
///
/// Contains feature flag definitions, group type mappings, and cohort definitions
/// that can be cached locally for flag evaluation without server round-trips.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalEvaluationResponse {
    /// List of feature flag definitions
    pub flags: Vec<FeatureFlag>,
    /// Mapping from group type keys to their display names
    #[serde(default)]
    pub group_type_mapping: HashMap<String, String>,
    /// Cohort definitions for evaluating cohort membership
    #[serde(default)]
    pub cohorts: HashMap<String, Cohort>,
}

/// A cohort definition for local evaluation.
///
/// Cohorts are groups of users defined by property filters, used for
/// targeting feature flags to specific user segments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cohort {
    /// Unique identifier for this cohort
    pub id: String,
    /// Human-readable name of the cohort
    pub name: String,
    /// Property filters that define cohort membership
    pub properties: serde_json::Value,
}

/// Thread-safe cache for feature flag definitions.
///
/// Stores feature flags, group type mappings, and cohort definitions that have
/// been fetched from the PostHog API. The cache is shared between the poller
/// (which updates it) and the evaluator (which reads from it).
#[derive(Clone)]
pub struct FlagCache {
    flags: Arc<RwLock<HashMap<String, FeatureFlag>>>,
    group_type_mapping: Arc<RwLock<HashMap<String, String>>>,
    cohorts: Arc<RwLock<HashMap<String, Cohort>>>,
}

impl Default for FlagCache {
    fn default() -> Self {
        Self::new()
    }
}

impl FlagCache {
    /// Create an empty shared flag cache.
    pub fn new() -> Self {
        Self {
            flags: Arc::new(RwLock::new(HashMap::new())),
            group_type_mapping: Arc::new(RwLock::new(HashMap::new())),
            cohorts: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Replace cached flags, group type mappings, and cohorts from a local
    /// evaluation API response.
    pub fn update(&self, response: LocalEvaluationResponse) {
        let flag_count = response.flags.len();
        let mut flags = self.flags.write().unwrap();
        flags.clear();
        for flag in response.flags {
            flags.insert(flag.key.clone(), flag);
        }

        let mut mapping = self.group_type_mapping.write().unwrap();
        *mapping = response.group_type_mapping;

        let mut cohorts = self.cohorts.write().unwrap();
        *cohorts = response.cohorts;

        debug!(flag_count, "Updated flag cache");
    }

    /// Return a cached feature flag by key.
    pub fn get_flag(&self, key: &str) -> Option<FeatureFlag> {
        self.flags.read().unwrap().get(key).cloned()
    }

    /// Return all cached feature flag definitions.
    pub fn get_all_flags(&self) -> Vec<FeatureFlag> {
        self.flags.read().unwrap().values().cloned().collect()
    }

    /// Return a cached cohort by ID.
    pub fn get_cohort(&self, id: &str) -> Option<Cohort> {
        self.cohorts.read().unwrap().get(id).cloned()
    }

    /// Return all cached cohorts, keyed by cohort ID.
    pub fn get_all_cohorts(&self) -> HashMap<String, Cohort> {
        self.cohorts.read().unwrap().clone()
    }

    /// Get all cohorts as CohortDefinitions for evaluation context
    pub fn get_cohort_definitions(&self) -> HashMap<String, CohortDefinition> {
        self.cohorts
            .read()
            .unwrap()
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    CohortDefinition {
                        id: v.id.clone(),
                        properties: v.properties.clone(),
                    },
                )
            })
            .collect()
    }

    /// Get all flags as a HashMap for evaluation context
    pub fn get_flags_map(&self) -> HashMap<String, FeatureFlag> {
        self.flags.read().unwrap().clone()
    }

    /// Get the group type mapping (group type index → group type name).
    pub fn get_group_type_mapping(&self) -> HashMap<String, String> {
        self.group_type_mapping.read().unwrap().clone()
    }

    /// Remove all cached flags, group type mappings, and cohorts.
    pub fn clear(&self) {
        self.flags.write().unwrap().clear();
        self.group_type_mapping.write().unwrap().clear();
        self.cohorts.write().unwrap().clear();
    }
}

/// Configuration for local flag evaluation.
///
/// Specifies the credentials and settings needed to fetch feature flag
/// definitions from the PostHog API for local evaluation.
#[derive(Clone)]
pub struct LocalEvaluationConfig {
    /// Personal API key for authentication (found in PostHog project settings)
    pub personal_api_key: String,
    /// Project API key to identify which project's flags to fetch
    pub project_api_key: String,
    /// PostHog API host URL (for example, `https://us.i.posthog.com`).
    /// Use `https://eu.i.posthog.com` for EU-hosted projects.
    pub api_host: String,
    /// How often to poll for updated flag definitions
    pub poll_interval: Duration,
    /// Timeout for API requests
    pub request_timeout: Duration,
}

/// Synchronous poller for feature flag definitions.
///
/// Runs a background thread that periodically fetches flag definitions from
/// the PostHog API and updates the shared cache. Use this for blocking/sync
/// applications. With the `async-client` feature enabled, use
/// [`AsyncFlagPoller`] for async applications instead.
pub struct FlagPoller {
    config: LocalEvaluationConfig,
    cache: FlagCache,
    client: reqwest::blocking::Client,
    stop_signal: Arc<AtomicBool>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
    /// Observability hooks, injected by the client builder before `start`.
    /// Kept here rather than on `LocalEvaluationConfig` so the public config
    /// struct stays unchanged.
    on_error: Vec<OnErrorHook>,
}

impl FlagPoller {
    /// Create a synchronous flag definition poller.
    ///
    /// # Parameters
    ///
    /// - `config`: Credentials, host, polling interval, and request timeout.
    /// - `cache`: Shared cache updated by the poller.
    pub fn new(config: LocalEvaluationConfig, cache: FlagCache) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(config.request_timeout)
            .build()
            .unwrap();

        Self {
            config,
            cache,
            client,
            stop_signal: Arc::new(AtomicBool::new(false)),
            thread_handle: None,
            on_error: Vec::new(),
        }
    }

    /// Register `on_error` hooks. Called by the client builder before
    /// [`FlagPoller::start`]; not part of the public flag-poller API.
    // Only the blocking client (built when `async-client` is off) injects hooks.
    #[cfg_attr(feature = "async-client", allow(dead_code))]
    pub(crate) fn set_on_error(&mut self, hooks: Vec<OnErrorHook>) {
        self.on_error = hooks;
    }

    /// Start the polling thread.
    ///
    /// Performs an initial synchronous load, then refreshes definitions in the
    /// background until [`FlagPoller::stop`] is called or the poller is dropped.
    pub fn start(&mut self) {
        info!(
            poll_interval_secs = self.config.poll_interval.as_secs(),
            "Starting feature flag poller"
        );

        // Initial load
        match self.load_flags() {
            Ok(()) => info!("Initial flag definitions loaded successfully"),
            Err(e) => warn!(error = %e, "Failed to load initial flags, will retry on next poll"),
        }

        let config = self.config.clone();
        let cache = self.cache.clone();
        let stop_signal = self.stop_signal.clone();
        let on_error = self.on_error.clone();

        let handle = std::thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(config.request_timeout)
                .build()
                .unwrap();

            let mut last_etag: Option<String> = None;

            loop {
                if sleep_until_stop(&stop_signal, config.poll_interval) {
                    debug!("Flag poller received stop signal");
                    break;
                }

                let url = format!(
                    "{}/flags/definitions/?send_cohorts",
                    config.api_host.trim_end_matches('/')
                );

                let mut request = client
                    .get(&url)
                    .header(
                        "Authorization",
                        format!("Bearer {}", config.personal_api_key),
                    )
                    .header("X-PostHog-Project-Api-Key", &config.project_api_key)
                    .header(USER_AGENT, get_default_user_agent());

                if let Some(ref etag) = last_etag {
                    request = request.header(IF_NONE_MATCH, etag.as_str());
                }

                match request.send() {
                    Ok(response) => {
                        let status = response.status();
                        if status == StatusCode::NOT_MODIFIED {
                            debug!("Flag definitions unchanged (304 Not Modified)");
                        } else if status.is_success() {
                            // Extract ETag before consuming the response body
                            let new_etag = extract_etag(response.headers());

                            match response.json::<LocalEvaluationResponse>() {
                                Ok(data) => {
                                    trace!("Successfully fetched flag definitions");
                                    cache.update(data);
                                    last_etag = new_etag;
                                }
                                Err(e) => {
                                    warn!(error = %e, "Failed to parse flag response");
                                    let err = Error::Serialization(e.to_string());
                                    report_local_eval_error(&on_error, Some(status.as_u16()), &err);
                                }
                            }
                        } else {
                            warn!(status = %status, "Failed to fetch flags");
                            let err = Error::Connection(format!("HTTP {}", status));
                            report_local_eval_error(&on_error, Some(status.as_u16()), &err);
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to fetch flags");
                        let err = Error::Connection(e.to_string());
                        report_local_eval_error(&on_error, None, &err);
                    }
                }
            }
        });

        self.thread_handle = Some(handle);
    }

    /// Load flags synchronously and update the cache once.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Connection`] for request failures or non-success HTTP
    /// statuses, and [`Error::Serialization`] when the response cannot be
    /// parsed.
    #[instrument(skip(self), level = "debug")]
    pub fn load_flags(&self) -> Result<(), Error> {
        let url = format!(
            "{}/flags/definitions/?send_cohorts",
            self.config.api_host.trim_end_matches('/')
        );

        let response = match self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.personal_api_key),
            )
            .header("X-PostHog-Project-Api-Key", &self.config.project_api_key)
            .header(USER_AGENT, get_default_user_agent())
            .send()
        {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, "Connection error loading flags");
                let err = Error::Connection(e.to_string());
                report_local_eval_error(&self.on_error, None, &err);
                return Err(err);
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            error!(status = %status, "HTTP error loading flags");
            let err = Error::Connection(format!("HTTP {}", status));
            report_local_eval_error(&self.on_error, Some(status.as_u16()), &err);
            return Err(err);
        }

        let status = response.status().as_u16();
        let data = match response.json::<LocalEvaluationResponse>() {
            Ok(d) => d,
            Err(e) => {
                error!(error = %e, "Failed to parse flag response");
                let err = Error::Serialization(e.to_string());
                report_local_eval_error(&self.on_error, Some(status), &err);
                return Err(err);
            }
        };

        self.cache.update(data);
        Ok(())
    }

    /// Stop the polling thread and wait for it to exit.
    pub fn stop(&mut self) {
        debug!("Stopping flag poller");
        self.stop_signal.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread_handle.take() {
            handle.join().ok();
        }
    }
}

impl Drop for FlagPoller {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Asynchronous poller for feature flag definitions.
///
/// Runs a tokio task that periodically fetches flag definitions from the
/// PostHog API and updates the shared cache. Use this for async applications.
/// For blocking/sync applications, use [`FlagPoller`] instead.
#[cfg(feature = "async-client")]
pub struct AsyncFlagPoller {
    config: LocalEvaluationConfig,
    cache: FlagCache,
    client: reqwest::Client,
    stop_signal: Arc<AtomicBool>,
    task_handle: Option<tokio::task::JoinHandle<()>>,
    is_running: Arc<tokio::sync::RwLock<bool>>,
    /// Observability hooks, injected by the client builder before `start`.
    /// Kept here rather than on `LocalEvaluationConfig` so the public config
    /// struct stays unchanged.
    on_error: Vec<OnErrorHook>,
}

#[cfg(feature = "async-client")]
impl AsyncFlagPoller {
    /// Create an asynchronous flag definition poller.
    ///
    /// # Parameters
    ///
    /// - `config`: Credentials, host, polling interval, and request timeout.
    /// - `cache`: Shared cache updated by the poller.
    pub fn new(config: LocalEvaluationConfig, cache: FlagCache) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .build()
            .unwrap();

        Self {
            config,
            cache,
            client,
            stop_signal: Arc::new(AtomicBool::new(false)),
            task_handle: None,
            is_running: Arc::new(tokio::sync::RwLock::new(false)),
            on_error: Vec::new(),
        }
    }

    /// Register `on_error` hooks. Called by the client builder before
    /// [`AsyncFlagPoller::start`]; not part of the public flag-poller API.
    pub(crate) fn set_on_error(&mut self, hooks: Vec<OnErrorHook>) {
        self.on_error = hooks;
    }

    /// Start the polling task.
    ///
    /// Performs an initial async load, then refreshes definitions in the
    /// background until [`AsyncFlagPoller::stop`] is called or the poller is
    /// dropped.
    pub async fn start(&mut self) {
        // Check if already running
        {
            let mut is_running = self.is_running.write().await;
            if *is_running {
                debug!("Flag poller already running, skipping start");
                return;
            }
            *is_running = true;
        }

        info!(
            poll_interval_secs = self.config.poll_interval.as_secs(),
            "Starting async feature flag poller"
        );

        // Initial load
        match self.load_flags().await {
            Ok(()) => info!("Initial flag definitions loaded successfully"),
            Err(e) => warn!(error = %e, "Failed to load initial flags, will retry on next poll"),
        }

        let config = self.config.clone();
        let cache = self.cache.clone();
        let stop_signal = self.stop_signal.clone();
        let is_running = self.is_running.clone();
        let client = self.client.clone();
        let on_error = self.on_error.clone();

        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.poll_interval);
            interval.tick().await; // Skip the first immediate tick

            let mut last_etag: Option<String> = None;

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if stop_signal.load(Ordering::Relaxed) {
                            debug!("Async flag poller received stop signal");
                            break;
                        }

                        let url = format!(
                            "{}/flags/definitions/?send_cohorts",
                            config.api_host.trim_end_matches('/')
                        );

                        let mut request = client
                            .get(&url)
                            .header("Authorization", format!("Bearer {}", config.personal_api_key))
                            .header("X-PostHog-Project-Api-Key", &config.project_api_key)
                            .header(USER_AGENT, get_default_user_agent());

                        if let Some(ref etag) = last_etag {
                            request = request.header(IF_NONE_MATCH, etag.as_str());
                        }

                        match request.send().await {
                            Ok(response) => {
                                let status = response.status();
                                if status == StatusCode::NOT_MODIFIED {
                                    debug!("Flag definitions unchanged (304 Not Modified)");
                                } else if status.is_success() {
                                    // Extract ETag before consuming the response body
                                    let new_etag = extract_etag(response.headers());

                                    match response.json::<LocalEvaluationResponse>().await {
                                        Ok(data) => {
                                            trace!("Successfully fetched flag definitions");
                                            cache.update(data);
                                            last_etag = new_etag;
                                        }
                                        Err(e) => {
                                            warn!(error = %e, "Failed to parse flag response");
                                            let err = Error::Serialization(e.to_string());
                                            report_local_eval_error(&on_error, Some(status.as_u16()), &err);
                                        }
                                    }
                                } else {
                                    warn!(status = %status, "Failed to fetch flags");
                                    let err = Error::Connection(format!("HTTP {}", status));
                                    report_local_eval_error(&on_error, Some(status.as_u16()), &err);
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to fetch flags");
                                let err = Error::Connection(e.to_string());
                                report_local_eval_error(&on_error, None, &err);
                            }
                        }
                    }
                }
            }

            // Clear running flag when task exits
            *is_running.write().await = false;
        });

        self.task_handle = Some(task);
    }

    /// Load flags asynchronously and update the cache once.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Connection`] for request failures or non-success HTTP
    /// statuses, and [`Error::Serialization`] when the response cannot be
    /// parsed.
    #[instrument(skip(self), level = "debug")]
    pub async fn load_flags(&self) -> Result<(), Error> {
        let url = format!(
            "{}/flags/definitions/?send_cohorts",
            self.config.api_host.trim_end_matches('/')
        );

        let response = match self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.personal_api_key),
            )
            .header("X-PostHog-Project-Api-Key", &self.config.project_api_key)
            .header(USER_AGENT, get_default_user_agent())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, "Connection error loading flags");
                let err = Error::Connection(e.to_string());
                report_local_eval_error(&self.on_error, None, &err);
                return Err(err);
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            error!(status = %status, "HTTP error loading flags");
            let err = Error::Connection(format!("HTTP {}", status));
            report_local_eval_error(&self.on_error, Some(status.as_u16()), &err);
            return Err(err);
        }

        let status = response.status().as_u16();
        let data = match response.json::<LocalEvaluationResponse>().await {
            Ok(d) => d,
            Err(e) => {
                error!(error = %e, "Failed to parse flag response");
                let err = Error::Serialization(e.to_string());
                report_local_eval_error(&self.on_error, Some(status), &err);
                return Err(err);
            }
        };

        self.cache.update(data);
        Ok(())
    }

    /// Stop the polling task.
    pub async fn stop(&mut self) {
        debug!("Stopping async flag poller");
        self.stop_signal.store(true, Ordering::Relaxed);
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
        }
        *self.is_running.write().await = false;
    }

    /// Check if the poller currently has a running background task.
    pub async fn is_running(&self) -> bool {
        *self.is_running.read().await
    }
}

#[cfg(feature = "async-client")]
impl Drop for AsyncFlagPoller {
    fn drop(&mut self) {
        // Abort the task if still running
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
        }
    }
}

/// Evaluates feature flags using locally cached definitions.
///
/// The evaluator reads from a [`FlagCache`] to determine flag values without
/// making network requests. Supports cohort membership checks and flag
/// dependencies through the evaluation context.
#[derive(Clone)]
pub struct LocalEvaluator {
    cache: FlagCache,
}

impl LocalEvaluator {
    /// Create an evaluator backed by a shared [`FlagCache`].
    pub fn new(cache: FlagCache) -> Self {
        Self { cache }
    }

    /// Access the underlying flag cache (e.g. to read group type mappings).
    pub fn cache(&self) -> &FlagCache {
        &self.cache
    }

    /// Evaluate a feature flag locally with full context support.
    ///
    /// Supports cohort membership checks, flag dependency evaluation, and
    /// group / mixed-targeting flags. `groups` and `group_properties` are
    /// only consulted when the flag (or one of its conditions) targets a
    /// group via `aggregation_group_type_index`; pass empty maps for
    /// person-targeted flags.
    ///
    /// # Returns
    ///
    /// `Ok(Some(value))` when the flag is present and evaluated,
    /// `Ok(None)` when the flag is absent from the cache.
    ///
    /// # Errors
    ///
    /// Returns [`InconclusiveMatchError`] when required properties, cohorts, or
    /// dependent flags are unavailable locally.
    #[instrument(
        skip(self, person_properties, groups, group_properties),
        level = "trace"
    )]
    pub fn evaluate_flag(
        &self,
        key: &str,
        distinct_id: &str,
        person_properties: &HashMap<String, serde_json::Value>,
        groups: &HashMap<String, String>,
        group_properties: &HashMap<String, HashMap<String, serde_json::Value>>,
    ) -> Result<Option<FlagValue>, InconclusiveMatchError> {
        match self.cache.get_flag(key) {
            Some(flag) => {
                // Build evaluation context with cohorts, flags, and group info
                let cohorts = self.cache.get_cohort_definitions();
                let flags = self.cache.get_flags_map();
                let group_type_mapping = self.cache.get_group_type_mapping();

                let ctx = EvaluationContext {
                    cohorts: &cohorts,
                    flags: &flags,
                    distinct_id,
                    groups,
                    group_properties,
                    group_type_mapping: &group_type_mapping,
                };

                let result = match_feature_flag_with_context(&flag, person_properties, &ctx);
                trace!(key, ?result, "Local flag evaluation");
                result.map(Some)
            }
            None => {
                trace!(key, "Flag not found in local cache");
                Ok(None)
            }
        }
    }

    /// Evaluate a feature flag locally without cohort or flag dependency
    /// support.
    ///
    /// Use this when you know the flag doesn't have cohort or flag dependency
    /// conditions.
    ///
    /// # Returns
    ///
    /// `Ok(Some(value))` when the flag is present and evaluated,
    /// `Ok(None)` when the flag is absent from the cache.
    ///
    /// # Errors
    ///
    /// Returns [`InconclusiveMatchError`] when required properties are
    /// unavailable locally.
    #[instrument(
        skip(self, person_properties, groups, group_properties),
        level = "trace"
    )]
    pub fn evaluate_flag_simple(
        &self,
        key: &str,
        distinct_id: &str,
        person_properties: &HashMap<String, serde_json::Value>,
        groups: &HashMap<String, String>,
        group_properties: &HashMap<String, HashMap<String, serde_json::Value>>,
    ) -> Result<Option<FlagValue>, InconclusiveMatchError> {
        match self.cache.get_flag(key) {
            Some(flag) => {
                let group_type_mapping = self.cache.get_group_type_mapping();
                let result = match_feature_flag(
                    &flag,
                    distinct_id,
                    person_properties,
                    groups,
                    group_properties,
                    &group_type_mapping,
                );
                trace!(key, ?result, "Local flag evaluation (simple)");
                result.map(Some)
            }
            None => {
                trace!(key, "Flag not found in local cache");
                Ok(None)
            }
        }
    }

    /// Get all flags and evaluate them with full context support.
    ///
    /// The returned map is keyed by feature flag key. Each value can be an
    /// inconclusive error if that particular flag could not be evaluated from
    /// the supplied context.
    #[instrument(
        skip(self, person_properties, groups, group_properties),
        level = "debug"
    )]
    pub fn evaluate_all_flags(
        &self,
        distinct_id: &str,
        person_properties: &HashMap<String, serde_json::Value>,
        groups: &HashMap<String, String>,
        group_properties: &HashMap<String, HashMap<String, serde_json::Value>>,
    ) -> HashMap<String, Result<FlagValue, InconclusiveMatchError>> {
        let mut results = HashMap::new();

        // Build evaluation context once for all flags
        let cohorts = self.cache.get_cohort_definitions();
        let flags = self.cache.get_flags_map();
        let group_type_mapping = self.cache.get_group_type_mapping();

        let ctx = EvaluationContext {
            cohorts: &cohorts,
            flags: &flags,
            distinct_id,
            groups,
            group_properties,
            group_type_mapping: &group_type_mapping,
        };

        for flag in self.cache.get_all_flags() {
            let result = match_feature_flag_with_context(&flag, person_properties, &ctx);
            results.insert(flag.key.clone(), result);
        }

        debug!(flag_count = results.len(), "Evaluated all local flags");
        results
    }
}
