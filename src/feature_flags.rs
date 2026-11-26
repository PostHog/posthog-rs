use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Mutex, OnceLock};

/// Global cache for compiled regexes to avoid recompilation on every flag evaluation
static REGEX_CACHE: OnceLock<Mutex<HashMap<String, CachedRegex>>> = OnceLock::new();

#[derive(Clone)]
enum CachedRegex {
    Compiled(Regex),
    Invalid,
    Inconclusive,
}

/// Salt used for rollout percentage hashing. Intentionally empty to match PostHog's
/// consistent hashing algorithm across all SDKs. This ensures the same user gets
/// the same rollout decision regardless of which SDK evaluates the flag.
const ROLLOUT_HASH_SALT: &str = "";

/// Salt used for multivariate variant selection. Uses "variant" to ensure consistent
/// variant assignment across all PostHog SDKs for the same user/flag combination.
const VARIANT_HASH_SALT: &str = "variant";

// fancy-regex (the flags service) accepts syntax regex does not, and gives some
// native successes different semantics. Defer these cases rather than negating a
// compiler failure or guessing at service execution/backtracking behavior.
fn regex_requires_remote_evaluation(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    let mut index = 0;
    let mut class_depth = 0usize;
    let mut has_atom = false;
    let mut quantifier = 0;
    while index < bytes.len() {
        let character = bytes[index];
        if character == b'\\' {
            index += 1;
            if let Some(&escaped) = bytes.get(index) {
                // The service accepts escaped Unicode literals that regex rejects.
                if !escaped.is_ascii()
                    || matches!(
                        escaped,
                        b'0'..=b'9'
                            | b'b'
                            | b'B'
                            | b'h'
                            | b'H'
                            | b'k'
                            | b'g'
                            | b'K'
                            | b'G'
                            | b'O'
                            | b'Z'
                            | b'e'
                            | b'<'
                            | b'>'
                    )
                    || (class_depth > 0 && matches!(escaped, b'A' | b'z'))
                {
                    return true;
                }
                // Braces in Unicode/hex escapes are not repetition operators.
                if matches!(escaped, b'p' | b'P' | b'x' | b'u' | b'U')
                    && bytes.get(index + 1) == Some(&b'{')
                {
                    while index < bytes.len() && bytes[index] != b'}' {
                        index += 1;
                    }
                }
            }
            has_atom = true;
            quantifier = 0;
        } else if character == b'[' {
            class_depth += 1;
        } else if character == b']' && class_depth > 0 {
            class_depth -= 1;
            has_atom = true;
            quantifier = 0;
        } else if class_depth == 0 {
            match character {
                b'(' => {
                    has_atom = false;
                    quantifier = 0;
                    if bytes.get(index + 1) == Some(&b'?') {
                        index += 2;
                        let mut disabled = false;
                        let mut seen_flags = 0u8;
                        while let Some(&flag) = bytes.get(index) {
                            let flag_bit = match flag {
                                b'i' => 1,
                                b'm' => 2,
                                b's' => 4,
                                b'U' => 8,
                                b'u' if !disabled => 16,
                                b'-' => {
                                    disabled = true;
                                    0
                                }
                                b':' | b')' => break,
                                // In particular, extended mode changes tokenization and
                                // native CRLF/disabled-Unicode flags aren't service flags.
                                // Named-capture naming/uniqueness rules also differ by engine.
                                _ => return true,
                            };
                            // fancy-regex accepts repeated flags that regex rejects.
                            if seen_flags & flag_bit != 0 {
                                return true;
                            }
                            seen_flags |= flag_bit;
                            index += 1;
                        }
                        if bytes.get(index) == Some(&b')')
                            && matches!(bytes.get(index + 1), Some(b'*' | b'+' | b'?' | b'{'))
                        {
                            return true;
                        }
                    }
                    // Empty groups under repetition can compile natively but fail in service.
                    if bytes.get(index + 1) == Some(&b')') {
                        return true;
                    }
                }
                b')' => {
                    has_atom = true;
                    quantifier = 0;
                }
                b'|' => {
                    has_atom = false;
                    quantifier = 0;
                }
                b'*' | b'+' | b'?' => {
                    if quantifier == 1 && character == b'?' {
                        quantifier = 2; // One lazy suffix is shared, stacked repetition is not.
                    } else if quantifier != 0 && has_atom {
                        return true;
                    } else {
                        quantifier = 1;
                    }
                }
                b'{' => {
                    if quantifier != 0
                        || !has_atom
                        || !matches!(bytes.get(index + 1), Some(b'0'..=b'9'))
                    {
                        return true;
                    }
                    // Malformed braces can be literals in the service, not syntax errors.
                    index += 1;
                    while matches!(bytes.get(index), Some(b'0'..=b'9')) {
                        index += 1;
                    }
                    if bytes.get(index) == Some(&b',') {
                        index += 1;
                        while matches!(bytes.get(index), Some(b'0'..=b'9')) {
                            index += 1;
                        }
                    }
                    if bytes.get(index) != Some(&b'}') {
                        return true;
                    }
                    // Native compilation validates the numeric bounds, including overflow.
                    quantifier = 1;
                }
                b'}' => return true,
                _ => {
                    has_atom = true;
                    quantifier = 0;
                }
            }
        }
        index += 1;
    }
    false
}

fn get_cached_regex(pattern: &str) -> Result<Option<Regex>, InconclusiveMatchError> {
    let cache = REGEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache_guard = cache
        .lock()
        .map_err(|_| InconclusiveMatchError::new("Regex cache mutex poisoned"))?;
    let cached = if let Some(cached) = cache_guard.get(pattern) {
        cached.clone()
    } else {
        let compiled = if regex_requires_remote_evaluation(pattern) {
            CachedRegex::Inconclusive
        } else {
            match Regex::new(pattern) {
                Ok(regex) => CachedRegex::Compiled(regex),
                Err(regex::Error::Syntax(_)) => CachedRegex::Invalid,
                Err(_) => CachedRegex::Inconclusive,
            }
        };
        cache_guard.insert(pattern.to_string(), compiled.clone());
        compiled
    };
    match cached {
        CachedRegex::Compiled(regex) => Ok(Some(regex)),
        CachedRegex::Invalid => Ok(None),
        CachedRegex::Inconclusive => Err(InconclusiveMatchError::new(
            "Regex requires server-side evaluation",
        )),
    }
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
    /// Create an inconclusive-match error with a human-readable message.
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
    /// Whether the flag is linked to an experiment, as reported by the
    /// local-evaluation definitions endpoint. Tri-state: `Some(true)`/`Some(false)`
    /// when the server reports it, `None` when it does not (older server). Drives
    /// the `$feature_flag_has_experiment` property and event minimization.
    #[serde(default)]
    pub has_experiment: Option<bool>,
}

/// Definition fields local evaluation needs that are not on the public
/// [`FeatureFlag`] and [`FeatureFlagFilters`]. Those structs can be built with a
/// literal, so a new public field would break callers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DefinitionMetadata {
    /// Marks an early-access feature flag. The person property
    /// `$feature_enrollment/<flag key>` decides it before its release conditions.
    pub(crate) feature_enrollment: bool,
    /// The flag's value is persisted per person, so only the server knows it.
    pub(crate) ensure_experience_continuity: bool,
    /// The flag's `filters.holdout`, as sent.
    pub(crate) holdout: Option<serde_json::Value>,
    /// Indexes of the conditions whose `aggregation_group_type_index` is an
    /// explicit JSON `null`. Such a condition targets persons even on a group
    /// flag, while a condition without the field uses the flag's aggregation.
    pub(crate) person_conditions: Vec<usize>,
}

impl DefinitionMetadata {
    /// Whether the holdout excludes anyone. A missing or malformed holdout
    /// excludes no one.
    fn holdout_excludes(&self) -> bool {
        self.holdout
            .as_ref()
            .and_then(|holdout| holdout.get("exclusion_percentage"))
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|percentage| percentage > 0.0)
    }

    /// The aggregation condition `index` uses: an explicit person condition
    /// targets persons, any other falls back to the flag's aggregation.
    fn condition_aggregation(
        &self,
        index: usize,
        condition: &FeatureFlagCondition,
        flag_aggregation: Option<i32>,
    ) -> Option<i32> {
        if self.person_conditions.contains(&index) {
            return None;
        }
        condition.aggregation_group_type_index.or(flag_aggregation)
    }
}

/// [`DefinitionMetadata`] by flag key. Flags without any are left out.
pub(crate) type DefinitionMetadataMap = HashMap<String, DefinitionMetadata>;

/// A flag definition as it is sent, with the fields of [`DefinitionMetadata`].
#[derive(Serialize, Deserialize)]
pub(crate) struct WireFlag {
    key: String,
    active: bool,
    #[serde(default)]
    filters: WireFilters,
    #[serde(default)]
    has_experiment: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ensure_experience_continuity: Option<bool>,
}

#[derive(Serialize, Deserialize, Default)]
struct WireFilters {
    #[serde(default)]
    groups: Vec<WireCondition>,
    #[serde(default)]
    multivariate: Option<MultivariateFilter>,
    #[serde(default)]
    payloads: HashMap<String, serde_json::Value>,
    #[serde(default)]
    aggregation_group_type_index: Option<i32>,
    #[serde(default)]
    early_exit: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    feature_enrollment: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    holdout: Option<serde_json::Value>,
}

/// A [`FeatureFlagCondition`] as it is sent, telling an explicit `null`
/// aggregation (`Some(None)`) apart from a missing one (`None`).
#[derive(Serialize, Deserialize)]
struct WireCondition {
    #[serde(default)]
    properties: Vec<Property>,
    rollout_percentage: Option<f64>,
    variant: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    aggregation_group_type_index: Option<Option<i32>>,
}

/// Reads a field that is present, `null` included, as `Some`.
fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl WireFlag {
    pub(crate) fn new(flag: FeatureFlag, metadata: DefinitionMetadata) -> Self {
        // Destructured in full, so a new public field can't be left out here.
        let FeatureFlag {
            key,
            active,
            filters:
                FeatureFlagFilters {
                    groups,
                    multivariate,
                    payloads,
                    aggregation_group_type_index,
                    early_exit,
                },
            has_experiment,
        } = flag;
        let groups = groups
            .into_iter()
            .enumerate()
            .map(|(index, condition)| {
                let aggregation = if metadata.person_conditions.contains(&index) {
                    Some(None)
                } else {
                    condition.aggregation_group_type_index.map(Some)
                };
                WireCondition {
                    properties: condition.properties,
                    rollout_percentage: condition.rollout_percentage,
                    variant: condition.variant,
                    aggregation_group_type_index: aggregation,
                }
            })
            .collect();
        Self {
            key,
            active,
            filters: WireFilters {
                groups,
                multivariate,
                payloads,
                aggregation_group_type_index,
                early_exit,
                feature_enrollment: metadata.feature_enrollment.then_some(true),
                holdout: metadata.holdout,
            },
            has_experiment,
            ensure_experience_continuity: metadata.ensure_experience_continuity.then_some(true),
        }
    }

    pub(crate) fn into_parts(self) -> (FeatureFlag, DefinitionMetadata) {
        let person_conditions = self
            .filters
            .groups
            .iter()
            .enumerate()
            .filter(|(_, condition)| condition.aggregation_group_type_index == Some(None))
            .map(|(index, _)| index)
            .collect();
        let metadata = DefinitionMetadata {
            feature_enrollment: self.filters.feature_enrollment.unwrap_or(false),
            ensure_experience_continuity: self.ensure_experience_continuity.unwrap_or(false),
            holdout: self.filters.holdout,
            person_conditions,
        };
        let groups = self
            .filters
            .groups
            .into_iter()
            .map(|condition| FeatureFlagCondition {
                properties: condition.properties,
                rollout_percentage: condition.rollout_percentage,
                variant: condition.variant,
                aggregation_group_type_index: condition.aggregation_group_type_index.flatten(),
            })
            .collect();
        let flag = FeatureFlag {
            key: self.key,
            active: self.active,
            filters: FeatureFlagFilters {
                groups,
                multivariate: self.filters.multivariate,
                payloads: self.filters.payloads,
                aggregation_group_type_index: self.filters.aggregation_group_type_index,
                early_exit: self.filters.early_exit,
            },
            has_experiment: self.has_experiment,
        };
        (flag, metadata)
    }
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
    /// Group type index this flag targets at the flag level. `None` means person
    /// targeting (or mixed, when individual conditions set their own).
    #[serde(default)]
    pub aggregation_group_type_index: Option<i32>,
    /// When `true`, local evaluation stops and returns a definitive disabled
    /// result as soon as a condition group's property filters match (or it has
    /// no property filters) but the rollout percentage excludes the user,
    /// instead of falling through to later condition groups. Defaults to
    /// `false`, which preserves the legacy fall-through behavior.
    #[serde(default)]
    pub early_exit: bool,
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
    /// Optional per-condition aggregation override used by mixed-targeting flags.
    /// When set, this condition targets the specified group type instead of the
    /// flag-level aggregation. `None` means person targeting under a mixed flag.
    #[serde(default)]
    pub aggregation_group_type_index: Option<i32>,
}

/// A property filter used in feature flag targeting.
///
/// Supports various operators for matching user properties against expected values.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "RawProperty")]
pub struct Property {
    /// The property key to match (e.g., "email", "country", "$feature/other-flag")
    pub key: String,
    /// The value to compare against. The presence operators (`"is_set"` and
    /// `"is_not_set"`) never read it, and the API may omit it for them; it
    /// then deserializes as JSON null.
    pub value: serde_json::Value,
    /// Comparison operator. Supported property operators include `"exact"`,
    /// `"is_not"`, `"icontains"`, `"not_icontains"`, `"starts_with"`,
    /// `"not_starts_with"`, `"ends_with"`, `"not_ends_with"`, `"regex"`,
    /// `"not_regex"`, `"gt"`, `"gte"`, `"lt"`, `"lte"`, `"is_set"`,
    /// `"is_not_set"`, `"is_date_before"`, `"is_date_after"`, and the
    /// `"semver_*"` operators used by PostHog version targeting.
    pub operator: String,
    /// Property type. Use `Some("cohort")` for cohort membership checks; flag
    /// dependency checks use a property key that starts with `$feature/`.
    #[serde(rename = "type")]
    pub property_type: Option<String>,
}

/// Wire shape of [`Property`], keeping an omitted `value` distinct from an
/// explicit JSON null.
#[derive(Deserialize)]
struct RawProperty {
    key: String,
    #[serde(default, deserialize_with = "deserialize_present_value")]
    value: Option<serde_json::Value>,
    #[serde(
        default = "default_operator",
        deserialize_with = "deserialize_operator"
    )]
    operator: String,
    #[serde(rename = "type")]
    property_type: Option<String>,
}

impl std::convert::TryFrom<RawProperty> for Property {
    type Error = &'static str;

    fn try_from(raw: RawProperty) -> Result<Self, Self::Error> {
        let value = match raw.value {
            Some(value) => value,
            None if matches!(raw.operator.as_str(), "is_set" | "is_not_set") => {
                serde_json::Value::Null
            }
            // Any other operator compares against the value, so a filter
            // without one must not be read as a comparison against null.
            None => return Err("missing field `value`"),
        };
        Ok(Property {
            key: raw.key,
            value,
            operator: raw.operator,
            property_type: raw.property_type,
        })
    }
}

fn default_operator() -> String {
    "exact".to_string()
}

fn deserialize_operator<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_else(default_operator))
}

/// Unlike `Option`'s own impl, maps an explicit JSON null to `Some(Null)`, so
/// that `None` only ever means the field was omitted.
fn deserialize_present_value<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct CohortProperty {
    #[serde(flatten)]
    property: Property,
    #[serde(default)]
    negation: bool,
}

/// Definition of a cohort for local evaluation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CohortDefinition {
    /// Unique identifier for the cohort.
    pub id: String,
    /// Properties can be either:
    /// - A JSON object with "type" and "values" for complex property groups
    /// - Or a direct `Vec<Property>` for simple cases
    #[serde(default)]
    pub properties: serde_json::Value,
}

impl CohortDefinition {
    /// Create a new cohort definition with a simple property list.
    ///
    /// # Parameters
    ///
    /// - `id`: Cohort identifier.
    /// - `properties`: Property filters that define cohort membership.
    pub fn new(id: String, properties: Vec<Property>) -> Self {
        Self {
            id,
            properties: serde_json::to_value(properties).unwrap_or_default(),
        }
    }

    /// Parse the properties from the JSON structure.
    ///
    /// PostHog cohort properties come in format:
    /// `{"type": "AND", "values": [{"type": "property", "key": "...", "value": "...", "operator": "..."}]}`.
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

/// Context for evaluating properties that may depend on cohorts or other flags.
///
/// `groups`, `group_properties`, and `group_type_mapping` are used to resolve
/// group-targeted and mixed-targeting flags. A group condition (one whose
/// `aggregation_group_type_index` is set, either at the flag or condition level)
/// is bucketed on the group key and matched against group properties looked up
/// via the group type mapping.
pub struct EvaluationContext<'a> {
    /// Cohort definitions available to local evaluation, keyed by cohort ID.
    pub cohorts: &'a HashMap<String, CohortDefinition>,
    /// Feature flag definitions available to evaluate flag dependencies, keyed
    /// by flag key.
    pub flags: &'a HashMap<String, FeatureFlag>,
    /// Distinct ID used for person-targeted flag bucketing.
    pub distinct_id: &'a str,
    /// Group keys for group-targeted flags, keyed by group type.
    pub groups: &'a HashMap<String, String>,
    /// Group properties for group-targeted flags, keyed by group type and then
    /// property name.
    pub group_properties: &'a HashMap<String, HashMap<String, serde_json::Value>>,
    /// Mapping from PostHog group type index to group type name.
    pub group_type_mapping: &'a HashMap<String, String>,
    /// Matching semantics pinned to the definitions snapshot. Exactly `2` uses
    /// explicit equality; `1` (and other versions) uses legacy boolean coercion.
    pub property_matching_version: i64,
}

/// The flags service keeps persons-table fields under this prefix, and a
/// known-absent person key never hides them.
const PERSON_METADATA_KEY_PREFIX: &str = "__posthog_person_metadata__";

/// Property keys the caller knows are absent for one evaluation. A listed key
/// is absent even when the supplied properties hold a value for it.
#[derive(Debug, Default)]
pub(crate) struct KnownAbsentProperties {
    person: HashSet<String>,
    groups: HashMap<String, HashSet<String>>,
}

impl KnownAbsentProperties {
    pub(crate) fn new(
        person: Option<&Vec<String>>,
        groups: Option<&HashMap<String, Vec<String>>>,
    ) -> Self {
        Self {
            person: person
                .into_iter()
                .flatten()
                .filter(|key| !key.starts_with(PERSON_METADATA_KEY_PREFIX))
                .cloned()
                .collect(),
            groups: groups
                .into_iter()
                .flatten()
                .filter(|(_, keys)| !keys.is_empty())
                .map(|(group_type, keys)| (group_type.clone(), keys.iter().cloned().collect()))
                .collect(),
        }
    }
}

/// The known-absent keys that apply to one condition, with the filter type
/// they resolve. Person keys resolve only `type: "person"` filters, and a
/// group type's keys resolve only `type: "group"` filters in conditions
/// aggregated on that group type. Any other filter type, such as
/// `person_metadata`, never resolves from a listing.
#[derive(Clone, Copy)]
struct KnownAbsentKeys<'a> {
    filter_type: &'static str,
    keys: &'a HashSet<String>,
}

impl KnownAbsentKeys<'_> {
    fn contains(&self, property: &Property) -> bool {
        // Flag dependency filters are resolved by evaluating the flag, never
        // by property absence.
        property.property_type.as_deref() == Some(self.filter_type)
            && !property.key.starts_with("$feature/")
            && self.keys.contains(&property.key)
    }
}

/// [`EvaluationContext`] plus the caller's known-absent keys, kept separate so
/// `EvaluationContext` stays constructible with a struct literal.
#[derive(Clone, Copy)]
struct PropertyMatchContext<'a> {
    evaluation: &'a EvaluationContext<'a>,
    /// The caller's person properties. A group condition matches group
    /// properties, so flag dependencies read these instead.
    person_properties: &'a HashMap<String, serde_json::Value>,
    known_absent: &'a KnownAbsentProperties,
    /// Known-absent keys for the properties being matched: the person list, or
    /// the list for the condition's group type.
    known_absent_keys: Option<KnownAbsentKeys<'a>>,
    /// Metadata of the loaded definitions, so flag dependencies honor it too.
    metadata: &'a DefinitionMetadataMap,
    /// The flags being evaluated, from the innermost dependency outward.
    dependency_path: Option<&'a DependencyPath<'a>>,
    /// Flag dependency work for this top-level evaluation.
    dependency_results: &'a RefCell<DependencyResults>,
    /// How many flag dependencies and cohort levels enclose the property being
    /// matched. The top-level flag's properties are at depth 0.
    nesting_depth: usize,
}

/// Flag dependency work shared by one top-level flag evaluation.
#[derive(Default)]
struct DependencyResults {
    /// Results keyed by flag key. Within one top-level evaluation a flag's
    /// result depends only on the shared inputs, so a flag referenced many
    /// times is evaluated once. Results that depend on where the flag was
    /// reached aren't stored.
    values: HashMap<String, CachedDependency>,
    /// Dependencies evaluated so far. Results that depend on where the flag was
    /// reached aren't reused, so this bounds the work they can repeat.
    evaluations: usize,
    /// The deepest dependency or cohort level reached by the flag being
    /// evaluated.
    deepest_dependency: usize,
    /// Whether the flag being evaluated hit the depth limit, the evaluation
    /// budget or a cycle, so its result depends on where it was reached.
    is_path_dependent: bool,
}

/// A flag dependency result, the inconclusive message if it had none, and how
/// much deeper than the flag its dependencies reach.
struct CachedDependency {
    result: Result<FlagValue, String>,
    height: usize,
}

/// One flag on the chain of flag dependencies being evaluated, so a flag that
/// depends on itself, directly or through others, is rejected as a cycle.
struct DependencyPath<'a> {
    flag_key: &'a str,
    parent: Option<&'a DependencyPath<'a>>,
}

impl DependencyPath<'_> {
    fn contains(&self, flag_key: &str) -> bool {
        let mut node = Some(self);
        while let Some(current) = node {
            if current.flag_key == flag_key {
                return true;
            }
            node = current.parent;
        }
        false
    }
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
#[non_exhaustive]
pub enum FeatureFlagsResponse {
    /// v2 API format from `/flags/?v=2` endpoint
    V2 {
        /// Map of flag keys to their successful detailed evaluation results
        #[serde(deserialize_with = "deserialize_evaluated_flags")]
        flags: HashMap<String, FlagDetail>,
        /// Whether any errors occurred during flag computation
        #[serde(rename = "errorsWhileComputingFlags")]
        #[serde(default)]
        errors_while_computing_flags: bool,
        /// Whether the response was returned without evaluation because the
        /// project is over its feature-flag quota.
        #[serde(rename = "quotaLimited")]
        #[serde(default)]
        quota_limited: bool,
        /// Unique identifier for this evaluation request, propagated to
        /// `$feature_flag_called` events as `$feature_flag_request_id`
        /// for experiment exposure tracking.
        #[serde(rename = "requestId")]
        #[serde(default)]
        request_id: Option<String>,
        /// Server-controlled gate: when `true`, `$feature_flag_called` events for
        /// non-experiment flags evaluated from this response are minimized to a
        /// strict property allowlist. Absent (older server) fails safe to `false`.
        #[serde(rename = "minimalFlagCalledEvents")]
        #[serde(default)]
        minimal_flag_called_events: bool,
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

// Filter at the shared wire boundary so normalization and snapshots cannot
// interpret a failed placeholder as a decision, without changing FlagDetail's API.
fn deserialize_evaluated_flags<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, FlagDetail>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct WireFlagDetail {
        #[serde(default)]
        failed: bool,
        #[serde(flatten)]
        detail: FlagDetail,
    }

    let flags = HashMap::<String, WireFlagDetail>::deserialize(deserializer)?;
    Ok(flags
        .into_iter()
        .filter(|(_, flag)| !flag.failed)
        .map(|(key, flag)| (key, flag.detail))
        .collect())
}

impl FeatureFlagsResponse {
    /// Convert the response to normalized flag values and payloads.
    ///
    /// # Returns
    ///
    /// A tuple of `(feature_flags, feature_flag_payloads)`, each keyed by flag
    /// key.
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
/// Returned by the `/flags/?v=2` endpoint with extended information about why a
/// flag evaluated to a particular value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
pub struct FlagMetadata {
    /// Unique identifier for this flag
    pub id: u64,
    /// Version number of the flag definition
    pub version: u32,
    /// Optional description of what this flag controls
    pub description: Option<String>,
    /// Optional JSON payload associated with the flag
    pub payload: Option<serde_json::Value>,
    /// Whether the flag is linked to an experiment. Tri-state: `Some(true)`/
    /// `Some(false)` when the `/flags?v=2` metadata reports it, `None` when it is
    /// absent (older server or degraded response). Drives the
    /// `$feature_flag_has_experiment` property and event minimization.
    #[serde(default)]
    pub has_experiment: Option<bool>,
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
    // The top 60 bits of the digest, i.e. the integer value of its first 15 hex
    // digits. Matches the Python SDK's `int(sha1(...).hexdigest()[:15], 16)`.
    let hash_val = result
        .first_chunk::<8>()
        .map_or(0, |head| u64::from_be_bytes(*head) >> 4);
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

/// Result of resolving a condition's effective bucketing + properties.
enum ConditionTarget<'a> {
    /// Use these for bucketing and property matching.
    Use {
        bucketing: String,
        properties: &'a HashMap<String, serde_json::Value>,
        known_absent_keys: Option<KnownAbsentKeys<'a>>,
    },
    /// Skip this condition (group type unknown, or the required group key is
    /// missing or empty).
    Skip,
    /// Required group properties not provided — try other conditions, surface
    /// inconclusive if nothing else matches.
    Inconclusive,
}

/// The group type and key for group type `index`, when the type is known and
/// its key is usable. An empty group key counts as no key, matching the PostHog
/// Rust backend's `is_usable_group_key`.
fn usable_group_key<'a>(
    index: i32,
    groups: &'a HashMap<String, String>,
    group_type_mapping: &'a HashMap<String, String>,
) -> Option<(&'a String, &'a String)> {
    let group_type = group_type_mapping.get(&index.to_string())?;
    let group_key = groups.get(group_type).filter(|key| !key.is_empty())?;
    Some((group_type, group_key))
}

/// Resolve effective bucketing id and properties for a single condition based on
/// its effective aggregation group type index. Pure-person flags
/// fall through with `distinct_id` and `person_properties`. Group conditions
/// (either flag-level or per-condition aggregation) require the corresponding
/// group key, and group properties unless the group type has known-absent keys.
#[allow(clippy::too_many_arguments)]
fn resolve_condition_target<'a>(
    effective_aggregation: Option<i32>,
    distinct_id: &str,
    person_properties: &'a HashMap<String, serde_json::Value>,
    groups: &HashMap<String, String>,
    group_properties: &'a HashMap<String, HashMap<String, serde_json::Value>>,
    group_type_mapping: &HashMap<String, String>,
    known_absent: &'a KnownAbsentProperties,
) -> ConditionTarget<'a> {
    static NO_PROPERTIES: OnceLock<HashMap<String, serde_json::Value>> = OnceLock::new();

    match effective_aggregation {
        None => ConditionTarget::Use {
            bucketing: distinct_id.to_string(),
            properties: person_properties,
            known_absent_keys: Some(KnownAbsentKeys {
                filter_type: "person",
                keys: &known_absent.person,
            }),
        },
        Some(idx) => {
            let Some((group_type, group_key)) = usable_group_key(idx, groups, group_type_mapping)
            else {
                return ConditionTarget::Skip;
            };
            let known_absent_keys =
                known_absent
                    .groups
                    .get(group_type)
                    .map(|keys| KnownAbsentKeys {
                        filter_type: "group",
                        keys,
                    });
            let props = match group_properties.get(group_type) {
                Some(props) => props,
                None if known_absent_keys.is_some() => NO_PROPERTIES.get_or_init(HashMap::new),
                None => return ConditionTarget::Inconclusive,
            };
            ConditionTarget::Use {
                bucketing: group_key.clone(),
                properties: props,
                known_absent_keys,
            }
        }
    }
}

