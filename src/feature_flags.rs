use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum FlagValue {
    Boolean(bool),
    String(String),
}

#[derive(Debug)]
pub(crate) struct InconclusiveMatchError {
    pub message: String,
}

impl InconclusiveMatchError {
    pub fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

impl Default for FlagValue {
    fn default() -> Self {
        FlagValue::Boolean(false)
    }
}

impl std::fmt::Display for FlagValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlagValue::Boolean(b) => write!(f, "{}", b),
            FlagValue::String(s) => write!(f, "{}", s),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlag {
    pub key: String,
    pub active: bool,
    #[serde(default)]
    pub(crate) filters: FeatureFlagFilters,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct FeatureFlagFilters {
    #[serde(default)]
    pub groups: Vec<FeatureFlagCondition>,
    #[serde(default)]
    pub multivariate: Option<MultivariateFilter>,
    #[serde(default)]
    pub payloads: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FeatureFlagCondition {
    #[serde(default)]
    pub properties: Vec<Property>,
    pub rollout_percentage: Option<f64>,
    pub variant: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Property {
    pub key: String,
    pub value: serde_json::Value,
    #[serde(default = "default_operator")]
    pub operator: String,
    #[serde(rename = "type")]
    pub property_type: Option<String>,
}

fn default_operator() -> String {
    "exact".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct MultivariateFilter {
    pub variants: Vec<MultivariateVariant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MultivariateVariant {
    pub key: String,
    pub rollout_percentage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FeatureFlagsResponse {
    pub flags: HashMap<String, FlagDetail>,
    #[serde(rename = "errorsWhileComputingFlags")]
    #[serde(default)]
    pub errors_while_computing_flags: bool,
    #[serde(rename = "requestId")]
    #[serde(default)]
    pub request_id: Option<String>,
}

impl FeatureFlagsResponse {
    /// Convert the response to a normalized format
    /// Returns: (flags, payloads, request_id, flag_details)
    pub fn normalize(
        self,
    ) -> (
        HashMap<String, FlagValue>,
        HashMap<String, serde_json::Value>,
        Option<String>,
        HashMap<String, FlagDetail>,
    ) {
        let mut feature_flags = HashMap::new();
        let mut payloads = HashMap::new();
        let flag_details = self.flags.clone(); // Keep full details for metadata

        for (key, detail) in self.flags {
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

        (feature_flags, payloads, self.request_id, flag_details)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagDetail {
    pub key: String,
    pub enabled: bool,
    pub variant: Option<String>,
    #[serde(default)]
    pub reason: Option<FlagReason>,
    #[serde(default)]
    pub metadata: Option<FlagMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagReason {
    pub code: String,
    #[serde(default)]
    pub condition_index: Option<usize>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagMetadata {
    pub id: u64,
    pub version: u32,
    pub description: Option<String>,
    pub payload: Option<serde_json::Value>,
}

const LONG_SCALE: f64 = 0xFFFFFFFFFFFFFFFu64 as f64; // Must be exactly 15 F's for hash compatibility

pub fn hash_key(key: &str, distinct_id: &str, salt: &str) -> f64 {
    let hash_key = format!("{key}.{distinct_id}{salt}");
    let mut hasher = Sha1::new();
    hasher.update(hash_key.as_bytes());
    let result = hasher.finalize();
    let hex_str = format!("{result:x}");
    let hash_val = u64::from_str_radix(&hex_str[..15], 16).unwrap_or(0);
    hash_val as f64 / LONG_SCALE
}

pub fn get_matching_variant(flag: &FeatureFlag, distinct_id: &str) -> Option<String> {
    let hash_value = hash_key(&flag.key, distinct_id, "variant");
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

pub(crate) fn match_feature_flag(
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
        let hash_value = hash_key(&flag.key, distinct_id, "");
        if hash_value > (rollout_percentage / 100.0) {
            return Ok(false);
        }
    }

    Ok(true)
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
            match regex::Regex::new(&regex_str) {
                Ok(re) => re.is_match(&prop_str),
                Err(_) => false,
            }
        }
        "not_regex" => {
            let prop_str = value_to_string(value);
            let regex_str = value_to_string(&property.value);
            match regex::Regex::new(&regex_str) {
                Ok(re) => !re.is_match(&prop_str),
                Err(_) => true,
            }
        }
        "gt" | "gte" | "lt" | "lte" => compare_numeric(&property.operator, &property.value, value),
        _ => false,
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

    #[test]
    fn test_hash_key() {
        let hash = hash_key("test-flag", "user-123", "");
        assert!(hash >= 0.0 && hash <= 1.0);

        // Same inputs should produce same hash
        let hash2 = hash_key("test-flag", "user-123", "");
        assert_eq!(hash, hash2);

        // Different inputs should produce different hash
        let hash3 = hash_key("test-flag", "user-456", "");
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
}
