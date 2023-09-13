use reqwest::blocking::Client as HttpClient;
use reqwest::blocking::Response;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use std::time::Duration;

use crate::config::{API_ENDPOINT, DEFAULT_TIMEOUT};
use crate::errors::Error;
use crate::types::APIResult;

/// Client option and configurations builder
pub struct ClientOptionsBuilder {
    api_endpoint: String,
    api_key: Option<String>,
    timeout: Duration,
}

impl ClientOptionsBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// API endpoint
    pub fn set_endpoint(mut self, api_endpoint: String) -> Self {
        self.api_endpoint = api_endpoint;
        self
    }

    /// API key - Note that this does not differentiate between public and private API and requests
    /// with a public API key towards the Private API will fail
    pub fn set_api_key(mut self, api_key: String) -> Self {
        self.api_key = Some(api_key);
        self
    }

    /// Timeout of all requests
    pub fn set_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn build(self) -> Result<ClientOptions, Error> {
        if let Some(api_key) = self.api_key {
            Ok(ClientOptions {
                api_endpoint: self.api_endpoint,
                api_key,
                timeout: self.timeout,
            })
        } else {
            Err(Error::ClientOptionConfigError("Missing API key!".into()))
        }
    }
}

impl Default for ClientOptionsBuilder {
    fn default() -> Self {
        Self {
            api_endpoint: API_ENDPOINT.to_string(),
            api_key: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

pub struct ClientOptions {
    pub api_endpoint: String,
    api_key: String,
    pub timeout: Duration,
}

/// Default client towards API
pub struct Client {
    options: ClientOptions,
    client: HttpClient,
}

impl Client {
    /// Create new API Client
    pub fn new(options: ClientOptions) -> Self {
        let client = HttpClient::builder()
            .timeout(Some(options.timeout))
            .build()
            .expect("Failed to create underlying HTTPClient"); // Unwrap here is as safe as `HttpClient::new`

        Self { options, client }
    }

    /// Combine url endpoint with API base url
    fn full_url(&self, endpoint: String) -> String {
        format!("{}{endpoint}", self.options.api_endpoint.clone())
    }

    /// Run get request towards API
    pub(crate) fn get_request(&self, endpoint: String) -> APIResult<Response> {
        self.client
            .get(self.full_url(endpoint))
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {}", self.options.api_key))
            .send()
            .map_err(|e| Error::Connection(e.to_string()))
    }

    /// Run post request towards API
    pub(crate) fn post_request_with_body<B>(&self, endpoint: String, body: B) -> APIResult<Response>
    where
        B: Sized + serde::Serialize,
    {
        self.client
            .get(self.full_url(endpoint))
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {}", self.options.api_key))
            .body(serde_json::to_string(&body).expect("unwrap here is safe"))
            .send()
            .map_err(|e| Error::Connection(e.to_string()))
    }
}