/// Evaluate a feature flag definition against person and optional group
/// context.
///
/// # Parameters
///
/// - `flag`: Feature flag definition to evaluate.
/// - `distinct_id`: Distinct ID used for person bucketing.
/// - `person_properties`: Person properties available to release conditions.
/// - `groups`: Group keys for group-targeted flags, keyed by group type.
/// - `group_properties`: Group properties for group-targeted flags.
/// - `group_type_mapping`: Mapping from PostHog group type index to group type
///   name.
///
/// # Returns
///
/// The matched flag value: `Boolean(false)` for inactive or unmatched flags,
/// `Boolean(true)` for matched boolean flags, or `String(variant)` for matched
/// multivariate flags.
///
/// A hand-built [`FeatureFlag`] can't express a condition with an explicit
/// person aggregation (JSON `null` under a group flag) or a holdout, so a
/// condition without its own aggregation uses the flag's, and holdouts are
/// ignored. Definitions loaded by [`LocalEvaluator`](crate::LocalEvaluator)
/// follow the flags service for explicit person conditions, and are
/// inconclusive when their holdout excludes anyone.
///
/// # Errors
///
/// Returns [`InconclusiveMatchError`] when required properties are missing or an
/// operator cannot be evaluated locally.
#[must_use = "feature flag evaluation result should be used"]
#[allow(clippy::too_many_arguments)]
pub fn match_feature_flag(
    flag: &FeatureFlag,
    distinct_id: &str,
    person_properties: &HashMap<String, serde_json::Value>,
    groups: &HashMap<String, String>,
    group_properties: &HashMap<String, HashMap<String, serde_json::Value>>,
    group_type_mapping: &HashMap<String, String>,
) -> Result<FlagValue, InconclusiveMatchError> {
    evaluate_feature_flag(
        flag,
        distinct_id,
        person_properties,
        groups,
        group_properties,
        group_type_mapping,
        &KnownAbsentProperties::default(),
        None,
        |property, properties, known_absent_keys| {
            match_property_with_version(property, properties, 1, known_absent_keys)
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn match_feature_flag_with_version(
    flag: &FeatureFlag,
    distinct_id: &str,
    person_properties: &HashMap<String, serde_json::Value>,
    groups: &HashMap<String, String>,
    group_properties: &HashMap<String, HashMap<String, serde_json::Value>>,
    group_type_mapping: &HashMap<String, String>,
    property_matching_version: i64,
    known_absent: &KnownAbsentProperties,
    metadata: &DefinitionMetadataMap,
) -> Result<FlagValue, InconclusiveMatchError> {
    evaluate_feature_flag(
        flag,
        distinct_id,
        person_properties,
        groups,
        group_properties,
        group_type_mapping,
        known_absent,
        metadata.get(&flag.key),
        |property, properties, known_absent_keys| {
            match_property_with_version(
                property,
                properties,
                property_matching_version,
                known_absent_keys,
            )
        },
    )
}

/// Outcome of evaluating a single condition group, mirroring the PostHog Rust
/// evaluation engine's tri-state so the local loop can distinguish a
/// property-filter miss (always fall through) from a rollout exclusion (which
/// can short-circuit when `early_exit` is enabled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConditionMatch {
    /// Property filters matched (or there were none) and the rollout included
    /// the user — the flag matches.
    Match,
    /// A property filter did not match — always continue to the next group.
    NoMatch,
    /// Property filters matched (or there were none) but the rollout excluded
    /// the user.
    OutOfRolloutBound,
}

fn is_condition_match<F>(
    flag: &FeatureFlag,
    bucketing_id: &str,
    condition: &FeatureFlagCondition,
    properties: &HashMap<String, serde_json::Value>,
    known_absent_keys: Option<KnownAbsentKeys>,
    property_matcher: &F,
) -> Result<ConditionMatch, InconclusiveMatchError>
where
    F: PropertyMatcher,
{
    // Check properties first. The caller supplies either the basic matcher or
    // the context-aware matcher; rollout handling remains identical.
    // Filters in a condition have no defined order, so an inconclusive filter
    // doesn't stop the loop: a later filter can still rule the condition out.
    let mut inconclusive = None;
    for prop in &condition.properties {
        match property_matcher(prop, properties, known_absent_keys) {
            Ok(true) => {}
            Ok(false) => return Ok(ConditionMatch::NoMatch),
            Err(err) => {
                inconclusive.get_or_insert(err);
            }
        }
    }
    if let Some(err) = inconclusive {
        return Err(err);
    }

    // If all properties match (or no properties), check rollout percentage
    if let Some(rollout_percentage) = condition.rollout_percentage {
        let hash_value = hash_key(&flag.key, bucketing_id, ROLLOUT_HASH_SALT);
        if hash_value > (rollout_percentage / 100.0) {
            return Ok(ConditionMatch::OutOfRolloutBound);
        }
    }

    Ok(ConditionMatch::Match)
}

/// Match a feature flag with full context (cohorts, other flags).
///
/// This version supports cohort membership checks and flag dependency checks.
/// `person_properties` carries person-level property values; group-level
/// properties are looked up from `ctx.group_properties` when a condition (or
/// the flag itself) targets a group via `aggregation_group_type_index`.
///
/// A hand-built [`FeatureFlag`] can't express a condition with an explicit
/// person aggregation (JSON `null` under a group flag) or a holdout, so a
/// condition without its own aggregation uses the flag's, and holdouts are
/// ignored. Definitions loaded by [`LocalEvaluator`](crate::LocalEvaluator)
/// follow the flags service for explicit person conditions, and are
/// inconclusive when their holdout excludes anyone.
///
/// # Errors
///
/// Returns [`InconclusiveMatchError`] when local evaluation cannot determine a
/// result with the provided context.
#[must_use = "feature flag evaluation result should be used"]
pub fn match_feature_flag_with_context(
    flag: &FeatureFlag,
    person_properties: &HashMap<String, serde_json::Value>,
    ctx: &EvaluationContext,
) -> Result<FlagValue, InconclusiveMatchError> {
    match_flag_definition(
        flag,
        person_properties,
        ctx,
        &KnownAbsentProperties::default(),
        &HashMap::new(),
    )
}

#[cfg(test)]
pub(crate) fn match_feature_flag_with_known_absent(
    flag: &FeatureFlag,
    person_properties: &HashMap<String, serde_json::Value>,
    ctx: &EvaluationContext,
    known_absent: &KnownAbsentProperties,
) -> Result<FlagValue, InconclusiveMatchError> {
    match_flag_definition(flag, person_properties, ctx, known_absent, &HashMap::new())
}

/// [`match_feature_flag_with_context`] with the caller's known-absent keys and
/// the metadata of loaded definitions.
pub(crate) fn match_flag_definition(
    flag: &FeatureFlag,
    person_properties: &HashMap<String, serde_json::Value>,
    ctx: &EvaluationContext,
    known_absent: &KnownAbsentProperties,
    metadata: &DefinitionMetadataMap,
) -> Result<FlagValue, InconclusiveMatchError> {
    match_feature_flag_in_context(
        flag,
        person_properties,
        ctx,
        known_absent,
        metadata,
        None,
        &RefCell::default(),
        0,
    )
}

/// Evaluates a top-level flag, or a flag dependency with the same top-level
/// inputs. `dependency_path` holds the flags already being evaluated, and
/// `nesting_depth` is the depth of the flag's properties.
#[allow(clippy::too_many_arguments)]
fn match_feature_flag_in_context(
    flag: &FeatureFlag,
    person_properties: &HashMap<String, serde_json::Value>,
    ctx: &EvaluationContext,
    known_absent: &KnownAbsentProperties,
    metadata: &DefinitionMetadataMap,
    dependency_path: Option<&DependencyPath>,
    dependency_results: &RefCell<DependencyResults>,
    nesting_depth: usize,
) -> Result<FlagValue, InconclusiveMatchError> {
    if let Some(result) = metadata_result(flag, person_properties, known_absent, metadata) {
        return result;
    }
    let dependency_path = DependencyPath {
        flag_key: &flag.key,
        parent: dependency_path,
    };
    evaluate_feature_flag(
        flag,
        ctx.distinct_id,
        person_properties,
        ctx.groups,
        ctx.group_properties,
        ctx.group_type_mapping,
        known_absent,
        metadata.get(&flag.key),
        |property, properties, known_absent_keys| {
            match_property_in_context(
                property,
                properties,
                &PropertyMatchContext {
                    evaluation: ctx,
                    person_properties,
                    known_absent,
                    known_absent_keys,
                    metadata,
                    dependency_path: Some(&dependency_path),
                    dependency_results,
                    nesting_depth,
                },
            )
        },
    )
}

/// Matches one property filter against the condition's properties and the
/// known-absent keys that apply to them.
trait PropertyMatcher:
    Fn(
    &Property,
    &HashMap<String, serde_json::Value>,
    Option<KnownAbsentKeys>,
) -> Result<bool, InconclusiveMatchError>
{
}

impl<F> PropertyMatcher for F where
    F: Fn(
        &Property,
        &HashMap<String, serde_json::Value>,
        Option<KnownAbsentKeys>,
    ) -> Result<bool, InconclusiveMatchError>
{
}

/// The result of an active flag whose metadata decides it before its group
/// aggregation and release conditions are read.
pub(crate) fn metadata_result(
    flag: &FeatureFlag,
    person_properties: &HashMap<String, serde_json::Value>,
    known_absent: &KnownAbsentProperties,
    metadata: &DefinitionMetadataMap,
) -> Option<Result<FlagValue, InconclusiveMatchError>> {
    let metadata = metadata.get(&flag.key).filter(|_| flag.active)?;
    if metadata.ensure_experience_continuity {
        return Some(Err(InconclusiveMatchError::new(
            "Flag has experience continuity enabled",
        )));
    }
    let enrollment_key = format!("$feature_enrollment/{}", flag.key);
    // A listed enrollment key means the person isn't enrolled, even when a
    // value was supplied, so the holdout and release conditions decide.
    (metadata.feature_enrollment && !known_absent.person.contains(&enrollment_key))
        .then(|| match_feature_enrollment(&enrollment_key, person_properties))
}

/// An early-access feature flag returns the person's enrollment at once:
/// `true` or `"true"` means enrolled, and any other value that was passed,
/// `null` included, means opted out. Without the enrollment property the flag
/// can't be resolved locally.
fn match_feature_enrollment(
    enrollment_key: &str,
    person_properties: &HashMap<String, serde_json::Value>,
) -> Result<FlagValue, InconclusiveMatchError> {
    match person_properties.get(enrollment_key) {
        None => Err(InconclusiveMatchError::new(&format!(
            "Flag has feature enrollment and '{enrollment_key}' was not passed"
        ))),
        Some(serde_json::Value::Bool(enrolled)) => Ok(FlagValue::Boolean(*enrolled)),
        Some(serde_json::Value::String(enrolled)) => Ok(FlagValue::Boolean(enrolled == "true")),
        Some(_) => Ok(FlagValue::Boolean(false)),
    }
}

/// Shared feature flag evaluation engine. Public entry points differ only in
/// how individual properties are matched.
#[allow(clippy::too_many_arguments)]
fn evaluate_feature_flag<F>(
    flag: &FeatureFlag,
    distinct_id: &str,
    person_properties: &HashMap<String, serde_json::Value>,
    groups: &HashMap<String, String>,
    group_properties: &HashMap<String, HashMap<String, serde_json::Value>>,
    group_type_mapping: &HashMap<String, String>,
    known_absent: &KnownAbsentProperties,
    definition: Option<&DefinitionMetadata>,
    property_matcher: F,
) -> Result<FlagValue, InconclusiveMatchError>
where
    F: PropertyMatcher,
{
    if !flag.active {
        return Ok(FlagValue::Boolean(false));
    }

    let no_metadata = DefinitionMetadata::default();
    let definition = definition.unwrap_or(&no_metadata);
    let flag_aggregation = flag.filters.aggregation_group_type_index;

    // The flags service checks the holdout before the release conditions, and
    // this SDK doesn't evaluate holdouts, so only the server can tell whether
    // the holdout catches the flag. Without a usable group key the cause is the
    // missing context, which the message names first.
    if definition.holdout_excludes() {
        let message = if flag_aggregation
            .is_some_and(|index| usable_group_key(index, groups, group_type_mapping).is_none())
        {
            "Flag has a holdout and its group key is missing"
        } else {
            "Flag has a holdout, which local evaluation doesn't support"
        };
        return Err(InconclusiveMatchError::new(message));
    }

    let mut is_inconclusive = false;

    for (index, condition) in flag.filters.groups.iter().enumerate() {
        let (effective_bucketing, effective_properties, known_absent_keys) =
            match resolve_condition_target(
                definition.condition_aggregation(index, condition, flag_aggregation),
                distinct_id,
                person_properties,
                groups,
                group_properties,
                group_type_mapping,
                known_absent,
            ) {
                ConditionTarget::Use {
                    bucketing,
                    properties,
                    known_absent_keys,
                } => (bucketing, properties, known_absent_keys),
                ConditionTarget::Skip => continue,
                ConditionTarget::Inconclusive => {
                    is_inconclusive = true;
                    continue;
                }
            };

        match is_condition_match(
            flag,
            &effective_bucketing,
            condition,
            effective_properties,
            known_absent_keys,
            &property_matcher,
        ) {
            Ok(ConditionMatch::Match) => {
                if let Some(variant_override) = &condition.variant {
                    let is_valid = flag
                        .filters
                        .multivariate
                        .as_ref()
                        .is_some_and(|multivariate| {
                            multivariate
                                .variants
                                .iter()
                                .any(|variant| variant.key == *variant_override)
                        });
                    if is_valid {
                        return Ok(FlagValue::String(variant_override.clone()));
                    }
                }

                if let Some(variant) = get_matching_variant(flag, &effective_bucketing) {
                    return Ok(FlagValue::String(variant));
                }
                return Ok(FlagValue::Boolean(true));
            }
            Ok(ConditionMatch::OutOfRolloutBound) => {
                // Later groups can't change the outcome. A prior inconclusive
                // condition still wins below, so the caller can fall back.
                if flag.filters.early_exit {
                    break;
                }
            }
            Ok(ConditionMatch::NoMatch) => continue,
            Err(_) => is_inconclusive = true,
        }
    }

    if is_inconclusive {
        return Err(InconclusiveMatchError::new(
            "Can't determine if feature flag is enabled or not with given properties",
        ));
    }

    Ok(FlagValue::Boolean(false))
}

/// Match a property with additional context for cohorts and flag dependencies.
///
/// Use this when evaluating local feature flag conditions that can reference
/// cohort membership (`type = "cohort"`) or another feature flag via
/// `$feature/<flag-key>`.
///
/// A referenced flag is evaluated with `properties` as the person properties.
///
/// # Errors
///
/// Returns [`InconclusiveMatchError`] when the property, cohort, or dependent
/// flag cannot be evaluated from the supplied context.
pub fn match_property_with_context(
    property: &Property,
    properties: &HashMap<String, serde_json::Value>,
    ctx: &EvaluationContext,
) -> Result<bool, InconclusiveMatchError> {
    match_property_in_context(
        property,
        properties,
        &PropertyMatchContext {
            evaluation: ctx,
            person_properties: properties,
            known_absent: &KnownAbsentProperties::default(),
            known_absent_keys: None,
            metadata: &HashMap::new(),
            dependency_path: None,
            dependency_results: &RefCell::default(),
            nesting_depth: 0,
        },
    )
}

fn match_property_in_context(
    property: &Property,
    properties: &HashMap<String, serde_json::Value>,
    ctx: &PropertyMatchContext,
) -> Result<bool, InconclusiveMatchError> {
    // Check if this is a cohort membership check
    if property.property_type.as_deref() == Some("cohort") {
        return match_cohort_property(property, properties, ctx);
    }

    // Check if this is a flag dependency check
    if property.key.starts_with("$feature/") {
        return match_flag_dependency_property(property, ctx);
    }

    // Use the same snapshot version for direct and recursive cohort leaves.
    match_property_with_version(
        property,
        properties,
        ctx.evaluation.property_matching_version,
        ctx.known_absent_keys,
    )
}

/// Evaluate cohort membership referenced by a flag property (`type = "cohort"`).
fn match_cohort_property(
    property: &Property,
    properties: &HashMap<String, serde_json::Value>,
    ctx: &PropertyMatchContext,
) -> Result<bool, InconclusiveMatchError> {
    let cohort_id = cohort_id_to_string(&property.value)
        .ok_or_else(|| InconclusiveMatchError::new("Cohort ID must be a string or number"))?;

    // Cohorts hold person filters, so a cohort in a group condition gets no
    // known-absent keys.
    let cohort_ctx = PropertyMatchContext {
        evaluation: ctx.evaluation,
        person_properties: ctx.person_properties,
        known_absent: ctx.known_absent,
        known_absent_keys: ctx
            .known_absent_keys
            .filter(|keys| keys.filter_type == "person"),
        metadata: ctx.metadata,
        dependency_path: ctx.dependency_path,
        dependency_results: ctx.dependency_results,
        nesting_depth: ctx.nesting_depth,
    };
    // Cohort levels count on from the enclosing flag dependency depth, so
    // flags and cohorts share one depth limit.
    let mut active_cohorts = HashSet::new();
    let is_in_cohort = match_cohort_by_id(
        &cohort_id,
        properties,
        &cohort_ctx,
        &mut active_cohorts,
        ctx.nesting_depth,
    )
    .map_err(CohortMatchError::into_inconclusive)?;

    // Cohort references omit the operator in API payloads, so "exact" means membership.
    Ok(match property.operator.as_str() {
        "exact" | "in" => is_in_cohort,
        "not_in" => !is_in_cohort,
        op => {
            return Err(InconclusiveMatchError::new(&format!(
                "Unknown cohort operator: {}",
                op
            )));
        }
    })
}

/// Cohort IDs arrive as either a JSON string or a number depending on where the
/// reference lives (top-level flag property vs a nested cohort filter). Normalize
/// both to the string keys used in the cohort cache.
fn cohort_id_to_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[derive(Debug)]
enum CohortMatchError {
    Inconclusive(InconclusiveMatchError),
    InvalidDefinition(InconclusiveMatchError),
    MissingCohort(InconclusiveMatchError),
}

impl CohortMatchError {
    fn into_inconclusive(self) -> InconclusiveMatchError {
        match self {
            Self::Inconclusive(error)
            | Self::InvalidDefinition(error)
            | Self::MissingCohort(error) => error,
        }
    }

    fn requires_server_evaluation(&self) -> bool {
        !matches!(self, Self::Inconclusive(_))
    }
}

/// How deeply local cohort evaluation will recurse through cohort references
/// and nested property groups combined, counting on from the flag dependency
/// depth the cohort is reached at.
///
/// A cycle check alone bounds repeats, not depth: a chain of distinct cohorts is
/// acyclic and still recurses once per link. Counting only cohort references is
/// also insufficient because each cohort can contain nested property groups.
/// Real cohort nesting is a handful of levels at most. Cached flag dependency
/// results are reused only where their cohort levels fit under the flag
/// dependency limit, so the two limits are the same.
const MAX_COHORT_RESOLUTION_DEPTH: usize = MAX_FLAG_DEPENDENCY_DEPTH;

/// How many flags deep local evaluation follows flag dependencies. Cohort levels
/// a dependency is reached through count too, so alternating cohorts and flags
/// can't nest past either limit.
const MAX_FLAG_DEPENDENCY_DEPTH: usize = 100;

/// How many flag dependencies one top-level flag evaluation evaluates at most.
const MAX_FLAG_DEPENDENCY_EVALUATIONS: usize = 1000;

/// Look up a cohort by ID and evaluate its property group against `properties`.
///
/// `active_cohorts` holds the IDs on the current resolution path so a cohort
/// that references itself, directly or through others, is rejected as a cycle.
/// `resolution_depth` bounds the combined path through cohort references and
/// nested property groups. IDs are removed on the way back out, so a cohort
/// referenced twice down sibling branches still resolves normally.
fn match_cohort_by_id(
    cohort_id: &str,
    properties: &HashMap<String, serde_json::Value>,
    ctx: &PropertyMatchContext,
    active_cohorts: &mut HashSet<String>,
    resolution_depth: usize,
) -> Result<bool, CohortMatchError> {
    let cohort = ctx.evaluation.cohorts.get(cohort_id).ok_or_else(|| {
        CohortMatchError::MissingCohort(InconclusiveMatchError::new(&format!(
            "Cohort '{}' not found in local cache",
            cohort_id
        )))
    })?;

    if resolution_depth >= MAX_COHORT_RESOLUTION_DEPTH {
        ctx.dependency_results.borrow_mut().is_path_dependent = true;
        return Err(CohortMatchError::InvalidDefinition(
            InconclusiveMatchError::new(&format!(
                "Cohort '{}' is nested deeper than the limit of {}",
                cohort_id, MAX_COHORT_RESOLUTION_DEPTH
            )),
        ));
    }

    if !active_cohorts.insert(cohort_id.to_string()) {
        return Err(CohortMatchError::InvalidDefinition(
            InconclusiveMatchError::new(&format!(
                "Cohort '{}' is part of a reference cycle",
                cohort_id
            )),
        ));
    }

    let result = match_property_group(
        &cohort.properties,
        properties,
        ctx,
        active_cohorts,
        resolution_depth,
    );
    active_cohorts.remove(cohort_id);
    result
}

/// Recursively evaluate a cohort property group against a user's properties.
///
/// The `/flags/definitions/?send_cohorts` payload nests groups as
/// `{"type": "AND"|"OR", "values": [...]}`, where each `values` entry is either
/// another group (it has its own `values`), a cohort reference
/// (`{"type": "cohort", "value": <id>, "negation": <bool>}`), or a leaf property
/// filter. This mirrors posthog-python's `match_property_group`: `AND` requires
/// every entry to match, `OR` requires any, and a bare JSON array is treated as
/// an implicit `AND` of leaf properties (as produced by
/// [`CohortDefinition::new`]).
fn match_property_group(
    group: &serde_json::Value,
    properties: &HashMap<String, serde_json::Value>,
    ctx: &PropertyMatchContext,
    active_cohorts: &mut HashSet<String>,
    resolution_depth: usize,
) -> Result<bool, CohortMatchError> {
    if resolution_depth >= MAX_COHORT_RESOLUTION_DEPTH {
        ctx.dependency_results.borrow_mut().is_path_dependent = true;
        return Err(CohortMatchError::InvalidDefinition(
            InconclusiveMatchError::new(&format!(
                "Cohort property groups are nested deeper than the limit of {}",
                MAX_COHORT_RESOLUTION_DEPTH
            )),
        ));
    }
    {
        let mut results = ctx.dependency_results.borrow_mut();
        results.deepest_dependency = results.deepest_dependency.max(resolution_depth);
    }

    if let Some(arr) = group.as_array() {
        return match_property_group_values(
            "AND",
            arr,
            properties,
            ctx,
            active_cohorts,
            resolution_depth,
        );
    }

    let Some(obj) = group.as_object() else {
        return Err(CohortMatchError::InvalidDefinition(
            InconclusiveMatchError::new("Cohort property group must be an object or array"),
        ));
    };

    // The backend serializes a valid empty PropertyGroup as an empty object.
    if obj.is_empty() {
        return Ok(true);
    }

    let group_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("AND");

    let Some(values) = obj.get("values").and_then(|v| v.as_array()) else {
        return Err(CohortMatchError::InvalidDefinition(
            InconclusiveMatchError::new("Cohort property group values must be an array"),
        ));
    };

    match_property_group_values(
        group_type,
        values,
        properties,
        ctx,
        active_cohorts,
        resolution_depth,
    )
}

/// Combine the `values` of a property group under AND/OR semantics.
///
/// An ordinary inconclusive entry (missing property, unknown operator, ...) is
/// remembered but deferred: a definitive result (`false` under AND, `true` under
/// OR) still resolves the group. Every entry is inspected so missing cohorts and
/// malformed definitions always force server-side evaluation regardless of
/// branch order.
fn match_property_group_values(
    group_type: &str,
    values: &[serde_json::Value],
    properties: &HashMap<String, serde_json::Value>,
    ctx: &PropertyMatchContext,
    active_cohorts: &mut HashSet<String>,
    resolution_depth: usize,
) -> Result<bool, CohortMatchError> {
    if values.is_empty() {
        return Ok(true);
    }

    let is_and = !group_type.eq_ignore_ascii_case("OR");
    let mut decisive_result = None;
    let mut inconclusive: Option<CohortMatchError> = None;

    for value in values {
        let result = if value.get("values").is_some() {
            // Nested property group.
            match_property_group(value, properties, ctx, active_cohorts, resolution_depth + 1)
        } else if value.get("type").and_then(|t| t.as_str()) == Some("cohort") {
            // A cohort filter nested inside another cohort.
            match_nested_cohort(value, properties, ctx, active_cohorts, resolution_depth + 1)
        } else {
            // Leaf property filter.
            match serde_json::from_value::<CohortProperty>(value.clone()) {
                Ok(prop) => match_property_in_context(
                    &prop.property,
                    properties,
                    &PropertyMatchContext {
                        nesting_depth: resolution_depth + 1,
                        ..*ctx
                    },
                )
                .map(|matches| matches != prop.negation)
                .map_err(CohortMatchError::Inconclusive),
                Err(e) => Err(CohortMatchError::InvalidDefinition(
                    InconclusiveMatchError::new(&format!("Unable to parse cohort property: {}", e)),
                )),
            }
        };

        match result {
            Ok(true) if !is_and => decisive_result = Some(true),
            Ok(false) if is_and => decisive_result = Some(false),
            Ok(_) => {}
            Err(error) if error.requires_server_evaluation() => return Err(error),
            Err(error) => inconclusive = Some(error),
        }
    }

    if let Some(result) = decisive_result {
        return Ok(result);
    }

    if let Some(error) = inconclusive {
        return Err(error);
    }

    // AND: every entry matched. OR: none matched.
    Ok(is_and)
}

/// Evaluate a cohort reference nested inside another cohort's property group,
/// honoring the optional `negation` flag.
fn match_nested_cohort(
    value: &serde_json::Value,
    properties: &HashMap<String, serde_json::Value>,
    ctx: &PropertyMatchContext,
    active_cohorts: &mut HashSet<String>,
    resolution_depth: usize,
) -> Result<bool, CohortMatchError> {
    let cohort_id = value
        .get("value")
        .and_then(cohort_id_to_string)
        .ok_or_else(|| {
            CohortMatchError::InvalidDefinition(InconclusiveMatchError::new(
                "Nested cohort ID must be a string or number",
            ))
        })?;

    let negation = value
        .get("negation")
        .and_then(|n| n.as_bool())
        .unwrap_or(false);

    let is_member = match_cohort_by_id(
        &cohort_id,
        properties,
        ctx,
        active_cohorts,
        resolution_depth,
    )?;
    Ok(is_member != negation)
}

/// Evaluate a flag dependency. The referenced flag is evaluated exactly as if
/// it were evaluated directly with the same top-level inputs, so it uses its
/// own aggregation, properties and bucketing identifier.
fn match_flag_dependency_property(
    property: &Property,
    ctx: &PropertyMatchContext,
) -> Result<bool, InconclusiveMatchError> {
    // Extract flag key from "$feature/flag-key"
    let flag_key = property
        .key
        .strip_prefix("$feature/")
        .ok_or_else(|| InconclusiveMatchError::new("Invalid flag dependency format"))?;

    let flag = ctx.evaluation.flags.get(flag_key).ok_or_else(|| {
        InconclusiveMatchError::new(&format!("Flag '{}' not found in local cache", flag_key))
    })?;

    let flag_value = evaluate_flag_dependency(flag, ctx)?;

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

/// Evaluate the flag a dependency filter references, or reuse its result from
/// earlier in the same top-level evaluation.
fn evaluate_flag_dependency(
    flag: &FeatureFlag,
    ctx: &PropertyMatchContext,
) -> Result<FlagValue, InconclusiveMatchError> {
    let results = ctx.dependency_results;
    let depth = ctx.nesting_depth + 1;

    if ctx
        .dependency_path
        .is_some_and(|path| path.contains(&flag.key))
    {
        results.borrow_mut().is_path_dependent = true;
        return Err(InconclusiveMatchError::new(&format!(
            "Flag '{}' is part of a dependency cycle",
            flag.key
        )));
    }

    if depth >= MAX_FLAG_DEPENDENCY_DEPTH {
        results.borrow_mut().is_path_dependent = true;
        return Err(InconclusiveMatchError::new(&format!(
            "Flag dependency chain is deeper than the limit of {}",
            MAX_FLAG_DEPENDENCY_DEPTH
        )));
    }

    // A cached result whose dependencies don't fit under the limit from here
    // is evaluated again, so it hits the limit exactly where a fresh
    // evaluation would.
    let cached = results.borrow().values.get(&flag.key).and_then(|cached| {
        (depth + cached.height < MAX_FLAG_DEPENDENCY_DEPTH)
            .then(|| (cached.result.clone(), depth + cached.height))
    });
    if let Some((result, deepest)) = cached {
        let mut results = results.borrow_mut();
        results.deepest_dependency = results.deepest_dependency.max(deepest);
        return result.map_err(|message| InconclusiveMatchError::new(&message));
    }

    let (outer_deepest, outer_is_path_dependent) = {
        let mut results = results.borrow_mut();
        if results.evaluations >= MAX_FLAG_DEPENDENCY_EVALUATIONS {
            results.is_path_dependent = true;
            return Err(InconclusiveMatchError::new(&format!(
                "Too many flag dependency evaluations, over the limit of {}",
                MAX_FLAG_DEPENDENCY_EVALUATIONS
            )));
        }
        results.evaluations += 1;
        let outer = (results.deepest_dependency, results.is_path_dependent);
        results.deepest_dependency = depth;
        results.is_path_dependent = false;
        outer
    };

    let result = match_feature_flag_in_context(
        flag,
        ctx.person_properties,
        ctx.evaluation,
        ctx.known_absent,
        ctx.metadata,
        ctx.dependency_path,
        results,
        depth,
    );

    let mut results = results.borrow_mut();
    let inner_deepest = results.deepest_dependency;
    let inner_is_path_dependent = results.is_path_dependent;
    if !inner_is_path_dependent {
        results.values.insert(
            flag.key.clone(),
            CachedDependency {
                result: result
                    .as_ref()
                    .map(FlagValue::clone)
                    .map_err(|error| error.message.clone()),
                height: inner_deepest - depth,
            },
        );
    }
    results.deepest_dependency = outer_deepest.max(inner_deepest);
    results.is_path_dependent = outer_is_path_dependent || inner_is_path_dependent;
    result
}

/// Parse a relative date string like "-7d", "7d", "-24h", "-2w", "-3m" or "-1y".
/// Returns the DateTime<Utc> that the relative date represents.
fn parse_relative_date(value: &str) -> Option<DateTime<Utc>> {
    parse_relative_date_at(value, Utc::now())
}

/// Relative date lookback from `now`, following the local evaluator spec and the
/// server's `relative_date.rs`: an optional leading `-`, a number below 10,000 and
/// one of `h`, `d`, `w`, `m` or `y`. Months and years are calendar steps that clamp
/// the day after every step, so March 31 minus two months is January 28.
fn parse_relative_date_at(value: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    static RELATIVE_DATE: OnceLock<Regex> = OnceLock::new();
    let captures = RELATIVE_DATE
        .get_or_init(|| Regex::new(r"^-?([0-9]+)([hdwmy])$").expect("valid relative date regex"))
        .captures(value.trim())?;
    let number: i64 = captures[1].parse().ok()?;
    // The cap also keeps the arithmetic below far away from chrono's limits.
    if number >= 10_000 {
        return None;
    }

    let naive = now.naive_utc();
    let result = match &captures[2] {
        "h" => naive - chrono::Duration::hours(number),
        "d" => naive - chrono::Duration::days(number),
        "w" => naive - chrono::Duration::weeks(number),
        "m" => subtract_months(naive, number)?,
        "y" => subtract_years(naive, number)?,
        _ => return None,
    };
    Some(result.and_utc())
}

/// Step back one month at a time and clamp the day after every step.
fn subtract_months(now: NaiveDateTime, months: i64) -> Option<NaiveDateTime> {
    // Count months from year 0 so stepping back never has to special-case January.
    let year_month = |index: i64| (index.div_euclid(12) as i32, index.rem_euclid(12) as u32 + 1);
    let mut index = i64::from(now.year()) * 12 + i64::from(now.month0());
    let mut day = now.day();
    for _ in 0..months {
        index -= 1;
        let (year, month) = year_month(index);
        day = day.min(days_in_month(year, month));
    }
    let (year, month) = year_month(index);
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    Some(date.and_time(now.time()))
}

/// Step back whole years. Only February 29 can clamp: the first step lands in a
/// common year, and the day stays the 28th after that.
fn subtract_years(now: NaiveDateTime, years: i64) -> Option<NaiveDateTime> {
    let day = if years > 0 && now.month() == 2 && now.day() == 29 {
        28
    } else {
        now.day()
    };
    let date = NaiveDate::from_ymd_opt(now.year() - years as i32, now.month(), day)?;
    Some(date.and_time(now.time()))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if NaiveDate::from_ymd_opt(year, 2, 29).is_some() => 29,
        2 => 28,
        _ => 31,
    }
}

/// Parse a date value from a string (ISO date, ISO datetime, or relative date)
fn parse_date_value(value: &serde_json::Value) -> Option<DateTime<Utc>> {
    let date_str = value.as_str()?;

    // Try relative date first (e.g., "-7d" or "7d")
    if let Some(dt) = parse_relative_date(date_str) {
        return Some(dt);
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

/// A parsed semantic version as (major, minor, patch)
type SemverTuple = (u64, u64, u64);

/// Parse a semantic version string into a (major, minor, patch) tuple.
///
/// Rules:
/// 1. Strip leading/trailing whitespace
/// 2. Strip `v` or `V` prefix (e.g., "v1.2.3" → "1.2.3")
/// 3. Strip pre-release and build metadata suffixes (split on `-` or `+`, take first part)
/// 4. Split on `.` and parse first 3 components as integers
/// 5. Default missing components to 0 (e.g., "1.2" → (1, 2, 0), "1" → (1, 0, 0))
/// 6. Ignore extra components beyond the third (e.g., "1.2.3.4" → (1, 2, 3))
/// 7. Return None for invalid input (empty string, non-numeric parts, leading dot,
///    or numeric components with leading zeros per semver 2.0.0 §2)
fn parse_semver(value: &str) -> Option<SemverTuple> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    // Strip v/V prefix
    let value = value
        .strip_prefix('v')
        .or_else(|| value.strip_prefix('V'))
        .unwrap_or(value);
    if value.is_empty() {
        return None;
    }

    // Strip pre-release/build metadata (everything after - or +)
    let value = value.split(['-', '+']).next().unwrap_or(value);
    if value.is_empty() {
        return None;
    }

    // Leading dot is invalid
    if value.starts_with('.') {
        return None;
    }

    // Split on dots and parse components
    let parts: Vec<&str> = value.split('.').collect();
    if parts.is_empty() {
        return None;
    }

    let major = parse_semver_numeric(parts.first()?)?;
    let minor = parts.get(1).map_or(Some(0), |s| parse_semver_numeric(s))?;
    let patch = parts.get(2).map_or(Some(0), |s| parse_semver_numeric(s))?;

    Some((major, minor, patch))
}

/// Parse a single semver numeric identifier.
///
/// Per semver 2.0.0 §2, numeric identifiers MUST NOT include leading zeros, so
/// "07" and "001" are rejected while the literal "0" remains valid.
fn parse_semver_numeric(part: &str) -> Option<u64> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if part.len() > 1 && part.starts_with('0') {
        return None;
    }
    part.parse().ok()
}

/// Parse a wildcard pattern like "1.*" or "1.2.*" and return (lower_bound, upper_bound)
/// Returns None if the pattern is invalid
fn parse_semver_wildcard(pattern: &str) -> Option<(SemverTuple, SemverTuple)> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return None;
    }

    // Strip v/V prefix
    let pattern = pattern
        .strip_prefix('v')
        .or_else(|| pattern.strip_prefix('V'))
        .unwrap_or(pattern);
    if pattern.is_empty() {
        return None;
    }

    let parts: Vec<&str> = pattern.split('.').collect();

    match parts.as_slice() {
        // "X.*" pattern
        [major_str, "*"] => {
            let major = parse_semver_numeric(major_str)?;
            Some(((major, 0, 0), (major + 1, 0, 0)))
        }
        // "X.Y.*" pattern
        [major_str, minor_str, "*"] => {
            let major = parse_semver_numeric(major_str)?;
            let minor = parse_semver_numeric(minor_str)?;
            Some(((major, minor, 0), (major, minor + 1, 0)))
        }
        _ => None,
    }
}

