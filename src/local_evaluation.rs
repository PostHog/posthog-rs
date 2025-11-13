use crate::feature_flags::{match_feature_flag, FeatureFlag, FlagValue, InconclusiveMatchError};
use crate::Error;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(feature = "async-client"))]
use std::sync::Mutex;
use std::sync::{Arc, RwLock};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LocalEvaluationResponse {
    pub flags: Vec<FeatureFlag>,
    #[serde(default)]
    pub group_type_mapping: HashMap<String, String>,
    #[serde(default)]
    pub cohorts: HashMap<String, Cohort>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Cohort {
    pub id: String,
    pub name: String,
    pub properties: serde_json::Value,
}

/// Manages locally cached feature flags for evaluation
#[derive(Clone)]
pub(crate) struct FlagCache {
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
        let mut flags = self.flags.write().unwrap();
        flags.clear();
        for flag in response.flags {
            flags.insert(flag.key.clone(), flag);
        }

        let mut mapping = self.group_type_mapping.write().unwrap();
        *mapping = response.group_type_mapping;

        let mut cohorts = self.cohorts.write().unwrap();
        *cohorts = response.cohorts;
    }

    pub fn get_flag(&self, key: &str) -> Option<FeatureFlag> {
        self.flags.read().unwrap().get(key).cloned()
    }

    #[cfg(test)] // only for tests uses
    fn clear(&self) {
        self.flags.write().unwrap().clear();
        self.group_type_mapping.write().unwrap().clear();
        self.cohorts.write().unwrap().clear();
    }
}

/// Configuration for local evaluation
#[derive(Clone)]
pub(crate) struct LocalEvaluationConfig {
    /// Personal API key for authentication (sensitive - transmitted via Authorization header only)
    pub personal_api_key: String,
    /// Project API key for project identification (public - safe to include in URLs)
    /// Note: PostHog project API keys (phc_*) are designed to be public and used in client-side code.
    /// See: <https://posthog.com/questions/api-key-security>
    pub project_api_key: String,
    pub api_host: String,
    pub poll_interval: Duration,
    pub request_timeout: Duration,
}

/// Manages polling for feature flag definitions
#[cfg(not(feature = "async-client"))]
pub(crate) struct FlagPoller {
    config: LocalEvaluationConfig,
    cache: FlagCache,
    client: reqwest::blocking::Client,
    stop_signal: Arc<AtomicBool>,
    thread_handle: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
}

#[cfg(not(feature = "async-client"))]
impl FlagPoller {
    fn new(config: LocalEvaluationConfig, cache: FlagCache) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(config.request_timeout)
            .build()
            .unwrap();

        Self {
            config,
            cache,
            client,
            stop_signal: Arc::new(AtomicBool::new(false)),
            thread_handle: Arc::new(Mutex::new(None)),
        }
    }

    /// Start the polling thread
    fn start(&self) {
        // Initial load
        if let Err(e) = self.load_flags() {
            eprintln!("Failed to load initial flags: {e}");
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
                    break;
                }

                // Note: project_api_key (phc_*) is public and safe in URLs - see `LocalEvaluationConfig ` struct docs
                let url = format!(
                    "{}/api/feature_flag/local_evaluation/?token={}&send_cohorts",
                    config.api_host.trim_end_matches('/'),
                    config.project_api_key
                );

                match client
                    .get(&url)
                    .header(
                        "Authorization",
                        format!("Bearer {}", config.personal_api_key),
                    )
                    .send()
                {
                    Ok(response) => {
                        if response.status().is_success() {
                            match response.json::<LocalEvaluationResponse>() {
                                Ok(data) => cache.update(data),
                                Err(e) => {
                                    eprintln!("[FEATURE FLAGS] Failed to parse flag response: {e}")
                                }
                            }
                        } else {
                            eprintln!(
                                "[FEATURE FLAGS] Failed to fetch flags: HTTP {}",
                                response.status()
                            );
                        }
                    }
                    Err(e) => eprintln!("[FEATURE FLAGS] Failed to fetch flags: {e}"),
                }
            }
        });

        *self.thread_handle.lock().unwrap() = Some(handle);
    }

    /// Load flags synchronously
    fn load_flags(&self) -> Result<(), Error> {
        // Note: project_api_key (phc_*) is public and safe in URLs - see `LocalEvaluationConfig ` struct docs
        let url = format!(
            "{}/api/feature_flag/local_evaluation/?token={}&send_cohorts",
            self.config.api_host.trim_end_matches('/'),
            self.config.project_api_key
        );

        let response = self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.personal_api_key),
            )
            .send()
            .map_err(|e| Error::Connection(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Error::Connection(format!("HTTP {}", response.status())));
        }

        let data = response
            .json::<LocalEvaluationResponse>()
            .map_err(|e| Error::Serialization(e.to_string()))?;

        self.cache.update(data);
        Ok(())
    }

    /// Stop the polling thread
    fn stop(&self) {
        self.stop_signal.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread_handle.lock().unwrap().take() {
            handle.join().ok();
        }
    }
}

