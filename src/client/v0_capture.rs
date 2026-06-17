//! Shared, runtime-agnostic helpers for the V0 capture pipeline.
//! Each client keeps only the I/O; this module owns event preparation and
//! payload construction.

// The transport worker is always blocking reqwest, even for the async client,
// so the v0 request helpers operate on the blocking RequestBuilder.
use chrono::{DateTime, Utc};
use reqwest::blocking::RequestBuilder;

use super::{
    common::{apply_before_send_hooks, apply_capture_defaults, apply_runtime_context},
    BeforeSendHook, CaptureDefaults, ClientOptions,
};
use crate::error::Error;
use crate::event::{BatchRequest, Event, InnerEvent};

// ---------------------------------------------------------------------------
// Event preparation
// ---------------------------------------------------------------------------

/// Apply client-level default properties (caller-wins) and V0 metadata stamping.
///
/// Uses `insert_prop_default` so a caller who explicitly set a property on the
/// event before calling `capture()` keeps their value.
pub(crate) fn prepare_event(event: &mut Event, defaults: &CaptureDefaults) {
    apply_capture_defaults(event, defaults);
    apply_runtime_context(event);
    event.prepare_for_v0();
}

// ---------------------------------------------------------------------------
// Payload building
// ---------------------------------------------------------------------------

/// Build the JSON body for a V0 batch capture request.
pub(crate) fn build_batch_payload(
    events: Vec<Event>,
    api_key: String,
    historical_migration: bool,
    sent_at: DateTime<Utc>,
    defaults: &CaptureDefaults,
    before_send: &[BeforeSendHook],
) -> Result<Option<String>, Error> {
    let inner_events: Vec<InnerEvent> = events
        .into_iter()
        .filter_map(|mut event| {
            prepare_event(&mut event, defaults);
            apply_before_send_hooks(before_send, event)
                .map(|event| InnerEvent::new(event, api_key.clone()))
        })
        .collect();

    if inner_events.is_empty() {
        return Ok(None);
    }

    let batch_request = BatchRequest {
        api_key,
        historical_migration,
        sent_at: sent_at.to_rfc3339(),
        batch: inner_events,
    };
    serde_json::to_string(&batch_request)
        .map(Some)
        .map_err(|e| Error::Serialization(e.to_string()))
}

/// Encode the V0 JSON body, compressing when configured. Returns the bytes and
/// the `Content-Encoding` token to advertise (`Some(token)` when compressed,
/// `None` when sent uncompressed). Routes through the shared `compress()`, so a
/// V0 build is gzip-only by construction; a non-gzip algorithm or a compression
/// failure falls back to uncompressed.
pub(crate) fn encode_body(
    options: &ClientOptions,
    json: String,
) -> (Vec<u8>, Option<&'static str>) {
    match options.capture_compression {
        Some(algo) => match crate::compression::compress(algo, json.as_bytes()) {
            Some((bytes, encoding)) => (bytes, Some(encoding)),
            None => (json.into_bytes(), None),
        },
        None => (json.into_bytes(), None),
    }
}

// ---------------------------------------------------------------------------
// Header helpers
// ---------------------------------------------------------------------------

/// Apply test-harness extra headers to a V0 request.
pub(crate) fn apply_extra_headers(
    #[allow(unused_variables)] options: &ClientOptions,
    #[allow(unused_mut)] mut request: RequestBuilder,
) -> RequestBuilder {
    #[cfg(feature = "test-harness")]
    if let Some(ref extra) = options.extra_capture_headers {
        for (k, v) in extra {
            request = request.header(k.as_str(), v.as_str());
        }
    }
    request
}
