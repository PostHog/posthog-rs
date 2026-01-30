use chrono::{DateTime, NaiveDate, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, OnceLock};

/// Global cache for compiled regexes to avoid recompilation on every flag evaluation
static REGEX_CACHE: OnceLock<Mutex<HashMap<String, Option<Regex>>>> = OnceLock::new();

/// Salt used for rollout percentage hashing. Intentionally empty to match PostHog's
/// consistent hashing algorithm across all SDKs. This ensures the same user gets
/// the same rollout decision regardless of which SDK evaluates the flag.
const ROLLOUT_HASH_SALT: &str = "";

/// Salt used for multivariate variant selection. Uses "variant" to ensure consistent
/// variant assignment across all PostHog SDKs for the same user/flag combination.
const VARIANT_HASH_SALT: &str = "variant";

fn get_cached_regex(pattern: &str) -> Option<Regex> {
    let cache = REGEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache_guard = match cache.lock() {
        Ok(guard) => guard,
        Err(_) => {
            tracing::warn!(
                pattern,
                "Regex cache mutex poisoned, treating as cache miss"
            );
            return None;
        }
    };

    if let Some(cached) = cache_guard.get(pattern) {
        return cached.clone();
    }

    let compiled = Regex::new(pattern).ok();
    cache_guard.insert(pattern.to_string(), compiled.clone());
    compiled
}

/// The value of a feature flag evaluation.
///
/// Feature flags can return either a boolean (enabled/disabled) or a string
/// (for multivariate flags where users are assigned to different variants).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum FlagValue {
    /// Flag is either enabled (true) or disabled (false)
    Boolean(bool),
    /// Flag returns a specific variant key (e.g., "control", "test", "variant-a")
    String(String),
}

/// Error returned when a feature flag cannot be evaluated locally.
///
/// This typically occurs when:
/// - Required person/group properties are missing
/// - A cohort referenced by the flag is not in the local cache
/// - A dependent flag is not available locally
/// - An unknown operator is encountered
#[derive(Debug)]
pub struct InconclusiveMatchError {
    /// Human-readable description of why evaluation was inconclusive
    pub message: String,
}

impl InconclusiveMatchError {
    pub fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

impl fmt::Display for InconclusiveMatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for InconclusiveMatchError {}

impl Default for FlagValue {
    fn default() -> Self {
        FlagValue::Boolean(false)
    }
}

/// A feature flag definition from PostHog.
///
/// Contains all the information needed to evaluate whether a flag should be
/// enabled for a given user, including targeting rules and rollout percentages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlag {
    /// Unique identifier for the flag (e.g., "new-checkout-flow")
    pub key: String,
    /// Whether the flag is currently active. Inactive flags always return false.
    pub active: bool,
    /// Targeting rules and rollout configuration
    #[serde(default)]
    pub filters: FeatureFlagFilters,
}

/// Targeting rules and configuration for a feature flag.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeatureFlagFilters {
    /// List of condition groups (evaluated with OR logic between groups)
    #[serde(default)]
    pub groups: Vec<FeatureFlagCondition>,
    /// Multivariate configuration for A/B tests with multiple variants
    #[serde(default)]
    pub multivariate: Option<MultivariateFilter>,
    /// JSON payloads associated with flag variants
    #[serde(default)]
    pub payloads: HashMap<String, serde_json::Value>,
}

/// A single condition group within a feature flag's targeting rules.
///
/// All properties within a condition must match (AND logic), and the user
/// must fall within the rollout percentage to be included.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlagCondition {
    /// Property filters that must all match (AND logic)
    #[serde(default)]
    pub properties: Vec<Property>,
    /// Percentage of matching users who should see this flag (0-100)
    pub rollout_percentage: Option<f64>,
    /// Specific variant to serve for this condition (for variant overrides)
    pub variant: Option<String>,
}

/// A property filter used in feature flag targeting.
///
/// Supports various operators for matching user properties against expected values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Property {
    /// The property key to match (e.g., "email", "country", "$feature/other-flag")
    pub key: String,
    /// The value to compare against
    pub value: serde_json::Value,
    /// Comparison operator: "exact", "is_not", "icontains", "not_icontains",
    /// "regex", "not_regex", "gt", "gte", "lt", "lte", "is_set", "is_not_set",
    /// "is_date_before", "is_date_after"
    #[serde(default = "default_operator")]
    pub operator: String,
    /// Property type, e.g., "cohort" for cohort membership checks
    #[serde(rename = "type")]
    pub property_type: Option<String>,
}

fn default_operator() -> String {
    "exact".to_string()
}

/// Definition of a cohort for local evaluation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CohortDefinition {
    pub id: String,
    /// Properties can be either:
    /// - A JSON object with "type" and "values" for complex property groups
    /// - Or a direct Vec<Property> for simple cases
    #[serde(default)]
    pub properties: serde_json::Value,
}

impl CohortDefinition {
    /// Create a new cohort definition with simple property list
    pub fn new(id: String, properties: Vec<Property>) -> Self {
        Self {
            id,
            properties: serde_json::to_value(properties).unwrap_or_default(),
        }
    }

    /// Parse the properties from the JSON structure
    /// PostHog cohort properties come in format:
    /// {"type": "AND", "values": [{"type": "property", "key": "...", "value": "...", "operator": "..."}]}
    pub fn parse_properties(&self) -> Vec<Property> {
        // If it's an array, treat it as direct property list
        if let Some(arr) = self.properties.as_array() {
            return arr
                .iter()
                .filter_map(|v| serde_json::from_value::<Property>(v.clone()).ok())
                .collect();
        }

        // If it's an object with "values" key, extract properties from there
        if let Some(obj) = self.properties.as_object() {
            if let Some(values) = obj.get("values") {
                if let Some(values_arr) = values.as_array() {
                    return values_arr
                        .iter()
                        .filter_map(|v| {
                            // Handle both direct property objects and nested property groups
                            if v.get("type").and_then(|t| t.as_str()) == Some("property") {
                                serde_json::from_value::<Property>(v.clone()).ok()
                            } else if let Some(inner_values) = v.get("values") {
                                // Recursively handle nested groups
                                inner_values.as_array().and_then(|arr| {
                                    arr.iter()
                                        .filter_map(|inner| {
                                            serde_json::from_value::<Property>(inner.clone()).ok()
                                        })
                                        .next()
                                })
                            } else {
                                None
                            }
                        })
                        .collect();
                }
            }
        }

        Vec::new()
    }
}

