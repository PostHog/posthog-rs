use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::client::PostHogAPIClient;
use crate::api::error::PostHogSDKError;

#[derive(Debug, Serialize, Deserialize)]
pub struct QueryRequest {
    pub query: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters_override: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QueryResponse {
    pub results: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl PostHogAPIClient {
    /// Execute a query against the PostHog Query API
    pub async fn query(&self, project_id: &str, request: QueryRequest) -> Result<QueryResponse, PostHogSDKError> {
        let endpoint = format!("/api/projects/{}/query", project_id);
        self.api_request(Method::POST, &endpoint, Some(&request)).await
    }

    /// Get the status or result of a previously executed query
    pub async fn get_query_status(&self, project_id: &str, query_id: &str) -> Result<QueryResponse, PostHogSDKError> {
        let endpoint = format!("/api/projects/{}/query/{}", project_id, query_id);
        self.api_request(Method::GET, &endpoint, None::<&()>).await
    }

    /// Cancel an ongoing query
    pub async fn cancel_query(&self, project_id: &str, query_id: &str) -> Result<(), PostHogSDKError> {
        let endpoint = format!("/api/projects/{}/query/{}", project_id, query_id);
        self.api_request_no_response_content(Method::DELETE, &endpoint, None::<&()>).await
    }

    /// Check authorization for executing asynchronous queries
    pub async fn check_async_query_auth(&self, project_id: &str) -> Result<(), PostHogSDKError> {
        let endpoint = format!("/api/projects/{}/query/check_auth_for_async", project_id);
        self.api_request_no_response_content(Method::POST, &endpoint, None::<&()>).await
    }
}
