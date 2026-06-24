#![cfg(all(not(feature = "async-client"), feature = "capture-v1"))]

//! V1 capture behavior (blocking client) under the background transport.
//! `capture` / `capture_batch` are non-blocking enqueues that always return
//! `Ok`, so these tests drive delivery with `flush()` (each flush makes one
//! attempt per pending batch, ignoring backoff). A retryable failure keeps the
//! batch queued, so a later flush re-attempts it with an incremented
//! `posthog-attempt`; exhausting the attempt budget takes `max_capture_attempts`
//! flushes. Wire-format assertions are unchanged from the synchronous model.

use httpmock::prelude::*;
use posthog_rs::{ClientOptionsBuilder, Event};
use serde_json::json;

fn create_v1_client(base_url: String) -> posthog_rs::Client {
    let options = ClientOptionsBuilder::default()
        .api_key("phc_test_token".to_string())
        .host(base_url)
        .max_capture_attempts(3u32)
        .retry_initial_backoff_ms(10u64)
        .retry_max_backoff_ms(50u64)
        .build()
        .unwrap();
    posthog_rs::client(options)
}

// Constants required because httpmock `matches` takes bare fn pointers (no captures).
const PARTIAL_UUID_OK: &str = "01920000-0000-7000-8000-0000000000a1";
const PARTIAL_UUID_RETRY: &str = "01920000-0000-7000-8000-0000000000a2";
const PARTIAL_UUID_DROP: &str = "01920000-0000-7000-8000-0000000000a3";

fn body_string(req: &HttpMockRequest) -> String {
    req.body
        .as_ref()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default()
}

/// Matcher: retry event present, ok event pruned.
fn retry_body_only_contains_retry_event(req: &HttpMockRequest) -> bool {
    let body = body_string(req);
    body.contains(PARTIAL_UUID_RETRY) && !body.contains(PARTIAL_UUID_OK)
}

/// Matcher: retry event present; the dropped (terminal) event is pruned.
fn retry_body_prunes_terminal_events(req: &HttpMockRequest) -> bool {
    let body = body_string(req);
    body.contains(PARTIAL_UUID_RETRY) && !body.contains(PARTIAL_UUID_DROP)
}

#[test]
fn v1_blocking_capture_success() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("authorization", "Bearer phc_test_token")
            .header_exists("posthog-request-id")
            .header_exists("posthog-attempt");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let client = create_v1_client(server.base_url());
    client.capture(Event::new("test_event", "user-1"));
    client.flush();
    mock.assert();
}

#[test]
fn v1_blocking_retries_on_server_error() {
    let server = MockServer::start();

    let fail_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "1");
        then.status(500).json_body(json!({
            "error": "internal_error",
            "error_description": "Internal Server Error"
        }));
    });

    let success_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "2");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let client = create_v1_client(server.base_url());
    client.capture(Event::new("test", "user-1"));

    // First flush hits the 500 (attempt 1) and keeps the batch queued; the second
    // flush re-attempts it (attempt 2) against the recovered endpoint.
    client.flush();
    client.flush();

    fail_mock.assert();
    success_mock.assert();
}

#[test]
fn v1_blocking_does_not_retry_on_401() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(401).body("Unauthorized");
    });

    let client = create_v1_client(server.base_url());
    client.capture(Event::new("test", "user-1"));

    // 401 is terminal: one attempt on flush, then dropped (not returned to the
    // caller). A second flush proves there is no retry.
    client.flush();
    mock.assert_hits(1);
    client.flush();
    mock.assert_hits(1);
}

#[test]
fn v1_blocking_partial_batch_retry() {
    let server = MockServer::start();

    let mut event1 = Event::new("event_1", "user-1");
    let mut event2 = Event::new("event_2", "user-1");

    let uuid1 = uuid::Uuid::parse_str(PARTIAL_UUID_OK).unwrap();
    let uuid2 = uuid::Uuid::parse_str(PARTIAL_UUID_RETRY).unwrap();
    event1.set_uuid(uuid1);
    event2.set_uuid(uuid2);

    let first_resp = json!({
        "results": {
            PARTIAL_UUID_OK: { "result": "ok" },
            PARTIAL_UUID_RETRY: { "result": "retry", "details": "not_persisted" }
        }
    });

    let retry_resp = json!({
        "results": {
            PARTIAL_UUID_RETRY: { "result": "ok" }
        }
    });

    let first_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "1")
            .body_contains(PARTIAL_UUID_OK)
            .body_contains(PARTIAL_UUID_RETRY);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(first_resp);
    });

    // Uses `matches` to assert the ok event was pruned (body_contains can't negate).
    let retry_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "2")
            .matches(retry_body_only_contains_retry_event);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(retry_resp);
    });

    let client = create_v1_client(server.base_url());
    client.capture_batch(vec![event1, event2], false);

    // First flush (attempt 1) sends both events; the response marks one `ok` and
    // one `retry`, so the worker keeps only the retry event. The second flush
    // (attempt 2) re-sends just that pruned subset.
    client.flush();
    client.flush();

    first_mock.assert();
    retry_mock.assert();
}