/// Context for evaluating properties that may depend on cohorts or other flags
pub struct EvaluationContext<'a> {
    pub cohorts: &'a HashMap<String, CohortDefinition>,
    pub flags: &'a HashMap<String, FeatureFlag>,
    pub distinct_id: &'a str,
}

/// Configuration for multivariate (A/B/n) feature flags.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MultivariateFilter {
    /// List of variants with their rollout percentages
    pub variants: Vec<MultivariateVariant>,
}

/// A single variant in a multivariate feature flag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultivariateVariant {
    /// Unique key for this variant (e.g., "control", "test", "variant-a")
    pub key: String,
    /// Percentage of users who should see this variant (0-100)
    pub rollout_percentage: f64,
}

/// Response from the PostHog feature flags API.
///
/// Supports both the v2 API format (with detailed flag information) and the
/// legacy format (simple flag values and payloads).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FeatureFlagsResponse {
    /// v2 API format from `/flags/?v=2` endpoint
    V2 {
        /// Map of flag keys to their detailed evaluation results
        flags: HashMap<String, FlagDetail>,
        /// Whether any errors occurred during flag computation
        #[serde(rename = "errorsWhileComputingFlags")]
        #[serde(default)]
        errors_while_computing_flags: bool,
    },
    /// Legacy format from older decide endpoint
    Legacy {
        /// Map of flag keys to their values
        #[serde(rename = "featureFlags")]
        feature_flags: HashMap<String, FlagValue>,
        /// Map of flag keys to their JSON payloads
        #[serde(rename = "featureFlagPayloads")]
        #[serde(default)]
        feature_flag_payloads: HashMap<String, serde_json::Value>,
        /// Any errors that occurred during evaluation
        #[serde(default)]
        errors: Option<Vec<String>>,
    },
}

impl FeatureFlagsResponse {
    /// Convert the response to a normalized format
    pub fn normalize(
        self,
    ) -> (
        HashMap<String, FlagValue>,
        HashMap<String, serde_json::Value>,
    ) {
        match self {
            FeatureFlagsResponse::V2 { flags, .. } => {
                let mut feature_flags = HashMap::new();
                let mut payloads = HashMap::new();

                for (key, detail) in flags {
                    if detail.enabled {
                        if let Some(variant) = detail.variant {
                            feature_flags.insert(key.clone(), FlagValue::String(variant));
                        } else {
                            feature_flags.insert(key.clone(), FlagValue::Boolean(true));
                        }
                    } else {
                        feature_flags.insert(key.clone(), FlagValue::Boolean(false));
                    }

                    if let Some(metadata) = detail.metadata {
                        if let Some(payload) = metadata.payload {
                            payloads.insert(key, payload);
                        }
                    }
                }

                (feature_flags, payloads)
            }
            FeatureFlagsResponse::Legacy {
                feature_flags,
                feature_flag_payloads,
                ..
            } => (feature_flags, feature_flag_payloads),
        }
    }
}

/// Detailed information about a feature flag evaluation result.
///
/// Returned by the `/decide` endpoint with extended information about
/// why a flag evaluated to a particular value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagDetail {
    /// The feature flag key
    pub key: String,
    /// Whether the flag is enabled for this user
    pub enabled: bool,
    /// The variant key if this is a multivariate flag
    pub variant: Option<String>,
    /// Reason explaining why the flag evaluated to this value
    #[serde(default)]
    pub reason: Option<FlagReason>,
    /// Additional metadata about the flag
    #[serde(default)]
    pub metadata: Option<FlagMetadata>,
}

/// Explains why a feature flag evaluated to a particular value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagReason {
    /// Reason code (e.g., "condition_match", "out_of_rollout_bound")
    pub code: String,
    /// Index of the condition that matched (if applicable)
    #[serde(default)]
    pub condition_index: Option<usize>,
    /// Human-readable description of the reason
    #[serde(default)]
    pub description: Option<String>,
}

/// Metadata about a feature flag from the PostHog server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagMetadata {
    /// Unique identifier for this flag
    pub id: u64,
    /// Version number of the flag definition
    pub version: u32,
    /// Optional description of what this flag controls
    pub description: Option<String>,
    /// Optional JSON payload associated with the flag
    pub payload: Option<serde_json::Value>,
}

const LONG_SCALE: f64 = 0xFFFFFFFFFFFFFFFu64 as f64; // Must be exactly 15 F's to match Python SDK

/// Compute a deterministic hash value for feature flag bucketing.
///
/// Uses SHA-1 to generate a consistent hash in the range [0, 1) for the given
/// key, distinct_id, and salt combination. This ensures users get consistent
/// flag values across requests.
pub fn hash_key(key: &str, distinct_id: &str, salt: &str) -> f64 {
    let hash_key = format!("{key}.{distinct_id}{salt}");
    let mut hasher = Sha1::new();
    hasher.update(hash_key.as_bytes());
    let result = hasher.finalize();
    let hex_str = format!("{result:x}");
    let hash_val = u64::from_str_radix(&hex_str[..15], 16).unwrap_or(0);
    hash_val as f64 / LONG_SCALE
}

/// Determine which variant a user should see for a multivariate flag.
///
/// Uses consistent hashing to assign users to variants based on their
/// rollout percentages. Returns `None` if the flag has no variants or
/// the user doesn't fall into any variant bucket.
pub fn get_matching_variant(flag: &FeatureFlag, distinct_id: &str) -> Option<String> {
    let hash_value = hash_key(&flag.key, distinct_id, VARIANT_HASH_SALT);
    let variants = flag.filters.multivariate.as_ref()?.variants.as_slice();

    let mut value_min = 0.0;
    for variant in variants {
        let value_max = value_min + variant.rollout_percentage / 100.0;
        if hash_value >= value_min && hash_value < value_max {
            return Some(variant.key.clone());
        }
        value_min = value_max;
    }
    None
}

