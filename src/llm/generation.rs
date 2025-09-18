use std::{collections::HashMap, time::Instant};

use serde::Serialize;

use crate::{Error, Event};
use super::privacy::{PrivacyMode, apply_privacy_to_value};

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
const K_MESSAGES: &str = "$ai_messages";
const K_SYSTEM_PROMPT: &str = "$ai_system_prompt";
const K_RESPONSE_FORMAT: &str = "$ai_response_format";
const K_TOOL_DEFS: &str = "$ai_tools";
const K_TOOL_CALLS: &str = "$ai_tool_calls";
const K_FINISH_REASON: &str = "$ai_finish_reason";
const K_TOP_P: &str = "$ai_top_p";
const K_TOP_K: &str = "$ai_top_k";
const K_FREQUENCY_PENALTY: &str = "$ai_frequency_penalty";
const K_PRESENCE_PENALTY: &str = "$ai_presence_penalty";
const K_SEED: &str = "$ai_seed";
const K_CACHE_HIT: &str = "$ai_cache_hit";
const K_CACHE_KEY: &str = "$ai_cache_key";
const K_RETRIEVAL_SOURCES: &str = "$ai_retrieval_sources";
const K_RETRIEVAL_BYTES: &str = "$ai_retrieval_bytes";
const K_RETRIEVAL_LATENCY_MS: &str = "$ai_retrieval_latency_ms";
const K_RETRIEVAL_RESULTS_COUNT: &str = "$ai_retrieval_results_count";
const K_INPUT_CHARACTERS: &str = "$ai_input_characters";
const K_OUTPUT_CHARACTERS: &str = "$ai_output_characters";
const K_STREAMING: &str = "$ai_streaming";
const K_COST_INPUT_USD: &str = "$ai_cost_input_usd";
const K_COST_OUTPUT_USD: &str = "$ai_cost_output_usd";
const K_SAFETY_RATINGS: &str = "$ai_safety_ratings";
const K_GUARDRAIL_TRIGGERED: &str = "$ai_guardrail_triggered";
const K_GUARDRAIL_NAME: &str = "$ai_guardrail_name";

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
    input_privacy: PrivacyMode,
    output_privacy: PrivacyMode,
    messages: Option<serde_json::Value>,
    system_prompt: Option<serde_json::Value>,
    response_format: Option<serde_json::Value>,
    tool_defs: Option<serde_json::Value>,
    tool_calls: Option<serde_json::Value>,
    finish_reason: Option<String>,
    top_p: Option<f64>,
    top_k: Option<u64>,
    frequency_penalty: Option<f64>,
    presence_penalty: Option<f64>,
    seed: Option<i64>,
    cache_hit: Option<bool>,
    cache_key: Option<String>,
    retrieval_sources: Option<serde_json::Value>,
    retrieval_bytes: Option<u64>,
    retrieval_latency_ms: Option<u64>,
    retrieval_results_count: Option<u64>,
    input_characters: Option<u64>,
    output_characters: Option<u64>,
    streaming: Option<bool>,
    cost_input_usd: Option<f64>,
    cost_output_usd: Option<f64>,
    safety_ratings: Option<serde_json::Value>,
    guardrail_triggered: Option<bool>,
    guardrail_name: Option<String>,
    system_privacy: PrivacyMode,
    messages_privacy: PrivacyMode,
}

impl GenerationBuilder {
    pub fn new() -> Self { Self { input_privacy: PrivacyMode::Full, output_privacy: PrivacyMode::Full, system_privacy: PrivacyMode::Full, messages_privacy: PrivacyMode::Full, ..Default::default() } }

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
    pub fn input_privacy(mut self, mode: PrivacyMode) -> Self { self.input_privacy = mode; self }
    pub fn output_privacy(mut self, mode: PrivacyMode) -> Self { self.output_privacy = mode; self }
    pub fn system_privacy(mut self, mode: PrivacyMode) -> Self { self.system_privacy = mode; self }
    pub fn messages_privacy(mut self, mode: PrivacyMode) -> Self { self.messages_privacy = mode; self }

