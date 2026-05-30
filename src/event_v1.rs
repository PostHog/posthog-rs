//! Wire format types for the V1 capture protocol (`POST /i/v1/analytics/events/`).
//!
//! These are placeholder shells that will be fleshed out as the V1 pipeline is
//! implemented. The types mirror the server-side definitions in
//! `posthog/rust/capture/src/v1/analytics/types.rs`.

#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single event in the V1 wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1Event {
    pub event: String,
    pub uuid: Uuid,
    pub distinct_id: String,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<serde_json::Value>,
}

/// The batch request body for V1 capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1BatchRequest {
    pub created_at: String,
    pub batch: Vec<V1Event>,
}

/// Per-event result status returned by the V1 endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum V1EventStatus {
    Ok,
    Drop,
    Limited,
    Retry,
}

/// Per-event result entry in the V1 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1EventResult {
    pub result: V1EventStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// The V1 batch response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1BatchResponse {
    pub results: HashMap<String, V1EventResult>,
}