#[test]
fn v1_blocking_does_not_retry_terminal_results() {
    let server = MockServer::start();

    let mut ev_ok = Event::new("ev_ok", "user-1");
    let mut ev_drop = Event::new("ev_drop", "user-1");
    let mut ev_warning = Event::new("ev_warning", "user-1");

    let uuid_ok = uuid::Uuid::now_v7();
    let uuid_drop = uuid::Uuid::now_v7();
    let uuid_warning = uuid::Uuid::now_v7();
    ev_ok.set_uuid(uuid_ok);
    ev_drop.set_uuid(uuid_drop);
    ev_warning.set_uuid(uuid_warning);

    let resp = json!({
        "results": {
            uuid_ok.to_string(): { "result": "ok" },
            uuid_drop.to_string(): { "result": "drop", "details": "billing_limit_exceeded" },
            uuid_warning.to_string(): { "result": "warning", "details": "person_processing_disabled" }
        }
    });

    let mock = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(resp);
    });

    let client = create_v1_client(server.base_url());
    client.capture_batch(vec![ev_ok, ev_drop, ev_warning], false);

    // All three per-event results are terminal (ok / drop / warning), so one
    // flush finalizes the batch. A second flush has nothing left to re-send.
    client.flush();
    mock.assert_hits(1);
    client.flush();
    mock.assert_hits(1);
}

#[test]
fn v1_blocking_whole_batch_resent_on_retryable_status() {
    for status in [408u16, 500, 502, 503, 504] {
        let server = MockServer::start();

        let mut event1 = Event::new("event_1", "user-1");
        let mut event2 = Event::new("event_2", "user-2");

        let uuid1 = uuid::Uuid::now_v7();
        let uuid2 = uuid::Uuid::now_v7();
        event1.set_uuid(uuid1);
        event2.set_uuid(uuid2);

        let ts = "2024-01-01T00:00:00.000Z";
        event1
            .set_timestamp(chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z").unwrap())
            .unwrap();
        event2
            .set_timestamp(chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z").unwrap())
            .unwrap();

        let uuid1_str = uuid1.to_string();
        let uuid2_str = uuid2.to_string();

        let fail_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "1");
            then.status(status)
                .header("content-type", "application/json")
                .header("retry-after", "0")
                .json_body(json!({
                    "error": "server_error",
                    "error_description": "transient"
                }));
        });

        let success_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "2")
                .body_contains(&uuid1_str)
                .body_contains(&uuid2_str)
                .body_contains(ts);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "results": {
                        uuid1_str: { "result": "ok" },
                        uuid2_str: { "result": "ok" }
                    }
                }));
        });

        let client = create_v1_client(server.base_url());
        client.capture_batch(vec![event1, event2], false);

        // First flush hits the retryable status (attempt 1) and keeps the whole
        // batch queued; the second flush resends every event (attempt 2).
        client.flush();
        client.flush();

        fail_mock.assert();
        success_mock.assert();
    }
}

#[test]
fn v1_blocking_prunes_terminal_events_on_partial_retry() {
    let server = MockServer::start();

    let mut ev_retry = Event::new("ev_retry", "user-1");
    let mut ev_drop = Event::new("ev_drop", "user-1");

    let uuid_retry = uuid::Uuid::parse_str(PARTIAL_UUID_RETRY).unwrap();
    let uuid_drop = uuid::Uuid::parse_str(PARTIAL_UUID_DROP).unwrap();
    ev_retry.set_uuid(uuid_retry);
    ev_drop.set_uuid(uuid_drop);

    let first_resp = json!({
        "results": {
            PARTIAL_UUID_RETRY: { "result": "retry", "details": "not_persisted" },
            PARTIAL_UUID_DROP: { "result": "drop", "details": "billing_limit_exceeded" }
        }
    });

    let retry_resp = json!({
        "results": {
            PARTIAL_UUID_RETRY: { "result": "ok" }
        }
    });

    let first_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "1")
            .body_contains(PARTIAL_UUID_RETRY)
            .body_contains(PARTIAL_UUID_DROP);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(first_resp);
    });

    let retry_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "2")
            .matches(retry_body_prunes_terminal_events);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(retry_resp);
    });

    let client = create_v1_client(server.base_url());
    client.capture_batch(vec![ev_retry, ev_drop], false);

    // First flush (attempt 1) sends both events; the response marks one `retry`
    // and one `drop` (terminal), so the worker keeps only the retry event. The
    // second flush (attempt 2) re-sends just that event, with the dropped one
    // pruned.
    client.flush();
    client.flush();

    first_mock.assert();
    retry_mock.assert();
}

