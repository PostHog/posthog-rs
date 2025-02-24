use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use super::{PostHogSDKClient, PostHogApiError};

/// Builder for constructing requests to the /decide/ endpoint.
/// 
/// This builder helps create properly structured requests for feature flag evaluation
/// and targeting. It supports setting user identification, groups, and properties.
/// 
/// # Example
/// ```rust
/// use std::collections::HashMap;
/// use serde_json::json;
/// use posthog_rs::sdk::decide::DecideRequestBuilder;
/// 
/// let request = DecideRequestBuilder::new("user-123".to_string())
///     .with_groups([("company".to_string(), json!("acme-corp"))].into())
///     .with_person_properties([("plan".to_string(), json!("premium"))].into())
///     .build();
/// ```
#[derive(Debug, Serialize)]
pub struct DecideRequestBuilder {
    /// The unique identifier for the user
    pub distinct_id: String,
    /// Optional group mappings for flag targeting
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<HashMap<String, Value>>,
    /// Optional user properties for flag targeting
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_properties: Option<HashMap<String, serde_json::Value>>,
    /// Optional group properties for flag targeting
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_properties: Option<HashMap<String, HashMap<String, serde_json::Value>>>,
}

impl DecideRequestBuilder {
    pub fn new(distinct_id: String) -> Self {
        Self {
            distinct_id,
            groups: None,
            person_properties: None,
            group_properties: None,
        }
    }

    pub fn build(self) -> Value {
        serde_json::json!(self)
    }

    pub fn with_groups(mut self, groups: HashMap<String, Value>) -> Self {
        self.groups = Some(groups);
        self
    }

    pub fn with_person_properties(
        mut self,
        person_properties: HashMap<String, serde_json::Value>,
    ) -> Self {
        self.person_properties = Some(person_properties);
        self
    }

    pub fn with_group_properties(
        mut self,
        group_properties: HashMap<String, HashMap<String, serde_json::Value>>,
    ) -> Self {
        self.group_properties = Some(group_properties);
        self
    }
}

/// Response from the /decide/ endpoint containing feature flag states and configurations.
/// 
/// This struct contains all feature flag related data including:
/// - Feature flag states (boolean or multi-variant) in the `feature_flags` field
/// - Feature flag payloads (additional configuration data) in the `feature_flag_payloads` field
/// - Toolbar configuration for the PostHog toolbar in the `toolbar_params` field
/// - Configuration settings in the `config` field
/// - Error state indicator in the `errors_while_computing_flags` field
/// 
/// # Example Response Structure
/// ```json
/// {
///     "featureFlags": {
///         "my-feature": true,
///         "awesome-multivariant": "var2"
///     },
///     "featureFlagPayloads": {
///         "awesome-multivariant": "{\"hello\": \"world\"}",
///         "config-example": "{\"version\": \"1\", \"example\": \"hello\"}"
///     },
///     "config": {},
///     "toolbarParams": {},
///     "errorsWhileComputingFlags": false
/// }
/// ```
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecideResponse {
    pub config: Value,
    pub toolbar_params: Value,
    pub errors_while_computing_flags: bool,
    pub is_authenticated: bool,
    pub supported_compression: Vec<String>,
    pub feature_flags: HashMap<String, Value>,
    pub feature_flag_payloads: HashMap<String, String>,
}

impl PostHogSDKClient {
    /// Fetches feature flag states for a given user from the /decide/ endpoint.
    /// 
    /// This method evaluates feature flags based on the provided request data and returns
    /// the current state of all flags for the user, including any associated payloads.
    /// The response includes boolean flags, multi-variant flags, and their associated
    /// configuration payloads.
    /// 
    /// # Arguments
    /// * `request` - A JSON Value containing the decide request data, typically created using `DecideRequestBuilder`
    /// 
    /// # Returns
    /// Returns a Result containing the DecideResponse or an error
    /// 
    /// # Example
    /// ```rust
    /// use std::collections::HashMap;
    /// use posthog_rs::sdk::decide::{DecideRequestBuilder, DecideResponse};
    /// 
    /// # async fn example() -> anyhow::Result<()> {
    /// let client = posthog_rs::sdk::PostHogSDKClient::new(
    ///     "your-api-key".to_string(),
    ///     "your-public-key".to_string(),
    ///     "https://app.posthog.com".to_string(),
    /// )?;
    /// 
    /// let request = DecideRequestBuilder::new("user123".to_string())
    ///     .build();
    /// 
    /// let response = client.decide(request).await?;
    /// println!("Feature flags: {:?}", response.feature_flags);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn decide(&self, mut request: Value) -> Result<DecideResponse, PostHogApiError> {
        debug!("Decide Endpoint called");
        // Serialize the request data
        request["api_key"] = self.public_key.clone().into();

        // Construct the query URL with the encoded data
        debug!("Sending Decide Request: {:?}", request);
        let (_status, response) = self
            .api_request(
                Method::POST,
                format!("/decide/?v=3").as_str(),
                Some(request),
                false,
            )
            .await?;

