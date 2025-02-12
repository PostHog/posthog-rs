use std::iter::FromIterator;

use crate::error::PostHogError;
use anyhow::Context;
use reqwest::{
    header::{self, HeaderMap},
    Client, Method, StatusCode,
};
use serde_json::Value;
use tracing::debug;

/// PostHog API client for interacting with the PostHog analytics service.
/// 
/// This client provides methods to send analytics events and interact with various PostHog features.
/// It handles authentication, request formatting, and error handling for the PostHog API.
/// 
/// # Fields
/// * `client` - The underlying HTTP client used for making requests
/// * `api_key` - The PostHog API key used for authentication
/// * `public_key` - The PostHog public key used for client-side features
/// * `base_url` - The base URL of the PostHog API
pub struct PostHogClient {
    pub client: Client,
    pub public_key: String,
    pub base_url: String,
}

impl PostHogClient {
    /// Creates a new PostHog client with default configuration.
    /// 
    /// # Arguments
    /// * `api_key` - The PostHog API key for authentication
    /// * `public_key` - The PostHog public key for client-side features
    /// * `base_url` - The base URL of the PostHog API
    /// 
    /// # Returns
    /// Returns a Result containing the configured PostHogClient or an error if initialization fails
    /// 
    /// # Errors
    /// * If the API key is invalid for header creation
    /// * If the HTTP client creation fails
    pub fn new(api_key: String, public_key: String, base_url: String) -> anyhow::Result<Self> {
        let headers = HeaderMap::from_iter([
            (
                reqwest::header::AUTHORIZATION,
                header::HeaderValue::from_str(format!("Bearer {api_key}").as_str()).context(
                    format!("Failed to create authorization header: Bearer {api_key}"),
                )?,
            ),
            (
                reqwest::header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            ),
        ]);

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
            public_key
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
    ) -> Result<(StatusCode, serde_json::Value), PostHogError> {

        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        debug!("Sending {} request to {}", method, url);

        let mut request = self
            .client
            .request(method, &url)
            // .header("Authorization", format!("Bearer {}", self.api_key))
            // .header("Content-Type", "application/json")
            ;

        if let Some(mut body) = body {
            if requires_public_key {
                body["api_key"] = self.public_key.clone().into();
            }

            request = request.json(&body);
        }

        let response = request.send().await.map_err(PostHogError::RequestError)?;
        let status = response.status();

        if !status.is_success() {
            let res = response.bytes().await.map_err(PostHogError::RequestError)?;
            let res = serde_json::from_slice(&res.to_vec()).map_err(PostHogError::JsonError)?;
            debug!("Response {}:\n{:?}", status, res);
            return Err(PostHogError::ResponseError(status, res));
        }

        let response = response.bytes().await.map_err(PostHogError::RequestError)?;

        let res = serde_json::from_slice(&response.to_vec()).map_err(PostHogError::JsonError)?;

        Ok((status, res))
    }
}