#[test]
fn v1_blocking_partial_retry_exhausts_attempts() {
    let server = MockServer::start();

    let mut event = Event::new("test", "user-1");
    let uuid = uuid::Uuid::now_v7();
    event.set_uuid(uuid);

    let mock = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(200)
            .header("content-type", "application/json")
            .header("retry-after", "0")
            .json_body(json!({
                "results": {
                    uuid.to_string(): { "result": "retry", "details": "not_persisted" }
                }
            }));
    });

    let client = create_v1_client(server.base_url());
    client.capture(event);

    // Every 200 response marks the event `retry`, so each flush re-attempts it
    // (attempts 1, 2, 3) until the budget is spent and the event is dropped.
    client.flush();
    client.flush();
    client.flush();
    mock.assert_hits(3);

    // Budget exhausted: a further flush is a no-op.
    client.flush();
    mock.assert_hits(3);
}

static CAPTURED_REQUEST_IDS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn capture_request_id(req: &HttpMockRequest) -> bool {
    if let Some(headers) = req.headers.as_ref() {
        for (key, value) in headers {
            if key.eq_ignore_ascii_case("posthog-request-id") {
                CAPTURED_REQUEST_IDS.lock().unwrap().push(value.clone());
                break;
            }
        }
    }
    true
}

#[test]
fn v1_blocking_request_id_stable_across_retries() {
    CAPTURED_REQUEST_IDS.lock().unwrap().clear();

    let server = MockServer::start();

    let fail_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "1")
            .matches(capture_request_id);
        then.status(503)
            .header("content-type", "application/json")
            .header("retry-after", "0")
            .json_body(json!({
                "error": "service_unavailable",
                "error_description": "transient"
            }));
    });

    let success_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "2")
            .matches(capture_request_id);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let client = create_v1_client(server.base_url());
    client.capture(Event::new("test", "user-1"));

    // First flush sends attempt 1 (503, kept queued); the second flush re-attempts
    // it (attempt 2). Both requests must carry the same posthog-request-id.
    client.flush();
    client.flush();

    fail_mock.assert();
    success_mock.assert();

    let ids = CAPTURED_REQUEST_IDS.lock().unwrap();
    assert_eq!(ids.len(), 2, "expected exactly 2 captured request-ids");
    assert_eq!(
        ids[0], ids[1],
        "posthog-request-id must be stable across retries"
    );
}

#[test]
fn v1_blocking_does_not_retry_on_402() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(402)
            .header("content-type", "application/json")
            .json_body(json!({
                "error": "billing_limit_exceeded",
                "error_description": "Billing quota exceeded."
            }));
    });

    let client = create_v1_client(server.base_url());
    client.capture(Event::new("test", "user-1"));

    // 402 (billing limit) is terminal: one attempt on flush, then dropped. The
    // billing error is logged, not returned to the caller. A second flush proves
    // there is no retry.
    client.flush();
    mock.assert_hits(1);
    client.flush();
    mock.assert_hits(1);
}

#[test]
fn v1_blocking_exhausts_retries() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(500)
            .header("content-type", "application/json")
            .json_body(json!({
                "error": "internal_error",
                "error_description": "Internal Server Error"
            }));
    });

    let client = create_v1_client(server.base_url());
    client.capture(Event::new("test", "user-1"));

    // 500 is retryable: one attempt per flush (attempts 1, 2, 3) until the budget
    // is spent and the event is dropped — the error is logged, not returned.
    client.flush();
    client.flush();
    client.flush();
    mock.assert_hits(3);

    // Budget exhausted: a further flush is a no-op.
    client.flush();
    mock.assert_hits(3);
}

