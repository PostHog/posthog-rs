//! Shared, runtime-agnostic helpers for the V1 capture pipeline.
//! Each client keeps only the I/O; this module owns everything else.

use std::collections::HashMap;
use std::time::Duration;

pub(crate) const V1_CAPTURE_PATH: &str = "/i/v1/analytics/events";

use chrono::Utc;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use tracing::debug;
use uuid::Uuid;

use super::{CaptureCompression, ClientOptions};
use crate::error::Error;
use crate::event::Event;
use crate::event_v1::{CaptureResponse, EventResult, EventStatus, V1ErrorResponse, V1Event};

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

pub(crate) fn build_events(events: &[Event], disable_geoip: bool) -> Vec<V1Event> {
    events
        .iter()
        .map(|event| {
            let mut v1 = V1Event::from_event(event);
            if disable_geoip {
                if let serde_json::Value::Object(ref mut map) = v1.properties {
                    map.insert("$geoip_disable".to_string(), serde_json::Value::Bool(true));
                }
            }
            v1
        })
        .collect()
}

pub(crate) fn build_headers(opts: &ClientOptions, request_id: &Uuid, attempt: u32) -> HeaderMap {
    let version = env!("CARGO_PKG_VERSION");
    let sdk_info = format!("posthog-rust/{version}");

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", opts.api_key))
            .unwrap_or_else(|_| HeaderValue::from_static("Bearer invalid")),
    );
    headers.insert(
        "user-agent",
        HeaderValue::from_str(&sdk_info)
            .unwrap_or_else(|_| HeaderValue::from_static("posthog-rust")),
    );
    headers.insert(
        "posthog-sdk-info",
        HeaderValue::from_str(&sdk_info)
            .unwrap_or_else(|_| HeaderValue::from_static("posthog-rust")),
    );
    headers.insert(
        "posthog-attempt",
        HeaderValue::from_str(&attempt.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("1")),
    );
    headers.insert(
        "posthog-request-id",
        HeaderValue::from_str(&request_id.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );
    headers.insert(
        "posthog-request-timestamp",
        HeaderValue::from_str(&Utc::now().to_rfc3339())
            .unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );
    #[cfg(feature = "test-harness")]
    if let Some(ref extra) = opts.extra_capture_headers {
        for (k, v) in extra {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                headers.insert(name, val);
            }
        }
    }
    headers
}

pub(crate) fn maybe_compress(
    compression: Option<CaptureCompression>,
    headers: &mut HeaderMap,
    payload: Vec<u8>,
) -> Vec<u8> {
    if let Some(algo) = compression {
        if let Some((compressed, encoding)) = crate::compression::compress(algo, &payload) {
            headers.insert("content-encoding", HeaderValue::from_static(encoding));
            return compressed;
        }
    }
    payload
}

// ---------------------------------------------------------------------------
// Retry helpers
// ---------------------------------------------------------------------------

pub(crate) fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 500 | 502 | 503 | 504)
}

pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
}

pub(crate) fn backoff_duration(
    opts: &ClientOptions,
    attempt: u32,
    retry_after_secs: Option<u64>,
) -> Duration {
    if let Some(secs) = retry_after_secs {
        Duration::from_secs(secs)
    } else {
        let base_ms = opts.retry_initial_backoff_ms;
        let max_ms = opts.retry_max_backoff_ms;
        let backoff_ms = base_ms.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1)));
        Duration::from_millis(backoff_ms.min(max_ms))
    }
}

// ---------------------------------------------------------------------------
// Response classification
// ---------------------------------------------------------------------------

pub(crate) fn count_results(resp: &CaptureResponse) -> HashMap<(String, Option<String>), usize> {
    let mut counts: HashMap<(String, Option<String>), usize> = HashMap::new();
    for result in resp.results.values() {
        let key = (
            format!("{:?}", result.result).to_lowercase(),
            result.details.clone(),
        );
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

/// O(n) consuming pass: records terminal results, returns only retry events.
/// Events absent from `results` are silently dropped.
pub(crate) fn process_batch_response(
    pending: Vec<V1Event>,
    results: &HashMap<Uuid, EventResult>,
    final_results: &mut HashMap<Uuid, EventResult>,
    is_final_attempt: bool,
) -> Vec<V1Event> {
    let mut next = Vec::new();
    for v1 in pending {
        match results.get(&v1.uuid) {
            Some(r) if r.result == EventStatus::Retry => {
                if is_final_attempt {
                    final_results.insert(v1.uuid, r.clone());
                } else {
                    next.push(v1);
                }
            }
            Some(r) => {
                final_results.insert(v1.uuid, r.clone());
            }
            None => {}
        }
    }
    next
}

// ---------------------------------------------------------------------------
// Sans-IO control flow
// ---------------------------------------------------------------------------

pub(crate) enum Step {
    Done,
    Backoff(Duration),
    Fail(Error),
}

pub(crate) fn after_transport_error(
    opts: &ClientOptions,
    request_id: &Uuid,
    attempt: u32,
    err_msg: String,
) -> Step {
    if attempt >= opts.max_capture_attempts {
        return Step::Fail(Error::Connection(err_msg));
    }
    debug!(
        request_id = %request_id,
        attempt,
        error = %err_msg,
        "V1 capture request failed, will retry"
    );
    Step::Backoff(backoff_duration(opts, attempt + 1, None))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn after_response(
    opts: &ClientOptions,
    request_id: &Uuid,
    attempt: u32,
    status: u16,
    retry_after: Option<u64>,
    body: &str,
    pending: &mut Vec<V1Event>,
    final_results: &mut HashMap<Uuid, EventResult>,
) -> Step {
    if status == 200 {
        let batch_resp: CaptureResponse = match serde_json::from_str(body) {
            Ok(r) => r,
            Err(e) => return Step::Fail(Error::Serialization(e.to_string())),
        };

        if tracing::enabled!(tracing::Level::DEBUG) {
            let result_counts = count_results(&batch_resp);
            debug!(
                request_id = %request_id,
                attempt,
                results = ?result_counts,
                "V1 capture batch response"
            );
        }

        let is_final = attempt >= opts.max_capture_attempts;
        let next = process_batch_response(
            std::mem::take(pending),
            &batch_resp.results,
            final_results,
            is_final,
        );
        *pending = next;

        if pending.is_empty() || is_final {
            Step::Done
        } else {
            Step::Backoff(backoff_duration(opts, attempt + 1, retry_after))
        }
    } else if is_retryable_status(status) {
        let error_desc = serde_json::from_str::<V1ErrorResponse>(body)
            .ok()
            .and_then(|e| e.error_description)
            .unwrap_or_else(|| body.to_string());

        debug!(
            request_id = %request_id,
            attempt,
            status,
            error = %error_desc,
            "V1 capture request failed, will retry"
        );

        if attempt >= opts.max_capture_attempts {
            Step::Fail(
                Error::from_http_response(status, body.to_string())
                    .unwrap_or_else(|| Error::Connection(format!("HTTP {status}"))),
            )
        } else {
            Step::Backoff(backoff_duration(opts, attempt + 1, retry_after))
        }
    } else {
        Step::Fail(
            Error::from_http_response(status, body.to_string())
                .unwrap_or_else(|| Error::Connection(format!("HTTP {status}"))),
        )
    }
}
