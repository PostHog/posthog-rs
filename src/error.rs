use std::fmt::Display;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Serialize, Deserialize)]
pub struct PostHogApiError {
    pub r#type: String,
    pub code: String,
    pub detail: String,
    pub attr: serde_json::Value,
}

impl Display for PostHogApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "type: {}, code: {}, detail: {}, attr: {:?}", self.r#type, self.code, self.detail, self.attr)
    }
}

#[derive(Error, Debug)]
pub enum PostHogError {
    #[error("Request failed: {0}")]
    RequestError(#[from] reqwest::Error),
    #[error("JSON serialization failed: {0}")]
    JsonError(#[from] serde_json::Error),
    #[error("API returned error status: {0}: {1}")]
    ResponseError(StatusCode, PostHogApiError),
}