/// Compute bounds for tilde range: ~X.Y.Z means >=X.Y.Z and <X.(Y+1).0
fn compute_tilde_bounds(version: SemverTuple) -> (SemverTuple, SemverTuple) {
    let (major, minor, patch) = version;
    ((major, minor, patch), (major, minor + 1, 0))
}

/// Compute bounds for caret range per semver spec:
/// - ^X.Y.Z where X > 0: >=X.Y.Z <(X+1).0.0
/// - ^0.Y.Z where Y > 0: >=0.Y.Z <0.(Y+1).0
/// - ^0.0.Z: >=0.0.Z <0.0.(Z+1)
fn compute_caret_bounds(version: SemverTuple) -> (SemverTuple, SemverTuple) {
    let (major, minor, patch) = version;
    if major > 0 {
        ((major, minor, patch), (major + 1, 0, 0))
    } else if minor > 0 {
        ((0, minor, patch), (0, minor + 1, 0))
    } else {
        ((0, 0, patch), (0, 0, patch + 1))
    }
}

fn parse_target_semver(
    target_value: &serde_json::Value,
) -> Result<SemverTuple, InconclusiveMatchError> {
    let target_str = value_to_string(target_value);
    parse_semver(&target_str).ok_or_else(|| {
        InconclusiveMatchError::new(&format!(
            "Unable to parse target semver value: {:?}",
            target_value
        ))
    })
}

#[cfg(test)]
fn match_property(
    property: &Property,
    properties: &HashMap<String, serde_json::Value>,
) -> Result<bool, InconclusiveMatchError> {
    match_property_with_version(property, properties, 1, None)
}

/// Filter types matched against the person or group properties the caller
/// passes. Cohort and flag filters have their own matching. Any other type, such
/// as `person_metadata` (a value `/flags` reads from the persons table, not from
/// person properties), can't be evaluated locally: matching it against a caller
/// property with the same key could disagree with `/flags`.
fn check_property_filter_type(property: &Property) -> Result<(), InconclusiveMatchError> {
    match property.property_type.as_deref() {
        None | Some("") | Some("person") | Some("group") => Ok(()),
        Some(other) => Err(InconclusiveMatchError::new(&format!(
            "Can't evaluate a '{other}' filter on '{}' locally",
            property.key
        ))),
    }
}

fn match_property_with_version(
    property: &Property,
    properties: &HashMap<String, serde_json::Value>,
    property_matching_version: i64,
    known_absent_keys: Option<KnownAbsentKeys>,
) -> Result<bool, InconclusiveMatchError> {
    check_property_filter_type(property)?;
    if known_absent_keys.is_some_and(|keys| keys.contains(property)) {
        return match_known_absent_property(property);
    }

    let value = match properties.get(&property.key) {
        Some(v) => v,
        None => {
            return Err(InconclusiveMatchError::new(&format!(
                "Property '{}' not found in provided properties",
                property.key
            )));
        }
    };

    let parse_property_semver = || {
        let prop_str = value_to_string(value);
        parse_semver(&prop_str).ok_or_else(|| {
            InconclusiveMatchError::new(&format!(
                "Unable to parse property semver value for '{}': {:?}",
                property.key, value
            ))
        })
    };
    let parse_semver_operands = || {
        Ok((
            parse_property_semver()?,
            parse_target_semver(&property.value)?,
        ))
    };

    Ok(match property.operator.as_str() {
        "exact" => compute_exact_match(&property.value, value, property_matching_version),
        "is_not" => !compute_exact_match(&property.value, value, property_matching_version),
        "is_set" => true,      // We already know the property exists
        "is_not_set" => false, // We already know the property exists
        "icontains" => {
            let prop_str = value_to_string(value);
            let search_str = value_to_string(&property.value);
            prop_str
                .to_ascii_lowercase()
                .contains(&search_str.to_ascii_lowercase())
        }
        "not_icontains" => {
            let prop_str = value_to_string(value);
            let search_str = value_to_string(&property.value);
            !prop_str
                .to_ascii_lowercase()
                .contains(&search_str.to_ascii_lowercase())
        }
        "starts_with" => {
            let prop_str = value_to_string(value);
            let search_str = value_to_string(&property.value);
            prop_str
                .to_ascii_lowercase()
                .starts_with(&search_str.to_ascii_lowercase())
        }
        "not_starts_with" => {
            let prop_str = value_to_string(value);
            let search_str = value_to_string(&property.value);
            !prop_str
                .to_ascii_lowercase()
                .starts_with(&search_str.to_ascii_lowercase())
        }
        "ends_with" => {
            let prop_str = value_to_string(value);
            let search_str = value_to_string(&property.value);
            prop_str
                .to_ascii_lowercase()
                .ends_with(&search_str.to_ascii_lowercase())
        }
        "not_ends_with" => {
            let prop_str = value_to_string(value);
            let search_str = value_to_string(&property.value);
            !prop_str
                .to_ascii_lowercase()
                .ends_with(&search_str.to_ascii_lowercase())
        }
        "regex" | "not_regex" => {
            let prop_str = value_to_string(value);
            let regex_str = value_to_string(&property.value);
            get_cached_regex(&regex_str)?
                .map(|re| re.is_match(&prop_str) != (property.operator == "not_regex"))
                .unwrap_or(false)
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
        // Semver comparison operators
        "semver_eq" | "semver_neq" | "semver_gt" | "semver_gte" | "semver_lt" | "semver_lte" => {
            let (prop_version, target_version) = parse_semver_operands()?;

            match property.operator.as_str() {
                "semver_eq" => prop_version == target_version,
                "semver_neq" => prop_version != target_version,
                "semver_gt" => prop_version > target_version,
                "semver_gte" => prop_version >= target_version,
                "semver_lt" => prop_version < target_version,
                "semver_lte" => prop_version <= target_version,
                _ => unreachable!(),
            }
        }
        "semver_tilde" => {
            let (prop_version, target_version) = parse_semver_operands()?;
            let (lower, upper) = compute_tilde_bounds(target_version);
            prop_version >= lower && prop_version < upper
        }
        "semver_caret" => {
            let (prop_version, target_version) = parse_semver_operands()?;
            let (lower, upper) = compute_caret_bounds(target_version);
            prop_version >= lower && prop_version < upper
        }
        "semver_wildcard" => {
            let prop_version = parse_property_semver()?;
            let target_str = value_to_string(&property.value);

            let (lower, upper) = parse_semver_wildcard(&target_str).ok_or_else(|| {
                InconclusiveMatchError::new(&format!(
                    "Unable to parse target semver wildcard pattern: {:?}",
                    property.value
                ))
            })?;

            prop_version >= lower && prop_version < upper
        }
        unknown => {
            return Err(InconclusiveMatchError::new(&format!(
                "Unknown operator: {}",
                unknown
            )));
        }
    })
}

/// Result for a property whose key the caller listed as known absent. Like the
/// flags service, this never parses, compiles or validates the condition
/// value. Unknown operators and a JSON null condition value stay inconclusive,
/// so they still fall back to remote evaluation.
fn match_known_absent_property(property: &Property) -> Result<bool, InconclusiveMatchError> {
    let is_absent_match = match property.operator.as_str() {
        "is_not_set" | "is_not" | "not_icontains" | "not_starts_with" | "not_ends_with"
        | "not_regex" => true,
        "is_set" | "exact" | "icontains" | "starts_with" | "ends_with" | "regex" | "gt" | "gte"
        | "lt" | "lte" | "is_date_before" | "is_date_after" | "semver_eq" | "semver_neq"
        | "semver_gt" | "semver_gte" | "semver_lt" | "semver_lte" | "semver_tilde"
        | "semver_caret" | "semver_wildcard" => false,
        unknown => {
            return Err(InconclusiveMatchError::new(&format!(
                "Unknown operator: {}",
                unknown
            )));
        }
    };
    // The flags service reads a JSON null condition value as omitted, and an
    // omitted value leaves every operator but the presence ones inconclusive.
    if property.value.is_null() && !matches!(property.operator.as_str(), "is_set" | "is_not_set") {
        return Err(InconclusiveMatchError::new(&format!(
            "Property '{}' has no condition value for operator '{}'",
            property.key, property.operator
        )));
    }
    Ok(is_absent_match)
}

fn compute_exact_match(
    value: &serde_json::Value,
    override_value: &serde_json::Value,
    property_matching_version: i64,
) -> bool {
    if property_matching_version != 2 && is_truthy_or_falsy_property_value(value) {
        return is_truthy_property_value(value) == is_truthy_property_value(override_value);
    }

    if let Some(values) = value.as_array() {
        // Empty filters retain recursive legacy truthiness in both versions.
        if values.is_empty() {
            return is_truthy_property_value(override_value);
        }
        return values
            .iter()
            .any(|candidate| compare_values(candidate, override_value));
    }

    compare_values(value, override_value)
}

fn is_truthy_or_falsy_property_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(_) => true,
        serde_json::Value::String(value) => {
            value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false")
        }
        serde_json::Value::Array(values) => values.iter().all(is_truthy_or_falsy_property_value),
        _ => false,
    }
}

fn is_truthy_property_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(value) => *value,
        serde_json::Value::String(value) => value.eq_ignore_ascii_case("true"),
        serde_json::Value::Array(values) => values.iter().all(is_truthy_property_value),
        _ => false,
    }
}

