use crate::feature_flags::{match_feature_flag, FeatureFlag, FlagValue, InconclusiveMatchError};
use crate::Error;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::{debug, error, info, instrument, trace, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalEvaluationResponse {
    pub flags: Vec<FeatureFlag>,
    #[serde(default)]
    pub group_type_mapping: HashMap<String, String>,
    #[serde(default)]
    pub cohorts: HashMap<String, Cohort>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cohort {
    pub id: String,
    pub name: String,
    pub properties: serde_json::Value,
}

/// Manages locally cached feature flags for evaluation
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
    pub fn new() -> Self {
        Self {
            flags: Arc::new(RwLock::new(HashMap::new())),
            group_type_mapping: Arc::new(RwLock::new(HashMap::new())),
            cohorts: Arc::new(RwLock::new(HashMap::new())),
        }
    }

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

    pub fn get_flag(&self, key: &str) -> Option<FeatureFlag> {
        self.flags.read().unwrap().get(key).cloned()
    }

    pub fn get_all_flags(&self) -> Vec<FeatureFlag> {
        self.flags.read().unwrap().values().cloned().collect()
    }

    pub fn clear(&self) {
        self.flags.write().unwrap().clear();
        self.group_type_mapping.write().unwrap().clear();
        self.cohorts.write().unwrap().clear();
    }
}

/// Configuration for local evaluation
#[derive(Clone)]
pub struct LocalEvaluationConfig {
    pub personal_api_key: String,
    pub project_api_key: String,
    pub api_host: String,
    pub poll_interval: Duration,
    pub request_timeout: Duration,
}

/// Manages polling for feature flag definitions
pub struct FlagPoller {
    config: LocalEvaluationConfig,
    cache: FlagCache,
    client: reqwest::blocking::Client,
    stop_signal: Arc<AtomicBool>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl FlagPoller {
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
        }
    }

    /// Start the polling thread
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

        let handle = std::thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(config.request_timeout)
                .build()
                .unwrap();

            loop {
                std::thread::sleep(config.poll_interval);

                if stop_signal.load(Ordering::Relaxed) {
                    debug!("Flag poller received stop signal");
                    break;
                }

                let url = format!(
                    "{}/api/feature_flag/local_evaluation/?send_cohorts",
                    config.api_host.trim_end_matches('/')
                );

                match client
                    .get(&url)
                    .header(
                        "Authorization",
                        format!("Bearer {}", config.personal_api_key),
                    )
                    .header("X-PostHog-Project-Api-Key", &config.project_api_key)
                    .send()
                {
                    Ok(response) => {
                        if response.status().is_success() {
                            match response.json::<LocalEvaluationResponse>() {
                                Ok(data) => {
                                    trace!("Successfully fetched flag definitions");
                                    cache.update(data);
                                }
                                Err(e) => {
                                    warn!(error = %e, "Failed to parse flag response");
                                }
                            }
                        } else {
                            warn!(status = %response.status(), "Failed to fetch flags");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to fetch flags");
                    }
                }
            }
        });

        self.thread_handle = Some(handle);
    }

    /// Load flags synchronously
    #[instrument(skip(self), level = "debug")]
    pub fn load_flags(&self) -> Result<(), Error> {
        let url = format!(
            "{}/api/feature_flag/local_evaluation/?send_cohorts",
            self.config.api_host.trim_end_matches('/')
        );

        let response = self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.personal_api_key),
            )
            .header("X-PostHog-Project-Api-Key", &self.config.project_api_key)
            .send()
            .map_err(|e| {
                error!(error = %e, "Connection error loading flags");
                Error::Connection(e.to_string())
            })?;

        if !response.status().is_success() {
            let status = response.status();
            error!(status = %status, "HTTP error loading flags");
            return Err(Error::Connection(format!("HTTP {}", status)));
        }

        let data = response.json::<LocalEvaluationResponse>().map_err(|e| {
            error!(error = %e, "Failed to parse flag response");
            Error::Serialization(e.to_string())
        })?;

        self.cache.update(data);
        Ok(())
    }

    /// Stop the polling thread
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

