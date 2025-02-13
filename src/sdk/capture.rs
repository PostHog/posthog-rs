use super::client::PostHogSDKClient;
use super::error::PostHogSDKError;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Response from the /capture/ endpoint.
///
/// Contains the status of the capture operation. A successful capture returns status 1,
/// while a failed capture will return a different status code along with an error message
/// in the ResponseError.
#[derive(Debug, Serialize, Deserialize)]
pub struct CaptureResponse {
    pub status: i32,
}

/// Response from the /batch/ endpoint for batch event capture.
///
/// Contains the status of the batch capture operation as a string.
/// A successful batch capture typically returns status "ok".
#[derive(Debug, Serialize, Deserialize)]
pub struct CaptureBatchResponse {
    pub status: String,
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
    /// let client = posthog_rs::sdk::PostHogSDKClient::new(
    ///     "your-api-key".to_string(),
    ///     "your-public-key".to_string(),
    ///     "https://app.posthog.com".to_string(),
    /// )?;
    ///
    /// // You may also use the Event Builder to create the event data:
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
    pub async fn capture(&self, mut req: Value) -> Result<CaptureResponse, PostHogSDKError> {
        req["api_key"] = self.public_key.clone().into();
        let (status, res) = self
            .api_request(Method::POST, "/capture/", Some(req), true)
            .await?;

        if !status.is_success() {
            let res = serde_json::from_value(res).map_err(PostHogSDKError::JsonError)?;
            return Err(PostHogSDKError::ResponseError(status, res));
        }

        let res = serde_json::from_value(res).map_err(PostHogSDKError::JsonError)?;

        Ok(res)
    }

    /// Captures multiple events in PostHog in a single batch request.
    ///
    /// This method sends multiple events to PostHog's /batch/ endpoint. This is more efficient
    /// than sending events individually when you need to capture many events at once.
    ///
    /// # Arguments
    /// * `historical_migration` - If true, events will be processed as historical data
    /// * `events` - A vector of JSON Values containing the event data to capture
    ///
    /// # Returns
    /// Returns a Result containing the CaptureBatchResponse or an error
    ///
    /// # Example
    /// ```rust
    /// # async fn example() -> anyhow::Result<()> {
    /// use serde_json::json;
    /// 
    /// let client = posthog_rs::sdk::PostHogSDKClient::new(
    ///     "your-api-key".to_string(),
    ///     "your-public-key".to_string(),
    ///     "https://app.posthog.com".to_string(),
    /// )?;
    ///
    /// let events = vec![
    ///     json!({
    ///         "event": "user.signed_up",
    ///         "distinct_id": "user-123",
    ///         "properties": { "plan": "premium" }
    ///     }),
    ///     json!({
    ///         "event": "dashboard.viewed",
    ///         "distinct_id": "user-123",
    ///         "properties": { "dashboard_id": "123" }
    ///     })
    /// ];
    ///
    /// let response = client.capture_batch(false, events).await?;
    /// assert_eq!(response.status, "ok");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn capture_batch(
        &self,
        historical_migration: bool,
        events: Vec<Value>,
    ) -> Result<CaptureBatchResponse, PostHogSDKError> {
        let req = json!({
            "api_key": self.public_key.clone(),
            "historical_migration": historical_migration,
            "batch": events
        });

        let (status, res) = self
            .api_request(Method::POST, "/batch/", Some(req), true)
            .await?;

        if !status.is_success() {
            let res = serde_json::from_value(res).map_err(PostHogSDKError::JsonError)?;
            return Err(PostHogSDKError::ResponseError(status, res));
        }

        let res = serde_json::from_value(res).map_err(PostHogSDKError::JsonError)?;

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

    #[tokio::test]
    pub async fn test_capture_batch() -> anyhow::Result<()> {
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
        let res = client.capture_batch(false, vec![req.clone(), req]).await;
        assert!(res.is_ok());

        assert_eq!(res.unwrap().status, "Ok".to_string());
        Ok(())
    }
}
