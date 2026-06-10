//! Shared, runtime-agnostic helpers for the V1 capture pipeline.
//! Each client keeps only the I/O; this module owns everything else.

use std::collections::HashMap;

pub(crate) const V1_CAPTURE_PATH: &str = "/i/v1/analytics/events";

use chrono::Utc;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use tracing::debug;
use uuid::Uuid;

use super::retry::{backoff_duration, is_retryable_status};
// Re-exported so the V1 capture loops in the client modules can reach them as
// `v1_capture::parse_retry_after` / `v1_capture::Step`.
pub(crate) use super::retry::{parse_retry_after, Step};
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
    // SDK identity: `<canonical-$lib-name>/<semver>`. The name must match v0's
    // `$lib` ("posthog-rs"); capture materializes it into `$lib`/`$lib_version`.
    let sdk_info = format!("posthog-rs/{version}");

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", opts.api_key))
            .unwrap_or_else(|_| HeaderValue::from_static("Bearer invalid")),
    );
    headers.insert(
        "user-agent",
        HeaderValue::from_str(&sdk_info).unwrap_or_else(|_| HeaderValue::from_static("posthog-rs")),
    );
    headers.insert(
        "posthog-sdk-info",
        HeaderValue::from_str(&sdk_info).unwrap_or_else(|_| HeaderValue::from_static("posthog-rs")),
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

/// Headers + (possibly compressed) body for a single-event, fire-and-forget
/// `$feature_flag_called` ship over the V1 endpoint. Always one attempt:
/// flag-event loss isn't worth retry traffic, and shipping must never block
/// flag reads.
pub(crate) fn build_flag_event_request(
    opts: &ClientOptions,
    event: &Event,
) -> Result<(HeaderMap, Vec<u8>), Error> {
    use crate::event_v1::V1BatchRequestRef;

    let batch = build_events(std::slice::from_ref(event), &opts.capture_defaults());
    let created_at = Utc::now().to_rfc3339();
    let req = V1BatchRequestRef {
        created_at: &created_at,
        historical_migration: None,
        batch: &batch,
    };
    let payload = serde_json::to_vec(&req).map_err(|e| Error::Serialization(e.to_string()))?;
    let mut headers = build_headers(opts, &Uuid::now_v7(), 1);
    let body = maybe_compress(opts.capture_compression, &mut headers, payload);
    Ok((headers, body))
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
    Step::Backoff(backoff_duration(opts, attempt, None))
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
    // The backend sends exactly 200 today; accept the whole 2xx class so a
    // future 201/202 isn't misclassified as a connection error.
    if (200..=299).contains(&status) {
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
            Step::Backoff(backoff_duration(opts, attempt, retry_after))
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
            Step::Backoff(backoff_duration(opts, attempt, retry_after))
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

    // -- backoff schedule ----------------------------------------------------

    /// Guards the `attempt + 1` off-by-one on the V1 call sites: the first
    /// retry must wait exactly `retry_initial_backoff_ms`, not double it.
    #[test]
    fn v1_backoff_schedule_starts_at_initial() {
        let opts = ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .max_capture_attempts(10u32)
            .retry_initial_backoff_ms(100u64)
            .retry_max_backoff_ms(1_000_000u64)
            .build()
            .unwrap();
        let rid = Uuid::now_v7();
        let ms = |step: Step| match step {
            Step::Backoff(d) => d.as_millis() as u64,
            _ => panic!("expected Step::Backoff"),
        };
        assert_eq!(
            ms(after_transport_error(&opts, &rid, 1, "timeout".into())),
            100,
            "first retry must honor retry_initial_backoff_ms exactly"
        );
        assert_eq!(
            ms(after_transport_error(&opts, &rid, 2, "timeout".into())),
            200
        );
        assert_eq!(
            ms(after_transport_error(&opts, &rid, 3, "timeout".into())),
            400
        );

        // Same schedule via a retryable HTTP response.
        let body = r#"{"error":"service_unavailable"}"#;
        let mut pending = vec![dummy_v1_event()];
        let mut final_results = HashMap::new();
        let step = after_response(
            &opts,
            &rid,
            1,
            503,
            None,
            body,
            &mut pending,
            &mut final_results,
        );
        assert_eq!(ms(step), 100);
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

    /// C3: any 2xx with a well-formed body is success, not a connection error.
    #[test]
    fn after_response_alternate_2xx_statuses_succeed() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        for status in [201u16, 202, 204, 207, 299] {
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
                status,
                None,
                &body,
                &mut pending,
                &mut final_results,
            );
            assert!(
                matches!(step, Step::Done),
                "HTTP {status} should be treated as success"
            );
            assert_eq!(final_results.len(), 1);
        }
    }

    /// C3: a 2xx with an unreadable body is a Serialization error, not success.
    #[test]
    fn after_response_alternate_2xx_malformed_body_fails() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        let mut pending = vec![dummy_v1_event()];
        let mut final_results = HashMap::new();
        let step = after_response(
            &opts,
            &rid,
            1,
            201,
            None,
            "not json",
            &mut pending,
            &mut final_results,
        );
        assert!(matches!(step, Step::Fail(Error::Serialization(_))));
    }

    // -- build_headers SDK identity -------------------------------------------

    /// C4: pins the wire identity `posthog-rs/<semver>` — the name must equal
    /// v0's `$lib` so capture's `$lib`/`$lib_version` materialization is correct.
    #[test]
    fn build_headers_sdk_info_is_canonical_lib_slash_version() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        let headers = build_headers(&opts, &rid, 1);

        let sdk_info = headers.get("posthog-sdk-info").unwrap().to_str().unwrap();
        let expected = format!("posthog-rs/{}", env!("CARGO_PKG_VERSION"));
        assert_eq!(sdk_info, expected);

        // Parse the way capture does: split at the last '/'.
        let (lib, version) = sdk_info.rsplit_once('/').unwrap();
        assert_eq!(lib, "posthog-rs");
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
        assert!(!version.is_empty());

        // user-agent mirrors the same identity string.
        let ua = headers.get("user-agent").unwrap().to_str().unwrap();
        assert_eq!(ua, expected);
    }

    // -- build_flag_event_request ----------------------------------------------

    fn flag_called_test_event() -> Event {
        let mut event = Event::new("$feature_flag_called", "user-1");
        event.insert_prop("$feature_flag", "my-flag").unwrap();
        event.insert_prop("$feature_flag_response", true).unwrap();
        event
    }

    #[test]
    fn build_flag_event_request_single_event_batch() {
        let opts = test_opts();
        let (headers, body) = build_flag_event_request(&opts, &flag_called_test_event()).unwrap();

        assert_eq!(headers.get("content-type").unwrap(), "application/json");
        assert_eq!(headers.get("posthog-attempt").unwrap(), "1");
        assert!(headers.get("posthog-request-id").is_some());
        assert_eq!(
            headers.get("authorization").unwrap().to_str().unwrap(),
            "Bearer phc_test"
        );
        assert!(headers.get("content-encoding").is_none());

        let parsed: crate::event_v1::V1BatchRequest = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed.batch.len(), 1);
        assert_eq!(parsed.batch[0].event, "$feature_flag_called");
        assert_eq!(parsed.batch[0].distinct_id, "user-1");
        assert!(parsed.historical_migration.is_none());
        let props = parsed.batch[0].properties.as_object().unwrap();
        assert_eq!(props.get("$feature_flag").unwrap(), "my-flag");
        // V1 carries SDK identity in headers, not properties.
        assert!(!props.contains_key("$lib"));
        assert!(!props.contains_key("$lib_version"));
    }

    #[test]
    fn build_flag_event_request_applies_capture_defaults() {
        let opts = ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .disable_geoip(true)
            .is_server(true)
            .build()
            .unwrap();
        let (_, body) = build_flag_event_request(&opts, &flag_called_test_event()).unwrap();
        let parsed: crate::event_v1::V1BatchRequest = serde_json::from_slice(&body).unwrap();
        let props = parsed.batch[0].properties.as_object().unwrap();
        assert_eq!(
            props.get("$geoip_disable").unwrap(),
            &serde_json::json!(true)
        );
        assert_eq!(props.get("$is_server").unwrap(), &serde_json::json!(true));
    }

    #[test]
    fn build_flag_event_request_compresses_when_configured() {
        let opts = ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .capture_compression(CaptureCompression::Gzip)
            .build()
            .unwrap();
        let (headers, body) = build_flag_event_request(&opts, &flag_called_test_event()).unwrap();
        assert_eq!(headers.get("content-encoding").unwrap(), "gzip");
        // gzip magic bytes — the body is actually compressed.
        assert_eq!(&body[..2], &[0x1f, 0x8b]);
    }
}
