#![cfg(feature = "rig-integration")]

use crate::{Event, Error};
use crate::llm::generation::GenerationBuilder;
use crate::llm::trace::{TraceBuilder, SpanBuilder};
use crate::llm::embedding::EmbeddingBuilder;

/// Minimal traits to avoid hard dependency on rig-core types in our public API.
/// Users can convert their Rig events into these data structs and call the helpers.

#[derive(Debug, Clone)]
pub struct RigGeneration<'a> {
    pub distinct_id: &'a str,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub input: Option<serde_json::Value>,
    pub output: Option<serde_json::Value>,
    pub latency_ms: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub request_id: Option<&'a str>,
    pub trace_id: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct RigEmbedding<'a> {
    pub distinct_id: &'a str,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub input: Option<serde_json::Value>,
    pub vector_dims: Option<u64>,
    pub vector_count: Option<u64>,
    pub input_tokens: Option<u64>,
    pub latency_ms: Option<u64>,
    pub request_id: Option<&'a str>,
    pub trace_id: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct RigSpan<'a> {
    pub distinct_id: &'a str,
    pub trace_id: Option<&'a str>,
    pub span_id: Option<&'a str>,
    pub parent_span_id: Option<&'a str>,
    pub name: Option<&'a str>,
    pub attributes: Option<serde_json::Value>,
    pub latency_ms: Option<u64>,
    pub status: Option<&'a str>,
}

pub fn generation_to_event(data: RigGeneration<'_>) -> Result<Event, Error> {
    let mut b = GenerationBuilder::new().distinct_id(data.distinct_id);
    if let Some(s) = data.provider { b = b.provider(s); }
    if let Some(s) = data.model { b = b.model(s); }
    if let Some(v) = data.input { b = b.input(v)?; }
    if let Some(v) = data.output { b = b.output(v)?; }
    if let Some(n) = data.latency_ms { b = b.latency_ms(n); }
    if let Some(n) = data.input_tokens { b = b.input_tokens(n); }
    if let Some(n) = data.output_tokens { b = b.output_tokens(n); }
    if let Some(n) = data.total_tokens { b = b.total_tokens(n); }
    if let Some(s) = data.request_id { b = b.request_id(s); }
    if let Some(s) = data.trace_id { b = b.trace_id(s); }
    b.build_event()
}

pub fn embedding_to_event(data: RigEmbedding<'_>) -> Result<Event, Error> {
    let mut b = EmbeddingBuilder::new().distinct_id(data.distinct_id);
    if let Some(s) = data.provider { b = b.provider(s); }
    if let Some(s) = data.model { b = b.model(s); }
    if let Some(v) = data.input { b = b.input(v)?; }
    if let Some(n) = data.vector_dims { b = b.vector_dims(n); }
    if let Some(n) = data.vector_count { b = b.vector_count(n); }
    if let Some(n) = data.input_tokens { b = b.input_tokens(n); }
    if let Some(n) = data.latency_ms { b = b.latency_ms(n); }
    if let Some(s) = data.request_id { b = b.request_id(s); }
    if let Some(s) = data.trace_id { b = b.trace_id(s); }
    b.build_event()
}

pub fn span_to_event(data: RigSpan<'_>) -> Result<Event, Error> {
    let mut b = SpanBuilder::new().distinct_id(data.distinct_id);
    if let Some(s) = data.trace_id { b = b.trace_id(s); }
    if let Some(s) = data.span_id { b = b.span_id(s); }
    if let Some(s) = data.parent_span_id { b = b.parent_span_id(s); }
    if let Some(s) = data.name { b = b.name(s); }
    if let Some(v) = data.attributes { b = b.attributes(v)?; }
    if let Some(n) = data.latency_ms { b = b.latency_ms(n); }
    if let Some(s) = data.status { b = b.status(s); }
    b.build_event()
}

// Observers that apps can call from Rig hooks/callbacks

#[cfg(feature = "async-client")]
pub struct AsyncRigPosthogObserver {
    client: crate::client::Client,
}

#[cfg(feature = "async-client")]
impl AsyncRigPosthogObserver {
    pub fn new(client: crate::client::Client) -> Self { Self { client } }

    pub async fn on_generation(&self, gen: RigGeneration<'_>) -> Result<(), Error> {
        let event = generation_to_event(gen)?;
        self.client.capture(event).await
    }

    pub async fn on_embedding(&self, emb: RigEmbedding<'_>) -> Result<(), Error> {
        let event = embedding_to_event(emb)?;
        self.client.capture(event).await
    }

    pub async fn on_span(&self, span: RigSpan<'_>) -> Result<(), Error> {
        let event = span_to_event(span)?;
        self.client.capture(event).await
    }
}

#[cfg(not(feature = "async-client"))]
pub struct RigPosthogObserver {
    client: crate::client::Client,
}

#[cfg(not(feature = "async-client"))]
impl RigPosthogObserver {
    pub fn new(client: crate::client::Client) -> Self { Self { client } }

    pub fn on_generation(&self, gen: RigGeneration<'_>) -> Result<(), Error> {
        let event = generation_to_event(gen)?;
        self.client.capture(event)
    }

    pub fn on_embedding(&self, emb: RigEmbedding<'_>) -> Result<(), Error> {
        let event = embedding_to_event(emb)?;
        self.client.capture(event)
    }

    pub fn on_span(&self, span: RigSpan<'_>) -> Result<(), Error> {
        let event = span_to_event(span)?;
        self.client.capture(event)
    }
}

