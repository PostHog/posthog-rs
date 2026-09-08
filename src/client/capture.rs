//! Shared, runtime-agnostic helpers for the capture pipeline.
//! Each client keeps only the I/O; this module owns everything else.

use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use tracing::debug;
use uuid::Uuid;

use super::retry::{backoff_duration, is_retryable_status};
// Re-exported so the capture loops in the client modules can reach them as
// `capture::parse_retry_after` / `capture::Step`.
pub(crate) use super::retry::{parse_retry_after, Step};
use super::{
    common::{apply_runtime_context, preprocess_capture_event},
    CaptureCompression, CaptureDefaults, ClientOptions,
};
use crate::capture_event::{
    BatchRequestRef, CaptureErrorResponse, CaptureEvent, CaptureResponse, EventResult, EventStatus,
};
use crate::client::get_default_user_agent;
use crate::endpoints::Endpoint;
use crate::error::Error;
use crate::event::{is_minimal_flag_called_property, Event};

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

/// Build V1 events with an injected `now` so the transport worker can stamp
/// deterministic event timestamps from its clock.
pub(crate) fn build_events_at(
    events: &[Event],
    defaults: &CaptureDefaults,
    now: DateTime<Utc>,
) -> Vec<CaptureEvent> {
    events
        .iter()
        .map(|event| build_event_at(event, defaults, now))
        .collect()
}

/// Build one V1 wire event: runtime context, client defaults (caller wins),
/// and the minimized-`$feature_flag_called` allowlist.
pub(crate) fn build_event_at(
    event: &Event,
    defaults: &CaptureDefaults,
    now: DateTime<Utc>,
) -> CaptureEvent {
    let mut event = event.clone();
    apply_runtime_context(&mut event);
    let minimal = event.is_minimal_flag_called();
    let mut v1 = CaptureEvent::from_event_at(&event, now);
    if let serde_json::Value::Object(ref mut map) = v1.properties {
        if defaults.disable_geoip {
            map.entry("$geoip_disable")
                .or_insert(serde_json::Value::Bool(true));
        }
        if defaults.is_server {
            map.entry("$is_server")
                .or_insert(serde_json::Value::Bool(true));
        }
        // Final step for minimized `$feature_flag_called` events: drop
        // everything outside the allowlist. Wire-lifted keys
        // ($session_id/$window_id/$process_person_profile) already moved
        // off `properties` and are preserved on the event elsewhere.
        if minimal {
            map.retain(|key, _| is_minimal_flag_called_property(key));
        }
    }
    v1
}

// ---------------------------------------------------------------------------
// Byte accounting (AI lane)
// ---------------------------------------------------------------------------

/// Serialized size of one wire event, measured without allocating the JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventSize {
    /// Bytes of the whole serialized [`CaptureEvent`]; what a batch body grows
    /// by when this event is appended (excluding the `,` separator).
    pub(crate) total: usize,
    /// Bytes of the serialized `properties` object alone — the denominator the
    /// capture backend applies its per-event AI ceiling to.
    pub(crate) properties: usize,
}

/// `io::Write` that only counts, so sizes cost one serializer pass and no
/// allocation. Serialization is deterministic for a given value, so the count
/// equals the bytes the request body will carry.
struct CountingWriter(usize);

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    let mut w = CountingWriter(0);
    // Serialization of an already-built `CaptureEvent` cannot fail short of an
    // allocation error; a failure here would also fail at send time, where it
    // is reported. Treat it as size 0 so the event is not dropped twice.
    let _ = serde_json::to_writer(&mut w, value);
    w.0
}

pub(crate) fn measure_event(event: &CaptureEvent) -> EventSize {
    EventSize {
        total: json_len(event),
        properties: json_len(&event.properties),
    }
}