#[cfg(not(feature = "async-client"))]
impl Drop for FlagPoller {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Async version of the flag poller
#[cfg(feature = "async-client")]
pub(crate) struct AsyncFlagPoller {
    config: LocalEvaluationConfig,
    cache: FlagCache,
    client: reqwest::Client,
    stop_signal: Arc<AtomicBool>,
    task_handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    is_running: Arc<AtomicBool>,
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
            task_handle: Arc::new(tokio::sync::Mutex::new(None)),
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start the polling task
    pub async fn start(&self) {
        // Check if already running
        if self.is_running.swap(true, Ordering::Relaxed) {
            return; // Already running
        }

        // Initial load
        if let Err(e) = self.load_flags().await {
            eprintln!("[FEATURE FLAGS] Failed to load initial flags: {e}");
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
                            break;
                        }

                        // Note: project_api_key (phc_*) is public and safe in URLs - see `LocalEvaluationConfig ` struct docs
                        let url = format!(
                            "{}/api/feature_flag/local_evaluation/?token={}&send_cohorts",
                            config.api_host.trim_end_matches('/'),
                            config.project_api_key
                        );

                        match client
                            .get(&url)
                            .header("Authorization", format!("Bearer {}", config.personal_api_key))
                            .send()
                            .await
                        {
                            Ok(response) => {
                                if response.status().is_success() {
                                    match response.json::<LocalEvaluationResponse>().await {
                                        Ok(data) => cache.update(data),
                                        Err(e) => eprintln!("[FEATURE FLAGS] Failed to parse flag response: {e}"),
                                    }
                                } else {
                                    eprintln!("[FEATURE FLAGS] Failed to fetch flags: HTTP {}", response.status());
                                }
                            }
                            Err(e) => eprintln!("[FEATURE FLAGS] Failed to fetch flags: {e}"),
                        }
                    }
                }
            }

            // Clear running flag when task exits
            is_running.store(false, Ordering::Relaxed);
        });

        *self.task_handle.lock().await = Some(task);
    }

    /// Load flags asynchronously
    pub async fn load_flags(&self) -> Result<(), Error> {
        // Note: project_api_key (phc_*) is public and safe in URLs - see `LocalEvaluationConfig ` struct docs
        let url = format!(
            "{}/api/feature_flag/local_evaluation/?token={}&send_cohorts",
            self.config.api_host.trim_end_matches('/'),
            self.config.project_api_key
        );

        let response = self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.personal_api_key),
            )
            .send()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Error::Connection(format!("HTTP {}", response.status())));
        }

        let data = response
            .json::<LocalEvaluationResponse>()
            .await
            .map_err(|e| Error::Serialization(e.to_string()))?;

        self.cache.update(data);
        Ok(())
    }
}

#[cfg(feature = "async-client")]
impl Drop for AsyncFlagPoller {
    fn drop(&mut self) {
        // Set stop signal
        self.stop_signal.store(true, Ordering::Relaxed);

        // Abort the task if still running
        if let Ok(mut guard) = self.task_handle.try_lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }
    }
}