        debug!("Got Response: {:#?}", response);
        // Deserialize the response
        let response = serde_json::from_value(response).map_err(PostHogApiError::JsonError)?;

        debug!("Returning response");
        Ok(response)
    }

    // ! feature flags helpers

    /// Checks if a specific feature flag is enabled for a user.
    /// 
    /// # Arguments
    /// * `request` - The decide request data
    /// * `key` - The feature flag key to check
    /// 
    /// # Returns
    /// * `Ok(true)` - If the flag exists and is enabled
    /// * `Ok(false)` - If the flag exists but is disabled
    /// * `Err(PostHogError::FeatureFlagNotFound)` - If the flag doesn't exist
    /// 
    /// # Example
    /// ```rust
    /// use posthog_rs::sdk::decide::DecideRequestBuilder;
    /// # async fn example() -> anyhow::Result<()> {
    /// let client = posthog_rs::sdk::PostHogSDKClient::new(
    ///     "your-api-key".to_string(),
    ///     "your-public-key".to_string(),
    ///     "https://app.posthog.com".to_string(),
    /// )?;
    ///
    /// let request = DecideRequestBuilder::new("user123".to_string()).build();
    /// let is_enabled = client.get_feature_flag_enabled(request, "my-feature".to_string()).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_feature_flag_enabled(
        &self,
        request: Value,
        key: String,
    ) -> Result<bool, PostHogApiError> {
        let res = self.decide(request).await?;

        let Some(feature_flag) = res.feature_flags.get(&key) else {
            return Err(PostHogApiError::FeatureFlagNotFound(key));
        };

        Ok(feature_flag == &Value::Bool(true))
    }

    /// Gets the variant value for a multi-variant feature flag.
    /// 
    /// Multi-variant feature flags can return different values for different users based on
    /// the flag's rollout rules. This method fetches the specific variant assigned to the user.
    /// Common use cases include A/B testing, gradual rollouts, or serving different configurations
    /// to different user segments.
    /// 
    /// # Arguments
    /// * `request` - The decide request data
    /// * `key` - The feature flag key to check
    /// 
    /// # Returns
    /// * `Ok((key, value))` - The flag key and its variant value (e.g., "var1", "var2")
    /// * `Err(PostHogError::FeatureFlagNotFound)` - If the flag doesn't exist
    /// 
    /// # Example
    /// ```rust
    /// use posthog_rs::sdk::decide::DecideRequestBuilder;
    /// # async fn example() -> anyhow::Result<()> {
    /// let client = posthog_rs::sdk::PostHogSDKClient::new(
    ///     "your-api-key".to_string(),
    ///     "your-public-key".to_string(),
    ///     "https://app.posthog.com".to_string(),
    /// )?;
    /// 
    /// let request = DecideRequestBuilder::new("user123".to_string()).build();
    /// let (key, variant) = client.get_feature_flag_multi_variant(request, "awesome-multivariant".to_string()).await?;
    /// // variant might be "var1", "var2", etc.
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_feature_flag_multi_variant(
        &self,
        request: Value,
        key: String,
    ) -> Result<(String, Value), PostHogApiError> {
        let res = self.decide(request).await?;

        let payload = res
            .feature_flags
            .get(&key)
            .cloned()
            .ok_or(PostHogApiError::FeatureFlagNotFound(key.clone()))?;

        Ok((key, payload))
    }

    /// Gets the payload data associated with a feature flag.
    /// 
    /// # Arguments
    /// * `request` - The decide request data
    /// * `key` - The feature flag key to get the payload for
    /// 
    /// # Returns
    /// * `Ok(String)` - The payload data as a JSON string
    /// * `Err(PostHogError::FeatureFlagNotFound)` - If the flag doesn't exist
    /// * `Err(PostHogError::FeatureFlagNotEnabled)` - If the flag exists but is disabled
    /// 
    /// # Example
    /// ```rust
    /// # async fn example() -> anyhow::Result<()> {
    /// use posthog_rs::sdk::decide::DecideRequestBuilder;
    /// let client = posthog_rs::sdk::PostHogSDKClient::new(
    ///     "your-api-key".to_string(),
    ///     "your-public-key".to_string(),
    ///     "https://app.posthog.com".to_string(),
    /// )?;
    ///
    /// let request = DecideRequestBuilder::new("user123".to_string()).build();
    /// let payload = client.get_feature_flag_payload(request, "config-example".to_string()).await?;
    /// let payload_json: serde_json::Value = serde_json::from_str(&payload)?;
    /// // Access payload data: payload_json["version"], payload_json["example"], etc.
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_feature_flag_payload(
        &self,
        request: Value,
        key: String,
    ) -> Result<String, PostHogApiError> {
        let res = self.decide(request).await?;

        let enabled = res
            .feature_flags
            .get(&key)
            .cloned()
            .ok_or(PostHogApiError::FeatureFlagNotFound(key.clone()))?;

        if enabled == Value::Bool(false) {
            return Err(PostHogApiError::FeatureFlagNotEnabled(key.clone()));
        }

        let payload = res
            .feature_flag_payloads
            .get(&key)
            .cloned()
            .ok_or(PostHogApiError::FeatureFlagNotFound(key.clone()))?;

        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{collections::HashMap, iter::FromIterator};

    fn client() -> PostHogSDKClient {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .compact()
            .try_init()
            .ok();
        dotenvy::dotenv().ok();

        let endpoint = std::env::var("POSTHOG_BASE_URL").unwrap();
        let public_key = std::env::var("POSTHOG_PUBLIC_KEY").unwrap();

        let client = PostHogSDKClient::new(public_key, endpoint).unwrap();

        client
    }

    #[tokio::test]
    async fn test_decide_basic_request() {
        let client = client();

        let request = DecideRequestBuilder::new("test-user-123".to_string()).build();

        let response = client.decide(request).await.unwrap();

        assert_eq!(response.feature_flags.len(), 3);
        assert!(response
            .feature_flags
            .contains_key(&"config-example".to_string()));
        assert!(response
            .feature_flags
            .contains_key(&"awesome-multivariant".to_string()));
        assert!(response
            .feature_flags
            .contains_key(&"my-feature".to_string()));
        assert!(!response.errors_while_computing_flags);
        assert_eq!(response.feature_flag_payloads.len(), 2);
    }

    #[tokio::test]
    async fn test_decide_with_groups() {
        let client = client();

        let mut groups = HashMap::new();
        groups.insert("company".to_string(), "acme-corp".to_string().into());

        let request = DecideRequestBuilder::new("test-user-123".to_string())
            .with_groups(groups)
            .build();

        let response = client.decide(request).await.unwrap();
        assert!(!response.errors_while_computing_flags);
    }

    #[tokio::test]
    async fn test_decide_with_properties() {
        let client = client();

        let mut person_props = HashMap::new();
        person_props.insert("email".to_string(), json!("test@example.com"));
        person_props.insert("plan".to_string(), json!("premium"));

        let mut group_props = HashMap::new();
        let mut company_props = HashMap::new();
        company_props.insert("name".to_string(), json!("Acme Corp"));
        group_props.insert("company".to_string(), company_props);

        let request = DecideRequestBuilder::new("test-user-123".to_string())
            .with_groups(HashMap::from_iter([(
                "company".to_string(),
                "acme-corp-id".to_string().into(),
            )]))
            .with_person_properties(person_props)
            .with_group_properties(group_props)
            .build();

        let response = client.decide(request).await.unwrap();
        assert!(!response.errors_while_computing_flags);
    }

    #[tokio::test]
    async fn test_decide_error_response() {
        let client = client();
        let result = client.decide(json!({"bad": "request"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_feature_flag_enabled() {
        let client = client();
        let request = DecideRequestBuilder::new("test-user-123".to_string()).build();

        // Test enabled flag
        let enabled = client
            .get_feature_flag_enabled(request.clone(), "my-feature".to_string())
            .await
            .unwrap();
        assert!(enabled);

        // Test non-existent flag
        let result = client
            .get_feature_flag_enabled(request.clone(), "non-existent".to_string())
            .await;
        assert!(matches!(result, Err(PostHogApiError::FeatureFlagNotFound(_))));
    }

    #[tokio::test]
    async fn test_get_feature_flag_multi_variant() {
        let client = client();
        let request = DecideRequestBuilder::new("test-user-123".to_string()).build();

        // Test multi-variant flag
        let (key, variant) = client
            .get_feature_flag_multi_variant(request.clone(), "awesome-multivariant".to_string())
            .await
            .unwrap();
        assert_eq!(key, "awesome-multivariant");
        assert_eq!(variant, Value::String("var2".to_string()));

        // Test non-existent flag
        let result = client
            .get_feature_flag_multi_variant(request.clone(), "non-existent".to_string())
            .await;
        assert!(matches!(result, Err(PostHogApiError::FeatureFlagNotFound(_))));
    }

    #[tokio::test]
    async fn test_get_feature_flag_payload() {
        let client = client();
        let request = DecideRequestBuilder::new("test-user-123".to_string()).build();

        // Test config flag payload
        let payload = client
            .get_feature_flag_payload(request.clone(), "config-example".to_string())
            .await
            .unwrap();
        let payload_json: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload_json["version"], "1");
        assert_eq!(payload_json["example"], "hello");

        // Test multi-variant flag payload
        let payload = client
            .get_feature_flag_payload(request.clone(), "awesome-multivariant".to_string())
            .await
            .unwrap();
        let payload_json: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload_json["hello"], "world");

        // Test non-existent flag
        let result = client
            .get_feature_flag_payload(request.clone(), "non-existent".to_string())
            .await;
        assert!(matches!(result, Err(PostHogApiError::FeatureFlagNotFound(_))));

        // Test disabled flag
        let request = DecideRequestBuilder::new("test-user-with-disabled-flag".to_string()).build();
        let result = client
            .get_feature_flag_payload(request, "disabled-flag".to_string())
            .await;
        assert!(matches!(result, Err(PostHogApiError::FeatureFlagNotFound(_))));
    }
}
