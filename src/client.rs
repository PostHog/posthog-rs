use reqwest::{Client, Method, StatusCode};
use serde::Serialize;
use crate::error::PostHogError;

pub struct PostHogClient {
    client: Client,
    api_key: String,
    base_url: String,
}

impl PostHogClient {
    pub fn new(api_key: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url,
        }
    }

    pub fn with_client(client: Client, api_key: String, base_url: String) -> Self {
        Self {
            client,
            api_key,
            base_url,
        }
    }

    pub(crate) async fn api_request<T: Serialize>(
        &self,
        method: &str,
        path: &str,
        body: Option<T>,
    ) -> Result<(StatusCode, serde_json::Value), PostHogError> {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        
        let mut request = self.client
            .request(Method::from_bytes(method.as_bytes()).unwrap(), &url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json");

        if let Some(body) = body {
            request = request.json(&body);
        }

        let response = request.send().await.map_err(PostHogError::RequestError)?;
        let status = response.status();

        if !status.is_success() {
            let res = response.bytes().await.map_err(PostHogError::RequestError)?;
            let res = serde_json::from_slice(&res.to_vec()).map_err(PostHogError::JsonError)?;
            return Err(PostHogError::ApiError(status, res));
        }

        let response = response.bytes().await.map_err(PostHogError::RequestError)?;

        let res = serde_json::from_slice(&response.to_vec()).map_err(PostHogError::JsonError)?;
        
        Ok((status, res))
    }
}
