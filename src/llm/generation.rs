use std::{collections::HashMap, time::Instant};

use serde::Serialize;

use crate::{Error, Event};

// Core event name for LLM generations used by PostHog SDKs
const EVENT_NAME: &str = "$ai_generation";

// Canonical property keys to avoid typos
const K_PROVIDER: &str = "$ai_provider";
const K_MODEL: &str = "$ai_model";
const K_INPUT: &str = "$ai_input";
const K_OUTPUT: &str = "$ai_output";
const K_PROMPT_ID: &str = "$ai_prompt_id";
const K_TEMPERATURE: &str = "$ai_temperature";
const K_MAX_OUTPUT_TOKENS: &str = "$ai_max_output_tokens";
const K_INPUT_TOKENS: &str = "$ai_input_tokens";
const K_OUTPUT_TOKENS: &str = "$ai_output_tokens";
const K_TOTAL_TOKENS: &str = "$ai_total_tokens";
const K_LATENCY_MS: &str = "$ai_latency_ms";
const K_COST_USD: &str = "$ai_total_cost_usd";
const K_REQUEST_ID: &str = "$ai_request_id";
const K_TRACE_ID: &str = "$ai_trace_id";
const K_METADATA: &str = "$ai_metadata";
const K_ERROR_CODE: &str = "$ai_error_code";
const K_ERROR_MESSAGE: &str = "$ai_error_message";
const K_CONVERSATION_ID: &str = "$ai_conversation_id";

/// Builder to construct a canonical PostHog AI Generation event
#[derive(Default, Debug, Clone)]
pub struct GenerationBuilder {
    distinct_id: Option<String>,
    model: Option<String>,
    input: Option<serde_json::Value>,
    output: Option<serde_json::Value>,
    prompt_id: Option<String>,
    provider: Option<String>,
    temperature: Option<f64>,
    max_output_tokens: Option<u64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    latency_ms: Option<u64>,
    cost_usd: Option<f64>,
    request_id: Option<String>,
    trace_id: Option<String>,
    conversation_id: Option<String>,
    error_code: Option<String>,
    error_message: Option<String>,
    metadata: Option<serde_json::Value>,
}

impl GenerationBuilder {
    pub fn new() -> Self { Self::default() }