/// Evaluator for locally cached flags
pub(crate) struct LocalEvaluator {
    cache: FlagCache,
}

impl LocalEvaluator {
    pub fn new(cache: FlagCache) -> Self {
        Self { cache }
    }

    /// Evaluate a feature flag locally
    pub fn evaluate_flag(
        &self,
        key: &str,
        distinct_id: &str,
        person_properties: &HashMap<String, serde_json::Value>,
    ) -> Result<Option<FlagValue>, InconclusiveMatchError> {
        match self.cache.get_flag(key) {
            Some(flag) => match_feature_flag(&flag, distinct_id, person_properties).map(Some),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature_flags::{FeatureFlagCondition, FeatureFlagFilters, Property};
    use serde_json::json;

    #[test]
    fn test_local_evaluation_basic() {
        // Create a cache and evaluator
        let cache = FlagCache::new();
        let evaluator = LocalEvaluator::new(cache.clone());

        // Create a simple flag
        let flag = FeatureFlag {
            key: "test-flag".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: vec![],
                    rollout_percentage: Some(100.0),
                    variant: None,
                }],
                multivariate: None,
                payloads: HashMap::new(),
            },
        };

        // Update cache with the flag
        let response = LocalEvaluationResponse {
            flags: vec![flag],
            group_type_mapping: HashMap::new(),
            cohorts: HashMap::new(),
        };
        cache.update(response);

        // Test evaluation
        let properties = HashMap::new();
        let result = evaluator.evaluate_flag("test-flag", "user-123", &properties);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));
    }

    #[test]
    fn test_local_evaluation_with_properties() {
        let cache = FlagCache::new();
        let evaluator = LocalEvaluator::new(cache.clone());

        // Create a flag with property conditions
        let flag = FeatureFlag {
            key: "premium-feature".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: vec![Property {
                        key: "plan".to_string(),
                        value: json!("premium"),
                        operator: "exact".to_string(),
                        property_type: None,
                    }],
                    rollout_percentage: Some(100.0),
                    variant: None,
                }],
                multivariate: None,
                payloads: HashMap::new(),
            },
        };

        // Update cache
        let response = LocalEvaluationResponse {
            flags: vec![flag],
            group_type_mapping: HashMap::new(),
            cohorts: HashMap::new(),
        };
        cache.update(response);

        // Test with matching properties
        let mut properties = HashMap::new();
        properties.insert("plan".to_string(), json!("premium"));

        let result = evaluator.evaluate_flag("premium-feature", "user-123", &properties);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(FlagValue::Boolean(true)));

        // Test with non-matching properties
        let mut properties = HashMap::new();
        properties.insert("plan".to_string(), json!("free"));

        let result = evaluator.evaluate_flag("premium-feature", "user-456", &properties);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(FlagValue::Boolean(false)));
    }

    #[test]
    fn test_local_evaluation_missing_flag() {
        let cache = FlagCache::new();
        let evaluator = LocalEvaluator::new(cache);

        let properties = HashMap::new();
        let result = evaluator.evaluate_flag("non-existent", "user-123", &properties);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_cache_operations() {
        let cache = FlagCache::new();

        // Create multiple flags
        let flags = vec![
            FeatureFlag {
                key: "flag1".to_string(),
                active: true,
                filters: FeatureFlagFilters {
                    groups: vec![],
                    multivariate: None,
                    payloads: HashMap::new(),
                },
            },
            FeatureFlag {
                key: "flag2".to_string(),
                active: true,
                filters: FeatureFlagFilters {
                    groups: vec![],
                    multivariate: None,
                    payloads: HashMap::new(),
                },
            },
        ];

        let response = LocalEvaluationResponse {
            flags: flags.clone(),
            group_type_mapping: HashMap::new(),
            cohorts: HashMap::new(),
        };

        cache.update(response);

        // Test get_flag
        assert!(cache.get_flag("flag1").is_some());
        assert!(cache.get_flag("flag2").is_some());
        assert!(cache.get_flag("flag3").is_none());

        // Test clear
        cache.clear();
        assert!(cache.get_flag("flag1").is_none());
    }
}