    pub fn messages<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.messages = Some(to_json(v)?); Ok(self) }
    pub fn system_prompt<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.system_prompt = Some(to_json(v)?); Ok(self) }
    pub fn response_format<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.response_format = Some(to_json(v)?); Ok(self) }
    pub fn tools<T: Serialize>(mut self, defs: T) -> Result<Self, Error> { self.tool_defs = Some(to_json(defs)?); Ok(self) }
    pub fn tool_calls<T: Serialize>(mut self, calls: T) -> Result<Self, Error> { self.tool_calls = Some(to_json(calls)?); Ok(self) }
    pub fn finish_reason<S: Into<String>>(mut self, r: S) -> Self { self.finish_reason = Some(r.into()); self }
    pub fn top_p(mut self, v: f64) -> Self { self.top_p = Some(v); self }
    pub fn top_k(mut self, v: u64) -> Self { self.top_k = Some(v); self }
    pub fn frequency_penalty(mut self, v: f64) -> Self { self.frequency_penalty = Some(v); self }
    pub fn presence_penalty(mut self, v: f64) -> Self { self.presence_penalty = Some(v); self }
    pub fn seed(mut self, v: i64) -> Self { self.seed = Some(v); self }
    pub fn cache_hit(mut self, v: bool) -> Self { self.cache_hit = Some(v); self }
    pub fn cache_key<S: Into<String>>(mut self, v: S) -> Self { self.cache_key = Some(v.into()); self }
    pub fn retrieval_sources<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.retrieval_sources = Some(to_json(v)?); Ok(self) }
    pub fn retrieval_bytes(mut self, v: u64) -> Self { self.retrieval_bytes = Some(v); self }
    pub fn retrieval_latency_ms(mut self, v: u64) -> Self { self.retrieval_latency_ms = Some(v); self }
    pub fn retrieval_results_count(mut self, v: u64) -> Self { self.retrieval_results_count = Some(v); self }
    pub fn input_characters(mut self, v: u64) -> Self { self.input_characters = Some(v); self }
    pub fn output_characters(mut self, v: u64) -> Self { self.output_characters = Some(v); self }
    pub fn streaming(mut self, v: bool) -> Self { self.streaming = Some(v); self }
    pub fn cost_input_usd(mut self, v: f64) -> Self { self.cost_input_usd = Some(v); self }
    pub fn cost_output_usd(mut self, v: f64) -> Self { self.cost_output_usd = Some(v); self }
    pub fn safety_ratings<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.safety_ratings = Some(to_json(v)?); Ok(self) }
    pub fn guardrail_triggered(mut self, v: bool) -> Self { self.guardrail_triggered = Some(v); self }
    pub fn guardrail_name<S: Into<String>>(mut self, v: S) -> Self { self.guardrail_name = Some(v.into()); self }

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
        let input = apply_privacy_to_value(self.input, self.input_privacy);
        if let Some(input) = input { props.insert(K_INPUT.into(), input); }
        let output = apply_privacy_to_value(self.output, self.output_privacy);
        if let Some(output) = output { props.insert(K_OUTPUT.into(), output); }
        let messages = apply_privacy_to_value(self.messages, self.messages_privacy);
        if let Some(messages) = messages { props.insert(K_MESSAGES.into(), messages); }
        let system_prompt = apply_privacy_to_value(self.system_prompt, self.system_privacy);
        if let Some(system) = system_prompt { props.insert(K_SYSTEM_PROMPT.into(), system); }
        if let Some(fmt) = self.response_format { props.insert(K_RESPONSE_FORMAT.into(), fmt); }
        if let Some(tools) = self.tool_defs { props.insert(K_TOOL_DEFS.into(), tools); }
        if let Some(calls) = self.tool_calls { props.insert(K_TOOL_CALLS.into(), calls); }
        if let Some(r) = self.finish_reason { props.insert(K_FINISH_REASON.into(), r.into()); }
        if let Some(v) = self.top_p { props.insert(K_TOP_P.into(), serde_json::json!(v)); }
        if let Some(v) = self.top_k { props.insert(K_TOP_K.into(), serde_json::json!(v)); }
        if let Some(v) = self.frequency_penalty { props.insert(K_FREQUENCY_PENALTY.into(), serde_json::json!(v)); }
        if let Some(v) = self.presence_penalty { props.insert(K_PRESENCE_PENALTY.into(), serde_json::json!(v)); }
        if let Some(v) = self.seed { props.insert(K_SEED.into(), serde_json::json!(v)); }
        if let Some(v) = self.cache_hit { props.insert(K_CACHE_HIT.into(), serde_json::json!(v)); }
        if let Some(v) = self.cache_key { props.insert(K_CACHE_KEY.into(), v.into()); }
        if let Some(v) = self.retrieval_sources { props.insert(K_RETRIEVAL_SOURCES.into(), v); }
        if let Some(v) = self.retrieval_bytes { props.insert(K_RETRIEVAL_BYTES.into(), serde_json::json!(v)); }
        if let Some(v) = self.retrieval_latency_ms { props.insert(K_RETRIEVAL_LATENCY_MS.into(), serde_json::json!(v)); }
        if let Some(v) = self.retrieval_results_count { props.insert(K_RETRIEVAL_RESULTS_COUNT.into(), serde_json::json!(v)); }
        if let Some(v) = self.input_characters { props.insert(K_INPUT_CHARACTERS.into(), serde_json::json!(v)); }
        if let Some(v) = self.output_characters { props.insert(K_OUTPUT_CHARACTERS.into(), serde_json::json!(v)); }
        if let Some(v) = self.streaming { props.insert(K_STREAMING.into(), serde_json::json!(v)); }
        if let Some(v) = self.cost_input_usd { props.insert(K_COST_INPUT_USD.into(), serde_json::json!(v)); }
        if let Some(v) = self.cost_output_usd { props.insert(K_COST_OUTPUT_USD.into(), serde_json::json!(v)); }
        if let Some(v) = self.safety_ratings { props.insert(K_SAFETY_RATINGS.into(), v); }
        if let Some(v) = self.guardrail_triggered { props.insert(K_GUARDRAIL_TRIGGERED.into(), serde_json::json!(v)); }
        if let Some(v) = self.guardrail_name { props.insert(K_GUARDRAIL_NAME.into(), v.into()); }
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

