use std::collections::HashMap;

use serde::Serialize;

use crate::{Error, Event};

// Core event name for LLM generations used by PostHog SDKs
const EVENT_NAME: &str = "$ai_generation";

/// Builder to construct a canonical PostHog AI Generation event
#[derive(Default, Debug, Clone)]
pub struct GenerationBuilder {
    pub distinct_id: Option<String>,
    pub model: Option<String>,
    pub input: Option<serde_json::Value>,
    pub output: Option<serde_json::Value>,
    pub prompt_id: Option<String>,
    pub provider: Option<String>,
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub latency_ms: Option<u64>,
    pub cost_usd: Option<f64>,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

impl GenerationBuilder {
    pub fn new() -> Self { Self::default() }

    pub fn distinct_id<S: Into<String>>(mut self, id: S) -> Self { self.distinct_id = Some(id.into()); self }
    pub fn model<S: Into<String>>(mut self, m: S) -> Self { self.model = Some(m.into()); self }
    pub fn input<T: Serialize>(mut self, v: T) -> Result<Self, Error> {
        self.input = Some(to_json(v)?); Ok(self)
    }
    pub fn output<T: Serialize>(mut self, v: T) -> Result<Self, Error> {
        self.output = Some(to_json(v)?); Ok(self)
    }
    pub fn prompt_id<S: Into<String>>(mut self, id: S) -> Self { self.prompt_id = Some(id.into()); self }
    pub fn provider<S: Into<String>>(mut self, p: S) -> Self { self.provider = Some(p.into()); self }
    pub fn temperature(mut self, t: f64) -> Self { self.temperature = Some(t); self }
    pub fn max_output_tokens(mut self, n: u64) -> Self { self.max_output_tokens = Some(n); self }
    pub fn input_tokens(mut self, n: u64) -> Self { self.input_tokens = Some(n); self }
    pub fn output_tokens(mut self, n: u64) -> Self { self.output_tokens = Some(n); self }
    pub fn total_tokens(mut self, n: u64) -> Self { self.total_tokens = Some(n); self }
    pub fn latency_ms(mut self, n: u64) -> Self { self.latency_ms = Some(n); self }
    pub fn cost_usd(mut self, c: f64) -> Self { self.cost_usd = Some(c); self }
    pub fn request_id<S: Into<String>>(mut self, id: S) -> Self { self.request_id = Some(id.into()); self }
    pub fn trace_id<S: Into<String>>(mut self, id: S) -> Self { self.trace_id = Some(id.into()); self }
    pub fn metadata<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.metadata = Some(to_json(v)?); Ok(self) }

    /// Build an Event with standard PostHog LLM property keys
    pub fn build_event(self) -> Result<Event, Error> {
        let distinct_id = self.distinct_id.ok_or_else(|| Error::InvalidGeneration("distinct_id is required".into()))?;
        let mut props: HashMap<String, serde_json::Value> = HashMap::new();

        if let Some(model) = self.model { props.insert("$ai_model".into(), model.into()); }
        if let Some(input) = self.input { props.insert("$ai_input".into(), input); }
        if let Some(output) = self.output { props.insert("$ai_output".into(), output); }
        if let Some(prompt_id) = self.prompt_id { props.insert("$ai_prompt_id".into(), prompt_id.into()); }
        if let Some(provider) = self.provider { props.insert("$ai_provider".into(), provider.into()); }
        if let Some(temperature) = self.temperature { props.insert("$ai_temperature".into(), serde_json::json!(temperature)); }
        if let Some(max_output_tokens) = self.max_output_tokens { props.insert("$ai_max_output_tokens".into(), serde_json::json!(max_output_tokens)); }
        if let Some(input_tokens) = self.input_tokens { props.insert("$ai_input_tokens".into(), serde_json::json!(input_tokens)); }
        if let Some(output_tokens) = self.output_tokens { props.insert("$ai_output_tokens".into(), serde_json::json!(output_tokens)); }
        if let Some(total_tokens) = self.total_tokens { props.insert("$ai_total_tokens".into(), serde_json::json!(total_tokens)); }
        if let Some(latency_ms) = self.latency_ms { props.insert("$ai_latency_ms".into(), serde_json::json!(latency_ms)); }
        if let Some(cost_usd) = self.cost_usd { props.insert("$ai_total_cost_usd".into(), serde_json::json!(cost_usd)); }
        if let Some(request_id) = self.request_id { props.insert("$ai_request_id".into(), request_id.into()); }
        if let Some(trace_id) = self.trace_id { props.insert("$ai_trace_id".into(), trace_id.into()); }
        if let Some(metadata) = self.metadata { props.insert("$ai_metadata".into(), metadata); }

        Ok(Event::from_properties(EVENT_NAME, distinct_id, props))
    }
}

fn to_json<T: Serialize>(v: T) -> Result<serde_json::Value, Error> {
    serde_json::to_value(v).map_err(|e| Error::Serialization(e.to_string()))
}