fn compare_values(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    value_to_string(a).to_lowercase() == value_to_string(b).to_lowercase()
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

    /// Bucketing must stay bit-identical across SDKs, so pin known values.
    /// Generated with the Python SDK's algorithm:
    /// `int(hashlib.sha1(f"{key}.{distinct_id}{salt}".encode()).hexdigest()[:15], 16) / 0xfffffffffffffff`
    #[test]
    fn test_hash_key_matches_known_vectors() {
        for (key, distinct_id, salt, expected) in [
            ("test-flag", "user-123", TEST_SALT, 0.982_062_667_408_254_5),
            ("test-flag", "user-456", TEST_SALT, 0.695_145_973_300_181_1),
            (
                "beta-feature",
                "distinct_id",
                ROLLOUT_HASH_SALT,
                0.875_596_347_947_407_8,
            ),
            (
                "beta-feature",
                "distinct_id",
                VARIANT_HASH_SALT,
                0.228_302_715_824_090_7,
            ),
            (
                "multivariate-flag",
                "user_1",
                ROLLOUT_HASH_SALT,
                0.223_607_742_058_685_7,
            ),
        ] {
            assert_eq!(
                hash_key(key, distinct_id, salt),
                expected,
                "hash_key({key:?}, {distinct_id:?}, {salt:?}) drifted from the other SDKs"
            );
        }
    }

    #[test]
    fn test_simple_flag_match() {
        let flag = FeatureFlag {
            key: "test-flag".to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: vec![],
                    rollout_percentage: Some(100.0),
                    variant: None,
                    aggregation_group_type_index: None,
                }],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: false,
            },
        };

        let properties = HashMap::new();
        let result = match_feature_flag(
            &flag,
            "user-123",
            &properties,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        )
        .unwrap();
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
    fn test_property_case_folding_matches_flags_service() {
        let matches = |operator: &str, expected, actual| {
            let property = Property {
                key: "key".to_string(),
                value: expected,
                operator: operator.to_string(),
                property_type: None,
            };
            match_property(&property, &HashMap::from([("key".to_string(), actual)])).unwrap()
        };

        // Serde preserves a float's decimal point; exact matching must not collapse it to an integer.
        assert_eq!(value_to_string(&json!(323.0)), "323.0");

        let cases = [
            // Exact matching stringifies both sides and applies Unicode lowercase.
            ("exact", json!("PRO"), json!("pro"), true),
            ("exact", json!("Ä"), json!("ä"), true),
            ("exact", json!("ß"), json!("ss"), false),
            ("exact", json!("Σ"), json!("ς"), false),
            ("exact", json!("ΟΣ"), json!("ος"), true),
            ("exact", json!("ΟΣ"), json!("οσ"), false),
            ("exact", json!("İ"), json!("i\u{0307}"), true),
            ("exact", json!("İ"), json!("i"), false),
            ("exact", json!(323), json!("323"), true),
            ("exact", json!(["FREE", "PRÖ"]), json!("prö"), true),
            ("is_not", json!(["FREE", "PRÖ"]), json!("prö"), false),
            ("is_not", json!(["FREE", "PRÖ"]), json!("team"), true),
            ("exact", json!("323.0"), json!(323.0), true),
            ("exact", json!("323"), json!(323.0), false),
            // Substring and anchored operators fold ASCII only.
            ("icontains", json!("ADMIN"), json!("admin-user"), true),
            ("icontains", json!("Ä"), json!("äbc"), false),
            ("not_icontains", json!("Ä"), json!("äbc"), true),
            ("starts_with", json!("Ä"), json!("äbc"), false),
            ("not_starts_with", json!("Ä"), json!("äbc"), true),
            ("ends_with", json!("Ä"), json!("bcä"), false),
            ("not_ends_with", json!("Ä"), json!("bcä"), true),
        ];

        for (operator, expected, actual, should_match) in cases {
            let result = matches(operator, expected.clone(), actual.clone());
            assert_eq!(
                result, should_match,
                "operator {operator} comparing {actual} against {expected}"
            );
        }
    }

    #[test]
    fn test_exact_boolean_coercion_matches_flags_service() {
        let matches = |operator: &str, expected, actual| {
            let property = Property {
                key: "key".to_string(),
                value: expected,
                operator: operator.to_string(),
                property_type: None,
            };
            match_property(&property, &HashMap::from([("key".to_string(), actual)])).unwrap()
        };

        let cases = [
            // Boolean-like filters compare aggregate truthiness before array membership.
            (json!(false), json!("banana"), true),
            (json!("false"), json!(0), true),
            (json!(["false"]), json!(null), true),
            (json!(["true", "false"]), json!("true"), false),
            (json!(["true", "false"]), json!("pro"), true),
            // Empty arrays are boolean-like and truthy because all() on an empty iterator is true.
            (json!([]), json!(true), true),
            (json!([]), json!("true"), true),
            (json!([]), json!([]), true),
            (json!([]), json!([true]), true),
            (json!([]), json!(false), false),
            (json!([]), json!("banana"), false),
            // Non-boolean-like arrays retain ordinary ANY membership.
            (json!(["FREE", "PRO"]), json!("pro"), true),
            (json!(["FREE", "PRO"]), json!("team"), false),
        ];

        for (expected, actual, exact_match) in cases {
            assert_eq!(
                matches("exact", expected.clone(), actual.clone()),
                exact_match,
                "exact comparing {actual} against {expected}"
            );
            assert_eq!(
                matches("is_not", expected.clone(), actual.clone()),
                !exact_match,
                "is_not comparing {actual} against {expected}"
            );
        }
    }

    #[test]
    fn test_null_property_operator_defaults_to_exact() {
        let prop: Property = serde_json::from_value(json!({
            "key": "country",
            "value": "US",
            "operator": null,
            "type": "person"
        }))
        .unwrap();

        assert_eq!(prop.operator, "exact");
    }

    #[test]
    fn test_multivariate_variants() {
        let mut flag = FeatureFlag {
            key: "test-flag".to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: vec![],
                    rollout_percentage: Some(100.0),
                    variant: None,
                    aggregation_group_type_index: None,
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
                aggregation_group_type_index: None,
                early_exit: false,
            },
        };

        // Independently computed SHA-1 variant buckets: user-0 = 0.0334, user-1 = 0.5658.
        for (user, override_variant, expected) in [
            ("user-0", None, "control"),
            ("user-1", None, "test"),
            ("user-0", Some("test"), "test"),
            ("user-1", Some("control"), "control"),
            ("user-0", Some("unknown"), "control"),
            ("user-1", Some("unknown"), "test"),
        ] {
            flag.filters.groups[0].variant = override_variant.map(str::to_string);
            let result = match_feature_flag(
                &flag,
                user,
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
            )
            .unwrap();
            assert_eq!(
                result,
                FlagValue::String(expected.into()),
                "{user}, {override_variant:?}"
            );
        }

        let variants = &mut flag.filters.multivariate.as_mut().unwrap().variants;
        variants[0].rollout_percentage = 0.0;
        variants[1].rollout_percentage = 50.0;
        assert_eq!(get_matching_variant(&flag, "user-0"), Some("test".into()));
        assert_eq!(get_matching_variant(&flag, "user-1"), None);
    }

    #[test]
    fn test_inactive_flag() {
        let flag = FeatureFlag {
            key: "inactive-flag".to_string(),
            active: false,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: vec![],
                    rollout_percentage: Some(100.0),
                    variant: None,
                    aggregation_group_type_index: None,
                }],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: false,
            },
        };

        let properties = HashMap::new();
        let result = match_feature_flag(
            &flag,
            "user-123",
            &properties,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(result, FlagValue::Boolean(false));
    }

    #[test]
    fn test_rollout_percentage() {
        let flag = FeatureFlag {
            key: "rollout-flag".to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: vec![],
                    rollout_percentage: Some(30.0), // 30% rollout
                    variant: None,
                    aggregation_group_type_index: None,
                }],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: false,
            },
        };

        let properties = HashMap::new();

        // Test with multiple users to ensure distribution
        let mut enabled_count = 0;
        for i in 0..1000 {
            let result = match_feature_flag(
                &flag,
                &format!("user-{}", i),
                &properties,
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
            )
            .unwrap();
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
    fn test_starts_with_operator() {
        let prop = Property {
            key: "name".to_string(),
            value: json!("Val"),
            operator: "starts_with".to_string(),
            property_type: None,
        };

        // Case-insensitive match anchored to the start.
        let mut properties = HashMap::new();
        properties.insert("name".to_string(), json!("value"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("name".to_string(), json!("VALUE"));
        assert!(match_property(&prop, &properties).unwrap());

        // Substring present but not at the start.
        properties.insert("name".to_string(), json!("prevalue"));
        assert!(!match_property(&prop, &properties).unwrap());

        properties.insert("name".to_string(), json!("Alakazam"));
        assert!(!match_property(&prop, &properties).unwrap());

        // Numeric property values are stringified before matching.
        let numeric_prop = Property {
            key: "name".to_string(),
            value: json!("3"),
            operator: "starts_with".to_string(),
            property_type: None,
        };

        properties.insert("name".to_string(), json!(323));
        assert!(match_property(&numeric_prop, &properties).unwrap());

        properties.insert("name".to_string(), json!(123));
        assert!(!match_property(&numeric_prop, &properties).unwrap());

        let negated_prop = Property {
            key: "name".to_string(),
            value: json!("Val"),
            operator: "not_starts_with".to_string(),
            property_type: None,
        };

        properties.insert("name".to_string(), json!("value"));
        assert!(!match_property(&negated_prop, &properties).unwrap());

        properties.insert("name".to_string(), json!("prevalue"));
        assert!(match_property(&negated_prop, &properties).unwrap());

        // Missing property is inconclusive.
        assert!(match_property(&prop, &HashMap::new()).is_err());
    }

    #[test]
    fn test_ends_with_operator() {
        let prop = Property {
            key: "name".to_string(),
            value: json!("lUe"),
            operator: "ends_with".to_string(),
            property_type: None,
        };

        // Case-insensitive match anchored to the end.
        let mut properties = HashMap::new();
        properties.insert("name".to_string(), json!("value"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("name".to_string(), json!("VALUE"));
        assert!(match_property(&prop, &properties).unwrap());

        // Substring present but not at the end.
        properties.insert("name".to_string(), json!("value2"));
        assert!(!match_property(&prop, &properties).unwrap());

        properties.insert("name".to_string(), json!("Alakazam"));
        assert!(!match_property(&prop, &properties).unwrap());

        // Numeric property values are stringified before matching.
        let numeric_prop = Property {
            key: "name".to_string(),
            value: json!("3"),
            operator: "ends_with".to_string(),
            property_type: None,
        };

        properties.insert("name".to_string(), json!(323));
        assert!(match_property(&numeric_prop, &properties).unwrap());

        properties.insert("name".to_string(), json!(321));
        assert!(!match_property(&numeric_prop, &properties).unwrap());

        let negated_prop = Property {
            key: "name".to_string(),
            value: json!("lUe"),
            operator: "not_ends_with".to_string(),
            property_type: None,
        };

        properties.insert("name".to_string(), json!("value"));
        assert!(!match_property(&negated_prop, &properties).unwrap());

        properties.insert("name".to_string(), json!("value2"));
        assert!(match_property(&negated_prop, &properties).unwrap());

        // Missing property is inconclusive.
        assert!(match_property(&prop, &HashMap::new()).is_err());
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
        for value in [
            json!(null),
            json!(false),
            json!(0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            properties.insert("email".to_string(), value);
            assert!(match_property(&prop, &properties).unwrap());
        }

        properties.remove("email");
        assert!(matches!(
            match_property(&prop, &properties),
            Err(InconclusiveMatchError { .. })
        ));
    }

    #[test]
    fn test_property_value_may_be_omitted_only_for_presence_operators() {
        for operator in ["is_set", "is_not_set"] {
            let prop: Property = serde_json::from_value(
                json!({"key": "plan", "type": "person", "operator": operator}),
            )
            .unwrap();
            assert!(prop.value.is_null(), "{}", operator);
        }

        // An omitted operator defaults to `exact`, which needs a value too.
        for operator in [Some("exact"), Some("is_not"), Some("regex"), None] {
            let mut raw = json!({"key": "plan", "type": "person"});
            if let Some(operator) = operator {
                raw["operator"] = json!(operator);
            }
            let error = serde_json::from_value::<Property>(raw).unwrap_err();
            assert!(error.to_string().contains("missing field `value`"));
        }

        // An explicit null is a value, not an omission.
        let prop: Property =
            serde_json::from_value(json!({"key": "plan", "value": null, "operator": "exact"}))
                .unwrap();
        assert!(prop.value.is_null());
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
        for value in [
            json!(null),
            json!(false),
            json!(0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            properties.insert("phone".to_string(), value);
            assert!(!match_property(&prop, &properties).unwrap());
        }

        properties.remove("phone");
        assert!(matches!(
            match_property(&prop, &properties),
            Err(InconclusiveMatchError { .. })
        ));
    }

    #[test]
    fn test_empty_groups() {
        let flag = FeatureFlag {
            key: "empty-groups".to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: false,
            },
        };

        let properties = HashMap::new();
        let result = match_feature_flag(
            &flag,
            "user-123",
            &properties,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        )
        .unwrap();
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

    fn utc(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn test_relative_months_and_years_clamp_after_every_step() {
        let cases = [
            ("2025-03-31T12:00:00Z", "-1m", "2025-02-28T12:00:00Z"),
            ("2025-05-31T12:00:00Z", "-1m", "2025-04-30T12:00:00Z"),
            ("2024-03-31T12:00:00Z", "-1m", "2024-02-29T12:00:00Z"),
            ("2025-03-31T12:00:00Z", "-2m", "2025-01-28T12:00:00Z"),
            ("2025-03-31T12:00:00Z", "-13m", "2024-02-28T12:00:00Z"),
            ("2025-12-31T12:00:00Z", "-10m", "2025-02-28T12:00:00Z"),
            ("2019-01-29T12:00:00Z", "-12m", "2018-01-28T12:00:00Z"),
            ("2025-01-08T00:00:00Z", "-12m", "2024-01-08T00:00:00Z"),
            ("2019-01-29T12:00:00Z", "-1y", "2018-01-29T12:00:00Z"),
            ("2024-02-29T12:00:00Z", "-1y", "2023-02-28T12:00:00Z"),
            ("2024-02-29T12:00:00Z", "-4y", "2020-02-28T12:00:00Z"),
            ("2025-03-31T12:00:00Z", "-0m", "2025-03-31T12:00:00Z"),
        ];
        for (now, value, expected) in cases.iter() {
            assert_eq!(
                parse_relative_date_at(value, utc(now)),
                Some(utc(expected)),
                "{} from {}",
                value,
                now
            );
        }
    }

    #[test]
    fn test_relative_date_grammar() {
        let now = utc("2025-01-08T00:00:00Z");
        assert_eq!(
            parse_relative_date_at("1d", now),
            Some(utc("2025-01-07T00:00:00Z"))
        );
        assert_eq!(
            parse_relative_date_at("-1h", now),
            Some(utc("2025-01-07T23:00:00Z"))
        );
        assert_eq!(
            parse_relative_date_at("-1w", now),
            Some(utc("2025-01-01T00:00:00Z"))
        );
        assert_eq!(
            parse_relative_date_at("1y", now),
            Some(utc("2024-01-08T00:00:00Z"))
        );
        assert!(parse_relative_date_at("-9999y", now).is_some());
        for value in [
            "10000d",
            "-10000h",
            "-999999999999d",
            "99999999999999999999d",
            "7",
            "d",
            "-d",
            "7D",
            "7s",
            "1.5d",
            "--7d",
            "+7d",
            "",
        ]
        .iter()
        {
            assert_eq!(parse_relative_date_at(value, now), None, "{}", value);
        }
    }

    #[test]
    fn test_is_date_after_with_unsigned_and_out_of_range_relative_dates() {
        let mut properties = HashMap::new();
        properties.insert(
            "last_seen".to_string(),
            json!(chrono::Utc::now().to_rfc3339()),
        );
        let prop = |value: &str| Property {
            key: "last_seen".to_string(),
            value: json!(value),
            operator: "is_date_after".to_string(),
            property_type: None,
        };

        assert!(match_property(&prop("7d"), &properties).unwrap());
        // Out of range is inconclusive, and the subtraction must not panic.
        assert!(match_property(&prop("-10000d"), &properties).is_err());
        assert!(match_property(&prop("-999999999999d"), &properties).is_err());
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

    // ==================== Tests for person_metadata filters ====================

    // A cohort can filter on Person metadata > First seen (type "person_metadata",
    // key "created_at"). /flags reads that value from the persons table and
    // ignores a created_at person property, so local evaluation must not match it
    // against one. See https://github.com/PostHog/posthog/issues/137503.

    fn first_seen_filter(property_type: &str) -> Property {
        Property {
            key: "created_at".to_string(),
            value: json!("2025-01-01"),
            operator: "is_date_after".to_string(),
            property_type: Some(property_type.to_string()),
        }
    }

    fn created_in_2026() -> HashMap<String, serde_json::Value> {
        HashMap::from([("created_at".to_string(), json!("2026-06-01"))])
    }

    #[test]
    fn person_metadata_filter_is_inconclusive() {
        assert!(match_property(&first_seen_filter("person_metadata"), &created_in_2026()).is_err());
    }

    #[test]
    fn untyped_and_person_filters_on_created_at_still_match() {
        for property_type in [json!("person"), json!(null), json!("")] {
            let filter: Property = serde_json::from_value(json!({
                "key": "created_at",
                "type": property_type,
                "operator": "is_date_after",
                "value": "2025-01-01"
            }))
            .unwrap();
            assert!(match_property(&filter, &created_in_2026()).unwrap());
        }
        let mut untyped = first_seen_filter("person");
        untyped.property_type = None;
        assert!(match_property(&untyped, &created_in_2026()).unwrap());
    }

    fn first_seen_cohort_flag(
        cohorts: &HashMap<String, CohortDefinition>,
        operator: &str,
    ) -> Result<FlagValue, InconclusiveMatchError> {
        let flag: FeatureFlag = serde_json::from_value(json!({
            "key": "first-seen-flag",
            "active": true,
            "filters": {
                "groups": [{
                    "properties": [{"key": "id", "type": "cohort", "operator": operator, "value": 7}],
                    "rollout_percentage": 100
                }]
            }
        }))
        .unwrap();
        let ctx = EvaluationContext {
            property_matching_version: 1,
            cohorts,
            flags: &HashMap::new(),
            distinct_id: "user-123",
            groups: &HashMap::new(),
            group_properties: &HashMap::new(),
            group_type_mapping: &HashMap::new(),
        };
        let mut person_properties = created_in_2026();
        person_properties.insert("plan".to_string(), json!("free"));
        match_feature_flag_with_context(&flag, &person_properties, &ctx)
    }

    fn cohort_7(values: serde_json::Value) -> HashMap<String, CohortDefinition> {
        HashMap::from([(
            "7".to_string(),
            CohortDefinition {
                id: "7".to_string(),
                properties: json!({"type": "AND", "values": values}),
            },
        )])
    }

    #[test]
    fn person_metadata_filter_in_a_cohort_is_inconclusive() {
        let leaf = json!({"key": "created_at", "type": "person_metadata", "operator": "is_date_after", "value": "2025-01-01"});
        let negated = json!({"key": "created_at", "type": "person_metadata", "operator": "is_date_after", "value": "2025-01-01", "negation": true});

        assert!(first_seen_cohort_flag(&cohort_7(json!([leaf.clone()])), "in").is_err());
        assert!(first_seen_cohort_flag(&cohort_7(json!([leaf])), "not_in").is_err());
        assert!(first_seen_cohort_flag(&cohort_7(json!([negated])), "in").is_err());
    }

    #[test]
    fn another_cohort_leaf_decides_the_group() {
        let cohorts = cohort_7(json!([
            {"key": "created_at", "type": "person_metadata", "operator": "is_date_after", "value": "2025-01-01"},
            {"key": "plan", "type": "person", "operator": "exact", "value": "pro"}
        ]));

        assert_eq!(
            first_seen_cohort_flag(&cohorts, "in").unwrap(),
            FlagValue::Boolean(false)
        );
    }

    fn flag_with_groups(groups: serde_json::Value) -> FeatureFlag {
        serde_json::from_value(json!({
            "key": "first-seen-flag",
            "active": true,
            "filters": {"groups": groups}
        }))
        .unwrap()
    }

    fn evaluate(
        flag: &FeatureFlag,
        person_properties: &HashMap<String, serde_json::Value>,
    ) -> Result<FlagValue, InconclusiveMatchError> {
        match_feature_flag(
            flag,
            "user-123",
            person_properties,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        )
    }

    #[test]
    fn unsupported_filter_types_make_the_flag_inconclusive() {
        for property_type in ["person_metadata", "future_type"] {
            let flag = flag_with_groups(json!([{
                "properties": [{"key": "created_at", "type": property_type, "operator": "is_date_after", "value": "2025-01-01"}],
                "rollout_percentage": 100
            }]));
            assert!(
                evaluate(&flag, &created_in_2026()).is_err(),
                "{}",
                property_type
            );
        }
    }

    #[test]
    fn a_false_filter_decides_a_condition_with_an_unsupported_filter() {
        let flag = flag_with_groups(json!([{
            "properties": [
                {"key": "region", "type": "future_type", "operator": "exact", "value": "eu"},
                {"key": "plan", "type": "person", "operator": "exact", "value": "pro"}
            ],
            "rollout_percentage": 100
        }]));
        let free = HashMap::from([("plan".to_string(), json!("free"))]);

        assert_eq!(evaluate(&flag, &free).unwrap(), FlagValue::Boolean(false));
    }

    #[test]
    fn a_later_condition_can_match_after_an_unsupported_filter() {
        let flag = flag_with_groups(json!([
            {
                "properties": [{"key": "created_at", "type": "person_metadata", "operator": "is_date_after", "value": "2025-01-01"}],
                "rollout_percentage": 100
            },
            {"properties": [], "rollout_percentage": 100}
        ]));

        assert_eq!(
            evaluate(&flag, &created_in_2026()).unwrap(),
            FlagValue::Boolean(true)
        );
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
            property_matching_version: 1,
            cohorts: &cohorts,
            flags: &HashMap::new(),
            distinct_id: "user-123",
            groups: &HashMap::new(),
            group_properties: &HashMap::new(),
            group_type_mapping: &HashMap::new(),
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
            property_matching_version: 1,
            cohorts: &cohorts,
            flags: &HashMap::new(),
            distinct_id: "user-123",
            groups: &HashMap::new(),
            group_properties: &HashMap::new(),
            group_type_mapping: &HashMap::new(),
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
            property_matching_version: 1,
            cohorts: &cohorts,
            flags: &HashMap::new(),
            distinct_id: "user-123",
            groups: &HashMap::new(),
            group_properties: &HashMap::new(),
            group_type_mapping: &HashMap::new(),
        };

        let result = match_property_with_context(&prop, &properties, &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("Cohort"));
    }

    /// Build an `EvaluationContext` over just a cohort map (no flags/groups),
    /// which is all cohort-membership tests need.
    fn cohort_ctx(cohorts: &HashMap<String, CohortDefinition>) -> EvaluationContext<'_> {
        EvaluationContext {
            property_matching_version: 1,
            cohorts,
            flags: EMPTY_FLAGS.get_or_init(HashMap::new),
            distinct_id: "user-123",
            groups: EMPTY_GROUPS.get_or_init(HashMap::new),
            group_properties: EMPTY_GROUP_PROPS.get_or_init(HashMap::new),
            group_type_mapping: EMPTY_GROUP_MAPPING.get_or_init(HashMap::new),
        }
    }

    static EMPTY_FLAGS: OnceLock<HashMap<String, FeatureFlag>> = OnceLock::new();
    static EMPTY_GROUPS: OnceLock<HashMap<String, String>> = OnceLock::new();
    static EMPTY_GROUP_PROPS: OnceLock<HashMap<String, HashMap<String, serde_json::Value>>> =
        OnceLock::new();
    static EMPTY_GROUP_MAPPING: OnceLock<HashMap<String, String>> = OnceLock::new();

    /// A cohort whose properties are an `OR` group must match when *either*
    /// branch matches — the old flatten-and-AND logic broke this entirely.
    #[test]
    fn test_cohort_or_group() {
        let mut cohorts = HashMap::new();
        cohorts.insert(
            "cohort_or".to_string(),
            CohortDefinition {
                id: "cohort_or".to_string(),
                properties: json!({
                    "type": "OR",
                    "values": [
                        {"key": "country", "value": "US", "operator": "exact", "type": "person"},
                        {"key": "country", "value": "CA", "operator": "exact", "type": "person"},
                    ],
                }),
            },
        );

        let prop = Property {
            key: "$cohort".to_string(),
            value: json!("cohort_or"),
            operator: "in".to_string(),
            property_type: Some("cohort".to_string()),
        };

        let ctx = cohort_ctx(&cohorts);

        // Either branch of the OR is enough.
        let mut properties = HashMap::new();
        properties.insert("country".to_string(), json!("US"));
        assert!(match_property_with_context(&prop, &properties, &ctx).unwrap());

        properties.insert("country".to_string(), json!("CA"));
        assert!(match_property_with_context(&prop, &properties, &ctx).unwrap());

        // Neither branch matches.
        properties.insert("country".to_string(), json!("UK"));
        assert!(!match_property_with_context(&prop, &properties, &ctx).unwrap());
    }

    /// A nested group inside an `AND` must be evaluated as a whole group, not
    /// collapsed to its first property (the old `.next()` bug).
    #[test]
    fn test_cohort_nested_and_of_or() {
        let mut cohorts = HashMap::new();
        cohorts.insert(
            "cohort_nested".to_string(),
            CohortDefinition {
                id: "cohort_nested".to_string(),
                properties: json!({
                    "type": "AND",
                    "values": [
                        {"key": "plan", "value": "paid", "operator": "exact", "type": "person"},
                        {
                            "type": "OR",
                            "values": [
                                {"key": "country", "value": "US", "operator": "exact", "type": "person"},
                                {"key": "country", "value": "CA", "operator": "exact", "type": "person"},
                            ],
                        },
                    ],
                }),
            },
        );

        let prop = Property {
            key: "$cohort".to_string(),
            value: json!("cohort_nested"),
            operator: "in".to_string(),
            property_type: Some("cohort".to_string()),
        };

        let ctx = cohort_ctx(&cohorts);

        // paid + (US or CA) → in cohort.
        let mut properties = HashMap::new();
        properties.insert("plan".to_string(), json!("paid"));
        properties.insert("country".to_string(), json!("CA"));
        assert!(match_property_with_context(&prop, &properties, &ctx).unwrap());

        // paid but country outside the OR → not in cohort. Under the old
        // first-property-only logic the OR group was dropped and this matched.
        properties.insert("country".to_string(), json!("UK"));
        assert!(!match_property_with_context(&prop, &properties, &ctx).unwrap());

        // Wrong plan → the leading AND term fails.
        properties.insert("plan".to_string(), json!("free"));
        properties.insert("country".to_string(), json!("US"));
        assert!(!match_property_with_context(&prop, &properties, &ctx).unwrap());
    }

    /// A cohort referenced from inside another cohort's group is resolved
    /// recursively, honoring the `negation` flag.
    #[test]
    fn test_cohort_nested_cohort_reference() {
        let mut cohorts = HashMap::new();
        cohorts.insert(
            "child".to_string(),
            CohortDefinition {
                id: "child".to_string(),
                properties: json!({
                    "type": "AND",
                    "values": [
                        {"key": "country", "value": "US", "operator": "exact", "type": "person"},
                    ],
                }),
            },
        );
        cohorts.insert(
            "parent".to_string(),
            CohortDefinition {
                id: "parent".to_string(),
                properties: json!({
                    "type": "AND",
                    "values": [
                        {"type": "cohort", "value": "child", "negation": false},
                    ],
                }),
            },
        );

        let prop = Property {
            key: "$cohort".to_string(),
            value: json!("parent"),
            operator: "in".to_string(),
            property_type: Some("cohort".to_string()),
        };

        let ctx = cohort_ctx(&cohorts);

        let mut properties = HashMap::new();
        properties.insert("country".to_string(), json!("US"));
        assert!(match_property_with_context(&prop, &properties, &ctx).unwrap());

        properties.insert("country".to_string(), json!("UK"));
        assert!(!match_property_with_context(&prop, &properties, &ctx).unwrap());
    }

    /// A cohort cycle must surface as inconclusive so the flag falls back to
    /// server-side evaluation. Recursing until the stack runs out aborts the
    /// process, which takes the caller's whole server down.
    #[test]
    fn test_cyclic_cohort_reference_is_inconclusive() {
        let mut cohorts = HashMap::new();
        cohorts.insert(
            "a".to_string(),
            CohortDefinition {
                id: "a".to_string(),
                properties: json!({
                    "type": "AND",
                    "values": [{"type": "cohort", "value": "b", "negation": false}],
                }),
            },
        );
        cohorts.insert(
            "b".to_string(),
            CohortDefinition {
                id: "b".to_string(),
                properties: json!({
                    "type": "AND",
                    "values": [{"type": "cohort", "value": "a", "negation": false}],
                }),
            },
        );

        let prop = Property {
            key: "$cohort".to_string(),
            value: json!("a"),
            operator: "in".to_string(),
            property_type: Some("cohort".to_string()),
        };
        let ctx = cohort_ctx(&cohorts);

        let error = match_property_with_context(&prop, &HashMap::new(), &ctx)
            .expect_err("a cohort cycle must not resolve locally");
        assert!(
            error.to_string().contains("cycle"),
            "error should name the cycle, got: {}",
            error
        );
    }

    /// A cycle guard alone only bounds repeats, not depth. A chain of distinct
    /// cohorts is acyclic, so it never trips the cycle check and recurses once
    /// per link. Each cohort is its own shallow map entry, so the manifest also
    /// clears serde_json's nesting limit on the way in.
    #[test]
    fn test_deep_acyclic_cohort_chain_is_inconclusive() {
        let chain_len = MAX_COHORT_RESOLUTION_DEPTH + 50;
        let mut cohorts = HashMap::new();
        for link in 0..chain_len {
            let properties = if link == chain_len - 1 {
                json!({
                    "type": "AND",
                    "values": [
                        {"key": "country", "value": "US", "operator": "exact", "type": "person"},
                    ],
                })
            } else {
                json!({
                    "type": "AND",
                    "values": [{"type": "cohort", "value": (link + 1).to_string()}],
                })
            };
            cohorts.insert(
                link.to_string(),
                CohortDefinition {
                    id: link.to_string(),
                    properties,
                },
            );
        }

        let prop = Property {
            key: "$cohort".to_string(),
            value: json!("0"),
            operator: "in".to_string(),
            property_type: Some("cohort".to_string()),
        };
        let ctx = cohort_ctx(&cohorts);
        let mut properties = HashMap::new();
        properties.insert("country".to_string(), json!("US"));

        assert!(
            match_property_with_context(&prop, &properties, &ctx).is_err(),
            "a cohort chain deeper than the limit must not resolve locally"
        );
    }

    /// The depth bound has to leave realistic nesting alone.
    #[test]
    fn test_cohort_chain_within_the_depth_limit_still_resolves() {
        let chain_len = 10;
        let mut cohorts = HashMap::new();
        for link in 0..chain_len {
            let properties = if link == chain_len - 1 {
                json!({
                    "type": "AND",
                    "values": [
                        {"key": "country", "value": "US", "operator": "exact", "type": "person"},
                    ],
                })
            } else {
                json!({
                    "type": "AND",
                    "values": [{"type": "cohort", "value": (link + 1).to_string()}],
                })
            };
            cohorts.insert(
                link.to_string(),
                CohortDefinition {
                    id: link.to_string(),
                    properties,
                },
            );
        }

        let prop = Property {
            key: "$cohort".to_string(),
            value: json!("0"),
            operator: "in".to_string(),
            property_type: Some("cohort".to_string()),
        };
        let ctx = cohort_ctx(&cohorts);
        let mut properties = HashMap::new();
        properties.insert("country".to_string(), json!("US"));

        assert!(match_property_with_context(&prop, &properties, &ctx).unwrap());
    }

    #[test]
    fn test_self_referencing_cohort_is_inconclusive() {
        let mut cohorts = HashMap::new();
        cohorts.insert(
            "loop".to_string(),
            CohortDefinition {
                id: "loop".to_string(),
                properties: json!({
                    "type": "AND",
                    "values": [{"type": "cohort", "value": "loop", "negation": false}],
                }),
            },
        );

        let prop = Property {
            key: "$cohort".to_string(),
            value: json!("loop"),
            operator: "in".to_string(),
            property_type: Some("cohort".to_string()),
        };
        let ctx = cohort_ctx(&cohorts);

        assert!(match_property_with_context(&prop, &HashMap::new(), &ctx).is_err());
    }

    /// Referencing the same cohort twice down different branches is a diamond,
    /// not a cycle. Tracking visited IDs for the whole evaluation instead of the
    /// active path would wrongly reject this.
    #[test]
    fn test_repeated_cohort_reference_is_not_a_cycle() {
        let mut cohorts = HashMap::new();
        cohorts.insert(
            "shared".to_string(),
            CohortDefinition {
                id: "shared".to_string(),
                properties: json!({
                    "type": "AND",
                    "values": [
                        {"key": "country", "value": "US", "operator": "exact", "type": "person"},
                    ],
                }),
            },
        );
        for branch in ["left", "right"] {
            cohorts.insert(
                branch.to_string(),
                CohortDefinition {
                    id: branch.to_string(),
                    properties: json!({
                        "type": "AND",
                        "values": [{"type": "cohort", "value": "shared", "negation": false}],
                    }),
                },
            );
        }
        cohorts.insert(
            "parent".to_string(),
            CohortDefinition {
                id: "parent".to_string(),
                properties: json!({
                    "type": "AND",
                    "values": [
                        {"type": "cohort", "value": "left", "negation": false},
                        {"type": "cohort", "value": "right", "negation": false},
                    ],
                }),
            },
        );

        let prop = Property {
            key: "$cohort".to_string(),
            value: json!("parent"),
            operator: "in".to_string(),
            property_type: Some("cohort".to_string()),
        };
        let ctx = cohort_ctx(&cohorts);

        let mut properties = HashMap::new();
        properties.insert("country".to_string(), json!("US"));
        assert!(match_property_with_context(&prop, &properties, &ctx).unwrap());

        properties.insert("country".to_string(), json!("UK"));
        assert!(!match_property_with_context(&prop, &properties, &ctx).unwrap());
    }

    #[test]
    fn test_cohort_leaf_negation() {
        let mut cohorts = HashMap::new();
        cohorts.insert(
            "negated_leaf".to_string(),
            CohortDefinition {
                id: "negated_leaf".to_string(),
                properties: json!({
                    "type": "AND",
                    "values": [
                        {
                            "key": "country",
                            "value": "US",
                            "operator": "exact",
                            "type": "person",
                            "negation": true
                        },
                    ],
                }),
            },
        );

        let prop = Property {
            key: "$cohort".to_string(),
            value: json!("negated_leaf"),
            operator: "in".to_string(),
            property_type: Some("cohort".to_string()),
        };
        let ctx = cohort_ctx(&cohorts);
        let mut properties = HashMap::new();

        properties.insert("country".to_string(), json!("US"));
        assert!(!match_property_with_context(&prop, &properties, &ctx).unwrap());

        properties.insert("country".to_string(), json!("UK"));
        assert!(match_property_with_context(&prop, &properties, &ctx).unwrap());
    }

    #[test]
    fn test_missing_nested_cohort_is_not_suppressed() {
        let missing_cohort = json!({"type": "cohort", "value": "missing"});
        let country_leaf = json!({
            "key": "country",
            "value": "US",
            "operator": "exact",
            "type": "person"
        });
        let mut cohorts = HashMap::new();
        cohorts.insert(
            "or_parent".to_string(),
            CohortDefinition {
                id: "or_parent".to_string(),
                properties: json!({
                    "type": "OR",
                    "values": [country_leaf.clone(), missing_cohort.clone()],
                }),
            },
        );
        cohorts.insert(
            "and_parent".to_string(),
            CohortDefinition {
                id: "and_parent".to_string(),
                properties: json!({
                    "type": "AND",
                    "values": [country_leaf, missing_cohort],
                }),
            },
        );

        let ctx = cohort_ctx(&cohorts);
        for (cohort_id, country) in [("or_parent", "US"), ("and_parent", "UK")] {
            let prop = Property {
                key: "$cohort".to_string(),
                value: json!(cohort_id),
                operator: "in".to_string(),
                property_type: Some("cohort".to_string()),
            };
            let properties = HashMap::from([("country".to_string(), json!(country))]);

            assert!(match_property_with_context(&prop, &properties, &ctx).is_err());
        }
    }

    #[test]
    fn test_malformed_cohort_groups_are_inconclusive() {
        let mut cohorts = HashMap::new();
        cohorts.insert(
            "scalar".to_string(),
            CohortDefinition {
                id: "scalar".to_string(),
                properties: json!("invalid"),
            },
        );
        cohorts.insert(
            "object_values".to_string(),
            CohortDefinition {
                id: "object_values".to_string(),
                properties: json!({"type": "AND", "values": {}}),
            },
        );
        cohorts.insert(
            "missing_values".to_string(),
            CohortDefinition {
                id: "missing_values".to_string(),
                properties: json!({"type": "AND"}),
            },
        );
        cohorts.insert(
            "empty_object".to_string(),
            CohortDefinition {
                id: "empty_object".to_string(),
                properties: json!({}),
            },
        );
        cohorts.insert(
            "empty_values".to_string(),
            CohortDefinition {
                id: "empty_values".to_string(),
                properties: json!({"type": "AND", "values": []}),
            },
        );

        let ctx = cohort_ctx(&cohorts);
        let properties = HashMap::new();
        for cohort_id in ["scalar", "object_values", "missing_values"] {
            let prop = Property {
                key: "$cohort".to_string(),
                value: json!(cohort_id),
                operator: "in".to_string(),
                property_type: Some("cohort".to_string()),
            };

            assert!(match_property_with_context(&prop, &properties, &ctx).is_err());
        }

        for cohort_id in ["empty_object", "empty_values"] {
            let empty = Property {
                key: "$cohort".to_string(),
                value: json!(cohort_id),
                operator: "in".to_string(),
                property_type: Some("cohort".to_string()),
            };
            assert!(match_property_with_context(&empty, &properties, &ctx).unwrap());
        }
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
                has_experiment: None,
                filters: FeatureFlagFilters {
                    groups: vec![FeatureFlagCondition {
                        properties: vec![],
                        rollout_percentage: Some(100.0),
                        variant: None,
                        aggregation_group_type_index: None,
                    }],
                    multivariate: None,
                    payloads: HashMap::new(),
                    aggregation_group_type_index: None,
                    early_exit: false,
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
            property_matching_version: 1,
            cohorts: &HashMap::new(),
            flags: &flags,
            distinct_id: "user-123",
            groups: &HashMap::new(),
            group_properties: &HashMap::new(),
            group_type_mapping: &HashMap::new(),
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
                has_experiment: None,
                filters: FeatureFlagFilters {
                    groups: vec![],
                    multivariate: None,
                    payloads: HashMap::new(),
                    aggregation_group_type_index: None,
                    early_exit: false,
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
            property_matching_version: 1,
            cohorts: &HashMap::new(),
            flags: &flags,
            distinct_id: "user-123",
            groups: &HashMap::new(),
            group_properties: &HashMap::new(),
            group_type_mapping: &HashMap::new(),
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
                has_experiment: None,
                filters: FeatureFlagFilters {
                    groups: vec![FeatureFlagCondition {
                        properties: vec![],
                        rollout_percentage: Some(100.0),
                        variant: None,
                        aggregation_group_type_index: None,
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
                    aggregation_group_type_index: None,
                    early_exit: false,
                },
            },
        );

        let mut prop = Property {
            key: "$feature/ab-test-flag".to_string(),
            value: json!("control"),
            operator: "exact".to_string(),
            property_type: None,
        };

        let properties = HashMap::new();
        let ctx = EvaluationContext {
            property_matching_version: 1,
            cohorts: &HashMap::new(),
            flags: &flags,
            distinct_id: "user-0",
            groups: &HashMap::new(),
            group_properties: &HashMap::new(),
            group_type_mapping: &HashMap::new(),
        };

        // Independently computed SHA-1 variant bucket for ab-test-flag.user-0: 0.3772.
        assert!(match_property_with_context(&prop, &properties, &ctx).unwrap());
        prop.value = json!("test");
        assert!(!match_property_with_context(&prop, &properties, &ctx).unwrap());
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
            property_matching_version: 1,
            cohorts: &HashMap::new(),
            flags: &flags,
            distinct_id: "user-123",
            groups: &HashMap::new(),
            group_properties: &HashMap::new(),
            group_type_mapping: &HashMap::new(),
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
    fn test_not_regex_with_invalid_pattern_returns_false() {
        // Invalid regex pattern (unclosed group)
        let prop = Property {
            key: "email".to_string(),
            value: json!("(unclosed"),
            operator: "not_regex".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("email".to_string(), json!("test@example.com"));

        // A negative operator only negates a valid comparison.
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_regex_with_various_invalid_patterns() {
        let invalid_patterns = vec![
            "(unclosed",   // Unclosed group
            "[unclosed",   // Unclosed bracket
            "*invalid",    // Invalid quantifier at start
            "(?:unclosed", // Unclosed noncapturing group
            r"\",          // Trailing backslash
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

            // Invalid patterns do not match not_regex either
            let not_regex_prop = Property {
                operator: "not_regex".to_string(),
                ..prop
            };
            assert!(
                !match_property(&not_regex_prop, &properties).unwrap(),
                "Invalid pattern '{}' should return false for not_regex",
                pattern
            );
        }
    }

    // ==================== Semver parsing tests ====================

    #[test]
    fn test_parse_semver_basic() {
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("0.0.0"), Some((0, 0, 0)));
        assert_eq!(parse_semver("10.20.30"), Some((10, 20, 30)));
    }

    #[test]
    fn test_parse_semver_v_prefix() {
        assert_eq!(parse_semver("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("V1.2.3"), Some((1, 2, 3)));
    }

    #[test]
    fn test_parse_semver_whitespace() {
        assert_eq!(parse_semver("  1.2.3  "), Some((1, 2, 3)));
        assert_eq!(parse_semver(" v1.2.3 "), Some((1, 2, 3)));
    }

    #[test]
    fn test_parse_semver_prerelease_stripped() {
        assert_eq!(parse_semver("1.2.3-alpha"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3-beta.1"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3-rc.1+build.123"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3+build.456"), Some((1, 2, 3)));
    }

    #[test]
    fn test_parse_semver_partial_versions() {
        assert_eq!(parse_semver("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_semver("1"), Some((1, 0, 0)));
        assert_eq!(parse_semver("v1.2"), Some((1, 2, 0)));
    }

    #[test]
    fn test_parse_semver_extra_components_ignored() {
        assert_eq!(parse_semver("1.2.3.4"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3.4.5.6"), Some((1, 2, 3)));
    }

    #[test]
    fn test_parse_semver_leading_zeros_rejected() {
        // Per semver 2.0.0 §2, numeric identifiers must not include leading zeros.
        assert_eq!(parse_semver("01.02.03"), None);
        assert_eq!(parse_semver("001.002.003"), None);
        assert_eq!(parse_semver("1.07.3"), None);
        assert_eq!(parse_semver("1.2.03"), None);
        assert_eq!(parse_semver("v01.2.3"), None);

        // Literal "0" components remain valid.
        assert_eq!(parse_semver("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_semver("1.0.0"), Some((1, 0, 0)));
        assert_eq!(parse_semver("0.0.0"), Some((0, 0, 0)));
    }

    #[test]
    fn test_parse_semver_invalid() {
        assert_eq!(parse_semver(""), None);
        assert_eq!(parse_semver("   "), None);
        assert_eq!(parse_semver("v"), None);
        assert_eq!(parse_semver(".1.2.3"), None);
        assert_eq!(parse_semver("abc"), None);
        assert_eq!(parse_semver("1.abc.3"), None);
        assert_eq!(parse_semver("1.2.abc"), None);
        assert_eq!(parse_semver("not-a-version"), None);
    }

    // ==================== Semver operator tests ====================

    fn assert_semver_cases(operator: &str, target: &str, cases: &[(&str, &str, bool)]) {
        let property = Property {
            key: "version".to_string(),
            value: json!(target),
            operator: operator.to_string(),
            property_type: None,
        };

        for &(label, value, expected) in cases {
            let properties = HashMap::from([("version".to_string(), json!(value))]);
            let actual = match_property(&property, &properties).unwrap_or_else(|error| {
                panic!(
                    "{}: evaluating {} {} {} returned an error: {}",
                    label, value, operator, target, error
                )
            });
            assert_eq!(
                actual, expected,
                "{label}: expected {value} {operator} {target} to be {expected}"
            );
        }
    }

    #[test]
    fn test_semver_eq_basic() {
        assert_semver_cases(
            "semver_eq",
            "1.2.3",
            &[
                ("exact match", "1.2.3", true),
                ("different patch", "1.2.4", false),
                ("different minor", "1.3.3", false),
                ("different major", "2.2.3", false),
            ],
        );
    }

    #[test]
    fn test_semver_eq_with_v_prefix() {
        let prop = Property {
            key: "version".to_string(),
            value: json!("1.2.3"),
            operator: "semver_eq".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();

        // v-prefix on property value
        properties.insert("version".to_string(), json!("v1.2.3"));
        assert!(match_property(&prop, &properties).unwrap());

        // v-prefix on target value
        let prop_with_v = Property {
            value: json!("v1.2.3"),
            ..prop.clone()
        };
        properties.insert("version".to_string(), json!("1.2.3"));
        assert!(match_property(&prop_with_v, &properties).unwrap());
    }

    #[test]
    fn test_semver_eq_prerelease_stripped() {
        let prop = Property {
            key: "version".to_string(),
            value: json!("1.2.3"),
            operator: "semver_eq".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();

        properties.insert("version".to_string(), json!("1.2.3-alpha"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("version".to_string(), json!("1.2.3-beta.1"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("version".to_string(), json!("1.2.3+build.456"));
        assert!(match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_semver_eq_partial_versions() {
        let prop = Property {
            key: "version".to_string(),
            value: json!("1.2.0"),
            operator: "semver_eq".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();

        // "1.2" should equal "1.2.0"
        properties.insert("version".to_string(), json!("1.2"));
        assert!(match_property(&prop, &properties).unwrap());

        // Target as partial version
        let partial_prop = Property {
            value: json!("1.2"),
            ..prop.clone()
        };
        properties.insert("version".to_string(), json!("1.2.0"));
        assert!(match_property(&partial_prop, &properties).unwrap());
    }

    #[test]
    fn test_semver_neq() {
        assert_semver_cases(
            "semver_neq",
            "1.2.3",
            &[
                ("exact match", "1.2.3", false),
                ("different patch", "1.2.4", true),
                ("different major", "2.0.0", true),
            ],
        );
    }

    // ==================== Semver gt/gte/lt/lte tests ====================

    #[test]
    fn test_semver_gt() {
        assert_semver_cases(
            "semver_gt",
            "1.2.3",
            &[
                ("greater patch", "1.2.4", true),
                ("greater minor", "1.3.0", true),
                ("greater major", "2.0.0", true),
                ("equal version", "1.2.3", false),
                ("lesser patch", "1.2.2", false),
                ("lesser minor", "1.1.9", false),
                ("lesser major", "0.9.9", false),
            ],
        );
    }

    #[test]
    fn test_semver_gte() {
        assert_semver_cases(
            "semver_gte",
            "1.2.3",
            &[
                ("greater patch", "1.2.4", true),
                ("greater major", "2.0.0", true),
                ("equal version", "1.2.3", true),
                ("lesser patch", "1.2.2", false),
                ("lesser major", "0.9.9", false),
            ],
        );
    }

    #[test]
    fn test_semver_lt() {
        assert_semver_cases(
            "semver_lt",
            "1.2.3",
            &[
                ("lesser patch", "1.2.2", true),
                ("lesser minor", "1.1.9", true),
                ("lesser major", "0.9.9", true),
                ("equal version", "1.2.3", false),
                ("greater patch", "1.2.4", false),
                ("greater major", "2.0.0", false),
            ],
        );
    }

    #[test]
    fn test_semver_lte() {
        assert_semver_cases(
            "semver_lte",
            "1.2.3",
            &[
                ("lesser patch", "1.2.2", true),
                ("lesser major", "0.9.9", true),
                ("equal version", "1.2.3", true),
                ("greater patch", "1.2.4", false),
                ("greater major", "2.0.0", false),
            ],
        );
    }

    // ==================== Semver tilde tests ====================

    #[test]
    fn test_semver_tilde_basic() {
        // ~1.2.3 means >=1.2.3 <1.3.0
        let prop = Property {
            key: "version".to_string(),
            value: json!("1.2.3"),
            operator: "semver_tilde".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();

        // Exact match
        properties.insert("version".to_string(), json!("1.2.3"));
        assert!(match_property(&prop, &properties).unwrap());

        // Within range
        properties.insert("version".to_string(), json!("1.2.4"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("version".to_string(), json!("1.2.99"));
        assert!(match_property(&prop, &properties).unwrap());

        // At upper bound (excluded)
        properties.insert("version".to_string(), json!("1.3.0"));
        assert!(!match_property(&prop, &properties).unwrap());

        // Above upper bound
        properties.insert("version".to_string(), json!("1.3.1"));
        assert!(!match_property(&prop, &properties).unwrap());

        properties.insert("version".to_string(), json!("2.0.0"));
        assert!(!match_property(&prop, &properties).unwrap());

        // Below lower bound
        properties.insert("version".to_string(), json!("1.2.2"));
        assert!(!match_property(&prop, &properties).unwrap());

        properties.insert("version".to_string(), json!("1.1.9"));
        assert!(!match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_semver_tilde_zero_versions() {
        // ~0.2.3 means >=0.2.3 <0.3.0
        let prop = Property {
            key: "version".to_string(),
            value: json!("0.2.3"),
            operator: "semver_tilde".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();

        properties.insert("version".to_string(), json!("0.2.3"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("version".to_string(), json!("0.2.9"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("version".to_string(), json!("0.3.0"));
        assert!(!match_property(&prop, &properties).unwrap());

        properties.insert("version".to_string(), json!("0.2.2"));
        assert!(!match_property(&prop, &properties).unwrap());
    }

    // ==================== Semver caret tests ====================

    #[test]
    fn test_semver_caret_major_nonzero() {
        // ^1.2.3 means >=1.2.3 <2.0.0.
        assert_semver_cases(
            "semver_caret",
            "1.2.3",
            &[
                ("exact lower bound", "1.2.3", true),
                ("within patch range", "1.2.4", true),
                ("within minor range", "1.3.0", true),
                ("within major range", "1.99.99", true),
                ("excluded upper bound", "2.0.0", false),
                ("above upper bound", "2.0.1", false),
                ("below lower patch", "1.2.2", false),
                ("below lower major", "0.9.9", false),
            ],
        );
    }

    #[test]
    fn test_semver_caret_major_zero_minor_nonzero() {
        // ^0.2.3 means >=0.2.3 <0.3.0.
        assert_semver_cases(
            "semver_caret",
            "0.2.3",
            &[
                ("exact lower bound", "0.2.3", true),
                ("within patch range", "0.2.4", true),
                ("within minor range", "0.2.99", true),
                ("excluded upper bound", "0.3.0", false),
                ("above upper minor", "0.3.1", false),
                ("above upper major", "1.0.0", false),
                ("below lower patch", "0.2.2", false),
                ("below lower minor", "0.1.9", false),
            ],
        );
    }

    #[test]
    fn test_semver_caret_major_zero_minor_zero() {
        // ^0.0.3 means >=0.0.3 <0.0.4.
        assert_semver_cases(
            "semver_caret",
            "0.0.3",
            &[
                ("exact lower bound", "0.0.3", true),
                ("excluded upper bound", "0.0.4", false),
                ("above upper patch", "0.0.5", false),
                ("above upper minor", "0.1.0", false),
                ("below lower bound", "0.0.2", false),
            ],
        );
    }

    // ==================== Semver wildcard tests ====================

    #[test]
    fn test_semver_wildcard_major() {
        // 1.* means >=1.0.0 <2.0.0.
        assert_semver_cases(
            "semver_wildcard",
            "1.*",
            &[
                ("exact lower bound", "1.0.0", true),
                ("within minor range", "1.2.3", true),
                ("within upper range", "1.99.99", true),
                ("excluded upper bound", "2.0.0", false),
                ("above upper bound", "2.0.1", false),
                ("below lower bound", "0.9.9", false),
            ],
        );
    }

    #[test]
    fn test_semver_wildcard_minor() {
        // 1.2.* means >=1.2.0 <1.3.0.
        assert_semver_cases(
            "semver_wildcard",
            "1.2.*",
            &[
                ("exact lower bound", "1.2.0", true),
                ("within patch range", "1.2.3", true),
                ("within upper range", "1.2.99", true),
                ("excluded upper bound", "1.3.0", false),
                ("above upper minor", "1.3.1", false),
                ("above upper major", "2.0.0", false),
                ("below lower bound", "1.1.9", false),
            ],
        );
    }

    #[test]
    fn test_semver_wildcard_zero() {
        // 0.* means >=0.0.0 <1.0.0.
        assert_semver_cases(
            "semver_wildcard",
            "0.*",
            &[
                ("exact lower bound", "0.0.0", true),
                ("within upper range", "0.99.99", true),
                ("excluded upper bound", "1.0.0", false),
            ],
        );
    }

    // ==================== Semver error handling tests ====================

    #[test]
    fn test_semver_invalid_property_value() {
        let prop = Property {
            key: "version".to_string(),
            value: json!("1.2.3"),
            operator: "semver_eq".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();

        // Invalid semver strings
        properties.insert("version".to_string(), json!("not-a-version"));
        assert!(match_property(&prop, &properties).is_err());

        properties.insert("version".to_string(), json!(""));
        assert!(match_property(&prop, &properties).is_err());

        properties.insert("version".to_string(), json!(".1.2.3"));
        assert!(match_property(&prop, &properties).is_err());

        properties.insert("version".to_string(), json!("abc.def.ghi"));
        assert!(match_property(&prop, &properties).is_err());
    }

    #[test]
    fn test_semver_invalid_target_value() {
        let mut properties = HashMap::new();
        properties.insert("version".to_string(), json!("1.2.3"));

        // Invalid target semver
        let prop = Property {
            key: "version".to_string(),
            value: json!("not-valid"),
            operator: "semver_eq".to_string(),
            property_type: None,
        };
        assert!(match_property(&prop, &properties).is_err());

        let prop = Property {
            key: "version".to_string(),
            value: json!(""),
            operator: "semver_gt".to_string(),
            property_type: None,
        };
        assert!(match_property(&prop, &properties).is_err());
    }

    #[test]
    fn test_semver_invalid_wildcard_pattern() {
        let mut properties = HashMap::new();
        properties.insert("version".to_string(), json!("1.2.3"));

        // Invalid wildcard patterns
        let invalid_patterns = vec![
            "*",       // Just wildcard
            "*.2.3",   // Wildcard in wrong position
            "1.*.3",   // Wildcard in wrong position
            "1.2.3.*", // Too many parts
            "abc.*",   // Non-numeric major
        ];

        for pattern in invalid_patterns {
            let prop = Property {
                key: "version".to_string(),
                value: json!(pattern),
                operator: "semver_wildcard".to_string(),
                property_type: None,
            };
            assert!(
                match_property(&prop, &properties).is_err(),
                "Pattern '{}' should be invalid",
                pattern
            );
        }
    }

    #[test]
    fn test_semver_missing_property() {
        let prop = Property {
            key: "version".to_string(),
            value: json!("1.2.3"),
            operator: "semver_eq".to_string(),
            property_type: None,
        };

        let properties = HashMap::new(); // Empty properties
        assert!(match_property(&prop, &properties).is_err());
    }

    #[test]
    fn test_semver_null_property_value() {
        let prop = Property {
            key: "version".to_string(),
            value: json!("1.2.3"),
            operator: "semver_eq".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("version".to_string(), json!(null));

        // null converts to "null" string which is not a valid semver
        assert!(match_property(&prop, &properties).is_err());
    }

    #[test]
    fn test_semver_numeric_property_value() {
        // When property value is a number, it gets converted to string
        let prop = Property {
            key: "version".to_string(),
            value: json!("1.0.0"),
            operator: "semver_eq".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        // Number 1 becomes "1" which parses as (1, 0, 0)
        properties.insert("version".to_string(), json!(1));
        assert!(match_property(&prop, &properties).unwrap());
    }

    // ==================== Semver edge cases ====================

    #[test]
    fn test_semver_four_part_versions() {
        let prop = Property {
            key: "version".to_string(),
            value: json!("1.2.3.4"),
            operator: "semver_eq".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();

        // 1.2.3.4 should equal 1.2.3 (extra parts ignored)
        properties.insert("version".to_string(), json!("1.2.3"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("version".to_string(), json!("1.2.3.4"));
        assert!(match_property(&prop, &properties).unwrap());

        properties.insert("version".to_string(), json!("1.2.3.999"));
        assert!(match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_semver_large_version_numbers() {
        let prop = Property {
            key: "version".to_string(),
            value: json!("1000.2000.3000"),
            operator: "semver_eq".to_string(),
            property_type: None,
        };

        let mut properties = HashMap::new();
        properties.insert("version".to_string(), json!("1000.2000.3000"));
        assert!(match_property(&prop, &properties).unwrap());
    }

    #[test]
    fn test_semver_comparison_ordering() {
        // Test that version ordering is correct across major/minor/patch
        let cases = vec![
            ("0.0.1", "0.0.2", "semver_lt", true),
            ("0.1.0", "0.0.99", "semver_gt", true),
            ("1.0.0", "0.99.99", "semver_gt", true),
            ("1.0.0", "1.0.0", "semver_eq", true),
            ("2.0.0", "10.0.0", "semver_lt", true), // Numeric, not string comparison
            ("9.0.0", "10.0.0", "semver_lt", true), // Numeric, not string comparison
            ("1.9.0", "1.10.0", "semver_lt", true), // Numeric, not string comparison
            ("1.2.9", "1.2.10", "semver_lt", true), // Numeric, not string comparison
        ];

        for (prop_val, target_val, op, expected) in cases {
            let prop = Property {
                key: "version".to_string(),
                value: json!(target_val),
                operator: op.to_string(),
                property_type: None,
            };

            let mut properties = HashMap::new();
            properties.insert("version".to_string(), json!(prop_val));

            assert_eq!(
                match_property(&prop, &properties).unwrap(),
                expected,
                "{} {} {} should be {}",
                prop_val,
                op,
                target_val,
                expected
            );
        }
    }

    #[test]
    fn test_match_property_semver_rejects_leading_zeros() {
        // Per semver 2.0.0 §2, numeric identifiers must not include leading zeros.
        // Both property (override) values and target (flag) values should fail to
        // parse, surfacing InconclusiveMatchError so the condition does not match.

        let bad_versions = ["1.07.3", "01.02.03", "1.2.03", "v01.2.3", "001.0.0"];

        // Override values are rejected across semver_eq.
        for bad in bad_versions {
            let prop = Property {
                key: "version".to_string(),
                value: json!("1.2.3"),
                operator: "semver_eq".to_string(),
                property_type: None,
            };
            let mut properties = HashMap::new();
            properties.insert("version".to_string(), json!(bad));
            assert!(
                match_property(&prop, &properties).is_err(),
                "override '{}' should be rejected",
                bad
            );
        }

        // Literal "0" components still work for semver_eq.
        for good in ["0.1.0", "1.0.0", "0.0.0"] {
            let prop = Property {
                key: "version".to_string(),
                value: json!(good),
                operator: "semver_eq".to_string(),
                property_type: None,
            };
            let mut properties = HashMap::new();
            properties.insert("version".to_string(), json!(good));
            assert!(
                match_property(&prop, &properties).unwrap(),
                "'{}' should parse and match itself",
                good
            );
        }

        // Flag (target) values are rejected across the remaining operators.
        let mut properties = HashMap::new();
        properties.insert("version".to_string(), json!("1.2.3"));

        for op in ["semver_gt", "semver_caret", "semver_tilde"] {
            for bad in bad_versions {
                let prop = Property {
                    key: "version".to_string(),
                    value: json!(bad),
                    operator: op.to_string(),
                    property_type: None,
                };
                assert!(
                    match_property(&prop, &properties).is_err(),
                    "target '{}' for {} should be rejected",
                    bad,
                    op
                );
            }
        }

        // Wildcard patterns with leading-zero numeric components are rejected.
        for bad_pattern in ["01.*", "1.07.*", "v01.2.*"] {
            let prop = Property {
                key: "version".to_string(),
                value: json!(bad_pattern),
                operator: "semver_wildcard".to_string(),
                property_type: None,
            };
            assert!(
                match_property(&prop, &properties).is_err(),
                "wildcard target '{}' should be rejected",
                bad_pattern
            );
        }
    }

    // ==================== Evaluation engine characterizations ====================

    #[test]
    fn test_variant_overrides_keep_definition_order_in_both_public_apis() {
        let matching_property = Property {
            key: "audience".to_string(),
            value: json!("matched"),
            operator: "exact".to_string(),
            property_type: None,
        };
        let flag = FeatureFlag {
            key: "stable-overrides".to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![
                    FeatureFlagCondition {
                        properties: vec![],
                        rollout_percentage: Some(100.0),
                        variant: None,
                        aggregation_group_type_index: None,
                    },
                    FeatureFlagCondition {
                        properties: vec![matching_property.clone()],
                        rollout_percentage: Some(100.0),
                        variant: Some("first".to_string()),
                        aggregation_group_type_index: None,
                    },
                    FeatureFlagCondition {
                        properties: vec![matching_property],
                        rollout_percentage: Some(100.0),
                        variant: Some("second".to_string()),
                        aggregation_group_type_index: None,
                    },
                ],
                multivariate: Some(MultivariateFilter {
                    variants: vec![
                        MultivariateVariant {
                            key: "first".to_string(),
                            rollout_percentage: 50.0,
                        },
                        MultivariateVariant {
                            key: "second".to_string(),
                            rollout_percentage: 50.0,
                        },
                    ],
                }),
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: false,
            },
        };
        let properties = HashMap::from([("audience".to_string(), json!("matched"))]);
        let empty_cohorts = HashMap::new();
        let empty_flags = HashMap::new();
        let empty_groups = HashMap::new();
        let empty_group_properties = HashMap::new();
        let empty_group_mapping = HashMap::new();
        let ctx = EvaluationContext {
            property_matching_version: 1,
            cohorts: &empty_cohorts,
            flags: &empty_flags,
            distinct_id: "user-123",
            groups: &empty_groups,
            group_properties: &empty_group_properties,
            group_type_mapping: &empty_group_mapping,
        };

        let expected = FlagValue::String("first".to_string());
        assert_eq!(
            match_feature_flag(
                &flag,
                ctx.distinct_id,
                &properties,
                ctx.groups,
                ctx.group_properties,
                ctx.group_type_mapping,
            )
            .unwrap(),
            expected
        );
        assert_eq!(
            match_feature_flag_with_context(&flag, &properties, &ctx).unwrap(),
            expected
        );
    }

    #[test]
    fn test_group_targeting_uses_group_properties_and_group_key_for_bucketing() {
        let flag = FeatureFlag {
            key: "group-bucketing".to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: vec![Property {
                        key: "plan".to_string(),
                        value: json!("enterprise"),
                        operator: "exact".to_string(),
                        property_type: None,
                    }],
                    rollout_percentage: Some(100.0),
                    variant: None,
                    aggregation_group_type_index: None,
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
                aggregation_group_type_index: Some(0),
                early_exit: false,
            },
        };
        let person_properties = HashMap::from([("plan".to_string(), json!("free"))]);
        let groups = HashMap::from([("company".to_string(), "acme".to_string())]);
        let group_properties = HashMap::from([(
            "company".to_string(),
            HashMap::from([("plan".to_string(), json!("enterprise"))]),
        )]);
        let group_type_mapping = HashMap::from([("0".to_string(), "company".to_string())]);
        let empty_cohorts = HashMap::new();
        let empty_flags = HashMap::new();
        let ctx = EvaluationContext {
            property_matching_version: 1,
            cohorts: &empty_cohorts,
            flags: &empty_flags,
            distinct_id: "different-person-id",
            groups: &groups,
            group_properties: &group_properties,
            group_type_mapping: &group_type_mapping,
        };

        let basic_result = match_feature_flag(
            &flag,
            "person-id",
            &person_properties,
            &groups,
            &group_properties,
            &group_type_mapping,
        )
        .unwrap();
        let context_result =
            match_feature_flag_with_context(&flag, &person_properties, &ctx).unwrap();
        assert_eq!(basic_result, context_result);
        assert!(matches!(basic_result, FlagValue::String(_)));

        // An unresolved group type or absent group key skips the condition.
        assert_eq!(
            match_feature_flag(
                &flag,
                "person-id",
                &person_properties,
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
            )
            .unwrap(),
            FlagValue::Boolean(false)
        );
        // Once the type and key resolve, missing group properties are
        // inconclusive rather than a definitive miss.
        assert!(match_feature_flag(
            &flag,
            "person-id",
            &person_properties,
            &groups,
            &HashMap::new(),
            &group_type_mapping,
        )
        .is_err());
    }

    #[test]
    fn test_rollout_boundary_is_inclusive_in_both_public_apis() {
        let distinct_id = "boundary-user";
        let key = "rollout-boundary";
        let boundary = hash_key(key, distinct_id, ROLLOUT_HASH_SALT) * 100.0;
        let flag = FeatureFlag {
            key: key.to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: vec![],
                    rollout_percentage: Some(boundary),
                    variant: None,
                    aggregation_group_type_index: None,
                }],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: false,
            },
        };
        let empty_cohorts = HashMap::new();
        let empty_flags = HashMap::new();
        let empty_groups = HashMap::new();
        let empty_group_properties = HashMap::new();
        let empty_group_mapping = HashMap::new();
        let properties = HashMap::new();
        let ctx = EvaluationContext {
            property_matching_version: 1,
            cohorts: &empty_cohorts,
            flags: &empty_flags,
            distinct_id,
            groups: &empty_groups,
            group_properties: &empty_group_properties,
            group_type_mapping: &empty_group_mapping,
        };

        assert_eq!(
            match_feature_flag(
                &flag,
                distinct_id,
                &properties,
                &empty_groups,
                &empty_group_properties,
                &empty_group_mapping,
            )
            .unwrap(),
            FlagValue::Boolean(true)
        );
        assert_eq!(
            match_feature_flag_with_context(&flag, &properties, &ctx).unwrap(),
            FlagValue::Boolean(true)
        );
    }

    #[test]
    fn test_context_engine_preserves_flag_dependency_matching() {
        let prerequisite = FeatureFlag {
            key: "prerequisite".to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: vec![],
                    rollout_percentage: Some(100.0),
                    variant: None,
                    aggregation_group_type_index: None,
                }],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: false,
            },
        };
        let dependent = FeatureFlag {
            key: "dependent".to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![FeatureFlagCondition {
                    properties: vec![Property {
                        key: "$feature/prerequisite".to_string(),
                        value: json!(true),
                        operator: "exact".to_string(),
                        property_type: None,
                    }],
                    rollout_percentage: Some(100.0),
                    variant: None,
                    aggregation_group_type_index: None,
                }],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: false,
            },
        };
        let flags = HashMap::from([("prerequisite".to_string(), prerequisite)]);
        let empty_cohorts = HashMap::new();
        let empty_groups = HashMap::new();
        let empty_group_properties = HashMap::new();
        let empty_group_mapping = HashMap::new();
        let ctx = EvaluationContext {
            property_matching_version: 1,
            cohorts: &empty_cohorts,
            flags: &flags,
            distinct_id: "user-123",
            groups: &empty_groups,
            group_properties: &empty_group_properties,
            group_type_mapping: &empty_group_mapping,
        };

        assert_eq!(
            match_feature_flag_with_context(&dependent, &HashMap::new(), &ctx).unwrap(),
            FlagValue::Boolean(true)
        );
    }

    // ==================== Tests for early_exit ====================

    /// Build a two-group flag where the first group always lands
    /// out-of-rollout-bound (no property filters, 0% rollout) and the second
    /// group always matches (no property filters, 100% rollout). Without
    /// `early_exit`, evaluation falls through to the matching second group.
    fn early_exit_flag(early_exit: bool) -> FeatureFlag {
        FeatureFlag {
            key: "early-exit-flag".to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![
                    // Group 1: matches on properties (none) but rollout excludes
                    // everyone -> OUT_OF_ROLLOUT_BOUND.
                    FeatureFlagCondition {
                        properties: vec![],
                        rollout_percentage: Some(0.0),
                        variant: None,
                        aggregation_group_type_index: None,
                    },
                    // Group 2: would match everyone -> MATCH.
                    FeatureFlagCondition {
                        properties: vec![],
                        rollout_percentage: Some(100.0),
                        variant: None,
                        aggregation_group_type_index: None,
                    },
                ],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit,
            },
        }
    }

    macro_rules! test_early_exit {
        ($name:ident, $early_exit:expr, $expected:expr) => {
            #[test]
            fn $name() {
                let flag = early_exit_flag($early_exit);
                let result = match_feature_flag(
                    &flag,
                    "user-123",
                    &HashMap::new(),
                    &HashMap::new(),
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap();
                assert_eq!(result, $expected);
            }
        };
    }

    test_early_exit!(
        test_early_exit_enabled_returns_false_without_evaluating_later_group,
        true,
        FlagValue::Boolean(false)
    );
    test_early_exit!(
        test_early_exit_unset_falls_through_to_matching_group,
        false,
        FlagValue::Boolean(true)
    );

    fn assert_early_exit_json_falls_through(label: &str, fixture: serde_json::Value) {
        let flag: FeatureFlag = serde_json::from_value(fixture).unwrap();
        assert!(
            !flag.filters.early_exit,
            "{}: early_exit should deserialize as false",
            label
        );
        let result = match_feature_flag(
            &flag,
            "user-123",
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(
            result,
            FlagValue::Boolean(true),
            "{label}: evaluation should fall through to the matching group"
        );
    }

    #[test]
    fn test_early_exit_default_is_false_from_json() {
        // Omitting `early_exit` must preserve the legacy fall-through behavior.
        let fixture = json!({
            "key": "early-exit-flag",
            "active": true,
            "filters": {
                "groups": [
                    { "properties": [], "rollout_percentage": 0.0, "variant": null },
                    { "properties": [], "rollout_percentage": 100.0, "variant": null }
                ]
            }
        });
        assert_early_exit_json_falls_through("omitted early_exit", fixture);
    }

    #[test]
    fn test_early_exit_explicit_false_falls_through() {
        let fixture = json!({
            "key": "early-exit-flag",
            "active": true,
            "filters": {
                "early_exit": false,
                "groups": [
                    { "properties": [], "rollout_percentage": 0.0, "variant": null },
                    { "properties": [], "rollout_percentage": 100.0, "variant": null }
                ]
            }
        });
        assert_early_exit_json_falls_through("explicit false early_exit", fixture);
    }

    #[test]
    fn test_early_exit_property_mismatch_does_not_short_circuit() {
        // First group fails on a property filter (NO_MATCH), not rollout, so
        // even with early_exit enabled we must fall through to the matching
        // second group.
        let flag = FeatureFlag {
            key: "early-exit-flag".to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![
                    FeatureFlagCondition {
                        properties: vec![Property {
                            key: "country".to_string(),
                            value: json!("US"),
                            operator: "exact".to_string(),
                            property_type: None,
                        }],
                        rollout_percentage: Some(100.0),
                        variant: None,
                        aggregation_group_type_index: None,
                    },
                    FeatureFlagCondition {
                        properties: vec![],
                        rollout_percentage: Some(100.0),
                        variant: None,
                        aggregation_group_type_index: None,
                    },
                ],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: true,
            },
        };

        let mut properties = HashMap::new();
        properties.insert("country".to_string(), json!("UK")); // does not match "US"

        let result = match_feature_flag(
            &flag,
            "user-123",
            &properties,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        )
        .unwrap();
        // Property mismatch must not trigger early-exit; second group matches.
        assert_eq!(result, FlagValue::Boolean(true));
    }

    macro_rules! test_early_exit_with_context {
        ($name:ident, $early_exit:expr, $expected:expr) => {
            #[test]
            fn $name() {
                let flag = early_exit_flag($early_exit);
                let ctx = EvaluationContext {
                    property_matching_version: 1,
                    cohorts: &HashMap::new(),
                    flags: &HashMap::new(),
                    distinct_id: "user-123",
                    groups: &HashMap::new(),
                    group_properties: &HashMap::new(),
                    group_type_mapping: &HashMap::new(),
                };
                let result = match_feature_flag_with_context(&flag, &HashMap::new(), &ctx).unwrap();
                assert_eq!(result, $expected);
            }
        };
    }

    test_early_exit_with_context!(
        test_early_exit_enabled_short_circuits_with_context,
        true,
        FlagValue::Boolean(false)
    );
    test_early_exit_with_context!(
        test_early_exit_unset_falls_through_with_context,
        false,
        FlagValue::Boolean(true)
    );

    #[test]
    fn test_early_exit_stays_inconclusive_when_prior_group_inconclusive() {
        // Group 1: group-targeted (aggregation_group_type_index = 0). The
        // group_type_mapping resolves "0" → "company" and groups supplies the
        // company key, but group_properties has no entry for "company" →
        // ConditionTarget::Inconclusive → is_inconclusive = true.
        // Group 2: person-targeted, rollout 0% → OutOfRolloutBound.
        // With early_exit = true, evaluation stops at group 2 but must not
        // return a definitive false: the result is InconclusiveMatchError.
        let flag = FeatureFlag {
            key: "early-exit-flag".to_string(),
            active: true,
            has_experiment: None,
            filters: FeatureFlagFilters {
                groups: vec![
                    FeatureFlagCondition {
                        properties: vec![],
                        rollout_percentage: Some(100.0),
                        variant: None,
                        aggregation_group_type_index: Some(0),
                    },
                    FeatureFlagCondition {
                        properties: vec![],
                        rollout_percentage: Some(0.0),
                        variant: None,
                        aggregation_group_type_index: None,
                    },
                ],
                multivariate: None,
                payloads: HashMap::new(),
                aggregation_group_type_index: None,
                early_exit: true,
            },
        };

        let mut group_type_mapping = HashMap::new();
        group_type_mapping.insert("0".to_string(), "company".to_string());

        let mut groups = HashMap::new();
        groups.insert("company".to_string(), "acme".to_string());

        // group_properties intentionally omitted for "company" → Inconclusive
        let result = match_feature_flag(
            &flag,
            "user-123",
            &HashMap::new(),
            &groups,
            &HashMap::new(), // empty group_properties
            &group_type_mapping,
        );
        assert!(
            result.is_err(),
            "expected InconclusiveMatchError, got {:?}",
            result
        );

        let empty_cohorts = HashMap::new();
        let empty_flags = HashMap::new();
        let empty_group_properties = HashMap::new();
        let ctx = EvaluationContext {
            property_matching_version: 1,
            cohorts: &empty_cohorts,
            flags: &empty_flags,
            distinct_id: "user-123",
            groups: &groups,
            group_properties: &empty_group_properties,
            group_type_mapping: &group_type_mapping,
        };
        assert!(match_feature_flag_with_context(&flag, &HashMap::new(), &ctx).is_err());
    }

    /// Evaluates through both public entry points and checks they agree.
    fn evaluate_both_ways(
        flag: &FeatureFlag,
        distinct_id: &str,
        properties: &HashMap<String, serde_json::Value>,
    ) -> Result<FlagValue, InconclusiveMatchError> {
        let basic = match_feature_flag(
            flag,
            distinct_id,
            properties,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let empty_cohorts = HashMap::new();
        let empty_flags = HashMap::new();
        let empty_groups = HashMap::new();
        let empty_group_properties = HashMap::new();
        let empty_group_type_mapping = HashMap::new();
        let ctx = EvaluationContext {
            property_matching_version: 1,
            cohorts: &empty_cohorts,
            flags: &empty_flags,
            distinct_id,
            groups: &empty_groups,
            group_properties: &empty_group_properties,
            group_type_mapping: &empty_group_type_mapping,
        };
        let with_context = match_feature_flag_with_context(flag, properties, &ctx);
        assert_eq!(
            basic.as_ref().ok(),
            with_context.as_ref().ok(),
            "match_feature_flag and match_feature_flag_with_context disagree"
        );
        basic
    }

    #[test]
    fn test_conditions_evaluated_in_order() {
        // A pro user matches the first condition, which has no override, so
        // the later catch-all overriding to "test" must not win.
        let flag: FeatureFlag = serde_json::from_value(json!({
            "key": "checkout",
            "active": true,
            "filters": {
                "groups": [
                    {
                        "properties": [
                            {"key": "plan", "value": "pro", "operator": "exact", "type": "person"}
                        ],
                        "rollout_percentage": 100
                    },
                    {"properties": [], "rollout_percentage": 100, "variant": "test"}
                ],
                "multivariate": {
                    "variants": [
                        {"key": "control", "rollout_percentage": 50},
                        {"key": "test", "rollout_percentage": 50}
                    ]
                }
            }
        }))
        .unwrap();

        // Variant buckets for "checkout", checked against posthog-python:
        // user-0 = 0.9407, user-1 = 0.5740, user-2 = 0.3536, user-3 = 0.2522.
        for (user, plan, expected) in [
            ("user-0", "pro", "test"),
            ("user-1", "pro", "test"),
            ("user-2", "pro", "control"),
            ("user-3", "pro", "control"),
            ("user-2", "free", "test"),
            ("user-3", "free", "test"),
        ] {
            let properties = HashMap::from([("plan".to_string(), json!(plan))]);
            assert_eq!(
                evaluate_both_ways(&flag, user, &properties).unwrap(),
                FlagValue::String(expected.into()),
                "{user}, {plan}"
            );
        }
    }

    #[test]
    fn test_early_exit_is_not_skipped_by_a_later_variant_override() {
        let flag: FeatureFlag = serde_json::from_value(json!({
            "key": "checkout",
            "active": true,
            "filters": {
                "early_exit": true,
                "groups": [
                    {
                        "properties": [
                            {"key": "plan", "value": "pro", "operator": "exact", "type": "person"}
                        ],
                        "rollout_percentage": 0
                    },
                    {"properties": [], "rollout_percentage": 100, "variant": "test"}
                ],
                "multivariate": {
                    "variants": [
                        {"key": "control", "rollout_percentage": 50},
                        {"key": "test", "rollout_percentage": 50}
                    ]
                }
            }
        }))
        .unwrap();

        for (plan, expected) in [
            ("pro", FlagValue::Boolean(false)),
            ("free", FlagValue::String("test".into())),
        ] {
            let properties = HashMap::from([("plan".to_string(), json!(plan))]);
            assert_eq!(
                evaluate_both_ways(&flag, "user-0", &properties).unwrap(),
                expected,
                "{plan}"
            );
        }
    }

    #[test]
    fn test_early_exit_after_inconclusive_group_does_not_reach_later_match() {
        // With `plan` missing, group 1 is inconclusive. Group 2 excludes the
        // user by rollout, so whatever `plan` is, the user can't get the flag:
        // evaluation must stop there and stay inconclusive, not return the
        // catch-all's "test".
        let flag: FeatureFlag = serde_json::from_value(json!({
            "key": "checkout",
            "active": true,
            "filters": {
                "early_exit": true,
                "groups": [
                    {
                        "properties": [
                            {"key": "plan", "value": "pro", "operator": "exact", "type": "person"}
                        ],
                        "rollout_percentage": 0
                    },
                    {
                        "properties": [
                            {"key": "region", "value": "US", "operator": "exact", "type": "person"}
                        ],
                        "rollout_percentage": 0,
                        "variant": "control"
                    },
                    {"properties": [], "rollout_percentage": 100, "variant": "test"}
                ],
                "multivariate": {
                    "variants": [
                        {"key": "control", "rollout_percentage": 50},
                        {"key": "test", "rollout_percentage": 50}
                    ]
                }
            }
        }))
        .unwrap();

        let properties = HashMap::from([("region".to_string(), json!("US"))]);
        let result = evaluate_both_ways(&flag, "user-0", &properties);
        assert!(result.is_err(), "expected inconclusive, got {:?}", result);

        for (plan, region, expected) in [
            ("pro", "US", FlagValue::Boolean(false)),
            ("free", "US", FlagValue::Boolean(false)),
            ("free", "EU", FlagValue::String("test".into())),
        ] {
            let properties = HashMap::from([
                ("plan".to_string(), json!(plan)),
                ("region".to_string(), json!(region)),
            ]);
            assert_eq!(
                evaluate_both_ways(&flag, "user-0", &properties).unwrap(),
                expected,
                "{plan}, {region}"
            );
        }
    }

    #[test]
    fn test_filter_that_does_not_match_wins_over_earlier_missing_property() {
        // `plan` is missing, so the first filter can't be checked. The second
        // one rules the condition out whatever `plan` is, so the flag is
        // definitively false, not inconclusive.
        let flag: FeatureFlag = serde_json::from_value(json!({
            "key": "checkout",
            "active": true,
            "filters": {
                "groups": [
                    {
                        "properties": [
                            {"key": "plan", "value": "pro", "operator": "exact", "type": "person"},
                            {"key": "region", "value": "US", "operator": "exact", "type": "person"}
                        ],
                        "rollout_percentage": 100
                    }
                ]
            }
        }))
        .unwrap();

        let properties = HashMap::from([("region".to_string(), json!("EU"))]);
        assert_eq!(
            evaluate_both_ways(&flag, "user-0", &properties).unwrap(),
            FlagValue::Boolean(false)
        );

        // When every filter that can be checked matches, the missing one still
        // decides, so the result stays inconclusive.
        let properties = HashMap::from([("region".to_string(), json!("US"))]);
        let result = evaluate_both_ways(&flag, "user-0", &properties);
        assert!(result.is_err(), "expected inconclusive, got {:?}", result);
    }
}

#[cfg(test)]
mod known_absent_tests {
    use super::*;
    use serde_json::json;

    fn person_keys(keys: &[&str]) -> KnownAbsentProperties {
        let keys: Vec<String> = keys.iter().map(|key| key.to_string()).collect();
        KnownAbsentProperties::new(Some(&keys), None)
    }

    fn group_keys(group_type: &str, keys: &[&str]) -> KnownAbsentProperties {
        let groups = HashMap::from([(
            group_type.to_string(),
            keys.iter().map(|key| key.to_string()).collect(),
        )]);
        KnownAbsentProperties::new(None, Some(&groups))
    }

    fn person_flag(key: &str, properties: serde_json::Value) -> FeatureFlag {
        serde_json::from_value(json!({
            "key": key,
            "active": true,
            "filters": {"groups": [{"properties": properties, "rollout_percentage": 100}]}
        }))
        .unwrap()
    }

    fn evaluate(
        flag: &FeatureFlag,
        person_properties: &HashMap<String, serde_json::Value>,
        known_absent: &KnownAbsentProperties,
    ) -> Result<FlagValue, InconclusiveMatchError> {
        evaluate_with(
            flag,
            person_properties,
            known_absent,
            &HashMap::new(),
            &HashMap::new(),
        )
    }

    fn evaluate_with(
        flag: &FeatureFlag,
        person_properties: &HashMap<String, serde_json::Value>,
        known_absent: &KnownAbsentProperties,
        cohorts: &HashMap<String, CohortDefinition>,
        flags: &HashMap<String, FeatureFlag>,
    ) -> Result<FlagValue, InconclusiveMatchError> {
        let groups = HashMap::from([("organization".to_string(), "org-1".to_string())]);
        let group_type_mapping = HashMap::from([
            ("0".to_string(), "organization".to_string()),
            ("1".to_string(), "project".to_string()),
        ]);
        let ctx = EvaluationContext {
            cohorts,
            flags,
            distinct_id: "user-1",
            groups: &groups,
            group_properties: &HashMap::new(),
            group_type_mapping: &group_type_mapping,
            property_matching_version: 1,
        };
        match_feature_flag_with_known_absent(flag, person_properties, &ctx, known_absent)
    }

    #[test]
    fn listed_key_resolves_every_supported_operator_without_a_value() {
        for (operator, value, expected) in [
            ("is_not_set", json!(null), true),
            ("is_not", json!("pro"), true),
            ("not_icontains", json!("pro"), true),
            ("not_starts_with", json!("pro"), true),
            ("not_ends_with", json!("pro"), true),
            ("not_regex", json!("^pro"), true),
            ("is_set", json!(null), false),
            ("exact", json!("pro"), false),
            ("icontains", json!("pro"), false),
            ("starts_with", json!("pro"), false),
            ("ends_with", json!("pro"), false),
            ("regex", json!("^pro"), false),
            ("gt", json!("10"), false),
            ("gte", json!("10"), false),
            ("lt", json!("10"), false),
            ("lte", json!("10"), false),
            ("semver_eq", json!("1.2.3"), false),
            ("semver_neq", json!("1.2.3"), false),
            ("semver_gt", json!("1.2.3"), false),
            ("semver_gte", json!("1.2.3"), false),
            ("semver_lt", json!("1.2.3"), false),
            ("semver_lte", json!("1.2.3"), false),
            ("semver_tilde", json!("1.2.3"), false),
            ("semver_caret", json!("1.2.3"), false),
            ("semver_wildcard", json!("1.2.*"), false),
            ("is_date_before", json!("2025-01-01T10:00:00Z"), false),
            ("is_date_after", json!("2025-01-01T10:00:00Z"), false),
        ] {
            let flag = person_flag(
                "flag",
                json!([{"key": "plan", "value": value, "operator": operator, "type": "person"}]),
            );
            let result = evaluate(&flag, &HashMap::new(), &person_keys(&["plan"]));
            assert_eq!(
                result.unwrap_or_else(|err| panic!("{} was inconclusive: {}", operator, err)),
                FlagValue::Boolean(expected),
                "{operator}"
            );
        }
    }

    #[test]
    fn null_condition_value_on_a_listed_key_stays_inconclusive() {
        for operator in ["is_not", "exact", "gt"] {
            let flag = person_flag(
                "flag",
                json!([{"key": "plan", "value": null, "operator": operator, "type": "person"}]),
            );
            assert!(
                evaluate(&flag, &HashMap::new(), &person_keys(&["plan"])).is_err(),
                "{} with a null value should stay inconclusive",
                operator
            );
        }

        for (operator, expected) in [("is_not_set", true), ("is_set", false)] {
            let flag = person_flag(
                "flag",
                json!([{"key": "plan", "value": null, "operator": operator, "type": "person"}]),
            );
            assert_eq!(
                evaluate(&flag, &HashMap::new(), &person_keys(&["plan"])).unwrap(),
                FlagValue::Boolean(expected),
                "{operator}"
            );
        }
    }

    #[test]
    fn listed_key_resolves_without_reading_the_condition_value() {
        for (operator, value, expected) in [
            ("not_regex", json!("["), true),
            ("regex", json!("["), false),
            ("not_regex", json!("a\\Kb"), true),
            ("regex", json!("a\\Kb"), false),
            ("not_regex", json!("\\p{L}"), true),
            ("regex", json!("\\p{L}"), false),
            ("not_regex", json!("(?<=a)b"), true),
            ("gt", json!("not-a-number"), false),
            ("gte", json!("not-a-number"), false),
            ("lt", json!("not-a-number"), false),
            ("lte", json!("not-a-number"), false),
            ("semver_eq", json!("not-a-version"), false),
            ("semver_tilde", json!("not-a-version"), false),
            ("semver_caret", json!("not-a-version"), false),
            ("semver_neq", json!("not-a-version"), false),
            ("semver_wildcard", json!("not-a-version"), false),
            ("semver_wildcard", json!("1.2.3"), false),
            ("is_date_before", json!("not-a-date"), false),
            ("is_date_after", json!("not-a-date"), false),
        ] {
            let flag = person_flag(
                "flag",
                json!([{"key": "plan", "value": value, "operator": operator, "type": "person"}]),
            );
            let result = evaluate(&flag, &HashMap::new(), &person_keys(&["plan"]));
            assert_eq!(
                result.unwrap_or_else(|err| panic!(
                    "{} {} was inconclusive: {}",
                    operator, value, err
                )),
                FlagValue::Boolean(expected),
                "{operator} {value}"
            );
        }
    }

    #[test]
    fn listed_key_overrides_a_supplied_value() {
        let flag = person_flag(
            "profile-incomplete",
            json!([{"key": "plan", "operator": "is_not_set", "type": "person"}]),
        );
        let properties = HashMap::from([("plan".to_string(), json!("pro"))]);

        assert_eq!(
            evaluate(&flag, &properties, &KnownAbsentProperties::default()).unwrap(),
            FlagValue::Boolean(false)
        );
        assert_eq!(
            evaluate(&flag, &properties, &person_keys(&["plan"])).unwrap(),
            FlagValue::Boolean(true)
        );
    }

    #[test]
    fn listed_key_overrides_the_injected_distinct_id() {
        let flag = person_flag(
            "flag",
            json!([{"key": "distinct_id", "operator": "is_set", "type": "person"}]),
        );
        let properties = HashMap::from([("distinct_id".to_string(), json!("user-1"))]);

        assert_eq!(
            evaluate(&flag, &properties, &person_keys(&["distinct_id"])).unwrap(),
            FlagValue::Boolean(false)
        );
    }

    #[test]
    fn omitted_key_that_is_not_listed_stays_inconclusive() {
        let flag = person_flag(
            "profile-incomplete",
            json!([{"key": "plan", "operator": "is_not_set", "type": "person"}]),
        );

        assert!(evaluate(&flag, &HashMap::new(), &person_keys(&["region"])).is_err());
    }

    #[test]
    fn person_metadata_keys_are_ignored() {
        let key = "__posthog_person_metadata__created_at";
        let flag = person_flag(
            "flag",
            json!([{"key": key, "operator": "is_set", "type": "person"}]),
        );
        let properties = HashMap::from([(key.to_string(), json!("2025-01-01"))]);

        assert_eq!(
            evaluate(&flag, &properties, &person_keys(&[key])).unwrap(),
            FlagValue::Boolean(true)
        );
    }

    #[test]
    fn group_keys_apply_only_to_their_group_type() {
        let flag: FeatureFlag = serde_json::from_value(json!({
            "key": "org-incomplete",
            "active": true,
            "filters": {
                "aggregation_group_type_index": 0,
                "groups": [{
                    "properties": [{"key": "tier", "operator": "is_not_set", "type": "group", "group_type_index": 0}],
                    "rollout_percentage": 100
                }]
            }
        }))
        .unwrap();

        assert!(evaluate(&flag, &HashMap::new(), &group_keys("project", &["tier"])).is_err());
        assert!(evaluate(&flag, &HashMap::new(), &person_keys(&["tier"])).is_err());
        assert_eq!(
            evaluate(
                &flag,
                &HashMap::new(),
                &group_keys("organization", &["tier"])
            )
            .unwrap(),
            FlagValue::Boolean(true)
        );
    }

    #[test]
    fn unsupported_operator_on_a_listed_key_stays_inconclusive() {
        for (operator, value) in [
            ("not_icontains_multi", json!(["pro"])),
            ("icontains_multi", json!(["pro"])),
            ("not_between", json!([1, 2])),
            ("between", json!([1, 2])),
            ("future_operator", json!("pro")),
        ] {
            let flag = person_flag(
                "flag",
                json!([{"key": "plan", "value": value, "operator": operator, "type": "person"}]),
            );
            assert!(
                evaluate(&flag, &HashMap::new(), &person_keys(&["plan"])).is_err(),
                "{} should stay inconclusive",
                operator
            );
        }
    }

    #[test]
    fn listed_key_does_not_resolve_a_cohort_filter_in_a_flag_dependency() {
        let dependency = person_flag(
            "in-cohort",
            json!([{"key": "id", "value": 1, "operator": "exact", "type": "cohort"}]),
        );
        let flags = HashMap::from([(dependency.key.clone(), dependency)]);
        let flag = person_flag(
            "dependent",
            json!([{"key": "$feature/in-cohort", "value": false, "operator": "exact", "type": "flag"}]),
        );

        assert!(evaluate_with(
            &flag,
            &HashMap::new(),
            &person_keys(&["id"]),
            &HashMap::new(),
            &flags
        )
        .is_err());
    }

    #[test]
    fn cohort_leaf_applies_negation_after_the_known_absent_result() {
        let cohorts = HashMap::from([(
            "1".to_string(),
            CohortDefinition {
                id: "1".to_string(),
                properties: json!({
                    "type": "AND",
                    "values": [{
                        "key": "plan",
                        "operator": "is_set",
                        "type": "person",
                        "negation": true
                    }]
                }),
            },
        )]);
        let flag = person_flag(
            "flag",
            json!([{"key": "id", "value": 1, "operator": "in", "type": "cohort"}]),
        );

        assert!(evaluate_with(
            &flag,
            &HashMap::new(),
            &KnownAbsentProperties::default(),
            &cohorts,
            &HashMap::new()
        )
        .is_err());
        assert_eq!(
            evaluate_with(
                &flag,
                &HashMap::new(),
                &person_keys(&["plan"]),
                &cohorts,
                &HashMap::new()
            )
            .unwrap(),
            FlagValue::Boolean(true)
        );
    }

    #[test]
    fn unsupported_operator_in_a_cohort_leaf_on_a_listed_key_stays_inconclusive() {
        for negation in [false, true] {
            let cohorts = HashMap::from([(
                "1".to_string(),
                CohortDefinition {
                    id: "1".to_string(),
                    properties: json!({
                        "type": "AND",
                        "values": [{
                            "key": "plan",
                            "value": [1, 2],
                            "operator": "not_between",
                            "type": "person",
                            "negation": negation
                        }]
                    }),
                },
            )]);
            let flag = person_flag(
                "flag",
                json!([{"key": "id", "value": 1, "operator": "in", "type": "cohort"}]),
            );

            assert!(
                evaluate_with(
                    &flag,
                    &HashMap::new(),
                    &person_keys(&["plan"]),
                    &cohorts,
                    &HashMap::new()
                )
                .is_err(),
                "not_between with negation {} should stay inconclusive",
                negation
            );
        }
    }

    #[test]
    fn flag_dependency_evaluates_with_the_same_known_absent_keys() {
        let dependency = person_flag(
            "profile-incomplete",
            json!([{"key": "plan", "operator": "is_not_set", "type": "person"}]),
        );
        let flags = HashMap::from([(dependency.key.clone(), dependency)]);
        let flag = person_flag(
            "dependent",
            json!([{"key": "$feature/profile-incomplete", "value": true, "operator": "exact", "type": "flag"}]),
        );

        assert!(evaluate_with(
            &flag,
            &HashMap::new(),
            &KnownAbsentProperties::default(),
            &HashMap::new(),
            &flags
        )
        .is_err());
        assert_eq!(
            evaluate_with(
                &flag,
                &HashMap::new(),
                &person_keys(&["plan"]),
                &HashMap::new(),
                &flags
            )
            .unwrap(),
            FlagValue::Boolean(true)
        );
    }

    fn group_flag(key: &str, properties: serde_json::Value) -> FeatureFlag {
        serde_json::from_value(json!({
            "key": key,
            "active": true,
            "filters": {
                "aggregation_group_type_index": 0,
                "groups": [{"properties": properties, "rollout_percentage": 100}]
            }
        }))
        .unwrap()
    }

    fn cohort(values: serde_json::Value) -> HashMap<String, CohortDefinition> {
        HashMap::from([(
            "1".to_string(),
            CohortDefinition {
                id: "1".to_string(),
                properties: json!({"type": "AND", "values": values}),
            },
        )])
    }

    const PERSON_METADATA_FILTERS: [(&str, &str); 2] =
        [("is_not_set", "null"), ("is_date_after", "\"2025-01-01\"")];

    #[test]
    fn listed_key_does_not_resolve_a_person_metadata_filter() {
        for (operator, value) in PERSON_METADATA_FILTERS {
            let value: serde_json::Value = serde_json::from_str(value).unwrap();
            let filter = json!({"key": "created_at", "value": value, "operator": operator, "type": "person_metadata"});
            let flag = person_flag("new-signups", json!([filter]));
            assert!(
                evaluate(&flag, &HashMap::new(), &person_keys(&["created_at"])).is_err(),
                "{} should stay inconclusive",
                operator
            );

            let cohorts = cohort(json!([filter]));
            let flag = person_flag(
                "new-signups",
                json!([{"key": "id", "value": 1, "operator": "in", "type": "cohort"}]),
            );
            assert!(
                evaluate_with(
                    &flag,
                    &HashMap::new(),
                    &person_keys(&["created_at"]),
                    &cohorts,
                    &HashMap::new()
                )
                .is_err(),
                "{} in a cohort leaf should stay inconclusive",
                operator
            );
        }
    }

    #[test]
    fn listed_key_leaves_a_person_metadata_filter_inconclusive_with_a_supplied_value() {
        // `/flags` reads `person_metadata` from the persons table, so neither the
        // listing nor a supplied person property decides it locally.
        let flag = person_flag(
            "new-signups",
            json!([{"key": "created_at", "value": "2025-01-01", "operator": "is_date_after", "type": "person_metadata"}]),
        );
        let properties = HashMap::from([("created_at".to_string(), json!("2025-06-01"))]);

        assert!(evaluate(&flag, &properties, &person_keys(&["created_at"])).is_err());
    }

    #[test]
    fn listed_person_key_resolves_only_person_filters() {
        for property_type in [None, Some("group"), Some("person_metadata"), Some("event")] {
            let flag = person_flag(
                "flag",
                json!([{"key": "plan", "operator": "is_not_set", "type": property_type}]),
            );
            assert!(
                evaluate(&flag, &HashMap::new(), &person_keys(&["plan"])).is_err(),
                "{:?} should stay inconclusive",
                property_type
            );
        }
    }

    #[test]
    fn listed_group_key_resolves_only_group_filters() {
        for property_type in [None, Some("person"), Some("person_metadata")] {
            let flag = group_flag(
                "org-flag",
                json!([{"key": "tier", "operator": "is_not_set", "type": property_type}]),
            );
            assert!(
                evaluate(
                    &flag,
                    &HashMap::new(),
                    &group_keys("organization", &["tier"])
                )
                .is_err(),
                "{:?} should stay inconclusive",
                property_type
            );
        }
    }

    #[test]
    fn listed_group_key_does_not_resolve_a_cohort_leaf() {
        for leaf_type in ["person", "group"] {
            let cohorts =
                cohort(json!([{"key": "tier", "operator": "is_not_set", "type": leaf_type}]));
            let flag = group_flag(
                "org-flag",
                json!([{"key": "id", "value": 1, "operator": "in", "type": "cohort"}]),
            );
            assert!(
                evaluate_with(
                    &flag,
                    &HashMap::new(),
                    &group_keys("organization", &["tier"]),
                    &cohorts,
                    &HashMap::new()
                )
                .is_err(),
                "{} cohort leaf should stay inconclusive",
                leaf_type
            );
        }
    }

    #[test]
    fn flag_dependency_uses_only_the_keys_for_its_own_aggregation() {
        let group_dependency = group_flag(
            "org-incomplete",
            json!([{"key": "tier", "operator": "is_not_set", "type": "group"}]),
        );
        let person_dependency = person_flag(
            "profile-incomplete",
            json!([{"key": "plan", "operator": "is_not_set", "type": "person"}]),
        );
        let flags = HashMap::from([
            (group_dependency.key.clone(), group_dependency),
            (person_dependency.key.clone(), person_dependency),
        ]);
        let person_parent = person_flag(
            "person-parent",
            json!([{"key": "$feature/org-incomplete", "value": true, "operator": "exact", "type": "flag"}]),
        );
        let group_parent = group_flag(
            "group-parent",
            json!([{"key": "$feature/profile-incomplete", "value": true, "operator": "exact", "type": "flag"}]),
        );

        assert!(evaluate_with(
            &person_parent,
            &HashMap::new(),
            &person_keys(&["tier"]),
            &HashMap::new(),
            &flags
        )
        .is_err());
        assert!(evaluate_with(
            &group_parent,
            &HashMap::new(),
            &group_keys("organization", &["plan"]),
            &HashMap::new(),
            &flags
        )
        .is_err());
        assert_eq!(
            evaluate_with(
                &person_parent,
                &HashMap::new(),
                &group_keys("organization", &["tier"]),
                &HashMap::new(),
                &flags
            )
            .unwrap(),
            FlagValue::Boolean(true)
        );
    }

    #[test]
    fn listed_flag_dependency_key_does_not_resolve_a_nested_dependency() {
        let leaf = person_flag(
            "leaf",
            json!([{"key": "plan", "operator": "is_set", "type": "person"}]),
        );
        let middle = person_flag(
            "middle",
            json!([{"key": "$feature/leaf", "value": true, "operator": "exact", "type": "flag"}]),
        );
        let flags = HashMap::from([(leaf.key.clone(), leaf), (middle.key.clone(), middle)]);
        let flag = person_flag(
            "outer",
            json!([{"key": "$feature/middle", "value": true, "operator": "exact", "type": "flag"}]),
        );

        assert!(evaluate_with(
            &flag,
            &HashMap::new(),
            &person_keys(&["$feature/leaf"]),
            &HashMap::new(),
            &flags
        )
        .is_err());
    }
}

#[cfg(test)]
mod flag_dependency_context_tests {
    use super::*;
    use serde_json::json;

    const DISTINCT_ID: &str = "user-1";

    fn flag(key: &str, aggregation: Option<i32>, condition: serde_json::Value) -> FeatureFlag {
        serde_json::from_value(json!({
            "key": key,
            "active": true,
            "filters": {"aggregation_group_type_index": aggregation, "groups": [condition]}
        }))
        .unwrap()
    }

    fn depends_on(key: &str, aggregation: Option<i32>, parent: &str) -> FeatureFlag {
        flag(
            key,
            aggregation,
            json!({
                "properties": [{"key": format!("$feature/{parent}"), "value": true, "operator": "exact", "type": "flag"}],
                "rollout_percentage": 100
            }),
        )
    }

    struct Inputs {
        distinct_id: String,
        person_properties: HashMap<String, serde_json::Value>,
        groups: HashMap<String, String>,
        group_properties: HashMap<String, HashMap<String, serde_json::Value>>,
        known_absent: KnownAbsentProperties,
        cohorts: HashMap<String, CohortDefinition>,
    }

    impl Default for Inputs {
        fn default() -> Self {
            Self {
                distinct_id: DISTINCT_ID.to_string(),
                person_properties: HashMap::new(),
                groups: HashMap::from([("organization".to_string(), "org-1".to_string())]),
                group_properties: HashMap::new(),
                known_absent: KnownAbsentProperties::default(),
                cohorts: HashMap::new(),
            }
        }
    }

    fn evaluate(
        flag: &FeatureFlag,
        flags: &[&FeatureFlag],
        inputs: &Inputs,
    ) -> Result<FlagValue, InconclusiveMatchError> {
        let flags: HashMap<String, FeatureFlag> = flags
            .iter()
            .map(|flag| (flag.key.clone(), (*flag).clone()))
            .collect();
        let group_type_mapping = HashMap::from([("0".to_string(), "organization".to_string())]);
        let ctx = EvaluationContext {
            cohorts: &inputs.cohorts,
            flags: &flags,
            distinct_id: &inputs.distinct_id,
            groups: &inputs.groups,
            group_properties: &inputs.group_properties,
            group_type_mapping: &group_type_mapping,
            property_matching_version: 1,
        };
        match_feature_flag_with_known_absent(
            flag,
            &inputs.person_properties,
            &ctx,
            &inputs.known_absent,
        )
    }

    fn props(entries: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn in_rollout(flag_key: &str, bucketing_id: &str, percentage: f64) -> bool {
        hash_key(flag_key, bucketing_id, ROLLOUT_HASH_SALT) <= percentage / 100.0
    }

    #[test]
    fn dependency_reads_the_callers_person_properties() {
        let parent = flag(
            "pro-plan",
            None,
            json!({"properties": [{"key": "plan", "value": "pro", "operator": "exact", "type": "person"}], "rollout_percentage": 100}),
        );
        let child = depends_on("child", None, "pro-plan");
        let inputs = Inputs {
            person_properties: props(&[("plan", json!("pro"))]),
            ..Inputs::default()
        };

        assert_eq!(
            evaluate(&parent, &[&parent], &inputs).unwrap(),
            FlagValue::Boolean(true)
        );
        assert_eq!(
            evaluate(&child, &[&parent], &inputs).unwrap(),
            FlagValue::Boolean(true)
        );
    }

    #[test]
    fn group_child_evaluates_a_person_parent_with_person_properties_and_distinct_id() {
        let parent = flag(
            "person-parent",
            None,
            json!({"properties": [{"key": "plan", "value": "pro", "operator": "exact", "type": "person"}], "rollout_percentage": 50}),
        );
        let child = depends_on("org-child", Some(0), "person-parent");
        let distinct_id = (0..1000)
            .map(|i| format!("user-{i}"))
            .find(|id| in_rollout("person-parent", id, 50.0))
            .unwrap();
        let group_key = (0..1000)
            .map(|i| format!("org-{i}"))
            .find(|key| !in_rollout("person-parent", key, 50.0))
            .unwrap();
        let inputs = Inputs {
            distinct_id,
            person_properties: props(&[("plan", json!("pro"))]),
            groups: HashMap::from([("organization".to_string(), group_key)]),
            group_properties: HashMap::from([(
                "organization".to_string(),
                props(&[("plan", json!("free"))]),
            )]),
            ..Inputs::default()
        };

        assert_eq!(
            evaluate(&child, &[&parent], &inputs).unwrap(),
            FlagValue::Boolean(true)
        );
    }

    #[test]
    fn dependency_sees_person_properties_alongside_known_absent_keys() {
        // The first group matches `tier` but is out of rollout, and `early_exit`
        // stops there, so the parent is false and so is the child.
        let parent: FeatureFlag = serde_json::from_value(json!({
            "key": "parent",
            "active": true,
            "filters": {
                "early_exit": true,
                "groups": [
                    {"properties": [{"key": "tier", "value": "vip", "operator": "exact", "type": "person"}], "rollout_percentage": 0},
                    {"properties": [{"key": "plan", "operator": "is_not_set", "type": "person"}], "rollout_percentage": 100}
                ]
            }
        }))
        .unwrap();
        let child = depends_on("child", None, "parent");
        let plan = vec!["plan".to_string()];
        let inputs = Inputs {
            person_properties: props(&[("tier", json!("vip"))]),
            known_absent: KnownAbsentProperties::new(Some(&plan), None),
            ..Inputs::default()
        };

        assert_eq!(
            evaluate(&parent, &[&parent], &inputs).unwrap(),
            FlagValue::Boolean(false)
        );
        assert_eq!(
            evaluate(&child, &[&parent], &inputs).unwrap(),
            FlagValue::Boolean(false)
        );
    }

    #[test]
    fn dependency_resolves_its_own_cohort_filter() {
        let parent = flag(
            "in-cohort",
            None,
            json!({"properties": [{"key": "id", "value": 1, "operator": "in", "type": "cohort"}], "rollout_percentage": 100}),
        );
        let child = depends_on("child", None, "in-cohort");
        let inputs = Inputs {
            person_properties: props(&[("plan", json!("pro"))]),
            cohorts: HashMap::from([(
                "1".to_string(),
                CohortDefinition {
                    id: "1".to_string(),
                    properties: json!({"type": "AND", "values": [{"key": "plan", "value": "pro", "operator": "exact", "type": "person"}]}),
                },
            )]),
            ..Inputs::default()
        };

        assert_eq!(
            evaluate(&child, &[&parent], &inputs).unwrap(),
            FlagValue::Boolean(true)
        );
    }

    #[test]
    fn dependency_resolves_its_own_flag_dependency() {
        let leaf = flag(
            "leaf",
            None,
            json!({"properties": [{"key": "plan", "value": "pro", "operator": "exact", "type": "person"}], "rollout_percentage": 100}),
        );
        let middle = depends_on("middle", None, "leaf");
        let child = depends_on("child", None, "middle");
        let inputs = Inputs {
            person_properties: props(&[("plan", json!("pro"))]),
            ..Inputs::default()
        };

        assert_eq!(
            evaluate(&child, &[&leaf, &middle], &inputs).unwrap(),
            FlagValue::Boolean(true)
        );
    }

    #[test]
    fn dependency_cycle_is_inconclusive() {
        let first = depends_on("first", None, "second");
        let second = depends_on("second", None, "first");
        let own = depends_on("own", None, "own");

        assert!(evaluate(&first, &[&first, &second], &Inputs::default()).is_err());
        assert!(evaluate(&own, &[&own], &Inputs::default()).is_err());
    }

    fn chain(length: usize) -> Vec<FeatureFlag> {
        let mut flags = vec![flag(
            "flag-0",
            None,
            json!({"properties": [], "rollout_percentage": 100}),
        )];
        for i in 1..length {
            flags.push(depends_on(
                &format!("flag-{i}"),
                None,
                &format!("flag-{}", i - 1),
            ));
        }
        flags
    }

    #[test]
    fn dependency_chain_under_the_depth_limit_resolves() {
        let flags = chain(MAX_FLAG_DEPENDENCY_DEPTH);
        let refs: Vec<&FeatureFlag> = flags.iter().collect();

        assert_eq!(
            evaluate(flags.last().unwrap(), &refs, &Inputs::default()).unwrap(),
            FlagValue::Boolean(true)
        );
    }

    #[test]
    fn dependency_chain_over_the_depth_limit_is_inconclusive() {
        // Deep enough to overflow the stack without a limit.
        let is_inconclusive = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let flags = chain(5_000);
                let refs: Vec<&FeatureFlag> = flags.iter().collect();
                evaluate(flags.last().unwrap(), &refs, &Inputs::default()).is_err()
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(is_inconclusive);
    }

    /// Each flag requires the previous two, so without reusing results the
    /// work doubles with every flag.
    fn branching_graph(length: usize) -> Vec<FeatureFlag> {
        let mut flags = vec![
            flag(
                "flag-0",
                None,
                json!({"properties": [], "rollout_percentage": 100}),
            ),
            flag(
                "flag-1",
                None,
                json!({"properties": [], "rollout_percentage": 100}),
            ),
        ];
        for i in 2..length {
            flags.push(flag(
                &format!("flag-{i}"),
                None,
                json!({
                    "properties": [
                        {"key": format!("$feature/flag-{}", i - 1), "value": true, "operator": "exact", "type": "flag"},
                        {"key": format!("$feature/flag-{}", i - 2), "value": true, "operator": "exact", "type": "flag"}
                    ],
                    "rollout_percentage": 100
                }),
            ));
        }
        flags
    }

    #[test]
    fn shared_dependencies_are_evaluated_once_per_evaluation() {
        let flags = branching_graph(40);
        let refs: Vec<&FeatureFlag> = flags.iter().collect();
        let started = std::time::Instant::now();

        assert_eq!(
            evaluate(flags.last().unwrap(), &refs, &Inputs::default()).unwrap(),
            FlagValue::Boolean(true)
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn inconclusive_shared_dependencies_stay_bounded() {
        // No result below the top is conclusive, so none can be reused: the
        // base flags either form a cycle or need a property that isn't given.
        let missing_plan = json!({"properties": [{"key": "plan", "value": "pro", "operator": "exact", "type": "person"}], "rollout_percentage": 100});
        for base in [
            [
                depends_on("flag-0", None, "flag-1"),
                depends_on("flag-1", None, "flag-0"),
            ],
            [
                flag("flag-0", None, missing_plan.clone()),
                flag("flag-1", None, missing_plan.clone()),
            ],
        ] {
            let mut flags = branching_graph(40);
            flags.splice(0..2, base);
            let refs: Vec<&FeatureFlag> = flags.iter().collect();
            let started = std::time::Instant::now();

            assert!(evaluate(flags.last().unwrap(), &refs, &Inputs::default()).is_err());
            assert!(
                started.elapsed() < std::time::Duration::from_secs(1),
                "took {:?}",
                started.elapsed()
            );
        }
    }

    #[test]
    fn dependency_budget_applies_to_each_top_level_flag() {
        let mut flags = branching_graph(40);
        flags.push(flag(
            "independent",
            None,
            json!({"properties": [], "rollout_percentage": 100}),
        ));
        flags[0] = flag(
            "flag-0",
            None,
            json!({"properties": [{"key": "plan", "value": "pro", "operator": "exact", "type": "person"}], "rollout_percentage": 100}),
        );
        let flags_by_key: HashMap<String, FeatureFlag> = flags
            .iter()
            .map(|flag| (flag.key.clone(), flag.clone()))
            .collect();
        let ctx = EvaluationContext {
            cohorts: &HashMap::new(),
            flags: &flags_by_key,
            distinct_id: DISTINCT_ID,
            groups: &HashMap::new(),
            group_properties: &HashMap::new(),
            group_type_mapping: &HashMap::new(),
            property_matching_version: 1,
        };
        let known_absent = KnownAbsentProperties::default();
        let heavy = match_feature_flag_with_known_absent(
            &flags_by_key["flag-39"],
            &HashMap::new(),
            &ctx,
            &known_absent,
        );
        let light = match_feature_flag_with_known_absent(
            &depends_on("light", None, "independent"),
            &HashMap::new(),
            &ctx,
            &known_absent,
        );

        assert!(heavy.is_err());
        assert_eq!(light.unwrap(), FlagValue::Boolean(true));
    }

    #[test]
    fn cached_dependency_respects_the_depth_limit_in_either_filter_order() {
        // `leaf` is reached directly and as the end of a chain that puts it one
        // flag past the depth limit. The filter order mustn't change the result.
        let leaf = flag(
            "leaf",
            None,
            json!({"properties": [], "rollout_percentage": 100}),
        );
        let mut chain = vec![depends_on("chain-1", None, "leaf")];
        let length = MAX_FLAG_DEPENDENCY_DEPTH - 1;
        for i in 2..=length {
            chain.push(depends_on(
                &format!("chain-{i}"),
                None,
                &format!("chain-{}", i - 1),
            ));
        }
        let leaf_filter =
            json!({"key": "$feature/leaf", "value": true, "operator": "exact", "type": "flag"});
        let chain_filter = json!({"key": format!("$feature/chain-{length}"), "value": true, "operator": "exact", "type": "flag"});
        let mut refs: Vec<&FeatureFlag> = chain.iter().collect();
        refs.push(&leaf);

        for filters in [
            json!([leaf_filter, chain_filter]),
            json!([chain_filter, leaf_filter]),
        ] {
            let top = flag(
                "top",
                None,
                json!({"properties": filters, "rollout_percentage": 100}),
            );
            assert!(
                evaluate(&top, &refs, &Inputs::default()).is_err(),
                "{}",
                filters
            );
        }
    }

    #[test]
    fn cached_dependency_too_deep_to_reuse_is_evaluated_again() {
        // `checked` is false at any depth: its `plan` filter rules out its only
        // condition, whatever its dependency gives. It's cached at the top, then
        // reached again one flag short of the limit, where reusing its cached
        // depth would exceed the limit but evaluating it again doesn't need to.
        let deep = flag(
            "deep",
            None,
            json!({"properties": [], "rollout_percentage": 100}),
        );
        let checked = flag(
            "checked",
            None,
            json!({"properties": [
                {"key": "$feature/deep", "value": true, "operator": "exact", "type": "flag"},
                {"key": "plan", "value": "pro", "operator": "exact", "type": "person"}
            ], "rollout_percentage": 100}),
        );
        let not_checked =
            json!({"key": "$feature/checked", "value": false, "operator": "exact", "type": "flag"});
        let length = MAX_FLAG_DEPENDENCY_DEPTH - 2;
        let mut chain = vec![flag(
            "chain-1",
            None,
            json!({"properties": [not_checked], "rollout_percentage": 100}),
        )];
        for i in 2..=length {
            chain.push(depends_on(
                &format!("chain-{i}"),
                None,
                &format!("chain-{}", i - 1),
            ));
        }
        let top = flag(
            "top",
            None,
            json!({"properties": [
                not_checked,
                {"key": format!("$feature/chain-{length}"), "value": true, "operator": "exact", "type": "flag"}
            ], "rollout_percentage": 100}),
        );
        let mut refs: Vec<&FeatureFlag> = chain.iter().collect();
        refs.extend([&deep, &checked]);
        let inputs = Inputs {
            person_properties: props(&[("plan", json!("free"))]),
            ..Inputs::default()
        };

        assert_eq!(
            evaluate(&top, &refs, &inputs).unwrap(),
            FlagValue::Boolean(true)
        );
    }

    #[test]
    fn inconclusive_dependency_is_evaluated_once() {
        // Every condition but the last references the same inconclusive
        // dependency and is then ruled out, more times than the evaluation
        // budget. The last condition's dependency still resolves.
        let unknown = flag(
            "unknown",
            None,
            json!({"properties": [{"key": "plan", "value": "pro", "operator": "exact", "type": "person"}], "rollout_percentage": 100}),
        );
        let known = flag(
            "known",
            None,
            json!({"properties": [], "rollout_percentage": 100}),
        );
        let mut conditions: Vec<serde_json::Value> = (0..MAX_FLAG_DEPENDENCY_EVALUATIONS + 10)
            .map(|_| {
                json!({"properties": [
                    {"key": "$feature/unknown", "value": true, "operator": "exact", "type": "flag"},
                    {"key": "region", "value": "us", "operator": "exact", "type": "person"}
                ], "rollout_percentage": 100})
            })
            .collect();
        conditions.push(json!({"properties": [
            {"key": "$feature/known", "value": true, "operator": "exact", "type": "flag"}
        ], "rollout_percentage": 100}));
        let top: FeatureFlag = serde_json::from_value(json!({
            "key": "top",
            "active": true,
            "filters": {"groups": conditions}
        }))
        .unwrap();
        let inputs = Inputs {
            person_properties: props(&[("region", json!("eu"))]),
            ..Inputs::default()
        };

        assert_eq!(
            evaluate(&top, &[&unknown, &known], &inputs).unwrap(),
            FlagValue::Boolean(true)
        );
    }

    /// A flag whose only filter is a cohort nested `depth` groups deep, with a
    /// leaf that depends on `next`.
    fn flag_through_nested_cohort(
        index: usize,
        depth: usize,
        next: &str,
    ) -> (FeatureFlag, CohortDefinition) {
        let mut group = json!({"type": "AND", "values": [
            {"key": format!("$feature/{next}"), "value": true, "operator": "exact", "type": "flag"}
        ]});
        for _ in 0..depth {
            group = json!({"type": "AND", "values": [group]});
        }
        (
            flag(
                &format!("flag-{index}"),
                None,
                json!({"properties": [{"key": "id", "value": index, "operator": "in", "type": "cohort"}], "rollout_percentage": 100}),
            ),
            CohortDefinition {
                id: index.to_string(),
                properties: group,
            },
        )
    }

    fn alternating_chain(
        length: usize,
        cohort_depth: usize,
    ) -> (Vec<FeatureFlag>, HashMap<String, CohortDefinition>) {
        let mut flags = vec![flag(
            &format!("flag-{length}"),
            None,
            json!({"properties": [], "rollout_percentage": 100}),
        )];
        let mut cohorts = HashMap::new();
        for i in 0..length {
            let (flag, cohort) =
                flag_through_nested_cohort(i, cohort_depth, &format!("flag-{}", i + 1));
            flags.push(flag);
            cohorts.insert(i.to_string(), cohort);
        }
        (flags, cohorts)
    }

    #[test]
    fn alternating_cohort_and_flag_chain_is_inconclusive() {
        // Each flag goes through a cohort nested just under the cohort limit,
        // so neither limit alone stops the chain.
        let is_inconclusive = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let (flags, cohorts) = alternating_chain(
                    MAX_FLAG_DEPENDENCY_DEPTH - 1,
                    MAX_COHORT_RESOLUTION_DEPTH - 5,
                );
                let refs: Vec<&FeatureFlag> = flags.iter().collect();
                let inputs = Inputs {
                    cohorts,
                    ..Inputs::default()
                };
                evaluate(&flags[1], &refs, &inputs).is_err()
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(is_inconclusive);
    }

    #[test]
    fn short_alternating_cohort_and_flag_chain_resolves() {
        let (flags, cohorts) = alternating_chain(3, 5);
        let refs: Vec<&FeatureFlag> = flags.iter().collect();
        let inputs = Inputs {
            cohorts,
            ..Inputs::default()
        };

        assert_eq!(
            evaluate(&flags[1], &refs, &inputs).unwrap(),
            FlagValue::Boolean(true)
        );
    }

    /// A chain of flags whose deepest flag, at the depth limit's last level,
    /// matches a cohort nested `cohort_depth` groups deep.
    fn chain_ending_in_a_nested_cohort(
        cohort_depth: usize,
    ) -> (Vec<FeatureFlag>, HashMap<String, CohortDefinition>) {
        let mut group = json!({"type": "AND", "values": [
            {"key": "plan", "value": "pro", "operator": "exact", "type": "person"}
        ]});
        for _ in 0..cohort_depth {
            group = json!({"type": "AND", "values": [group]});
        }
        let mut flags = chain(MAX_FLAG_DEPENDENCY_DEPTH);
        flags[0] = flag(
            "flag-0",
            None,
            json!({"properties": [{"key": "id", "value": 0, "operator": "in", "type": "cohort"}], "rollout_percentage": 100}),
        );
        let cohorts = HashMap::from([(
            "0".to_string(),
            CohortDefinition {
                id: "0".to_string(),
                properties: group,
            },
        )]);
        (flags, cohorts)
    }

    #[test]
    fn cohort_nesting_counts_from_the_enclosing_dependency_depth() {
        for (cohort_depth, is_resolved) in [(0, true), (50, false)] {
            let (flags, cohorts) = chain_ending_in_a_nested_cohort(cohort_depth);
            let refs: Vec<&FeatureFlag> = flags.iter().collect();
            let inputs = Inputs {
                person_properties: props(&[("plan", json!("pro"))]),
                cohorts,
                ..Inputs::default()
            };

            let result = evaluate(flags.last().unwrap(), &refs, &inputs);
            assert_eq!(result.is_ok(), is_resolved, "{cohort_depth}: {result:?}");
        }
    }

    #[test]
    fn cached_dependency_respects_cohort_nesting_in_either_filter_order() {
        // `nested` matches a cohort 50 groups deep. It's reached directly and
        // at the end of a chain that puts it at the last flag dependency level,
        // where its cohort is past the depth limit. The filter order mustn't
        // change the result.
        let (flags, cohorts) = chain_ending_in_a_nested_cohort(50);
        let mut nested = flags[0].clone();
        nested.key = "nested".to_string();
        let mut chain = vec![depends_on("chain-1", None, "nested")];
        let length = MAX_FLAG_DEPENDENCY_DEPTH - 2;
        for i in 2..=length {
            chain.push(depends_on(
                &format!("chain-{i}"),
                None,
                &format!("chain-{}", i - 1),
            ));
        }
        let nested_filter =
            json!({"key": "$feature/nested", "value": true, "operator": "exact", "type": "flag"});
        let chain_filter = json!({"key": format!("$feature/chain-{length}"), "value": true, "operator": "exact", "type": "flag"});
        let mut refs: Vec<&FeatureFlag> = chain.iter().collect();
        refs.push(&nested);
        let inputs = Inputs {
            person_properties: props(&[("plan", json!("pro"))]),
            cohorts,
            ..Inputs::default()
        };

        for filters in [
            json!([nested_filter, chain_filter]),
            json!([chain_filter, nested_filter]),
        ] {
            let top = flag(
                "top",
                None,
                json!({"properties": filters, "rollout_percentage": 100}),
            );
            assert!(evaluate(&top, &refs, &inputs).is_err(), "{}", filters);
        }
    }

    #[test]
    fn person_child_reads_group_properties_for_an_org_parent() {
        // D1: the parent reads the org's region, not the person's.
        let parent = flag(
            "org-in-us",
            Some(0),
            json!({"properties": [{"key": "region", "value": "us", "operator": "exact", "type": "group"}], "rollout_percentage": 100}),
        );
        let child = depends_on("child", None, "org-in-us");
        let inputs = Inputs {
            person_properties: props(&[("region", json!("us"))]),
            group_properties: HashMap::from([(
                "organization".to_string(),
                props(&[("region", json!("eu"))]),
            )]),
            ..Inputs::default()
        };

        assert_eq!(
            evaluate(&parent, &[&parent], &inputs).unwrap(),
            FlagValue::Boolean(false)
        );
        assert_eq!(
            evaluate(&child, &[&parent], &inputs).unwrap(),
            FlagValue::Boolean(false)
        );
    }

    #[test]
    fn org_parent_without_its_group_is_false() {
        // D2: no organization group, even with organization known-absent keys.
        let parent = flag(
            "org-incomplete",
            Some(0),
            json!({"properties": [{"key": "tier", "operator": "is_not_set", "type": "group"}], "rollout_percentage": 100}),
        );
        let child = depends_on("child", None, "org-incomplete");
        let groups = HashMap::from([("organization".to_string(), vec!["tier".to_string()])]);
        let inputs = Inputs {
            groups: HashMap::new(),
            known_absent: KnownAbsentProperties::new(None, Some(&groups)),
            ..Inputs::default()
        };

        assert_eq!(
            evaluate(&parent, &[&parent], &inputs).unwrap(),
            FlagValue::Boolean(false)
        );
        assert_eq!(
            evaluate(&child, &[&parent], &inputs).unwrap(),
            FlagValue::Boolean(false)
        );
    }

    #[test]
    fn org_parent_rollout_follows_the_group_key() {
        let parent = flag(
            "org-rollout",
            Some(0),
            json!({"properties": [], "rollout_percentage": 50}),
        );
        let child = depends_on("child", None, "org-rollout");
        let in_ids = (0..1000)
            .map(|i| format!("id-{i}"))
            .filter(|id| in_rollout("org-rollout", id, 50.0));
        let out_ids = (0..1000)
            .map(|i| format!("id-{i}"))
            .filter(|id| !in_rollout("org-rollout", id, 50.0));
        let (included, excluded) = (
            in_ids.take(1).next().unwrap(),
            out_ids.take(1).next().unwrap(),
        );

        for (distinct_id, group_key, expected) in [
            (included.clone(), excluded.clone(), false),
            (excluded, included, true),
        ] {
            let inputs = Inputs {
                distinct_id,
                groups: HashMap::from([("organization".to_string(), group_key)]),
                group_properties: HashMap::from([("organization".to_string(), HashMap::new())]),
                ..Inputs::default()
            };
            assert_eq!(
                evaluate(&child, &[&parent], &inputs).unwrap(),
                FlagValue::Boolean(expected)
            );
        }
    }

    /// The empty string is the only unusable group key the `String`-typed
    /// `groups` map can carry, and it counts as no key.
    fn empty_group_key_inputs(group_properties: bool) -> Inputs {
        Inputs {
            groups: HashMap::from([("organization".to_string(), String::new())]),
            group_properties: if group_properties {
                HashMap::from([(
                    "organization".to_string(),
                    props(&[("tier", json!("enterprise"))]),
                )])
            } else {
                HashMap::new()
            },
            ..Inputs::default()
        }
    }

    #[test]
    fn empty_group_key_skips_group_conditions() {
        let org_flag = flag(
            "org-flag",
            Some(0),
            json!({"properties": [{"key": "tier", "value": "enterprise", "operator": "exact", "type": "group"}], "rollout_percentage": 100}),
        );
        let org_rollout = flag(
            "org-rollout",
            Some(0),
            json!({"properties": [], "rollout_percentage": 100}),
        );
        for with_properties in [false, true] {
            let inputs = empty_group_key_inputs(with_properties);
            for flag in [&org_flag, &org_rollout] {
                assert_eq!(
                    evaluate(flag, &[], &inputs).unwrap(),
                    FlagValue::Boolean(false),
                    "{} with group properties: {with_properties}",
                    flag.key
                );
            }
        }
    }

    #[test]
    fn empty_group_key_leaves_the_person_condition_to_decide() {
        let mixed: FeatureFlag = serde_json::from_value(json!({
            "key": "mixed",
            "active": true,
            "filters": {"groups": [
                {"properties": [{"key": "tier", "value": "enterprise", "operator": "exact", "type": "group", "group_type_index": 0}], "rollout_percentage": 100, "aggregation_group_type_index": 0},
                {"properties": [{"key": "plan", "value": "pro", "operator": "exact", "type": "person"}], "rollout_percentage": 100}
            ]}
        }))
        .unwrap();
        for (plan, expected) in [("pro", true), ("free", false)] {
            for with_properties in [false, true] {
                let inputs = Inputs {
                    person_properties: props(&[("plan", json!(plan))]),
                    ..empty_group_key_inputs(with_properties)
                };
                assert_eq!(
                    evaluate(&mixed, &[], &inputs).unwrap(),
                    FlagValue::Boolean(expected),
                    "plan {plan}, group properties: {with_properties}"
                );
            }
        }
    }

    #[test]
    fn group_dependency_with_an_empty_group_key_is_false() {
        let parent = flag(
            "org-parent",
            Some(0),
            json!({"properties": [], "rollout_percentage": 100}),
        );
        let child = depends_on("child", None, "org-parent");
        for with_properties in [false, true] {
            assert_eq!(
                evaluate(&child, &[&parent], &empty_group_key_inputs(with_properties)).unwrap(),
                FlagValue::Boolean(false),
                "group properties: {with_properties}"
            );
        }
    }

    #[test]
    fn usable_group_key_without_group_properties_stays_inconclusive() {
        let org_flag = flag(
            "org-flag",
            Some(0),
            json!({"properties": [{"key": "tier", "value": "enterprise", "operator": "exact", "type": "group"}], "rollout_percentage": 100}),
        );
        assert!(evaluate(&org_flag, &[], &Inputs::default()).is_err());
    }

    #[test]
    fn group_filter_without_an_index_reads_the_conditions_group_type() {
        // G1: a listed key wins over the supplied value for the condition's group.
        for (operator, value, expected) in [
            ("exact", json!("enterprise"), false),
            ("is_not_set", json!(null), true),
        ] {
            let org_flag = flag(
                "org-flag",
                Some(0),
                json!({"properties": [{"key": "tier", "value": value, "operator": operator, "type": "group"}], "rollout_percentage": 100}),
            );
            let groups = HashMap::from([("organization".to_string(), vec!["tier".to_string()])]);
            let inputs = Inputs {
                group_properties: HashMap::from([(
                    "organization".to_string(),
                    props(&[("tier", json!("enterprise"))]),
                )]),
                known_absent: KnownAbsentProperties::new(None, Some(&groups)),
                ..Inputs::default()
            };
            assert_eq!(
                evaluate(&org_flag, &[], &inputs).unwrap(),
                FlagValue::Boolean(expected),
                "{operator}"
            );
        }
    }
}
