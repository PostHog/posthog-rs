use std::{collections::HashMap, time::Instant};

use serde::Serialize;

use crate::{Error, Event};

const TRACE_EVENT: &str = "$ai_trace";
const SPAN_EVENT: &str = "$ai_span";

const K_TRACE_ID: &str = "$ai_trace_id";
const K_SPAN_ID: &str = "$ai_span_id";
const K_PARENT_SPAN_ID: &str = "$ai_parent_span_id";
const K_NAME: &str = "$ai_name";
const K_LATENCY_MS: &str = "$ai_latency_ms";
const K_STATUS: &str = "$ai_status";
const K_ATTRIBUTES: &str = "$ai_attributes";

#[derive(Debug, Clone, Default)]
pub struct TraceBuilder {
    distinct_id: Option<String>,
    trace_id: Option<String>,
    name: Option<String>,
    attributes: Option<serde_json::Value>,
    latency_ms: Option<u64>,
    status: Option<String>,
}

impl TraceBuilder {
    pub fn new() -> Self { Self::default() }
    pub fn distinct_id<S: Into<String>>(mut self, id: S) -> Self { self.distinct_id = Some(id.into()); self }
    pub fn trace_id<S: Into<String>>(mut self, id: S) -> Self { self.trace_id = Some(id.into()); self }
    pub fn name<S: Into<String>>(mut self, name: S) -> Self { self.name = Some(name.into()); self }
    pub fn attributes<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.attributes = Some(to_json(v)?); Ok(self) }
    pub fn latency_ms(mut self, n: u64) -> Self { self.latency_ms = Some(n); self }
    pub fn status<S: Into<String>>(mut self, s: S) -> Self { self.status = Some(s.into()); self }

    pub fn start_timer(self) -> TraceTimer { TraceTimer { builder: self, start: Instant::now() } }

    pub fn build_event(self) -> Result<Event, Error> {
        let distinct_id = self.distinct_id.ok_or_else(|| Error::InvalidGeneration("distinct_id is required".into()))?;
        let mut props: HashMap<String, serde_json::Value> = HashMap::new();
        if let Some(trace_id) = self.trace_id { props.insert(K_TRACE_ID.into(), trace_id.into()); }
        if let Some(name) = self.name { props.insert(K_NAME.into(), name.into()); }
        if let Some(attrs) = self.attributes { props.insert(K_ATTRIBUTES.into(), attrs); }
        if let Some(latency) = self.latency_ms { props.insert(K_LATENCY_MS.into(), serde_json::json!(latency)); }
        if let Some(status) = self.status { props.insert(K_STATUS.into(), status.into()); }
        Ok(Event::from_properties(TRACE_EVENT, distinct_id, props))
    }
}

#[derive(Debug)]
pub struct TraceTimer { builder: TraceBuilder, start: Instant }

impl TraceTimer {
    pub fn finish(self) -> Result<Event, Error> {
        let elapsed = self.start.elapsed();
        self.builder.latency_ms(elapsed.as_millis() as u64).build_event()
    }
}

#[derive(Debug, Clone, Default)]
pub struct SpanBuilder {
    distinct_id: Option<String>,
    trace_id: Option<String>,
    span_id: Option<String>,
    parent_span_id: Option<String>,
    name: Option<String>,
    attributes: Option<serde_json::Value>,
    latency_ms: Option<u64>,
    status: Option<String>,
}

impl SpanBuilder {
    pub fn new() -> Self { Self::default() }
    pub fn distinct_id<S: Into<String>>(mut self, id: S) -> Self { self.distinct_id = Some(id.into()); self }
    pub fn trace_id<S: Into<String>>(mut self, id: S) -> Self { self.trace_id = Some(id.into()); self }
    pub fn span_id<S: Into<String>>(mut self, id: S) -> Self { self.span_id = Some(id.into()); self }
    pub fn parent_span_id<S: Into<String>>(mut self, id: S) -> Self { self.parent_span_id = Some(id.into()); self }
    pub fn name<S: Into<String>>(mut self, name: S) -> Self { self.name = Some(name.into()); self }
    pub fn attributes<T: Serialize>(mut self, v: T) -> Result<Self, Error> { self.attributes = Some(to_json(v)?); Ok(self) }
    pub fn latency_ms(mut self, n: u64) -> Self { self.latency_ms = Some(n); self }
    pub fn status<S: Into<String>>(mut self, s: S) -> Self { self.status = Some(s.into()); self }

    pub fn start_timer(self) -> SpanTimer { SpanTimer { builder: self, start: Instant::now() } }

    pub fn build_event(self) -> Result<Event, Error> {
        let distinct_id = self.distinct_id.ok_or_else(|| Error::InvalidGeneration("distinct_id is required".into()))?;
        let mut props: HashMap<String, serde_json::Value> = HashMap::new();
        if let Some(trace_id) = self.trace_id { props.insert(K_TRACE_ID.into(), trace_id.into()); }
        if let Some(span_id) = self.span_id { props.insert(K_SPAN_ID.into(), span_id.into()); }
        if let Some(parent_span_id) = self.parent_span_id { props.insert(K_PARENT_SPAN_ID.into(), parent_span_id.into()); }
        if let Some(name) = self.name { props.insert(K_NAME.into(), name.into()); }
        if let Some(attrs) = self.attributes { props.insert(K_ATTRIBUTES.into(), attrs); }
        if let Some(latency) = self.latency_ms { props.insert(K_LATENCY_MS.into(), serde_json::json!(latency)); }
        if let Some(status) = self.status { props.insert(K_STATUS.into(), status.into()); }
        Ok(Event::from_properties(SPAN_EVENT, distinct_id, props))
    }
}

#[derive(Debug)]
pub struct SpanTimer { builder: SpanBuilder, start: Instant }

impl SpanTimer {
    pub fn finish(self) -> Result<Event, Error> {
        let elapsed = self.start.elapsed();
        self.builder.latency_ms(elapsed.as_millis() as u64).build_event()
    }
}

fn to_json<T: Serialize>(v: T) -> Result<serde_json::Value, Error> {
    serde_json::to_value(v).map_err(|e| Error::Serialization(e.to_string()))
}

