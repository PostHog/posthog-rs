//! Shared, runtime-agnostic helpers for the V1 capture pipeline.
//! Each client keeps only the I/O; this module owns everything else.

use std::collections::HashMap;
use std::time::Duration;

pub(crate) const V1_CAPTURE_PATH: &str = "/i/v1/analytics/events";

use chrono::Utc;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use tracing::debug;
use uuid::Uuid;

use super::{CaptureCompression, CaptureDefaults, ClientOptions};
use crate::error::Error;
use crate::event::Event;
use crate::event_v1::{CaptureResponse, EventResult, EventStatus, V1ErrorResponse, V1Event};

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

pub(crate) fn build_events(events: &[Event], defaults: &CaptureDefaults) -> Vec<V1Event> {
    events
        .iter()
        .map(|event| {
            let mut v1 = V1Event::from_event(event);
            if let serde_json::Value::Object(ref mut map) = v1.properties {
                if defaults.disable_geoip {
                    map.entry("$geoip_disable")
                        .or_insert(serde_json::Value::Bool(true));
                }
                if defaults.is_server {
                    map.entry("$is_server")
                        .or_insert(serde_json::Value::Bool(true));
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use reqwest::header::{HeaderMap, HeaderValue};
    use uuid::Uuid;

    use super::*;
    use crate::client::ClientOptionsBuilder;
    use crate::event_v1::{CaptureResponse, EventResult, EventStatus, V1Event};

    fn test_opts() -> ClientOptions {
        ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .max_capture_attempts(3u32)
            .retry_initial_backoff_ms(100u64)
            .retry_max_backoff_ms(5000u64)
            .build()
            .unwrap()
    }

    fn dummy_v1_event() -> V1Event {
        V1Event {
            event: "$pageview".into(),
            uuid: Uuid::now_v7(),
            distinct_id: "user-1".into(),
            timestamp: "2026-05-28T12:00:00.000Z".into(),
            session_id: None,
            window_id: None,
            options: Default::default(),
            properties: serde_json::json!({}),
        }
    }

    fn event_result(status: EventStatus, details: Option<&str>) -> EventResult {
        EventResult {
            result: status,
            details: details.map(String::from),
        }
    }

    // -- is_retryable_status -------------------------------------------------

    #[test]
    fn retryable_statuses() {
        for code in [408, 500, 502, 503, 504] {
            assert!(
                is_retryable_status(code),
                "expected {} to be retryable",
                code
            );
        }
    }

    #[test]
    fn non_retryable_statuses() {
        for code in [200, 201, 400, 401, 402, 403, 413, 415, 418, 404] {
            assert!(
                !is_retryable_status(code),
                "expected {} to NOT be retryable",
                code
            );
        }
    }

    // -- parse_retry_after ---------------------------------------------------

    #[test]
    fn parse_retry_after_valid() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("5"));
        assert_eq!(parse_retry_after(&headers), Some(5));
    }

    #[test]
    fn parse_retry_after_missing() {
        let headers = HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn parse_retry_after_non_numeric() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("not-a-number"));
        assert_eq!(parse_retry_after(&headers), None);
    }

    // -- backoff_duration ----------------------------------------------------

    #[test]
    fn backoff_explicit_retry_after_wins() {
        let opts = test_opts();
        let d = backoff_duration(&opts, 1, Some(42));
        assert_eq!(d, Duration::from_secs(42));
    }

    #[test]
    fn backoff_exponential_growth() {
        let opts = test_opts();
        let d1 = backoff_duration(&opts, 1, None);
        let d2 = backoff_duration(&opts, 2, None);
        let d3 = backoff_duration(&opts, 3, None);
        assert_eq!(d1, Duration::from_millis(100));
        assert_eq!(d2, Duration::from_millis(200));
        assert_eq!(d3, Duration::from_millis(400));
    }

    #[test]
    fn backoff_clamped_to_max() {
        let opts = ClientOptionsBuilder::default()
            .api_key("k".to_string())
            .retry_initial_backoff_ms(100u64)
            .retry_max_backoff_ms(150u64)
            .build()
            .unwrap();
        let d = backoff_duration(&opts, 3, None);
        assert_eq!(d, Duration::from_millis(150));
    }

    // -- count_results -------------------------------------------------------

    #[test]
    fn count_results_aggregates() {
        let u1 = Uuid::now_v7();
        let u2 = Uuid::now_v7();
        let u3 = Uuid::now_v7();
        let resp = CaptureResponse {
            results: HashMap::from([
                (u1, event_result(EventStatus::Ok, None)),
                (u2, event_result(EventStatus::Ok, None)),
                (u3, event_result(EventStatus::Retry, Some("not_persisted"))),
            ]),
        };
        let counts = count_results(&resp);
        assert_eq!(counts[&("ok".to_string(), None)], 2);
        assert_eq!(
            counts[&("retry".to_string(), Some("not_persisted".to_string()))],
            1
        );
    }

    // -- process_batch_response ----------------------------------------------

    #[test]
    fn process_batch_retry_kept_when_not_final() {
        let e1 = dummy_v1_event();
        let e2 = dummy_v1_event();
        let results = HashMap::from([
            (e1.uuid, event_result(EventStatus::Ok, None)),
            (
                e2.uuid,
                event_result(EventStatus::Retry, Some("not_persisted")),
            ),
        ]);
        let mut final_results = HashMap::new();
        let next = process_batch_response(
            vec![e1.clone(), e2.clone()],
            &results,
            &mut final_results,
            false,
        );
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].uuid, e2.uuid);
        assert!(final_results.contains_key(&e1.uuid));
        assert!(!final_results.contains_key(&e2.uuid));
    }

    #[test]
    fn process_batch_retry_finalized_when_final() {
        let e1 = dummy_v1_event();
        let results = HashMap::from([(
            e1.uuid,
            event_result(EventStatus::Retry, Some("not_persisted")),
        )]);
        let mut final_results = HashMap::new();
        let next = process_batch_response(vec![e1.clone()], &results, &mut final_results, true);
        assert!(next.is_empty());
        assert!(final_results.contains_key(&e1.uuid));
    }

    #[test]
    fn process_batch_terminal_results_finalized() {
        let ok_ev = dummy_v1_event();
        let drop_ev = dummy_v1_event();
        let warn_ev = dummy_v1_event();
        let results = HashMap::from([
            (ok_ev.uuid, event_result(EventStatus::Ok, None)),
            (
                drop_ev.uuid,
                event_result(EventStatus::Drop, Some("billing")),
            ),
            (
                warn_ev.uuid,
                event_result(EventStatus::Warning, Some("pp_disabled")),
            ),
        ]);
        let mut final_results = HashMap::new();
        let next = process_batch_response(
            vec![ok_ev.clone(), drop_ev.clone(), warn_ev.clone()],
            &results,
            &mut final_results,
            false,
        );
        assert!(next.is_empty());
        assert_eq!(final_results.len(), 3);
    }

    #[test]
    fn process_batch_missing_uuid_silently_dropped() {
        let e = dummy_v1_event();
        let results = HashMap::new();
        let mut final_results = HashMap::new();
        let next = process_batch_response(vec![e.clone()], &results, &mut final_results, false);
        assert!(next.is_empty());
        assert!(final_results.is_empty());
    }

    // -- after_transport_error -----------------------------------------------

    #[test]
    fn after_transport_error_fails_at_max() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        let step = after_transport_error(&opts, &rid, 3, "timeout".into());
        assert!(matches!(step, Step::Fail(Error::Connection(_))));
    }

    #[test]
    fn after_transport_error_backs_off_below_max() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        let step = after_transport_error(&opts, &rid, 1, "timeout".into());
        assert!(matches!(step, Step::Backoff(_)));
    }

    // -- after_response ------------------------------------------------------

    #[test]
    fn after_response_200_all_ok_is_done() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        let e = dummy_v1_event();
        let body = serde_json::json!({
            "results": { e.uuid.to_string(): { "result": "ok" } }
        })
        .to_string();
        let mut pending = vec![e];
        let mut final_results = HashMap::new();
        let step = after_response(
            &opts,
            &rid,
            1,
            200,
            None,
            &body,
            &mut pending,
            &mut final_results,
        );
        assert!(matches!(step, Step::Done));
        assert!(pending.is_empty());
    }

    #[test]
    fn after_response_200_partial_retry_backs_off() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        let e1 = dummy_v1_event();
        let e2 = dummy_v1_event();
        let body = serde_json::json!({
            "results": {
                e1.uuid.to_string(): { "result": "ok" },
                e2.uuid.to_string(): { "result": "retry", "details": "not_persisted" }
            }
        })
        .to_string();
        let mut pending = vec![e1.clone(), e2.clone()];
        let mut final_results = HashMap::new();
        let step = after_response(
            &opts,
            &rid,
            1,
            200,
            None,
            &body,
            &mut pending,
            &mut final_results,
        );
        assert!(matches!(step, Step::Backoff(_)));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].uuid, e2.uuid);
        assert!(final_results.contains_key(&e1.uuid));
    }

    #[test]
    fn after_response_retryable_status_backs_off() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        let body = r#"{"error":"service_unavailable"}"#;
        let mut pending = vec![dummy_v1_event()];
        let mut final_results = HashMap::new();
        let step = after_response(
            &opts,
            &rid,
            1,
            503,
            Some(1),
            body,
            &mut pending,
            &mut final_results,
        );
        assert!(matches!(step, Step::Backoff(_)));
    }

    #[test]
    fn after_response_retryable_status_fails_at_max() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        let body = r#"{"error":"service_unavailable"}"#;
        let mut pending = vec![dummy_v1_event()];
        let mut final_results = HashMap::new();
        let step = after_response(
            &opts,
            &rid,
            3,
            503,
            None,
            body,
            &mut pending,
            &mut final_results,
        );
        assert!(matches!(step, Step::Fail(_)));
    }

    #[test]
    fn after_response_non_retryable_status_fails() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        let body = r#"{"error":"billing_limit_exceeded"}"#;
        let mut pending = vec![dummy_v1_event()];
        let mut final_results = HashMap::new();
        let step = after_response(
            &opts,
            &rid,
            1,
            402,
            None,
            body,
            &mut pending,
            &mut final_results,
        );
        assert!(matches!(step, Step::Fail(Error::BillingLimitExceeded(_))));
    }

    #[test]
    fn after_response_malformed_200_body_fails() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        let mut pending = vec![dummy_v1_event()];
        let mut final_results = HashMap::new();
        let step = after_response(
            &opts,
            &rid,
            1,
            200,
            None,
            "not json",
            &mut pending,
            &mut final_results,
        );
        assert!(matches!(step, Step::Fail(Error::Serialization(_))));
    }
}
