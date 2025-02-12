use crate::error::PostHogError;
use crate::PostHogClient;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[async_trait]
pub trait CaptureEndpoints {
    async fn capture(&self, req: Value) -> Result<CaptureResponse, PostHogError>;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CaptureResponse {
    pub status: i32,
}

#[async_trait]
impl CaptureEndpoints for PostHogClient {
    async fn capture(&self, req: Value) -> Result<CaptureResponse, PostHogError> {
        let res = self.api_request("POST", "/capture/", Some(req), true).await?;

        let res = serde_json::from_value(res.1).map_err(PostHogError::JsonError)?;

        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::models::event::EventBuilder;

    use super::*;

    #[tokio::test]
    async fn test_capture() -> anyhow::Result<()> {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .compact()
            .init();
        dotenvy::dotenv()?;
        let api_key = std::env::var("POSTHOG_API_KEY").unwrap();
        let public_key = std::env::var("POSTHOG_PUBLIC_KEY").unwrap();
        let base_url = std::env::var("POSTHOG_BASE_URL").unwrap();

        let client = PostHogClient::new(api_key, public_key, base_url)?;

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
