//! Shared, runtime-agnostic helpers for the capture pipeline.
//! Each client keeps only the I/O; this module owns everything else.

use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use tracing::debug;
use uuid::Uuid;

use super::retry::{capture_backoff_duration, is_retryable_status};
// Re-exported so the capture loops in the client modules can reach them as
// `capture::parse_retry_after` / `capture::Step`.
pub(crate) use super::retry::{parse_retry_after, Step};
use super::{common::preprocess_capture_event, CaptureCompression, ClientOptions};
use crate::capture_event::{
    BatchRequestRef, CaptureErrorResponse, CaptureEvent, CaptureResponse, EventResult, EventStatus,
};
use crate::client::get_default_user_agent;
use crate::endpoints::Endpoint;
use crate::error::Error;
use crate::event::Event;

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

/// Build V1 events with an injected `now` so the transport worker can stamp
/// deterministic event timestamps from its clock.
pub(crate) fn build_events_at(events: &[Event], now: DateTime<Utc>) -> Vec<CaptureEvent> {
    events
        .iter()
        .map(|event| build_event_at(event, now))
        .collect()
}

/// Build one V1 wire event from a preprocessed event. It adds no SDK values:
/// those go in before `before_send`, which has the final say over them.
pub(crate) fn build_event_at(event: &Event, now: DateTime<Utc>) -> CaptureEvent {
    CaptureEvent::from_event_at(event, now)
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

/// Serialized size of the whole wire event, for lanes with no `properties` ceiling.
pub(crate) fn measure_total(event: &CaptureEvent) -> usize {
    json_len(event)
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

/// The requests of an inline immediate capture, plus the verdicts the SDK
/// decided locally for events it does not send.
#[derive(Default)]
pub(crate) struct ImmediatePlan {
    pub(crate) requests: Vec<Prepared>,
    pub(crate) local_results: HashMap<Uuid, EventResult>,
}

/// Per-lane settings for an inline immediate capture. The analytics lane sends
/// one request; the AI lane splits the batch by bytes. Each reads its own
/// compression option.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImmediateLane {
    pub(crate) endpoint: Endpoint,
    /// `None`: everything goes in one request.
    pub(crate) batch_bytes_target: Option<usize>,
    /// Ceiling on an event's serialized `properties`. A larger event is not
    /// sent and gets a local `drop` verdict. `None`: no ceiling.
    pub(crate) max_event_bytes: Option<usize>,
}

impl ImmediateLane {
    pub(crate) fn analytics() -> Self {
        Self {
            endpoint: Endpoint::Capture,
            batch_bytes_target: None,
            max_event_bytes: None,
        }
    }

    pub(crate) fn ai() -> Self {
        Self {
            endpoint: Endpoint::CaptureAi,
            batch_bytes_target: Some(super::transport::AI_BATCH_BYTES_TARGET),
            max_event_bytes: Some(super::transport::AI_MAX_EVENT_BYTES),
        }
    }

    /// The codec for this lane's request bodies, from the lane's client option.
    pub(crate) fn compression(&self, opts: &ClientOptions) -> Option<CaptureCompression> {
        match self.endpoint {
            Endpoint::CaptureAi => opts.capture_ai_compression,
            _ => opts.capture_compression,
        }
    }
}

/// The `details` of the local verdict for an AI event over the size ceiling,
/// the same code the backend uses when it refuses one.
const AI_EVENT_TOO_BIG: &str = "ai_event_too_big";

/// Measure each event, and keep only those within `max`. An event whose
/// serialized `properties` are strictly larger than `max` gets a local `drop`
/// verdict instead, as the backend would refuse it.
fn measure_within_limit(
    events: Vec<CaptureEvent>,
    max: Option<usize>,
    local_results: &mut HashMap<Uuid, EventResult>,
) -> Vec<(CaptureEvent, usize)> {
    events
        .into_iter()
        .filter_map(|event| {
            let size = measure_event(&event);
            if max.is_some_and(|max| size.properties > max) {
                local_results.insert(
                    event.uuid,
                    EventResult {
                        result: EventStatus::Drop,
                        details: Some(AI_EVENT_TOO_BIG.to_string()),
                        duplicates_dropped: None,
                    },
                );
                return None;
            }
            Some((event, size.total))
        })
        .collect()
}

/// Prepare an inline immediate capture: apply client defaults + `before_send`,
/// then build the wire events and the per-request identity (request id,
/// `created_at`, URL) for each request. Returns no requests when nothing
/// survives filtering (an empty or fully `before_send`-dropped batch), so the
/// caller returns a default summary without sending. The analytics lane always
/// yields exactly one `Prepared`; the AI lane yields one per byte-sized chunk,
/// in order. An event over the lane's `max_event_bytes` is not sent; its local
/// `drop` verdict is in `local_results`.
pub(crate) fn prepare_immediate(
    opts: &ClientOptions,
    lane: ImmediateLane,
    events: Vec<Event>,
    historical_migration: bool,
) -> ImmediatePlan {
    let defaults = opts.capture_defaults();
    let events: Vec<Event> = events
        .into_iter()
        .filter_map(|event| preprocess_capture_event(event, &defaults, &opts.before_send))
        .collect();
    if events.is_empty() {
        return ImmediatePlan::default();
    }
    let mut local_results = HashMap::new();

    let historical_migration = historical_migration.then_some(true);
    let url = opts.endpoints().build_url(lane.endpoint);
    let built = build_events_at(&events, Utc::now());
    let chunks = match lane.batch_bytes_target {
        Some(target) => chunk_by_bytes(
            measure_within_limit(built, lane.max_event_bytes, &mut local_results),
            target,
        ),
        None if lane.max_event_bytes.is_none() => vec![built],
        None => vec![
            measure_within_limit(built, lane.max_event_bytes, &mut local_results)
                .into_iter()
                .map(|(event, _)| event)
                .collect(),
        ],
    };
    let requests = chunks
        .into_iter()
        .filter(|pending| !pending.is_empty())
        .map(|pending| Prepared {
            url: url.clone(),
            request_id: Uuid::now_v7(),
            created_at: Utc::now().to_rfc3339(),
            historical_migration,
            submitted: pending.len(),
            pending,
        })
        .collect();
    ImmediatePlan {
        requests,
        local_results,
    }
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
///
/// When a result carries `duplicates_dropped: n`, the first event with that
/// uuid takes the result and the next `n` copies are terminal drops: neither
/// resent nor recorded, and counted through the first copy's entry.
pub(crate) fn process_batch_response(
    pending: Vec<CaptureEvent>,
    results: &HashMap<Uuid, EventResult>,
    final_results: &mut HashMap<Uuid, EventResult>,
    is_final_attempt: bool,
) -> Vec<CaptureEvent> {
    let mut next = Vec::new();
    // `HashMap::new` does not allocate, so batches without duplicates pay nothing.
    let mut copies_seen: HashMap<Uuid, u32> = HashMap::new();
    for v1 in pending {
        let Some(r) = results.get(&v1.uuid) else {
            continue;
        };
        if let Some(dropped) = r.duplicates_dropped {
            let seen = copies_seen.entry(v1.uuid).or_insert(0);
            *seen = seen.saturating_add(1);
            if (1..=dropped).contains(&(*seen - 1)) {
                continue;
            }
        }
        if r.result == EventStatus::Retry && !is_final_attempt {
            next.push(v1);
        } else {
            final_results.insert(v1.uuid, r.clone());
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
    Step::Backoff(capture_backoff_duration(opts, attempt, None))
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
            Step::Backoff(capture_backoff_duration(opts, attempt, retry_after))
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
            Step::Backoff(capture_backoff_duration(opts, attempt, retry_after))
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
    use crate::client::{CaptureDefaults, ClientOptionsBuilder};
    use crate::event::MINIMAL_FLAG_CALLED_EVENT_PROPERTIES;

    fn build_preprocessed(event: Event, defaults: &CaptureDefaults) -> CaptureEvent {
        let event = preprocess_capture_event(event, defaults, &[]).unwrap();
        build_event_at(&event, Utc::now())
    }

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
            ("$cookieless_mode", serde_json::json!(true)),
            ("$ignore_sent_at", serde_json::json!(true)),
            ("$product_tour_id", serde_json::json!("tour_1")),
            ("$process_person_profile", serde_json::json!(false)),
            ("$session_id", serde_json::json!("session-1")),
            ("$window_id", serde_json::json!("window-1")),
        ] {
            event.insert_prop(k, v).unwrap();
        }

        let defaults = CaptureDefaults {
            disable_geoip: true,
            is_server: true,
        };
        let built = [build_preprocessed(event, &defaults)];
        assert_eq!(
            serde_json::Value::Object(built[0].options.clone()),
            serde_json::json!({
                "cookieless_mode": true,
                "disable_skew_correction": true,
                "product_tour_id": "tour_1",
                "process_person_profile": false,
            }),
            "the allowlist keeps every legacy option property for the wire builder"
        );
        assert_eq!(built[0].session_id.as_deref(), Some("session-1"));
        assert_eq!(built[0].window_id.as_deref(), Some("window-1"));
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
    fn before_send_sees_sdk_values_and_its_removals_hold_on_the_wire() {
        let opts = ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .disable_geoip(true)
            .before_send(|mut event| {
                let seen: Vec<String> = ["$os", "$os_version", "$is_server", "$geoip_disable"]
                    .iter()
                    .filter(|key| event.properties().contains_key(**key))
                    .map(|key| key.to_string())
                    .collect();
                event.insert_prop("hook_saw", seen).unwrap();
                event.remove_prop("$os");
                event.remove_prop("$is_server");
                event.remove_prop("$geoip_disable");
                Some(event)
            })
            .build()
            .unwrap();
        let mut event = Event::new("test", "user-1");
        event
            .insert_prop("$os_version", "caller-os-version")
            .unwrap();

        let event =
            preprocess_capture_event(event, &opts.capture_defaults(), &opts.before_send).unwrap();
        let wire = build_event_at(&event, Utc::now());

        let map = wire.properties.as_object().unwrap();
        assert_eq!(
            map.get("hook_saw"),
            Some(&serde_json::json!([
                "$os",
                "$os_version",
                "$is_server",
                "$geoip_disable"
            ]))
        );
        assert_eq!(
            map.get("$os_version"),
            Some(&serde_json::json!("caller-os-version"))
        );
        for key in ["$os", "$is_server", "$geoip_disable"] {
            assert!(!map.contains_key(key), "{} came back after the hook", key);
        }
    }

    #[test]
    fn before_send_sees_groups_and_the_minimal_trim_and_its_changes_are_sent() {
        let opts = ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .before_send(|mut event| {
                let groups = event.remove_prop("$groups");
                event.insert_prop("hook_saw_groups", groups).unwrap();
                let saw_custom = event.properties().contains_key("custom");
                event.insert_prop("hook_saw_custom", saw_custom).unwrap();
                event.add_group("team", "added-by-hook");
                Some(event)
            })
            .build()
            .unwrap();

        for minimal in [false, true] {
            let mut event = Event::new("$feature_flag_called", "user-1");
            if minimal {
                event.mark_minimal_flag_called();
            }
            event.add_group("company", "acme");
            event.insert_prop("custom", "value").unwrap();

            let event =
                preprocess_capture_event(event, &opts.capture_defaults(), &opts.before_send)
                    .unwrap();
            let wire = build_event_at(&event, Utc::now());

            let map = wire.properties.as_object().unwrap();
            assert_eq!(
                map.get("hook_saw_groups"),
                Some(&serde_json::json!({"company": "acme"})),
                "minimal={minimal}"
            );
            assert_eq!(
                map.get("hook_saw_custom"),
                Some(&serde_json::json!(!minimal)),
                "minimal={minimal}"
            );
            assert_eq!(
                map.get("$groups"),
                Some(&serde_json::json!({"team": "added-by-hook"})),
                "the hook's removal holds and its group is sent; minimal={minimal}"
            );
        }
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
        let built = [build_preprocessed(event, &defaults)];
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
            duplicates_dropped: None,
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

    /// The first copy of a uuid takes its result; the next `duplicates_dropped`
    /// copies are terminal and never resent; copies past that count keep the
    /// plain per-result handling.
    #[test]
    fn process_batch_duplicate_copies() {
        use EventStatus::{Drop, Ok, Retry, Warning};
        // (first copy's status, later copies, duplicates_dropped, final attempt,
        //  events resent, finalized status of the uuid)
        type Case = (
            EventStatus,
            usize,
            u32,
            bool,
            &'static [&'static str],
            Option<EventStatus>,
        );
        let cases: [Case; 7] = [
            (Ok, 1, 1, false, &[], Some(Ok)),
            (Drop, 2, 2, false, &[], Some(Drop)),
            (Warning, 1, 1, false, &[], Some(Warning)),
            (Retry, 2, 2, false, &["first"], None),
            (Retry, 1, 1, true, &[], Some(Retry)),
            (Ok, 2, 1, false, &[], Some(Ok)),
            (Retry, 2, 1, false, &["first", "copy-2"], None),
        ];
        for (status, copies, dropped, is_final, resent, finalized) in cases {
            let first = CaptureEvent {
                event: "first".into(),
                ..dummy_event()
            };
            let other = dummy_event();
            let mut pending = vec![first.clone(), other.clone()];
            pending.extend((1..=copies).map(|i| CaptureEvent {
                event: format!("copy-{i}"),
                ..first.clone()
            }));
            let mut result = event_result(status.clone(), Some("reason"));
            result.duplicates_dropped = Some(dropped);
            let results =
                HashMap::from([(first.uuid, result), (other.uuid, event_result(Ok, None))]);
            let mut final_results = HashMap::new();
            let next = process_batch_response(pending, &results, &mut final_results, is_final);

            let case = format!("{status:?} copies={copies} dropped={dropped} final={is_final}");
            let next_names: Vec<&str> = next.iter().map(|e| e.event.as_str()).collect();
            assert_eq!(next_names, resent, "{case}");
            let entry = final_results.get(&first.uuid);
            assert_eq!(entry.map(|r| r.result.clone()), finalized, "{case}");
            if let Some(entry) = entry {
                assert_eq!(entry.details.as_deref(), Some("reason"), "{case}");
                assert_eq!(entry.duplicates_dropped, Some(dropped), "{case}");
            }
            assert_eq!(final_results[&other.uuid].result, Ok, "{case}");
        }
    }

    // -- backoff schedule ----------------------------------------------------

    /// Guards the `attempt + 1` off-by-one on the V1 call sites: the first
    /// retry must wait `retry_initial_backoff_ms` give or take the jitter, not
    /// double it.
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
        for (attempt, base) in [(1, 100), (2, 200), (3, 400)] {
            let wait = ms(after_transport_error(
                &opts,
                &rid,
                attempt,
                "timeout".into(),
            ));
            assert!(
                (base * 3 / 4..=base * 5 / 4).contains(&wait),
                "attempt {} waited {}ms, expected {}ms +/- 25%",
                attempt,
                wait,
                base
            );
        }
        let waits: Vec<u64> = (0..50)
            .map(|_| ms(after_transport_error(&opts, &rid, 3, "timeout".into())))
            .collect();
        assert!(
            waits.iter().any(|w| *w != waits[0]),
            "capture retries must use the jittered backoff"
        );

        // Same schedule via a retryable HTTP response.
        let body = r#"{"error":"service_unavailable"}"#;
        let mut pending = vec![dummy_event()];
        let mut final_results = HashMap::new();
        let waits: Vec<u64> = (0..50)
            .map(|_| {
                ms(after_response(
                    &opts,
                    &rid,
                    1,
                    503,
                    None,
                    body,
                    &mut pending,
                    &mut final_results,
                ))
            })
            .collect();
        assert!(waits.iter().all(|w| (75..=125).contains(w)));
        assert!(waits.iter().any(|w| *w != waits[0]));
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

    /// Capture derives `$lib`/`$lib_version` from this header, so the name must
    /// stay `posthog-rs`.
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
        assert_eq!(
            ai.compression(&opts),
            None,
            "AI lane is raw unless its own option is set"
        );
        let mut with_ai = ClientOptionsBuilder::default();
        with_ai
            .api_key("phc_test".to_string())
            .capture_ai_compression(CaptureCompression::Zstd);
        let with_ai = with_ai.build().unwrap();
        assert_eq!(analytics.compression(&with_ai), None);
        assert_eq!(ai.compression(&with_ai), Some(CaptureCompression::Zstd));
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
        let prepared = prepare_immediate(&opts, analytics, events.clone(), false).requests;
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].submitted, 3);
        assert!(prepared[0].url.ends_with("/i/v1/analytics/events"));

        // AI: split by bytes. Shrink the target to fit exactly two of the three
        // (equal-size) events, so the third needs a second request; identity
        // (request id, created_at) is per request.
        let size = measure_event(&build_preprocessed(
            events[0].clone(),
            &opts.capture_defaults(),
        ))
        .total;
        let small_target = ImmediateLane {
            batch_bytes_target: Some(2 * size + size / 2),
            ..ai
        };
        let prepared = prepare_immediate(&opts, small_target, events.clone(), true).requests;
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
        let plan = prepare_immediate(&filtered, ai, events, false);
        assert!(plan.requests.is_empty());
        assert!(plan.local_results.is_empty());
    }

    #[test]
    fn prepare_immediate_drops_events_over_the_lane_ceiling_locally() {
        let mut builder = ClientOptionsBuilder::default();
        builder.api_key("phc_test".to_string());
        let opts = builder.build().unwrap();
        let event = |blob: usize| {
            let mut e = Event::new("$ai_generation".to_string(), "user-1".to_string());
            e.insert_prop("blob", "x".repeat(blob)).unwrap();
            e
        };
        let unlimited = ImmediateLane {
            max_event_bytes: None,
            ..ImmediateLane::ai()
        };
        let sent = prepare_immediate(&opts, unlimited, vec![event(64)], false);
        let limit = measure_event(&sent.requests[0].pending[0]).properties;

        // An event exactly at the ceiling is sent; one byte over is a local drop.
        // The analytics lane covers a ceiling without byte chunking.
        for lane in [ImmediateLane::ai(), ImmediateLane::analytics()] {
            let lane = ImmediateLane {
                max_event_bytes: Some(limit),
                ..lane
            };
            let (mut at_limit, mut over) = (event(64), event(65));
            let (at_limit_uuid, over_uuid) = (Uuid::now_v7(), Uuid::now_v7());
            at_limit.set_uuid(at_limit_uuid);
            over.set_uuid(over_uuid);
            let plan = prepare_immediate(&opts, lane, vec![at_limit, over], false);
            assert_eq!(plan.requests.len(), 1, "{:?}", lane.endpoint);
            assert_eq!(plan.requests[0].submitted, 1);
            assert_eq!(plan.requests[0].pending[0].uuid, at_limit_uuid);
            assert_eq!(plan.local_results.len(), 1);
            let verdict = &plan.local_results[&over_uuid];
            assert_eq!(verdict.result, EventStatus::Drop);
            assert_eq!(verdict.details.as_deref(), Some("ai_event_too_big"));
        }

        // Every event over the ceiling: no request, only local verdicts.
        let lane = ImmediateLane {
            max_event_bytes: Some(limit),
            ..ImmediateLane::ai()
        };
        let plan = prepare_immediate(&opts, lane, vec![event(65), event(66)], false);
        assert!(plan.requests.is_empty());
        assert_eq!(plan.local_results.len(), 2);
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
