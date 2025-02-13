use super::error::PostHogSDKError;
use super::client::PostHogSDKClient;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Response from the /capture/ endpoint.
/// 
/// Contains the status of the capture operation and any associated message.
/// A successful capture typically returns status 1.
#[derive(Debug, Serialize, Deserialize)]
pub struct CaptureResponse {
    pub status: i32,
}

impl PostHogSDKClient {
    /// Captures an event in PostHog.
    /// 
    /// This method sends event data to PostHog's /capture/ endpoint. The event data can include
    /// user identification, properties, and other metadata.
    /// 
    /// # Arguments
    /// * `req` - A JSON Value containing the event data to capture
    /// 
    /// # Returns
    /// Returns a Result containing the CaptureResponse or an error
    /// 
    /// # Example
    /// ```rust
    /// # async fn example() -> anyhow::Result<()> {
    /// use serde_json::json;
    /// 
    /// let event = json!({
    ///     "event": "user.signed_up",
    ///     "distinct_id": "user-123",
    ///     "properties": {
    ///         "plan": "premium",
    ///         "source": "website"
    ///     }
    /// });
    /// 
    /// let response = client.capture(event).await?;
    /// assert_eq!(response.status, 1);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn capture(&self, req: Value) -> Result<CaptureResponse, PostHogSDKError> {
        let res = self.api_request(Method::POST, "/capture/", Some(req), true).await?;

        let res = serde_json::from_value(res.1).map_err(PostHogSDKError::JsonError)?;

        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::sdk::models::event::EventBuilder;

    use super::*;

    #[tokio::test]
    async fn test_capture() -> anyhow::Result<()> {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .compact()
            .try_init();
        dotenvy::dotenv()?;
        let api_key = std::env::var("POSTHOG_API_KEY").unwrap();
        let public_key = std::env::var("POSTHOG_PUBLIC_KEY").unwrap();
        let base_url = std::env::var("POSTHOG_BASE_URL").unwrap();

        let client = PostHogSDKClient::new(api_key, public_key, base_url)?;

        let req = EventBuilder::new("test")
            .distinct_id("user123".to_string())
            .properties(json!({"key": "value"}))
            .timestamp_now()
            .build();
        let res = client.capture(req).await.unwrap();

        assert_eq!(res.status, 1);
        Ok(())
    }
}