/// Split measured events into batches under a soft byte target using the
/// guarded check-before-append rule: a batch is closed only when it is
/// non-empty and the next event would push it past `target`, so an event
/// larger than the target still ships alone (worst-case body =
/// `max(target, largest event)`). Order is preserved.
pub(crate) fn chunk_by_bytes(
    events: Vec<(CaptureEvent, usize)>,
    target: usize,
) -> Vec<Vec<CaptureEvent>> {
    let mut batches: Vec<Vec<CaptureEvent>> = Vec::new();
    let mut current: Vec<CaptureEvent> = Vec::new();
    let mut current_bytes = 0usize;
    for (event, bytes) in events {
        if !current.is_empty() && current_bytes + bytes > target {
            batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current.push(event);
        current_bytes += bytes;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

#[cfg(test)]
pub(crate) fn build_headers(opts: &ClientOptions, request_id: &Uuid, attempt: u32) -> HeaderMap {
    build_headers_at(opts, request_id, attempt, Utc::now())
}

/// Like [`build_headers`] but with an injected `now` for a deterministic
/// `posthog-request-timestamp` (used by the transport worker via its clock).
pub(crate) fn build_headers_at(
    opts: &ClientOptions,
    request_id: &Uuid,
    attempt: u32,
    now: DateTime<Utc>,
) -> HeaderMap {
    let sdk_info = get_default_user_agent();

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
        HeaderValue::from_str(&now.to_rfc3339())
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
// Inline (immediate) capture preparation
// ---------------------------------------------------------------------------

/// Everything an inline immediate capture needs after event preparation:
/// built once, then reused across retry attempts. The async and blocking
/// clients share this (it is I/O-free) and keep only the send loop.
pub(crate) struct Prepared {
    pub(crate) url: String,
    pub(crate) request_id: Uuid,
    pub(crate) created_at: String,
    pub(crate) historical_migration: Option<bool>,
    pub(crate) pending: Vec<CaptureEvent>,
    pub(crate) submitted: usize,
}

/// Per-lane settings for an inline immediate capture. The analytics lane sends
/// one request with the client's compression; the AI lane forces its codec and
/// splits the batch by bytes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImmediateLane {
    pub(crate) endpoint: Endpoint,
    /// `None`: use `ClientOptions::capture_compression`.
    pub(crate) compression_override: Option<CaptureCompression>,
    /// `None`: everything goes in one request.
    pub(crate) batch_bytes_target: Option<usize>,
}

impl ImmediateLane {
    pub(crate) fn analytics() -> Self {
        Self {
            endpoint: Endpoint::Capture,
            compression_override: None,
            batch_bytes_target: None,
        }
    }

    pub(crate) fn ai() -> Self {
        Self {
            endpoint: Endpoint::CaptureAi,
            compression_override: Some(super::transport::AI_COMPRESSION),
            batch_bytes_target: Some(super::transport::AI_BATCH_BYTES_TARGET),
        }
    }

    pub(crate) fn compression(&self, opts: &ClientOptions) -> Option<CaptureCompression> {
        self.compression_override.or(opts.capture_compression)
    }
}

/// Prepare an inline immediate capture: apply client defaults + `before_send`,
/// then build the wire events and the per-request identity (request id,
/// `created_at`, URL) for each request. Returns an empty `Vec` when nothing
/// survives filtering (an empty or fully `before_send`-dropped batch), so the
/// caller returns a default summary without sending. The analytics lane always
/// yields exactly one `Prepared`; the AI lane yields one per byte-sized chunk,
/// in order.
pub(crate) fn prepare_immediate(
    opts: &ClientOptions,
    lane: ImmediateLane,
    events: Vec<Event>,
    historical_migration: bool,
) -> Vec<Prepared> {
    let defaults = opts.capture_defaults();
    let events: Vec<Event> = events
        .into_iter()
        .filter_map(|event| preprocess_capture_event(event, &defaults, &opts.before_send))
        .collect();
    if events.is_empty() {
        return Vec::new();
    }

    let historical_migration = historical_migration.then_some(true);
    let url = opts.endpoints().build_url(lane.endpoint);
    let built = build_events_at(&events, &defaults, Utc::now());
    let chunks = match lane.batch_bytes_target {
        Some(target) => chunk_by_bytes(
            built
                .into_iter()
                .map(|event| {
                    let bytes = measure_event(&event).total;
                    (event, bytes)
                })
                .collect(),
            target,
        ),
        None => vec![built],
    };
    chunks
        .into_iter()
        .map(|pending| Prepared {
            url: url.clone(),
            request_id: Uuid::now_v7(),
            created_at: Utc::now().to_rfc3339(),
            historical_migration,
            submitted: pending.len(),
            pending,
        })
        .collect()
}

/// Build the headers and (optionally compressed) body for one immediate V1
/// attempt. Kept separate from [`prepare_immediate`] because `pending` is pruned
/// and the attempt number advances between retries. I/O-free.
pub(crate) fn build_attempt_parts(
    opts: &ClientOptions,
    compression: Option<CaptureCompression>,
    request_id: &Uuid,
    attempt: u32,
    created_at: &str,
    historical_migration: Option<bool>,
    pending: &[CaptureEvent],
) -> Result<(HeaderMap, Vec<u8>), Error> {
    let request = BatchRequestRef {
        created_at,
        historical_migration,
        batch: pending,
    };
    let payload = serde_json::to_vec(&request).map_err(|e| Error::Serialization(e.to_string()))?;
    let mut headers = build_headers_at(opts, request_id, attempt, Utc::now());
    let body = maybe_compress(compression, &mut headers, payload);
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
    pending: Vec<CaptureEvent>,
    results: &HashMap<Uuid, EventResult>,
    final_results: &mut HashMap<Uuid, EventResult>,
    is_final_attempt: bool,
) -> Vec<CaptureEvent> {
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
        "Capture request failed, will retry"
    );
    Step::Backoff(backoff_duration(opts, attempt, None))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn after_response(
    opts: &ClientOptions,
    request_id: &Uuid,
    attempt: u32,
    status: u16,
    retry_after: Option<Duration>,
    body: &str,
    pending: &mut Vec<CaptureEvent>,
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
                "Capture batch response"
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
        let error_desc = serde_json::from_str::<CaptureErrorResponse>(body)
            .ok()
            .and_then(|e| e.error_description)
            .unwrap_or_else(|| body.to_string());

        debug!(
            request_id = %request_id,
            attempt,
            status,
            error = %error_desc,
            "Capture request failed, will retry"
        );

        if attempt >= opts.max_capture_attempts {
            Step::Fail(terminal_response_error(status, body))
        } else {
            Step::Backoff(backoff_duration(opts, attempt, retry_after))
        }
    } else {
        Step::Fail(terminal_response_error(status, body))
    }
}

/// Convert a terminal HTTP response into its status-specific SDK error. The
/// fallback preserves the prior defensive behavior if a success status ever
/// reaches a terminal branch.
fn terminal_response_error(status: u16, body: &str) -> Error {
    Error::from_http_response(status, body.to_string())
        .unwrap_or_else(|| Error::Connection(format!("HTTP {status}")))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use uuid::Uuid;

    use super::*;
    use crate::capture_event::{CaptureEvent, CaptureResponse, EventResult, EventStatus};
    use crate::client::ClientOptionsBuilder;
    use crate::event::MINIMAL_FLAG_CALLED_EVENT_PROPERTIES;

    fn test_opts() -> ClientOptions {
        ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .max_capture_attempts(3u32)
            .retry_initial_backoff_ms(100u64)
            .retry_max_backoff_ms(5000u64)
            .build()
            .unwrap()
    }

    #[test]
    fn minimal_flag_called_event_keeps_only_allowlisted_properties() {
        use std::collections::HashSet;

        let mut event = Event::new("$feature_flag_called", "user-1");
        event.mark_minimal_flag_called();
        event.add_group("company", "acme");
        for (k, v) in [
            ("$feature_flag", serde_json::json!("my-flag")),
            ("$feature_flag_response", serde_json::json!(true)),
            ("$feature/my-flag", serde_json::json!(true)), // stripped
            ("$feature_flag_payload", serde_json::json!({"a": 1})), // stripped
            ("$feature_flag_has_experiment", serde_json::json!(false)),
            ("locally_evaluated", serde_json::json!(false)),
            ("custom_super_property", serde_json::json!("leak")), // stripped
        ] {
            event.insert_prop(k, v).unwrap();
        }

        let defaults = CaptureDefaults {
            disable_geoip: true,
            is_server: true,
        };
        let built = build_events_at(&[event], &defaults, Utc::now());
        let map = built[0].properties.as_object().unwrap();
        let keys: HashSet<&str> = map.keys().map(String::as_str).collect();
        let allow: HashSet<&str> = MINIMAL_FLAG_CALLED_EVENT_PROPERTIES
            .iter()
            .copied()
            .collect();

        assert!(
            keys.is_subset(&allow),
            "unexpected keys leaked: {:?}",
            keys.difference(&allow).collect::<Vec<_>>()
        );
        assert!(keys.contains("$feature_flag"));
        assert!(keys.contains("$feature_flag_has_experiment"));
        assert!(keys.contains("$groups"));
        assert!(keys.contains("$geoip_disable"));
        assert!(keys.contains("$os"));
        assert!(!keys.contains("$feature/my-flag"));
        assert!(!keys.contains("$feature_flag_payload"));
        assert!(!keys.contains("custom_super_property"));
    }

    #[test]
    fn non_minimal_flag_called_event_keeps_everything() {
        let mut event = Event::new("$feature_flag_called", "user-1");
        event
            .insert_prop("$feature/my-flag", serde_json::json!(true))
            .unwrap();
        event
            .insert_prop("custom_super_property", serde_json::json!("keep"))
            .unwrap();

        let defaults = CaptureDefaults {
            disable_geoip: false,
            is_server: false,
        };
        let built = build_events_at(&[event], &defaults, Utc::now());
        let map = built[0].properties.as_object().unwrap();
        assert_eq!(
            map.get("custom_super_property"),
            Some(&serde_json::json!("keep"))
        );
        assert_eq!(map.get("$feature/my-flag"), Some(&serde_json::json!(true)));
    }

    fn dummy_event() -> CaptureEvent {
        CaptureEvent {
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
        let e1 = dummy_event();
        let e2 = dummy_event();
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
        let e1 = dummy_event();
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
        let ok_ev = dummy_event();
        let drop_ev = dummy_event();
        let warn_ev = dummy_event();
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
        let e = dummy_event();
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
    fn backoff_schedule_starts_at_initial() {
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
        let mut pending = vec![dummy_event()];
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
        let e = dummy_event();
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
        let e1 = dummy_event();
        let e2 = dummy_event();
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
        let mut pending = vec![dummy_event()];
        let mut final_results = HashMap::new();
        let step = after_response(
            &opts,
            &rid,
            1,
            503,
            Some(Duration::from_secs(1)),
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
        let mut pending = vec![dummy_event()];
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
        match step {
            Step::Fail(Error::ServerError { status, message }) => {
                assert_eq!(status, 503);
                assert_eq!(message, body);
            }
            other => panic!("expected status-preserving ServerError, got {:?}", other),
        }
    }

    #[test]
    fn after_response_non_retryable_status_fails() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        let body = r#"{"error":"billing_limit_exceeded"}"#;
        let mut pending = vec![dummy_event()];
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
        match step {
            Step::Fail(Error::BillingLimitExceeded(message)) => assert_eq!(message, body),
            other => panic!(
                "expected body-preserving BillingLimitExceeded, got {:?}",
                other
            ),
        }
    }

    /// C3: a 2xx with an unreadable body is a Serialization error, not success.
    #[test]
    fn after_response_malformed_200_and_201_bodies_fail() {
        for (case, status) in [("standard success", 200), ("alternate 2xx", 201)] {
            let opts = test_opts();
            let rid = Uuid::now_v7();
            let mut pending = vec![dummy_event()];
            let mut final_results = HashMap::new();
            let step = after_response(
                &opts,
                &rid,
                1,
                status,
                None,
                "not json",
                &mut pending,
                &mut final_results,
            );
            assert!(
                matches!(step, Step::Fail(Error::Serialization(_))),
                "{} (HTTP {}) should fail with a Serialization error",
                case,
                status
            );
        }
    }

    /// C3: any 2xx with a well-formed body is success, not a connection error.
    #[test]
    fn after_response_alternate_2xx_statuses_succeed() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        for status in [201u16, 202, 204, 207, 299] {
            let e = dummy_event();
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
                "HTTP {} should be treated as success",
                status
            );
            assert_eq!(final_results.len(), 1);
        }
    }

    /// A body-less 2xx (e.g. 204 from beacon mode) is terminal on the first
    /// attempt: Serialization error, no retry, final_results left empty.
    /// posthog-rs never opts into beacon mode, so this is a safety net.
    #[test]
    fn after_response_204_empty_body_is_terminal_serialization_error() {
        let opts = test_opts();
        let rid = Uuid::now_v7();
        let mut pending = vec![dummy_event()];
        let mut final_results = HashMap::new();
        let step = after_response(
            &opts,
            &rid,
            1,
            204,
            None,
            "",
            &mut pending,
            &mut final_results,
        );
        assert!(
            matches!(step, Step::Fail(Error::Serialization(_))),
            "expected terminal Serialization error, got {:?}",
            step
        );
        assert!(
            final_results.is_empty(),
            "no events should be finalized from a body-less 2xx"
        );
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

    // -- byte accounting (AI lane) -------------------------------------------

    fn sized(name: &str, bytes: usize) -> CaptureEvent {
        let mut e = dummy_event();
        e.event = name.into();
        e.properties = serde_json::json!({ "blob": "x".repeat(bytes) });
        e
    }

    #[test]
    fn measure_event_matches_the_serialized_bytes() {
        let e = sized("$ai_generation", 500);
        let size = measure_event(&e);
        assert_eq!(size.total, serde_json::to_vec(&e).unwrap().len());
        assert_eq!(
            size.properties,
            serde_json::to_vec(&e.properties).unwrap().len()
        );
        assert!(size.properties < size.total);
        // `properties` is what the backend's AI ceiling is charged against, so it
        // must exclude the envelope fields: exactly `{"blob":"xxx…"}`.
        assert_eq!(size.properties, "{\"blob\":\"\"}".len() + 500);
    }

    #[test]
    fn chunk_by_bytes_is_guarded_check_before_append_with_carryover() {
        let s = 100;
        let events: Vec<(CaptureEvent, usize)> = ["e1", "e2", "e3"]
            .iter()
            .map(|n| (sized(n, 10), s))
            .collect();
        let names = |batches: &Vec<Vec<CaptureEvent>>| -> Vec<Vec<String>> {
            batches
                .iter()
                .map(|b| b.iter().map(|e| e.event.clone()).collect())
                .collect()
        };

        // Target between 2S and 3S: [e1, e2] closes when e3 would overflow it.
        let batches = chunk_by_bytes(events.clone(), 2 * s + s / 2);
        assert_eq!(names(&batches), vec![vec!["e1", "e2"], vec!["e3"]]);

        // Target exactly 2S: two fit (`>` not `>=`), the third carries over.
        let batches = chunk_by_bytes(events.clone(), 2 * s);
        assert_eq!(names(&batches), vec![vec!["e1", "e2"], vec!["e3"]]);

        // Target below one event: every event ships alone; nothing is lost.
        let batches = chunk_by_bytes(events.clone(), s / 2);
        assert_eq!(names(&batches), vec![vec!["e1"], vec!["e2"], vec!["e3"]]);

        // A huge target keeps everything together, in order.
        let batches = chunk_by_bytes(events.clone(), usize::MAX);
        assert_eq!(names(&batches), vec![vec!["e1", "e2", "e3"]]);

        // Order is preserved when a big event lands between small ones.
        let mixed = vec![
            (sized("a", 10), s),
            (sized("big", 10), 10 * s),
            (sized("b", 10), s),
        ];
        let batches = chunk_by_bytes(mixed, 2 * s);
        assert_eq!(names(&batches), vec![vec!["a"], vec!["big"], vec!["b"]]);

        assert!(chunk_by_bytes(Vec::new(), s).is_empty());
    }

    #[test]
    fn immediate_lanes_pick_endpoint_compression_and_chunking() {
        let mut builder = ClientOptionsBuilder::default();
        builder
            .api_key("phc_test".to_string())
            .capture_compression(CaptureCompression::Gzip);
        let opts = builder.build().unwrap();

        let analytics = ImmediateLane::analytics();
        assert_eq!(analytics.endpoint, Endpoint::Capture);
        assert_eq!(analytics.compression(&opts), Some(CaptureCompression::Gzip));
        let ai = ImmediateLane::ai();
        assert_eq!(ai.endpoint, Endpoint::CaptureAi);
        assert_eq!(ai.compression(&opts), Some(CaptureCompression::Zstd));
        assert_eq!(
            ai.batch_bytes_target,
            Some(super::super::transport::AI_BATCH_BYTES_TARGET)
        );

        // Analytics: one request, whatever the size.
        let events: Vec<Event> = (0..3)
            .map(|i| {
                let mut e = Event::new(format!("e{i}"), "user-1".to_string());
                e.insert_prop("blob", "x".repeat(4096)).unwrap();
                e
            })
            .collect();
        let prepared = prepare_immediate(&opts, analytics, events.clone(), false);
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].submitted, 3);
        assert!(prepared[0].url.ends_with("/i/v1/analytics/events"));

        // AI: split by bytes. Shrink the target to fit exactly two of the three
        // (equal-size) events, so the third needs a second request; identity
        // (request id, created_at) is per request.
        let size = measure_event(&build_event_at(
            &events[0],
            &opts.capture_defaults(),
            Utc::now(),
        ))
        .total;
        let small_target = ImmediateLane {
            batch_bytes_target: Some(2 * size + size / 2),
            ..ai
        };
        let prepared = prepare_immediate(&opts, small_target, events.clone(), true);
        assert_eq!(prepared.len(), 2);
        assert_eq!(prepared[0].pending.len(), 2);
        assert_eq!(prepared[1].pending.len(), 1);
        assert_eq!(prepared[0].submitted + prepared[1].submitted, 3);
        assert_ne!(prepared[0].request_id, prepared[1].request_id);
        assert!(prepared.iter().all(|p| p.url.ends_with("/i/v1/ai/events")));
        assert!(prepared
            .iter()
            .all(|p| p.historical_migration == Some(true)));

        // Nothing surviving before_send → nothing to send.
        let mut filtered = ClientOptionsBuilder::default();
        filtered
            .api_key("phc_test".to_string())
            .before_send(|_| None);
        let filtered = filtered.build().unwrap();
        assert!(prepare_immediate(&filtered, ai, events, false).is_empty());
    }

    #[test]
    fn build_attempt_parts_uses_the_lane_compression() {
        let opts = test_opts();
        let pending = vec![dummy_event()];
        let (headers, body) = build_attempt_parts(
            &opts,
            Some(CaptureCompression::Zstd),
            &Uuid::now_v7(),
            1,
            "2026-05-28T12:00:00Z",
            None,
            &pending,
        )
        .unwrap();
        assert_eq!(headers.get("content-encoding").unwrap(), "zstd");
        let decoded = zstd::decode_all(body.as_slice()).unwrap();
        assert!(String::from_utf8_lossy(&decoded).contains("\"event\":\"$pageview\""));

        let (headers, body) = build_attempt_parts(
            &opts,
            None,
            &Uuid::now_v7(),
            1,
            "2026-05-28T12:00:00Z",
            None,
            &pending,
        )
        .unwrap();
        assert!(headers.get("content-encoding").is_none());
        assert!(String::from_utf8_lossy(&body).contains("\"event\":\"$pageview\""));
    }
}