#[test]
fn v1_blocking_sends_event_options() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .body_contains("\"cookieless_mode\":true")
            .body_contains("\"process_person_profile\":false");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let client = create_v1_client(server.base_url());
    let mut event = Event::new("test", "user-1");
    event.insert_prop("$cookieless_mode", true).unwrap();
    event.insert_prop("$process_person_profile", false).unwrap();

    client.capture(event);
    client.flush();
    mock.assert();
}

#[test]
fn v1_blocking_injects_geoip_disable_when_configured() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .body_contains("\"$geoip_disable\":true");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let options = ClientOptionsBuilder::default()
        .api_key("phc_test_token".to_string())
        .host(server.base_url())
        .disable_geoip(true)
        .build()
        .unwrap();
    let client = posthog_rs::client(options);

    client.capture(Event::new("test", "user-1"));
    client.flush();
    mock.assert();
}

#[test]
fn v1_blocking_injects_is_server_by_default() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .body_contains("\"$is_server\":true");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let client = create_v1_client(server.base_url());
    client.capture(Event::new("test", "user-1"));
    client.flush();
    mock.assert();
}

#[test]
fn v1_blocking_caller_override_wins_for_is_server() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .body_contains("\"$is_server\":false");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let client = create_v1_client(server.base_url());
    let mut event = Event::new("test", "user-1");
    event.insert_prop("$is_server", false).unwrap();
    client.capture(event);
    client.flush();
    mock.assert();
}

#[test]
fn v1_blocking_batch_sets_historical_migration() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .body_contains("\"historical_migration\":true");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let client = create_v1_client(server.base_url());
    let events = vec![Event::new("a", "user-1"), Event::new("b", "user-1")];
    client.capture_batch(events, true);
    client.flush();
    mock.assert();
}

/// Matcher: the request body is valid gzip that decodes to the expected event.
fn v1_body_gunzips_to_user1(req: &HttpMockRequest) -> bool {
    use std::io::Read;

    let Some(body) = req.body.as_ref() else {
        return false;
    };
    let mut decoder = flate2::read::GzDecoder::new(&body[..]);
    let mut decoded = String::new();
    match decoder.read_to_string(&mut decoded) {
        Ok(_) => decoded.contains(r#""distinct_id":"user-1""#),
        Err(_) => false,
    }
}

#[test]
fn v1_blocking_sends_gzip_content_encoding() {
    use posthog_rs::CaptureCompression;

    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("content-encoding", "gzip")
            .matches(v1_body_gunzips_to_user1);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let options = ClientOptionsBuilder::default()
        .api_key("phc_test_token".to_string())
        .host(server.base_url())
        .capture_compression(CaptureCompression::Gzip)
        .build()
        .unwrap();
    let client = posthog_rs::client(options);

    client.capture(Event::new("test", "user-1"));
    client.flush();
    mock.assert();
}

#[test]
fn v1_blocking_preserves_uuid_and_timestamp_across_retries() {
    let server = MockServer::start();
    let uuid = uuid::Uuid::now_v7();
    let ts = "2024-01-01T00:00:00.000Z";

    let fail_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "1")
            .body_contains(uuid.to_string())
            .body_contains(ts);
        then.status(503)
            .header("retry-after", "0")
            .json_body(json!({ "error": "service_unavailable" }));
    });
    let success_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-attempt", "2")
            .body_contains(uuid.to_string())
            .body_contains(ts);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let client = create_v1_client(server.base_url());
    let mut event = Event::new("test", "user-1");
    event.set_uuid(uuid);
    event
        .set_timestamp(chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z").unwrap())
        .unwrap();

    client.capture(event);
    // First flush sends attempt 1 (503, kept queued); second flush re-sends the
    // same bytes as attempt 2 — proving uuid + timestamp survive the retry.
    client.flush();
    client.flush();
    fail_mock.assert();
    success_mock.assert();
}

#[test]
fn v1_blocking_capture_batch_empty_is_noop() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });

    let client = create_v1_client(server.base_url());
    client.capture_batch(vec![], false);
    client.flush();

    // An empty batch enqueues nothing, so flushing sends no request.
    mock.assert_hits(0);
}

#[test]
fn v1_blocking_disabled_client_noop() {
    let options = ClientOptionsBuilder::default()
        .api_key("phc_test".to_string())
        .disabled(true)
        .build()
        .unwrap();
    let client = posthog_rs::client(options);

    client.capture(Event::new("test", "user-1"));
}
