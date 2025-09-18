use std::{collections::HashMap, time::Instant};

use serde::Serialize;

use crate::{Error, Event};

const EMBEDDING_EVENT: &str = "$ai_embedding";

const K_PROVIDER: &str = "$ai_provider";
const K_MODEL: &str = "$ai_model";
const K_INPUT: &str = "$ai_input";
const K_VECTOR_DIM: &str = "$ai_vector_dims";
const K_VECTOR_COUNT: &str = "$ai_vector_count";
const K_INPUT_TOKENS: &str = "$ai_input_tokens";
const K_LATENCY_MS: &str = "$ai_latency_ms";
const K_COST_USD: &str = "$ai_total_cost_usd";
const K_REQUEST_ID: &str = "$ai_request_id";
const K_TRACE_ID: &str = "$ai_trace_id";
const K_METADATA: &str = "$ai_metadata";

#[derive(Debug, Clone, Default)]
pub struct EmbeddingBuilder {
    distinct_id: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    input: Option<serde_json::Value>,
    vector_dims: Option<u64>,
    vector_count: Option<u64>,
    input_tokens: Option<u64>,
    latency_ms: Option<u64>,
    cost_usd: Option<f64>,
    request_id: Option<String>,
    trace_id: Option<String>,
    metadata: Option<serde_json::Value>,
}

impl EmbeddingBuilder {
    pub fn new() -> Self { Self::default() }
    pub fn distinct_id<S: Into<String>>(mut self, id: S) -> Self { self.distinct_id = Some(id.into()); self }
    pub fn provider<S: Into<String>>(mut self, s: S) -> Self { self.provider = Some(s.into()); self }
    pub fn model<S: Into<String>>(mut self, s: S) -> Self { self.model = Some(s.into()); self }
    pub fn input<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.input = Some(to_json(v)?); Ok(self) }
    pub fn vector_dims(mut self, n: u64) -> Self { self.vector_dims = Some(n); self }
    pub fn vector_count(mut self, n: u64) -> Self { self.vector_count = Some(n); self }
    pub fn input_tokens(mut self, n: u64) -> Self { self.input_tokens = Some(n); self }
    pub fn latency_ms(mut self, n: u64) -> Self { self.latency_ms = Some(n); self }
    pub fn cost_usd(mut self, c: f64) -> Self { self.cost_usd = Some(c); self }
    pub fn request_id<S: Into<String>>(mut self, s: S) -> Self { self.request_id = Some(s.into()); self }
    pub fn trace_id<S: Into<String>>(mut self, s: S) -> Self { self.trace_id = Some(s.into()); self }
    pub fn metadata<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.metadata = Some(to_json(v)?); Ok(self) }

    pub fn start_timer(self) -> EmbeddingTimer { EmbeddingTimer { builder: self, start: Instant::now() } }

    pub fn build_event(self) -> Result<Event, Error> {
        let distinct_id = self.distinct_id.ok_or_else(|| Error::InvalidGeneration("distinct_id is required".into()))?;
        let mut props: HashMap<String, serde_json::Value> = HashMap::new();
        if let Some(provider) = self.provider { props.insert(K_PROVIDER.into(), provider.into()); }
        if let Some(model) = self.model { props.insert(K_MODEL.into(), model.into()); }
        if let Some(input) = self.input { props.insert(K_INPUT.into(), input); }
        if let Some(n) = self.vector_dims { props.insert(K_VECTOR_DIM.into(), serde_json::json!(n)); }
        if let Some(n) = self.vector_count { props.insert(K_VECTOR_COUNT.into(), serde_json::json!(n)); }
        if let Some(n) = self.input_tokens { props.insert(K_INPUT_TOKENS.into(), serde_json::json!(n)); }
        if let Some(n) = self.latency_ms { props.insert(K_LATENCY_MS.into(), serde_json::json!(n)); }
        if let Some(c) = self.cost_usd { props.insert(K_COST_USD.into(), serde_json::json!(c)); }
        if let Some(s) = self.request_id { props.insert(K_REQUEST_ID.into(), s.into()); }
        if let Some(s) = self.trace_id { props.insert(K_TRACE_ID.into(), s.into()); }
        if let Some(m) = self.metadata { props.insert(K_METADATA.into(), m); }
        Ok(Event::from_properties(EMBEDDING_EVENT, distinct_id, props))
    }
}

#[derive(Debug)]
pub struct EmbeddingTimer { builder: EmbeddingBuilder, start: Instant }

impl EmbeddingTimer {
    pub fn finish(self) -> Result<Event, Error> {
        let elapsed = self.start.elapsed();
        self.builder.latency_ms(elapsed.as_millis() as u64).build_event()
    }
}

fn to_json<T: Serialize>(v: T) -> Result<serde_json::Value, Error> {
    serde_json::to_value(v).map_err(|e| Error::Serialization(e.to_string()))
}

