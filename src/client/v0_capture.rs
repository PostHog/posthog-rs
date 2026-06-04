//! Shared, runtime-agnostic helpers for the V0 capture pipeline.
//! Each client keeps only the I/O; this module owns event preparation and
//! payload construction.

#[cfg(feature = "async-client")]
use reqwest::RequestBuilder;
#[cfg(not(feature = "async-client"))]
use reqwest::blocking::RequestBuilder;

use super::ClientOptions;
use crate::error::Error;
use crate::event::{BatchRequest, Event, InnerEvent};

// ---------------------------------------------------------------------------
// Event preparation
// ---------------------------------------------------------------------------

/// Apply client-level default properties and V0 metadata stamping.
pub(crate) fn prepare_event(event: &mut Event, options: &ClientOptions) {
    if options.disable_geoip {
        event.insert_prop("$geoip_disable", true).ok();
    }
    if options.is_server {
        event.insert_prop("$is_server", true).ok();
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
    options: &ClientOptions,
) -> Result<String, Error> {
    let inner_events: Vec<InnerEvent> = events
        .into_iter()
        .map(|mut event| {
            prepare_event(&mut event, options);
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
