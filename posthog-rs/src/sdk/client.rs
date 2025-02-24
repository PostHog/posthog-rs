use crate::error::PostHogServerError;
use std::iter::FromIterator;

use super::PostHogApiError;

use anyhow::Context;
use reqwest::{
    header::{self, HeaderMap},
    Client, Method, StatusCode,
};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tracing::debug;

/// PostHog API client for interacting with the PostHog analytics service.
///
/// This client provides methods to send analytics events and interact with various PostHog features.
/// It handles authentication, request formatting, and error handling for the PostHog API.
///
/// # Fields
/// * `client` - The underlying HTTP client used for making requests
/// * `public_key` - The PostHog public key used for client-side features
/// * `base_url` - The base URL of the PostHog API
pub struct PostHogSDKClient {
    pub client: Client,
    pub public_key: String,
    pub base_url: String,
}

impl PostHogSDKClient {
    /// Creates a new PostHog client with default configuration.
    ///
    /// # Arguments
    /// * `public_key` - The PostHog public key for client-side features
    /// * `base_url` - The base URL of the PostHog API
    ///
    /// # Returns
    /// Returns a Result containing the configured PostHogClient or an error if initialization fails
    ///
    /// # Errors
    /// * If the API key is invalid for header creation
    /// * If the HTTP client creation fails
    pub fn new(public_key: String, base_url: String) -> anyhow::Result<Self> {
        let headers = HeaderMap::from_iter([(
            reqwest::header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        )]);

        let client = Client::builder()
            .default_headers(headers)
            .build()
            .context("Failed to create reqwest client")?;

        Ok(Self {
            client,
            public_key,
            base_url,
        })
    }

    /// Creates a new PostHog client with a custom HTTP client.
    ///
    /// This method allows you to provide your own configured reqwest Client instance,
    /// which can be useful for custom configurations like proxies or custom TLS settings.
    ///
    /// # Arguments
    /// * `client` - A pre-configured reqwest Client instance
    /// * `public_key` - The PostHog public key for client-side features
    /// * `base_url` - The base URL of the PostHog API
    ///
    /// # Returns
    /// Returns a configured PostHogClient instance
    pub fn with_client(client: Client, public_key: String, base_url: String) -> Self {
        Self {
            client,
            base_url,
            public_key,
        }
    }

    /// Makes an HTTP request to the PostHog API.
    ///
    /// This internal method handles the common logic for all API requests, including:
    /// - URL construction
    /// - Request method and body setup
    /// - Error handling and response parsing
    /// - Public key injection for client-side endpoints
    ///
    /// # Arguments
    /// * `method` - The HTTP method to use (e.g., "GET", "POST")
    /// * `path` - The API endpoint path
    /// * `body` - Optional JSON body to send with the request
    /// * `requires_public_key` - Whether to inject the public key into the request body
    ///
    /// # Returns
    /// Returns a Result containing a tuple of (StatusCode, JSON Value) or a PostHogError
    ///
    /// # Errors
    /// * `RequestError` - If the HTTP request fails
    /// * `JsonError` - If response parsing fails
    /// * `ResponseError` - If the API returns an error status code
    pub(crate) async fn api_request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        requires_public_key: bool,
    ) -> Result<(StatusCode, serde_json::Value), PostHogApiError> {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        debug!("Sending {} request to {}", method, url);

        let mut request = self.client.request(method, &url);

        if let Some(mut body) = body {
            if requires_public_key {
                body["api_key"] = self.public_key.clone().into();
            }

            // Encode body to JSON then compress it
            let body = serde_json::to_vec(&body)?;

            let mut writer = async_compression::tokio::write::GzipEncoder::new(Vec::<u8>::new());
            writer.write_all(body.as_slice()).await?;
            writer.shutdown().await?;

            let body = writer.into_inner();

            request = request
                .header("Content-Encoding", "gzip")
                .header("Content-Type", "application/json")
                .body(body);
        }

        let response = request.send().await?;
        let status = response.status();

        if !status.is_success() {
            let res = response.bytes().await?;
            let res = res.to_vec();
            let res = serde_json::from_slice(&res).unwrap_or(PostHogServerError {
                r#type: "unknown".to_string(),
                code: "unknown".to_string(),
                detail: String::from_utf8_lossy(&res).to_string(),
                attr: serde_json::Value::Null,
            });
            debug!("Response {}:\n{:?}", status, res);
            return Err(PostHogApiError::ResponseError(status, res));
        }

        let response = response.bytes().await?;

        let res = serde_json::from_slice(&response.to_vec())?;

        Ok((status, res))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_compression() -> anyhow::Result<()> {
        let input = "helloooooooooooooooooooo o o o o o ooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooo world!".to_string();

        let mut writer = async_compression::tokio::write::GzipEncoder::new(Vec::<u8>::new());
        writer.write_all(input.as_bytes()).await?;
        writer.flush().await?;

        let output = writer.into_inner();

        let og_size = input.as_bytes().len();
        let compressed_size = output.len();

        println!("Original Size: {}", og_size);
        println!("Compressed Size: {}", compressed_size);
        assert!(compressed_size < og_size);
        Ok(())
    }
}
