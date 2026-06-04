//! Shared, runtime-agnostic helpers for the V0 capture pipeline.
//! Each client keeps only the I/O; this module owns event preparation and
//! payload construction.

#[cfg(not(feature = "async-client"))]
use reqwest::blocking::RequestBuilder;
#[cfg(feature = "async-client")]
use reqwest::RequestBuilder;

use super::{CaptureCompression, CaptureDefaults, ClientOptions};
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
    if defaults.disable_geoip {
        event.insert_prop_default("$geoip_disable", serde_json::Value::Bool(true));
    }
    if defaults.is_server {
        event.insert_prop_default("$is_server", serde_json::Value::Bool(true));
    }
    event.prepare_for_v0();
}

// ---------------------------------------------------------------------------
// Payload building
// ---------------------------------------------------------------------------

/// Build the JSON body for a single-event V0 capture request.
pub(crate) fn build_capture_payload(event: Event, api_key: String) -> Result<String, Error> {
    let inner_event = InnerEvent::new(event, api_key);
    serde_json::to_string(&inner_event).map_err(|e| Error::Serialization(e.to_string()))
}

/// Build the JSON body for a V0 batch capture request.
pub(crate) fn build_batch_payload(
    events: Vec<Event>,
    api_key: String,
    historical_migration: bool,
    defaults: &CaptureDefaults,
) -> Result<String, Error> {
    let inner_events: Vec<InnerEvent> = events
        .into_iter()
        .map(|mut event| {
            prepare_event(&mut event, defaults);
            InnerEvent::new(event, api_key.clone())
        })
        .collect();

    let batch_request = BatchRequest {
        api_key,
        historical_migration,
        batch: inner_events,
    };
    serde_json::to_string(&batch_request).map_err(|e| Error::Serialization(e.to_string()))
}

/// Encode the V0 JSON body, gzip-compressing when gzip is configured. Returns
/// the bytes and whether they are gzipped. V0 supports gzip only; other
/// algorithms (or a gzip failure) fall back to uncompressed.
pub(crate) fn encode_body(options: &ClientOptions, json: String) -> (Vec<u8>, bool) {
    match options.capture_compression {
        Some(CaptureCompression::Gzip) => match crate::compression::gzip(json.as_bytes()) {
            Some(bytes) => (bytes, true),
            None => (json.into_bytes(), false),
        },
        Some(other) => {
            tracing::warn!(
                ?other,
                "v0 capture supports gzip only; sending uncompressed"
            );
            (json.into_bytes(), false)
        }
        None => (json.into_bytes(), false),
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