#[must_use = "feature flag evaluation result should be used"]
pub fn match_feature_flag(
    flag: &FeatureFlag,
    distinct_id: &str,
    properties: &HashMap<String, serde_json::Value>,
) -> Result<FlagValue, InconclusiveMatchError> {
    if !flag.active {
        return Ok(FlagValue::Boolean(false));
    }

    let conditions = &flag.filters.groups;

    // Sort conditions to evaluate variant overrides first
    let mut sorted_conditions = conditions.clone();
    sorted_conditions.sort_by_key(|c| if c.variant.is_some() { 0 } else { 1 });

    let mut is_inconclusive = false;

    for condition in sorted_conditions {
        match is_condition_match(flag, distinct_id, &condition, properties) {
            Ok(true) => {
                if let Some(variant_override) = &condition.variant {
                    // Check if variant is valid
                    if let Some(ref multivariate) = flag.filters.multivariate {
                        let valid_variants: Vec<String> = multivariate
                            .variants
                            .iter()
                            .map(|v| v.key.clone())
                            .collect();

                        if valid_variants.contains(variant_override) {
                            return Ok(FlagValue::String(variant_override.clone()));
                        }
                    }
                }

                // Try to get matching variant or return true
                if let Some(variant) = get_matching_variant(flag, distinct_id) {
                    return Ok(FlagValue::String(variant));
                }
                return Ok(FlagValue::Boolean(true));
            }
            Ok(false) => continue,
            Err(_) => {
                is_inconclusive = true;
            }
        }
    }

    if is_inconclusive {
        return Err(InconclusiveMatchError::new(
            "Can't determine if feature flag is enabled or not with given properties",
        ));
    }

    Ok(FlagValue::Boolean(false))
}