    pub fn distinct_id<S: Into<String>>(mut self, id: S) -> Self { self.distinct_id = Some(id.into()); self }
    pub fn model<S: Into<String>>(mut self, m: S) -> Self { self.model = Some(m.into()); self }
    pub fn provider<S: Into<String>>(mut self, p: S) -> Self { self.provider = Some(p.into()); self }
    pub fn input<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.input = Some(to_json(v)?); Ok(self) }
    pub fn output<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.output = Some(to_json(v)?); Ok(self) }
    pub fn prompt_id<S: Into<String>>(mut self, id: S) -> Self { self.prompt_id = Some(id.into()); self }
    pub fn temperature(mut self, t: f64) -> Self { self.temperature = Some(t); self }
    pub fn max_output_tokens(mut self, n: u64) -> Self { self.max_output_tokens = Some(n); self }
    pub fn input_tokens(mut self, n: u64) -> Self { self.input_tokens = Some(n); self }
    pub fn output_tokens(mut self, n: u64) -> Self { self.output_tokens = Some(n); self }
    pub fn total_tokens(mut self, n: u64) -> Self { self.total_tokens = Some(n); self }
    pub fn latency_ms(mut self, n: u64) -> Self { self.latency_ms = Some(n); self }
    pub fn cost_usd(mut self, c: f64) -> Self { self.cost_usd = Some(c); self }
    pub fn request_id<S: Into<String>>(mut self, id: S) -> Self { self.request_id = Some(id.into()); self }
    pub fn trace_id<S: Into<String>>(mut self, id: S) -> Self { self.trace_id = Some(id.into()); self }
    pub fn conversation_id<S: Into<String>>(mut self, id: S) -> Self { self.conversation_id = Some(id.into()); self }
    pub fn error_code<S: Into<String>>(mut self, code: S) -> Self { self.error_code = Some(code.into()); self }
    pub fn error_message<S: Into<String>>(mut self, msg: S) -> Self { self.error_message = Some(msg.into()); self }
    pub fn metadata<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.metadata = Some(to_json(v)?); Ok(self) }

    /// Populate token counts from Google Gemini usage metadata
    pub fn gemini_usage(mut self, prompt_tokens: u64, candidates_tokens: u64, total_tokens: u64) -> Self {
        self.input_tokens = Some(prompt_tokens);
        self.output_tokens = Some(candidates_tokens);
        self.total_tokens = Some(total_tokens);
        self
    }

    /// Start a timer that will set latency automatically when finished
    pub fn start_timer(self) -> GenerationTimer { GenerationTimer { builder: self, start: Instant::now() } }

    /// Build an Event with standard PostHog LLM property keys
    pub fn build_event(self) -> Result<Event, Error> {
        let distinct_id = self.distinct_id.ok_or_else(|| Error::InvalidGeneration("distinct_id is required".into()))?;
        let mut props: HashMap<String, serde_json::Value> = HashMap::new();

        if let Some(model) = self.model { props.insert(K_MODEL.into(), model.into()); }
        if let Some(input) = self.input { props.insert(K_INPUT.into(), input); }
        if let Some(output) = self.output { props.insert(K_OUTPUT.into(), output); }
        if let Some(prompt_id) = self.prompt_id { props.insert(K_PROMPT_ID.into(), prompt_id.into()); }
        if let Some(provider) = self.provider { props.insert(K_PROVIDER.into(), provider.into()); }
        if let Some(temperature) = self.temperature { props.insert(K_TEMPERATURE.into(), serde_json::json!(temperature)); }
        if let Some(max_output_tokens) = self.max_output_tokens { props.insert(K_MAX_OUTPUT_TOKENS.into(), serde_json::json!(max_output_tokens)); }
        if let Some(input_tokens) = self.input_tokens { props.insert(K_INPUT_TOKENS.into(), serde_json::json!(input_tokens)); }
        if let Some(output_tokens) = self.output_tokens { props.insert(K_OUTPUT_TOKENS.into(), serde_json::json!(output_tokens)); }
        if let Some(total_tokens) = self.total_tokens { props.insert(K_TOTAL_TOKENS.into(), serde_json::json!(total_tokens)); }
        if let Some(latency_ms) = self.latency_ms { props.insert(K_LATENCY_MS.into(), serde_json::json!(latency_ms)); }
        if let Some(cost_usd) = self.cost_usd { props.insert(K_COST_USD.into(), serde_json::json!(cost_usd)); }
        if let Some(request_id) = self.request_id { props.insert(K_REQUEST_ID.into(), request_id.into()); }
        if let Some(trace_id) = self.trace_id { props.insert(K_TRACE_ID.into(), trace_id.into()); }
        if let Some(conversation_id) = self.conversation_id { props.insert(K_CONVERSATION_ID.into(), conversation_id.into()); }
        if let Some(err) = self.error_code { props.insert(K_ERROR_CODE.into(), err.into()); }
        if let Some(msg) = self.error_message { props.insert(K_ERROR_MESSAGE.into(), msg.into()); }
        if let Some(metadata) = self.metadata { props.insert(K_METADATA.into(), metadata); }

        Ok(Event::from_properties(EVENT_NAME, distinct_id, props))
    }
}

/// RAII helper to record latency automatically
#[derive(Debug)]
pub struct GenerationTimer {
    builder: GenerationBuilder,
    start: Instant,
}

impl GenerationTimer {
    /// Finish with output value and return an Event with latency populated
    pub fn finish_with_output<T: Serialize>(mut self, output: T) -> Result<Event, Error> {
        let elapsed = self.start.elapsed();
        self.builder = self.builder.latency_ms(elapsed.as_millis() as u64);
        self.builder = self.builder.output(output)?;
        self.builder.build_event()
    }

    /// Finish with error details and return an Event with latency populated
    pub fn finish_with_error<S: Into<String>>(mut self, code: S, message: S) -> Result<Event, Error> {
        let elapsed = self.start.elapsed();
        self.builder = self.builder.latency_ms(elapsed.as_millis() as u64);
        self.builder = self.builder.error_code(code);
        self.builder = self.builder.error_message(message);
        self.builder.build_event()
    }
}

fn to_json<T: Serialize>(v: T) -> Result<serde_json::Value, Error> {
    serde_json::to_value(v).map_err(|e| Error::Serialization(e.to_string()))
}

