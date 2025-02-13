use reqwest::{Method, StatusCode};
use serde::{de::DeserializeOwned, Serialize};
use std::time::Duration;

use crate::api::error::PostHogSDKError;

const RATE_LIMIT_WAIT_TIME: Duration = Duration::from_secs(5);
const MAX_RETRIES: u32 = 3;

pub struct PostHogAPIClient {
    pub client: reqwest::Client,
    pub api_key: String,
    pub base_url: String,
}

impl PostHogAPIClient {
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://app.posthog.com".to_string()),
        }
    }

    /// Makes an API request with automatic rate limit handling and retries
    pub(crate) async fn api_request<T, R>(
        &self,
        method: Method,
        endpoint: &str,
        body: Option<&T>,
    ) -> Result<R, PostHogSDKError>
    where
        T: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url.trim_end_matches("/"), endpoint);
        let mut retries = 0;

        loop {
            let request = self.client
                .request(method.clone(), &url)
                .header("Authorization", format!("Bearer {}", self.api_key));

            let request = if let Some(body) = body {
                request.json(body)
            } else {
                request
            };

            let response = request.send().await?;
            let status = response.status();

            match status {
                // Success case
                s if s.is_success() => {
                    return Ok(response.json().await?);
                }
                // Rate limit case
                StatusCode::TOO_MANY_REQUESTS => {
                    if retries >= MAX_RETRIES {
                        let error = response.json().await?;
                        return Err(PostHogSDKError::ResponseError(status, error));
                    }
                    // Get retry-after header if available, otherwise use default wait time
                    let wait_time = response
                        .headers()
                        .get("retry-after")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(Duration::from_secs)
                        .unwrap_or(RATE_LIMIT_WAIT_TIME);

                    tokio::time::sleep(wait_time).await;
                    retries += 1;
                    continue;
                }
                // Other error cases
                _ => {
                    let error = response.json().await?;
                    return Err(PostHogSDKError::ResponseError(status, error));
                }
            }
        }
    }

    /// Makes an API request that returns no content (204 response)
    pub(crate) async fn api_request_no_response_content<T>(
        &self,
        method: Method,
        endpoint: &str,
        body: Option<&T>,
    ) -> Result<(), PostHogSDKError>
    where
        T: Serialize + ?Sized,
    {
        let url = format!("{}{}", self.base_url, endpoint);
        let mut retries = 0;

        loop {
            let request = self.client
                .request(method.clone(), &url)
                .header("Authorization", format!("Bearer {}", self.api_key));

            let request = if let Some(body) = body {
                request.json(body)
            } else {
                request
            };

            let response = request.send().await?;
            let status = response.status();

            match status {
                // Success case
                s if s.is_success() => return Ok(()),
                // Rate limit case
                StatusCode::TOO_MANY_REQUESTS => {
                    if retries >= MAX_RETRIES {
                        let error = response.json().await?;
                        return Err(PostHogSDKError::ResponseError(status, error));
                    }
                    // Get retry-after header if available, otherwise use default wait time
                    let wait_time = response
                        .headers()
                        .get("retry-after")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(Duration::from_secs)
                        .unwrap_or(RATE_LIMIT_WAIT_TIME);

                    tokio::time::sleep(wait_time).await;
                    retries += 1;
                    continue;
                }
                // Other error cases
                _ => {
                    let error = response.json().await?;
                    return Err(PostHogSDKError::ResponseError(status, error));
                }
            }
        }
    }
}