fn is_condition_match(
    flag: &FeatureFlag,
    distinct_id: &str,
    condition: &FeatureFlagCondition,
    properties: &HashMap<String, serde_json::Value>,
) -> Result<bool, InconclusiveMatchError> {
    // Check properties first
    for prop in &condition.properties {
        if !match_property(prop, properties)? {
            return Ok(false);
        }
    }

    // If all properties match (or no properties), check rollout percentage
    if let Some(rollout_percentage) = condition.rollout_percentage {
        let hash_value = hash_key(&flag.key, distinct_id, ROLLOUT_HASH_SALT);
        if hash_value > (rollout_percentage / 100.0) {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Match a feature flag with full context (cohorts, other flags)
/// This version supports cohort membership checks and flag dependency checks
#[must_use = "feature flag evaluation result should be used"]
pub fn match_feature_flag_with_context(
    flag: &FeatureFlag,
    distinct_id: &str,
    properties: &HashMap<String, serde_json::Value>,
    ctx: &EvaluationContext,
) -> Result<FlagValue, InconclusiveMatchError> {
    if !flag.active {
        return Ok(FlagValue::Boolean(false));
    }

    let conditions = &flag.filters.groups;

    // Sort conditions to evaluate variant overrides first
    let mut sorted_conditions = conditions.clone();
    sorted_conditions.sort_by_key(|c| if c.variant.is_some() { 0 } else { 1 });

    let mut is_inconclusive = false;

    for condition in sorted_conditions {
        match is_condition_match_with_context(flag, distinct_id, &condition, properties, ctx) {
            Ok(true) => {
                if let Some(variant_override) = &condition.variant {
                    // Check if variant is valid
                    if let Some(ref multivariate) = flag.filters.multivariate {
                        let valid_variants: Vec<String> = multivariate
                            .variants
                            .iter()
                            .map(|v| v.key.clone())
                            .collect();

                        if valid_variants.contains(variant_override) {
                            return Ok(FlagValue::String(variant_override.clone()));
                        }
                    }
                }

                // Try to get matching variant or return true
                if let Some(variant) = get_matching_variant(flag, distinct_id) {
                    return Ok(FlagValue::String(variant));
                }
                return Ok(FlagValue::Boolean(true));
            }
            Ok(false) => continue,
            Err(_) => {
                is_inconclusive = true;
            }
        }
    }

    if is_inconclusive {
        return Err(InconclusiveMatchError::new(
            "Can't determine if feature flag is enabled or not with given properties",
        ));
    }

    Ok(FlagValue::Boolean(false))
}

fn is_condition_match_with_context(
    flag: &FeatureFlag,
    distinct_id: &str,
    condition: &FeatureFlagCondition,
    properties: &HashMap<String, serde_json::Value>,
    ctx: &EvaluationContext,
) -> Result<bool, InconclusiveMatchError> {
    // Check properties first (using context-aware matching for cohorts/flag dependencies)
    for prop in &condition.properties {
        if !match_property_with_context(prop, properties, ctx)? {
            return Ok(false);
        }
    }

    // If all properties match (or no properties), check rollout percentage
    if let Some(rollout_percentage) = condition.rollout_percentage {
        let hash_value = hash_key(&flag.key, distinct_id, ROLLOUT_HASH_SALT);
        if hash_value > (rollout_percentage / 100.0) {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Match a property with additional context for cohorts and flag dependencies
pub fn match_property_with_context(
    property: &Property,
    properties: &HashMap<String, serde_json::Value>,
    ctx: &EvaluationContext,
) -> Result<bool, InconclusiveMatchError> {
    // Check if this is a cohort membership check
    if property.property_type.as_deref() == Some("cohort") {
        return match_cohort_property(property, properties, ctx);
    }

    // Check if this is a flag dependency check
    if property.key.starts_with("$feature/") {
        return match_flag_dependency_property(property, ctx);
    }

    // Fall back to regular property matching
    match_property(property, properties)
}

/// Evaluate cohort membership
fn match_cohort_property(
    property: &Property,
    properties: &HashMap<String, serde_json::Value>,
    ctx: &EvaluationContext,
) -> Result<bool, InconclusiveMatchError> {
    let cohort_id = property
        .value
        .as_str()
        .ok_or_else(|| InconclusiveMatchError::new("Cohort ID must be a string"))?;

    let cohort = ctx.cohorts.get(cohort_id).ok_or_else(|| {
        InconclusiveMatchError::new(&format!("Cohort '{}' not found in local cache", cohort_id))
    })?;

    // Parse and evaluate all cohort properties against the user's properties
    let cohort_properties = cohort.parse_properties();
    let mut is_in_cohort = true;
    for cohort_prop in &cohort_properties {
        match match_property(cohort_prop, properties) {
            Ok(true) => continue,
            Ok(false) => {
                is_in_cohort = false;
                break;
            }
            Err(e) => {
                // If we can't evaluate a cohort property, the cohort membership is inconclusive
                return Err(InconclusiveMatchError::new(&format!(
                    "Cannot evaluate cohort '{}' property '{}': {}",
                    cohort_id, cohort_prop.key, e.message
                )));
            }
        }
    }

    // Handle "in" vs "not_in" operator
    Ok(match property.operator.as_str() {
        "in" => is_in_cohort,
        "not_in" => !is_in_cohort,
        op => {
            return Err(InconclusiveMatchError::new(&format!(
                "Unknown cohort operator: {}",
                op
            )));
        }
    })
}

/// Evaluate flag dependency
fn match_flag_dependency_property(
    property: &Property,
    ctx: &EvaluationContext,
) -> Result<bool, InconclusiveMatchError> {
    // Extract flag key from "$feature/flag-key"
    let flag_key = property
        .key
        .strip_prefix("$feature/")
        .ok_or_else(|| InconclusiveMatchError::new("Invalid flag dependency format"))?;

    let flag = ctx.flags.get(flag_key).ok_or_else(|| {
        InconclusiveMatchError::new(&format!("Flag '{}' not found in local cache", flag_key))
    })?;

    // Evaluate the dependent flag for this user (with empty properties to avoid recursion issues)
    let empty_props = HashMap::new();
    let flag_value = match_feature_flag(flag, ctx.distinct_id, &empty_props)?;

    // Compare the flag value with the expected value
    let expected = &property.value;

    let matches = match (&flag_value, expected) {
        (FlagValue::Boolean(b), serde_json::Value::Bool(expected_b)) => b == expected_b,
        (FlagValue::String(s), serde_json::Value::String(expected_s)) => {
            s.eq_ignore_ascii_case(expected_s)
        }
        (FlagValue::Boolean(true), serde_json::Value::String(s)) => {
            // Flag is enabled (boolean true) but we're checking for a specific variant
            // This should not match
            s.is_empty() || s == "true"
        }
        (FlagValue::Boolean(false), serde_json::Value::String(s)) => s.is_empty() || s == "false",
        (FlagValue::String(s), serde_json::Value::Bool(true)) => {
            // Flag returns a variant string, checking for "enabled" (any variant is enabled)
            !s.is_empty()
        }
        (FlagValue::String(_), serde_json::Value::Bool(false)) => false,
        _ => false,
    };

    // Handle different operators
    Ok(match property.operator.as_str() {
        "exact" => matches,
        "is_not" => !matches,
        op => {
            return Err(InconclusiveMatchError::new(&format!(
                "Unknown flag dependency operator: {}",
                op
            )));
        }
    })
}

/// Parse a relative date string like "-7d", "-24h", "-2w", "-3m", "-1y"
/// Returns the DateTime<Utc> that the relative date represents
fn parse_relative_date(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    // Need at least 3 chars: "-", digit(s), and unit (e.g., "-7d")
    if value.len() < 3 || !value.starts_with('-') {
        return None;
    }

    let (num_str, unit) = value[1..].split_at(value.len() - 2);
    let num: i64 = num_str.parse().ok()?;

    let duration = match unit {
        "h" => chrono::Duration::hours(num),
        "d" => chrono::Duration::days(num),
        "w" => chrono::Duration::weeks(num),
        "m" => chrono::Duration::days(num * 30), // Approximate month as 30 days
        "y" => chrono::Duration::days(num * 365), // Approximate year as 365 days
        _ => return None,
    };

    Some(Utc::now() - duration)
}

/// Parse a date value from a string (ISO date, ISO datetime, or relative date)
fn parse_date_value(value: &serde_json::Value) -> Option<DateTime<Utc>> {
    let date_str = value.as_str()?;

    // Try relative date first (e.g., "-7d")
    if date_str.starts_with('-') && date_str.len() > 1 {
        if let Some(dt) = parse_relative_date(date_str) {
            return Some(dt);
        }
    }

    // Try ISO datetime with timezone (e.g., "2024-06-15T10:30:00Z")
    if let Ok(dt) = DateTime::parse_from_rfc3339(date_str) {
        return Some(dt.with_timezone(&Utc));
    }

    // Try ISO date only (e.g., "2024-06-15")
    if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
        return Some(
            date.and_hms_opt(0, 0, 0)
                .expect("midnight is always valid")
                .and_utc(),
        );
    }

    None
}

fn match_property(
    property: &Property,
    properties: &HashMap<String, serde_json::Value>,
) -> Result<bool, InconclusiveMatchError> {
    let value = match properties.get(&property.key) {
        Some(v) => v,
        None => {
            // Handle is_not_set operator
            if property.operator == "is_not_set" {
                return Ok(true);
            }
            // Handle is_set operator
            if property.operator == "is_set" {
                return Ok(false);
            }
            // For other operators, missing property is inconclusive
            return Err(InconclusiveMatchError::new(&format!(
                "Property '{}' not found in provided properties",
                property.key
            )));
        }
    };

    Ok(match property.operator.as_str() {
        "exact" => {
            if property.value.is_array() {
                if let Some(arr) = property.value.as_array() {
                    for val in arr {
                        if compare_values(val, value) {
                            return Ok(true);
                        }
                    }
                    return Ok(false);
                }
            }
            compare_values(&property.value, value)
        }
        "is_not" => {
            if property.value.is_array() {
                if let Some(arr) = property.value.as_array() {
                    for val in arr {
                        if compare_values(val, value) {
                            return Ok(false);
                        }
                    }
                    return Ok(true);
                }
            }
            !compare_values(&property.value, value)
        }
        "is_set" => true,      // We already know the property exists
        "is_not_set" => false, // We already know the property exists
        "icontains" => {
            let prop_str = value_to_string(value);
            let search_str = value_to_string(&property.value);
            prop_str.to_lowercase().contains(&search_str.to_lowercase())
        }
        "not_icontains" => {
            let prop_str = value_to_string(value);
            let search_str = value_to_string(&property.value);
            !prop_str.to_lowercase().contains(&search_str.to_lowercase())
        }
        "regex" => {
            let prop_str = value_to_string(value);
            let regex_str = value_to_string(&property.value);
            get_cached_regex(&regex_str)
                .map(|re| re.is_match(&prop_str))
                .unwrap_or(false)
        }
        "not_regex" => {
            let prop_str = value_to_string(value);
            let regex_str = value_to_string(&property.value);
            get_cached_regex(&regex_str)
                .map(|re| !re.is_match(&prop_str))
                .unwrap_or(true)
        }
        "gt" | "gte" | "lt" | "lte" => compare_numeric(&property.operator, &property.value, value),
        "is_date_before" | "is_date_after" => {
            let target_date = parse_date_value(&property.value).ok_or_else(|| {
                InconclusiveMatchError::new(&format!(
                    "Unable to parse target date value: {:?}",
                    property.value
                ))
            })?;

            let prop_date = parse_date_value(value).ok_or_else(|| {
                InconclusiveMatchError::new(&format!(
                    "Unable to parse property date value for '{}': {:?}",
                    property.key, value
                ))
            })?;

            if property.operator == "is_date_before" {
                prop_date < target_date
            } else {
                prop_date > target_date
            }
        }
        unknown => {
            return Err(InconclusiveMatchError::new(&format!(
                "Unknown operator: {}",
                unknown
            )));
        }
    })
}

fn compare_values(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    // Case-insensitive string comparison
    if let (Some(a_str), Some(b_str)) = (a.as_str(), b.as_str()) {
        return a_str.eq_ignore_ascii_case(b_str);
    }

    // Direct comparison for other types
    a == b
}

fn value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        _ => value.to_string(),
    }
}

fn compare_numeric(
    operator: &str,
    property_value: &serde_json::Value,
    value: &serde_json::Value,
) -> bool {
    let prop_num = match property_value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    };

    let val_num = match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    };

    if let (Some(prop), Some(val)) = (prop_num, val_num) {
        match operator {
            "gt" => val > prop,
            "gte" => val >= prop,
            "lt" => val < prop,
            "lte" => val <= prop,
            _ => false,
        }
    } else {
        // Fall back to string comparison
        let prop_str = value_to_string(property_value);
        let val_str = value_to_string(value);
        match operator {
            "gt" => val_str > prop_str,
            "gte" => val_str >= prop_str,
            "lt" => val_str < prop_str,
            "lte" => val_str <= prop_str,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Test salt constant to avoid CodeQL warnings about empty cryptographic values
    const TEST_SALT: &str = "test-salt";

    #[test]
    fn test_hash_key() {
        let hash = hash_key("test-flag", "user-123", TEST_SALT);
        assert!((0.0..=1.0).contains(&hash));

        // Same inputs should produce same hash
        let hash2 = hash_key("test-flag", "user-123", TEST_SALT);
        assert_eq!(hash, hash2);

        // Different inputs should produce different hash
        let hash3 = hash_key("test-flag", "user-456", TEST_SALT);
        assert_ne!(hash, hash3);
    }

    #[test]
    fn test_simple_flag_match() {
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

        let properties = HashMap::new();
        let result = match_feature_flag(&flag, "user-123", &properties).unwrap();
        assert_eq!(result, FlagValue::Boolean(true));
    }

    #[test]
    fn test_property_matching() {
        let prop = Property {
            key: "country".to_string(),
            value: json!("US"),
            operator: "exact".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("country".to_string(), json!("US"));

        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("country".to_string(), json!("UK"));
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_multivariate_variants() {
        let flag = FeatureFlag {
            key: "test-flag".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: vec![],
                    rollout_percentage: Some(100.0),
                    variant: None,
                }],
                multivariate: Some(MultivariateFilter {
                    variants: vec![
                        MultivariateVariant {
                            key: "control".to_string(),
                            rollout_percentage: 50.0,
                        },
                        MultivariateVariant {
                            key: "test".to_string(),
                            rollout_percentage: 50.0,
                        },
                    ],
                }),
                payloads: HashMap::new(),
            },
        };

        let properties = HashMap::new();
        let result = match_feature_flag(&flag, "user-123", &properties).unwrap();

        match result {
            FlagValue::String(variant) => {
                assert!(variant == "control" || variant == "test");
            }
            _ => panic!("Expected string variant"),
        }
    }

    #[test]
    fn test_inactive_flag() {
        let flag = FeatureFlag {
            key: "inactive-flag".to_string(),
            active: false,
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

        let properties = HashMap::new();
        let result = match_feature_flag(&flag, "user-123", &properties).unwrap();
        assert_eq!(result, FlagValue::Boolean(false));
    }

    #[test]
    fn test_rollout_percentage() {
        let flag = FeatureFlag {
            key: "rollout-flag".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: vec![],
                    rollout_percentage: Some(30.0), // 30% rollout
                    variant: None,
                }],
                multivariate: None,
                payloads: HashMap::new(),
            },
        };

        let properties = HashMap::new();

        // Test with multiple users to ensure distribution
        let mut enabled_count = 0;
        for i in 0..1000 {
            let result = match_feature_flag(&flag, &format!("user-{}", i), &properties).unwrap();
            if result == FlagValue::Boolean(true) {
                enabled_count += 1;
            }
        }

        // Should be roughly 30% enabled (allow for some variance)
        assert!(enabled_count > 250 && enabled_count < 350);
    }

    #[test]
    fn test_regex_operator() {
        let prop = Property {
            key: "email".to_string(),
            value: json!(".*@company\\.com$"),
            operator: "regex".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("email".to_string(), json!("user@company.com"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("email".to_string(), json!("user@example.com"));
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_icontains_operator() {
        let prop = Property {
            key: "name".to_string(),
            value: json!("ADMIN"),
            operator: "icontains".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("name".to_string(), json!("admin_user"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("name".to_string(), json!("regular_user"));
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_numeric_operators() {
        // Greater than
        let prop_gt = Property {
            key: "age".to_string(),
            value: json!(18),
            operator: "gt".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("age".to_string(), json!(25));
        assert!(match_property(&prop_gt, &properties).unwrap());

        properties.insert("age".to_string(), json!(15));
        assert!(!match_property(&prop_gt, &properties).unwrap());

        // Less than or equal
        let prop_lte = Property {
            key: "score".to_string(),
            value: json!(100),
            operator: "lte".to_string(),
            property_type: None,
        };

        properties.insert("score".to_string(), json!(100));
        assert!(match_property(&prop_lte, &properties).unwrap());

        properties.insert("score".to_string(), json!(101));
        assert!(!match_property(&prop_lte, &properties).unwrap());
    }

    #[test]
    fn test_is_set_operator() {
        let prop = Property {
            key: "email".to_string(),
            value: json!(true),
            operator: "is_set".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("email".to_string(), json!("test@example.com"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.remove("email");
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_is_not_set_operator() {
        let prop = Property {
            key: "phone".to_string(),
            value: json!(true),
            operator: "is_not_set".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("phone".to_string(), json!("+1234567890"));
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_empty_groups() {
        let flag = FeatureFlag {
            key: "empty-groups".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![],
                multivariate: None,
                payloads: HashMap::new(),
            },
        };

        let properties = HashMap::new();
        let result = match_feature_flag(&flag, "user-123", &properties).unwrap();
        assert_eq!(result, FlagValue::Boolean(false));
    }

    #[test]
    fn test_hash_scale_constant() {
        // Verify the constant is exactly 15 F's (not 16)
        assert_eq!(LONG_SCALE, 0xFFFFFFFFFFFFFFFu64 as f64);
        assert_ne!(LONG_SCALE, 0xFFFFFFFFFFFFFFFFu64 as f64);
    }

    // ==================== Tests for missing operators ====================

    #[test]
    fn test_unknown_operator_returns_inconclusive_error() {
        let prop = Property {
            key: "status".to_string(),
            value: json!("active"),
            operator: "unknown_operator".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("status".to_string(), json!("active"));

        let result = match_property(&prop, &properties);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("unknown_operator"));
    }

    #[test]
    fn test_is_date_before_with_relative_date() {
        let prop = Property {
            key: "signup_date".to_string(),
            value: json!("-7d"), // 7 days ago
            operator: "is_date_before".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        // Date 10 days ago should be before -7d
        let ten_days_ago = chrono::Utc::now() - chrono::Duration::days(10);
        properties.insert(
            "signup_date".to_string(),
            json!(ten_days_ago.format("%Y-%m-%d").to_string()),
        );
        assert!(match_property(&prop, &properties).unwrap());

        // Date 3 days ago should NOT be before -7d
        let three_days_ago = chrono::Utc::now() - chrono::Duration::days(3);
        properties.insert(
            "signup_date".to_string(),
            json!(three_days_ago.format("%Y-%m-%d").to_string()),
        );
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_is_date_after_with_relative_date() {
        let prop = Property {
            key: "last_seen".to_string(),
            value: json!("-30d"), // 30 days ago
            operator: "is_date_after".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        // Date 10 days ago should be after -30d
        let ten_days_ago = chrono::Utc::now() - chrono::Duration::days(10);
        properties.insert(
            "last_seen".to_string(),
            json!(ten_days_ago.format("%Y-%m-%d").to_string()),
        );
        assert!(match_property(&prop, &properties).unwrap());

        // Date 60 days ago should NOT be after -30d
        let sixty_days_ago = chrono::Utc::now() - chrono::Duration::days(60);
        properties.insert(
            "last_seen".to_string(),
            json!(sixty_days_ago.format("%Y-%m-%d").to_string()),
        );
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_is_date_before_with_iso_date() {
        let prop = Property {
            key: "expiry_date".to_string(),
            value: json!("2024-06-15"),
            operator: "is_date_before".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("expiry_date".to_string(), json!("2024-06-10"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("expiry_date".to_string(), json!("2024-06-20"));
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_is_date_after_with_iso_date() {
        let prop = Property {
            key: "start_date".to_string(),
            value: json!("2024-01-01"),
            operator: "is_date_after".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("start_date".to_string(), json!("2024-03-15"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("start_date".to_string(), json!("2023-12-01"));
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_is_date_with_relative_hours() {
        let prop = Property {
            key: "last_active".to_string(),
            value: json!("-24h"), // 24 hours ago
            operator: "is_date_after".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        // 12 hours ago should be after -24h
        let twelve_hours_ago = chrono::Utc::now() - chrono::Duration::hours(12);
        properties.insert(
            "last_active".to_string(),
            json!(twelve_hours_ago.to_rfc3339()),
        );
        assert!(match_property(&prop, &properties).unwrap());

        // 48 hours ago should NOT be after -24h
        let forty_eight_hours_ago = chrono::Utc::now() - chrono::Duration::hours(48);
        properties.insert(
            "last_active".to_string(),
            json!(forty_eight_hours_ago.to_rfc3339()),
        );
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_is_date_with_relative_weeks() {
        let prop = Property {
            key: "joined".to_string(),
            value: json!("-2w"), // 2 weeks ago
            operator: "is_date_before".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        // 3 weeks ago should be before -2w
        let three_weeks_ago = chrono::Utc::now() - chrono::Duration::weeks(3);
        properties.insert(
            "joined".to_string(),
            json!(three_weeks_ago.format("%Y-%m-%d").to_string()),
        );
        assert!(match_property(&prop, &properties).unwrap());

        // 1 week ago should NOT be before -2w
        let one_week_ago = chrono::Utc::now() - chrono::Duration::weeks(1);
        properties.insert(
            "joined".to_string(),
            json!(one_week_ago.format("%Y-%m-%d").to_string()),
        );
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_is_date_with_relative_months() {
        let prop = Property {
            key: "subscription_date".to_string(),
            value: json!("-3m"), // 3 months ago
            operator: "is_date_after".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        // 1 month ago should be after -3m
        let one_month_ago = chrono::Utc::now() - chrono::Duration::days(30);
        properties.insert(
            "subscription_date".to_string(),
            json!(one_month_ago.format("%Y-%m-%d").to_string()),
        );
        assert!(match_property(&prop, &properties).unwrap());

        // 6 months ago should NOT be after -3m
        let six_months_ago = chrono::Utc::now() - chrono::Duration::days(180);
        properties.insert(
            "subscription_date".to_string(),
            json!(six_months_ago.format("%Y-%m-%d").to_string()),
        );
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_is_date_with_relative_years() {
        let prop = Property {
            key: "created_at".to_string(),
            value: json!("-1y"), // 1 year ago
            operator: "is_date_before".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        // 2 years ago should be before -1y
        let two_years_ago = chrono::Utc::now() - chrono::Duration::days(730);
        properties.insert(
            "created_at".to_string(),
            json!(two_years_ago.format("%Y-%m-%d").to_string()),
        );
        assert!(match_property(&prop, &properties).unwrap());

        // 6 months ago should NOT be before -1y
        let six_months_ago = chrono::Utc::now() - chrono::Duration::days(180);
        properties.insert(
            "created_at".to_string(),
            json!(six_months_ago.format("%Y-%m-%d").to_string()),
        );
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_is_date_with_invalid_date_format() {
        let prop = Property {
            key: "date".to_string(),
            value: json!("-7d"),
            operator: "is_date_before".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("date".to_string(), json!("not-a-date"));

        // Invalid date formats should return inconclusive
        let result = match_property(&prop, &properties);
        assert!(result.is_err());
    }

    #[test]
    fn test_is_date_with_iso_datetime() {
        let prop = Property {
            key: "event_time".to_string(),
            value: json!("2024-06-15T10:30:00Z"),
            operator: "is_date_before".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("event_time".to_string(), json!("2024-06-15T08:00:00Z"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("event_time".to_string(), json!("2024-06-15T12:00:00Z"));
        assert!(!match_property(&prop, &properties).unwrap());
    }

    // ==================== Tests for cohort membership ====================

    #[test]
    fn test_cohort_membership_in() {
        // Create a cohort that matches users with country = US
        let mut cohorts = HashMap::new();
        cohorts.insert(
            "cohort_1".to_string(),
            CohortDefinition::new(
                "cohort_1".to_string(),
                vec![Property {
                    key: "country".to_string(),
                    value: json!("US"),
                    operator: "exact".to_string(),
                    property_type: None,
                }],
            ),
        );

        // Property filter checking cohort membership
        let prop = Property {
            key: "$cohort".to_string(),
            value: json!("cohort_1"),
            operator: "in".to_string(),
            property_type: Some("cohort".to_string()),
        };

        // User with country = US should be in the cohort
        let mut properties = HashMap::new();
        properties.insert("country".to_string(), json!("US"));

        let ctx = EvaluationContext {
            cohorts: &cohorts,
            flags: &HashMap::new(),
            distinct_id: "user-123",
        };
        assert!(match_property_with_context(&prop, &properties, &ctx).unwrap());

        // User with country = UK should NOT be in the cohort
        properties.insert("country".to_string(), json!("UK"));
        assert!(!match_property_with_context(&prop, &properties, &ctx).unwrap());
    }

    #[test]
    fn test_cohort_membership_not_in() {
        let mut cohorts = HashMap::new();
        cohorts.insert(
            "cohort_blocked".to_string(),
            CohortDefinition::new(
                "cohort_blocked".to_string(),
                vec![Property {
                    key: "status".to_string(),
                    value: json!("blocked"),
                    operator: "exact".to_string(),
                    property_type: None,
                }],
            ),
        );

        let prop = Property {
            key: "$cohort".to_string(),
            value: json!("cohort_blocked"),
            operator: "not_in".to_string(),
            property_type: Some("cohort".to_string()),
        };

        let mut properties = HashMap::new();
        properties.insert("status".to_string(), json!("active"));

        let ctx = EvaluationContext {
            cohorts: &cohorts,
            flags: &HashMap::new(),
            distinct_id: "user-123",
        };
        // User with status = active should NOT be in the blocked cohort (so not_in returns true)
        assert!(match_property_with_context(&prop, &properties, &ctx).unwrap());

        // User with status = blocked IS in the cohort (so not_in returns false)
        properties.insert("status".to_string(), json!("blocked"));
        assert!(!match_property_with_context(&prop, &properties, &ctx).unwrap());
    }

    #[test]
    fn test_cohort_not_found_returns_inconclusive() {
        let cohorts = HashMap::new(); // No cohorts defined

        let prop = Property {
            key: "$cohort".to_string(),
            value: json!("nonexistent_cohort"),
            operator: "in".to_string(),
            property_type: Some("cohort".to_string()),
        };

        let properties = HashMap::new();
        let ctx = EvaluationContext {
            cohorts: &cohorts,
            flags: &HashMap::new(),
            distinct_id: "user-123",
        };

        let result = match_property_with_context(&prop, &properties, &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("Cohort"));
    }

    // ==================== Tests for flag dependencies ====================

    #[test]
    fn test_flag_dependency_enabled() {
        let mut flags = HashMap::new();
        flags.insert(
            "prerequisite-flag".to_string(),
            FeatureFlag {
                key: "prerequisite-flag".to_string(),
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
            },
        );

        // Property checking if prerequisite-flag is enabled
        let prop = Property {
            key: "$feature/prerequisite-flag".to_string(),
            value: json!(true),
            operator: "exact".to_string(),
            property_type: None,
        };

        let properties = HashMap::new();
        let ctx = EvaluationContext {
            cohorts: &HashMap::new(),
            flags: &flags,
            distinct_id: "user-123",
        };

        // The prerequisite flag is enabled for user-123, so this should match
        assert!(match_property_with_context(&prop, &properties, &ctx).unwrap());
    }

    #[test]
    fn test_flag_dependency_disabled() {
        let mut flags = HashMap::new();
        flags.insert(
            "disabled-flag".to_string(),
            FeatureFlag {
                key: "disabled-flag".to_string(),
                active: false, // Flag is inactive
                filters: FeatureFlagFilters {
                    groups: vec![],
                    multivariate: None,
                    payloads: HashMap::new(),
                },
            },
        );

        // Property checking if disabled-flag is enabled
        let prop = Property {
            key: "$feature/disabled-flag".to_string(),
            value: json!(true),
            operator: "exact".to_string(),
            property_type: None,
        };

        let properties = HashMap::new();
        let ctx = EvaluationContext {
            cohorts: &HashMap::new(),
            flags: &flags,
            distinct_id: "user-123",
        };

        // The flag is disabled, so checking for true should fail
        assert!(!match_property_with_context(&prop, &properties, &ctx).unwrap());
    }

    #[test]
    fn test_flag_dependency_variant_match() {
        let mut flags = HashMap::new();
        flags.insert(
            "ab-test-flag".to_string(),
            FeatureFlag {
                key: "ab-test-flag".to_string(),
                active: true,
                filters: FeatureFlagFilters {
                    groups: vec![FeatureFlagCondition {
                        properties: vec![],
                        rollout_percentage: Some(100.0),
                        variant: None,
                    }],
                    multivariate: Some(MultivariateFilter {
                        variants: vec![
                            MultivariateVariant {
                                key: "control".to_string(),
                                rollout_percentage: 50.0,
                            },
                            MultivariateVariant {
                                key: "test".to_string(),
                                rollout_percentage: 50.0,
                            },
                        ],
                    }),
                    payloads: HashMap::new(),
                },
            },
        );

        // Check if user is in "control" variant
        let prop = Property {
            key: "$feature/ab-test-flag".to_string(),
            value: json!("control"),
            operator: "exact".to_string(),
            property_type: None,
        };

        let properties = HashMap::new();
        let ctx = EvaluationContext {
            cohorts: &HashMap::new(),
            flags: &flags,
            distinct_id: "user-gets-control", // This distinct_id should deterministically get "control"
        };

        // The result depends on the hash - we just check it doesn't error
        let result = match_property_with_context(&prop, &properties, &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_flag_dependency_not_found_returns_inconclusive() {
        let flags = HashMap::new(); // No flags defined

        let prop = Property {
            key: "$feature/nonexistent-flag".to_string(),
            value: json!(true),
            operator: "exact".to_string(),
            property_type: None,
        };

        let properties = HashMap::new();
        let ctx = EvaluationContext {
            cohorts: &HashMap::new(),
            flags: &flags,
            distinct_id: "user-123",
        };

        let result = match_property_with_context(&prop, &properties, &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("Flag"));
    }

    // ==================== Date parsing edge case tests ====================

    #[test]
    fn test_parse_relative_date_edge_cases() {
        // These test the internal parse_relative_date function indirectly via match_property
        let prop = Property {
            key: "date".to_string(),
            value: json!("placeholder"),
            operator: "is_date_before".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("date".to_string(), json!("2024-01-01"));

        // Empty string as target date should fail
        let empty_prop = Property {
            value: json!(""),
            ..prop.clone()
        };
        assert!(match_property(&empty_prop, &properties).is_err());

        // Single dash should fail
        let dash_prop = Property {
            value: json!("-"),
            ..prop.clone()
        };
        assert!(match_property(&dash_prop, &properties).is_err());

        // Missing unit (just "-7") should fail
        let no_unit_prop = Property {
            value: json!("-7"),
            ..prop.clone()
        };
        assert!(match_property(&no_unit_prop, &properties).is_err());

        // Missing number (just "-d") should fail
        let no_number_prop = Property {
            value: json!("-d"),
            ..prop.clone()
        };
        assert!(match_property(&no_number_prop, &properties).is_err());

        // Invalid unit should fail
        let invalid_unit_prop = Property {
            value: json!("-7x"),
            ..prop.clone()
        };
        assert!(match_property(&invalid_unit_prop, &properties).is_err());
    }

    #[test]
    fn test_parse_relative_date_large_values() {
        // Very large relative dates should work
        let prop = Property {
            key: "created_at".to_string(),
            value: json!("-1000d"), // ~2.7 years ago
            operator: "is_date_before".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        // Date 5 years ago should be before -1000d
        let five_years_ago = chrono::Utc::now() - chrono::Duration::days(1825);
        properties.insert(
            "created_at".to_string(),
            json!(five_years_ago.format("%Y-%m-%d").to_string()),
        );
        assert!(match_property(&prop, &properties).unwrap());
    }

    // ==================== Tests for invalid regex patterns ====================

    #[test]
    fn test_regex_with_invalid_pattern_returns_false() {
        // Invalid regex pattern (unclosed group)
        let prop = Property {
            key: "email".to_string(),
            value: json!("(unclosed"),
            operator: "regex".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("email".to_string(), json!("test@example.com"));

        // Invalid regex should return false (not match)
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_not_regex_with_invalid_pattern_returns_true() {
        // Invalid regex pattern (unclosed group)
        let prop = Property {
            key: "email".to_string(),
            value: json!("(unclosed"),
            operator: "not_regex".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("email".to_string(), json!("test@example.com"));

        // Invalid regex with not_regex should return true (no match means "not matching")
        assert!(match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_regex_with_various_invalid_patterns() {
        let invalid_patterns = vec![
            "(unclosed", // Unclosed group
            "[unclosed", // Unclosed bracket
            "*invalid",  // Invalid quantifier at start
            "(?P<bad",   // Unclosed named group
            r"\",        // Trailing backslash
        ];

        for pattern in invalid_patterns {
            let prop = Property {
                key: "value".to_string(),
                value: json!(pattern),
                operator: "regex".to_string(),
                property_type: None,
            };

            let mut properties = HashMap::new();
            properties.insert("value".to_string(), json!("test"));

            // All invalid patterns should return false for regex
            assert!(
                !match_property(&prop, &properties).unwrap(),
                "Invalid pattern '{}' should return false for regex",
                pattern
            );

            // And true for not_regex
            let not_regex_prop = Property {
                operator: "not_regex".to_string(),
                ..prop
            };
            assert!(
                match_property(&not_regex_prop, &properties).unwrap(),
                "Invalid pattern '{}' should return true for not_regex",
                pattern
            );
        }
    }
}