/// Async version of the flag poller
#[cfg(feature = "async-client")]
pub struct AsyncFlagPoller {
    config: LocalEvaluationConfig,
    cache: FlagCache,
    client: reqwest::Client,
    stop_signal: Arc<AtomicBool>,
    task_handle: Option<tokio::task::JoinHandle<()>>,
    is_running: Arc<tokio::sync::RwLock<bool>>,
}

#[cfg(feature = "async-client")]
impl AsyncFlagPoller {
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
        }
    }

    /// Start the polling task
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

        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.poll_interval);
            interval.tick().await; // Skip the first immediate tick

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if stop_signal.load(Ordering::Relaxed) {
                            debug!("Async flag poller received stop signal");
                            break;
                        }

                        let url = format!(
                            "{}/api/feature_flag/local_evaluation/?send_cohorts",
                            config.api_host.trim_end_matches('/')
                        );

                        match client
                            .get(&url)
                            .header("Authorization", format!("Bearer {}", config.personal_api_key))
                            .header("X-PostHog-Project-Api-Key", &config.project_api_key)
                            .send()
                            .await
                        {
                            Ok(response) => {
                                if response.status().is_success() {
                                    match response.json::<LocalEvaluationResponse>().await {
                                        Ok(data) => {
                                            trace!("Successfully fetched flag definitions");
                                            cache.update(data);
                                        }
                                        Err(e) => {
                                            warn!(error = %e, "Failed to parse flag response");
                                        }
                                    }
                                } else {
                                    warn!(status = %response.status(), "Failed to fetch flags");
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to fetch flags");
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

    /// Load flags asynchronously
    #[instrument(skip(self), level = "debug")]
    pub async fn load_flags(&self) -> Result<(), Error> {
        let url = format!(
            "{}/api/feature_flag/local_evaluation/?send_cohorts",
            self.config.api_host.trim_end_matches('/')
        );

        let response = self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.personal_api_key),
            )
            .header("X-PostHog-Project-Api-Key", &self.config.project_api_key)
            .send()
            .await
            .map_err(|e| {
                error!(error = %e, "Connection error loading flags");
                Error::Connection(e.to_string())
            })?;

        if !response.status().is_success() {
            let status = response.status();
            error!(status = %status, "HTTP error loading flags");
            return Err(Error::Connection(format!("HTTP {}", status)));
        }

        let data = response
            .json::<LocalEvaluationResponse>()
            .await
            .map_err(|e| {
                error!(error = %e, "Failed to parse flag response");
                Error::Serialization(e.to_string())
            })?;

        self.cache.update(data);
        Ok(())
    }

    /// Stop the polling task
    pub async fn stop(&mut self) {
        debug!("Stopping async flag poller");
        self.stop_signal.store(true, Ordering::Relaxed);
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
        }
        *self.is_running.write().await = false;
    }

    /// Check if the poller is running
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

/// Evaluator for locally cached flags
pub struct LocalEvaluator {
    cache: FlagCache,
}

impl LocalEvaluator {
    pub fn new(cache: FlagCache) -> Self {
        Self { cache }
    }

    /// Evaluate a feature flag locally
    #[instrument(skip(self, person_properties), level = "trace")]
    pub fn evaluate_flag(
        &self,
        key: &str,
        distinct_id: &str,
        person_properties: &HashMap<String, serde_json::Value>,
    ) -> Result<Option<FlagValue>, InconclusiveMatchError> {
        match self.cache.get_flag(key) {
            Some(flag) => {
                let result = match_feature_flag(&flag, distinct_id, person_properties);
                trace!(key, ?result, "Local flag evaluation");
                result.map(Some)
            }
            None => {
                trace!(key, "Flag not found in local cache");
                Ok(None)
            }
        }
    }

    /// Get all flags and evaluate them
    #[instrument(skip(self, person_properties), level = "debug")]
    pub fn evaluate_all_flags(
        &self,
        distinct_id: &str,
        person_properties: &HashMap<String, serde_json::Value>,
    ) -> HashMap<String, Result<FlagValue, InconclusiveMatchError>> {
        let mut results = HashMap::new();

        for flag in self.cache.get_all_flags() {
            let result = match_feature_flag(&flag, distinct_id, person_properties);
            results.insert(flag.key.clone(), result);
        }

        debug!(flag_count = results.len(), "Evaluated all local flags");
        results
    }
}
