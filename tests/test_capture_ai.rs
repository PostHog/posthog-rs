//! End-to-end behavior of the AI capture lane (`capture_ai*`) against a mock
//! server, for both client flavors.
//!
//! The lane is asserted from the outside only: which path each method hits,
//! that AI bodies are zstd-compressed regardless of the client's compression
//! setting, that `flush`/`shutdown`/`Drop` drain both lanes, that the lane is
//! never started unless used, that the real 8 MiB ceiling and 5 MiB batch
//! target apply, and that the backend's per-event verdicts reach `on_error`
//! and the immediate-call summaries with their detail strings intact. The SDK
//! never tests the event name; the backend does.

use std::sync::{Arc, Mutex};

use httpmock::prelude::*;
use posthog_rs::{
    CaptureCompression, ClientOptionsBuilder, Endpoint, Event, EventStatus, PostHogError,
};
use serde_json::json;
use uuid::Uuid;

const AI_PATH: &str = "/i/v1/ai/events";
const ANALYTICS_PATH: &str = "/i/v1/analytics/events";
const MIB: usize = 1024 * 1024;

/// A recorded `on_error` capture failure: the lane endpoint plus the
/// `(uuid, details)` pairs of every `drop` verdict.
type Failures = Arc<Mutex<Vec<(Endpoint, Vec<(Uuid, String)>)>>>;

fn options(base_url: String) -> ClientOptionsBuilder {
    let mut builder = ClientOptionsBuilder::default();
    builder
        .api_key("phc_test_token".to_string())
        .host(base_url)
        .max_capture_attempts(1u32)
        .flush_interval_ms(60_000u64)
        .shutdown_timeout_ms(5_000u64);
    builder
}

fn failure_sink() -> (Failures, impl Fn(&PostHogError<'_>) + Send + Sync + 'static) {
    let sink: Failures = Arc::new(Mutex::new(Vec::new()));
    let recorder = sink.clone();
    let hook = move |failure: &PostHogError<'_>| {
        if let PostHogError::Capture(c) = failure {
            let mut drops: Vec<(Uuid, String)> = c
                .event_results()
                .iter()
                .filter(|(_, r)| r.result == EventStatus::Drop)
                .map(|(u, r)| (*u, r.details.clone().unwrap_or_default()))
                .collect();
            drops.sort();
            recorder.lock().unwrap().push((c.endpoint(), drops));
        }
    };
    (sink, hook)
}

/// Decode a zstd request body into its JSON text.
fn zstd_body(req: &HttpMockRequest) -> String {
    let bytes = zstd::decode_all(req.body_ref()).expect("AI bodies are zstd");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// 200 with an empty verdict map on `path`.
fn ok_mock<'a>(server: &'a MockServer, path: &str) -> httpmock::Mock<'a> {
    let path = path.to_string();
    server.mock(move |when, then| {
        when.method(POST).path(path);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    })
}

/// The AI path with every v1 required header present and a zstd body that
/// contains `needle` once decoded.
fn ai_mock_expecting<'a>(server: &'a MockServer, needle: &str) -> httpmock::Mock<'a> {
    let needle = needle.to_string();
    server.mock(move |when, then| {
        when.method(POST)
            .path(AI_PATH)
            .header("content-encoding", "zstd")
            .header("content-type", "application/json")
            .header_exists("authorization")
            .header_exists("user-agent")
            .header_exists("posthog-sdk-info")
            .header_exists("posthog-attempt")
            .header_exists("posthog-request-id")
            .header_exists("posthog-request-timestamp")
            .is_true(move |req| zstd_body(req).contains(&needle));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    })
}

/// The AI path answering `results` for every request.
fn ai_verdict_mock<'a>(server: &'a MockServer, results: serde_json::Value) -> httpmock::Mock<'a> {
    server.mock(move |when, then| {
        when.method(POST).path(AI_PATH);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": results }));
    })
}

fn ai_event(name: &str) -> Event {
    let mut event = Event::new(name, "user-1");
    event.insert_prop("$ai_model", "gpt").unwrap();
    event
}

fn ai_event_with_uuid(name: &str, uuid: Uuid) -> Event {
    let mut event = ai_event(name);
    event.set_uuid(uuid);
    event
}

/// An AI event whose `$ai_output` property is `bytes` characters long.
fn sized_ai_event(bytes: usize) -> Event {
    let mut event = ai_event("$ai_generation");
    event.insert_prop("$ai_output", "x".repeat(bytes)).unwrap();
    event
}

/// Three ~2 MiB events: two fit under the 5 MiB target, the third does not.
fn three_two_mib_events() -> (Vec<Uuid>, Vec<Event>) {
    let uuids: Vec<Uuid> = (0..3).map(|_| Uuid::now_v7()).collect();
    let events = uuids
        .iter()
        .map(|u| {
            let mut e = sized_ai_event(2 * MIB);
            e.set_uuid(*u);
            e
        })
        .collect();
    (uuids, events)
}

fn all_ok(uuids: &[Uuid]) -> serde_json::Value {
    json!(uuids
        .iter()
        .map(|u| (u.to_string(), json!({ "result": "ok" })))
        .collect::<serde_json::Map<_, _>>())
}

#[cfg(feature = "async-client")]
mod async_client {
    use super::*;

    async fn client(builder: &mut ClientOptionsBuilder) -> posthog_rs::Client {
        posthog_rs::client(builder.build().unwrap()).await
    }

    #[tokio::test]
    async fn capture_ai_rides_the_ai_lane_and_capture_stays_on_analytics() {
        let server = MockServer::start();
        let ai = ai_mock_expecting(&server, "\"event\":\"$ai_generation\"");
        let analytics = server.mock(|when, then| {
            when.method(POST)
                .path(ANALYTICS_PATH)
                .header_missing("content-encoding")
                .body_includes("\"event\":\"button_clicked\"");
            then.status(200).json_body(json!({ "results": {} }));
        });
        let client = client(&mut options(server.base_url())).await;
        client.capture(Event::new("button_clicked", "user-1"));
        client.capture_ai(ai_event("$ai_generation"));
        client.flush().await;
        analytics.assert_calls(1);
        ai.assert_calls(1);
        client.shutdown().await;
    }

    #[tokio::test]
    async fn client_compression_setting_does_not_change_ai_lane_zstd() {
        let server = MockServer::start();
        let ai = ai_mock_expecting(&server, "\"event\":\"$ai_span\"");
        let analytics = server.mock(|when, then| {
            when.method(POST)
                .path(ANALYTICS_PATH)
                .header("content-encoding", "gzip");
            then.status(200).json_body(json!({ "results": {} }));
        });
        let mut builder = options(server.base_url());
        builder.capture_compression(CaptureCompression::Gzip);
        let client = client(&mut builder).await;
        client.capture(Event::new("button_clicked", "user-1"));
        client.capture_ai(ai_event("$ai_span"));
        client.flush().await;
        analytics.assert_calls(1);
        ai.assert_calls(1);
    }

    #[tokio::test]
    async fn unused_ai_lane_sends_nothing() {
        let server = MockServer::start();
        let ai = ok_mock(&server, AI_PATH);
        let analytics = ok_mock(&server, ANALYTICS_PATH);
        let client = client(&mut options(server.base_url())).await;
        client.capture(Event::new("button_clicked", "user-1"));
        client.flush().await;
        client.shutdown().await;
        analytics.assert_calls(1);
        ai.assert_calls(0);
    }

    #[tokio::test]
    async fn shutdown_drains_both_lanes_and_drops_later_ai_captures() {
        let server = MockServer::start();
        let ai = ok_mock(&server, AI_PATH);
        let analytics = ok_mock(&server, ANALYTICS_PATH);
        let client = client(&mut options(server.base_url())).await;
        client.capture(Event::new("button_clicked", "user-1"));
        client.capture_ai(ai_event("$ai_generation"));
        client.shutdown().await;
        analytics.assert_calls(1);
        ai.assert_calls(1);
        client.capture_ai(ai_event("$ai_generation"));
        client.flush().await;
        client.shutdown().await;
        ai.assert_calls(1);
    }

    #[tokio::test]
    async fn drop_drains_the_ai_lane() {
        let server = MockServer::start();
        let ai = ok_mock(&server, AI_PATH);
        let client = client(&mut options(server.base_url())).await;
        client.capture_ai(ai_event("$ai_generation"));
        drop(client);
        ai.assert_calls(1);
    }

    #[tokio::test]
    async fn background_lane_drops_events_over_eight_mib_locally() {
        let server = MockServer::start();
        let ai = ok_mock(&server, AI_PATH);
        let client = client(&mut options(server.base_url())).await;
        client.capture_ai(sized_ai_event(8 * MIB + 1));
        client.flush().await;
        ai.assert_calls(0);
        client.capture_ai(sized_ai_event(MIB));
        client.flush().await;
        ai.assert_calls(1);
    }

    #[tokio::test]
    async fn capture_ai_batch_splits_at_the_byte_target_and_passes_the_flag() {
        let server = MockServer::start();
        let ai = server.mock(|when, then| {
            when.method(POST)
                .path(AI_PATH)
                .is_true(|req| zstd_body(req).contains("\"historical_migration\":true"));
            then.status(200).json_body(json!({ "results": {} }));
        });
        let client = client(&mut options(server.base_url())).await;
        let (_, events) = three_two_mib_events();
        client.capture_ai_batch(events, true);
        client.flush().await;
        ai.assert_calls(2);
    }

    #[tokio::test]
    async fn on_error_reports_the_ai_endpoint_and_server_verdicts() {
        let server = MockServer::start();
        let misrouted = Uuid::now_v7();
        let ok = Uuid::now_v7();
        let _ai = ai_verdict_mock(
            &server,
            json!({
                misrouted.to_string(): { "result": "drop", "details": "non_ai_event" },
                ok.to_string(): { "result": "ok" }
            }),
        );
        let (failures, hook) = failure_sink();
        let mut builder = options(server.base_url());
        builder.on_error(hook);
        let client = client(&mut builder).await;
        client.capture_ai(ai_event_with_uuid("not_an_ai_event", misrouted));
        client.capture_ai(ai_event_with_uuid("$ai_generation", ok));
        client.flush().await;
        let failures = failures.lock().unwrap();
        assert_eq!(
            *failures,
            vec![(
                Endpoint::CaptureAi,
                vec![(misrouted, "non_ai_event".to_string())]
            )]
        );
    }

    #[tokio::test]
    async fn capture_ai_immediate_returns_server_verdicts_including_too_big() {
        let server = MockServer::start();
        let too_big = Uuid::now_v7();
        let ai = server.mock(|when, then| {
            when.method(POST)
                .path(AI_PATH)
                .header("content-encoding", "zstd");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {
                    too_big.to_string(): { "result": "drop", "details": "ai_event_too_big" }
                }}));
        });
        let client = client(&mut options(server.base_url())).await;
        // Over the background lane's local ceiling, yet sent: the immediate path
        // leaves the size verdict to the backend and returns it.
        let mut event = sized_ai_event(8 * MIB + 1);
        event.set_uuid(too_big);
        let summary = client.capture_ai_immediate(event).await.unwrap();
        ai.assert_calls(1);
        assert_eq!(summary.submitted(), 1);
        assert_eq!(summary.not_persisted(), 1);
        assert_eq!(
            summary.event_results()[&too_big].details.as_deref(),
            Some("ai_event_too_big")
        );
    }

    #[tokio::test]
    async fn capture_ai_batch_immediate_chunks_by_bytes_and_merges_results() {
        let server = MockServer::start();
        let (uuids, events) = three_two_mib_events();
        let ai = ai_verdict_mock(&server, all_ok(&uuids));
        let client = client(&mut options(server.base_url())).await;
        let summary = client
            .capture_ai_batch_immediate(events, false)
            .await
            .unwrap();
        ai.assert_calls(2);
        assert_eq!(summary.submitted(), 3);
        assert!(summary.all_persisted());
        assert_eq!(summary.event_results().len(), 3);
    }

    #[tokio::test]
    async fn capture_ai_batch_immediate_returns_err_on_terminal_status() {
        let server = MockServer::start();
        let ai = server.mock(|when, then| {
            when.method(POST).path(AI_PATH);
            then.status(401)
                .json_body(json!({ "error": "invalid_api_token" }));
        });
        let client = client(&mut options(server.base_url())).await;
        let result = client
            .capture_ai_batch_immediate(vec![ai_event("$ai_generation")], false)
            .await;
        ai.assert_calls(1);
        assert!(result.is_err());
    }
}

#[cfg(not(feature = "async-client"))]
mod blocking {
    use super::*;

    fn client(builder: &mut ClientOptionsBuilder) -> posthog_rs::Client {
        posthog_rs::client(builder.build().unwrap())
    }

    #[test]
    fn capture_ai_rides_the_ai_lane_and_capture_stays_on_analytics() {
        let server = MockServer::start();
        let ai = ai_mock_expecting(&server, "\"event\":\"$ai_generation\"");
        let analytics = server.mock(|when, then| {
            when.method(POST)
                .path(ANALYTICS_PATH)
                .header_missing("content-encoding")
                .body_includes("\"event\":\"button_clicked\"");
            then.status(200).json_body(json!({ "results": {} }));
        });
        let client = client(&mut options(server.base_url()));
        client.capture(Event::new("button_clicked", "user-1"));
        client.capture_ai(ai_event("$ai_generation"));
        client.flush();
        analytics.assert_calls(1);
        ai.assert_calls(1);
        client.shutdown();
    }

    #[test]
    fn client_compression_setting_does_not_change_ai_lane_zstd() {
        let server = MockServer::start();
        let ai = ai_mock_expecting(&server, "\"event\":\"$ai_span\"");
        let analytics = server.mock(|when, then| {
            when.method(POST)
                .path(ANALYTICS_PATH)
                .header("content-encoding", "gzip");
            then.status(200).json_body(json!({ "results": {} }));
        });
        let mut builder = options(server.base_url());
        builder.capture_compression(CaptureCompression::Gzip);
        let client = client(&mut builder);
        client.capture(Event::new("button_clicked", "user-1"));
        client.capture_ai(ai_event("$ai_span"));
        client.flush();
        analytics.assert_calls(1);
        ai.assert_calls(1);
    }

    #[test]
    fn unused_ai_lane_sends_nothing() {
        let server = MockServer::start();
        let ai = ok_mock(&server, AI_PATH);
        let analytics = ok_mock(&server, ANALYTICS_PATH);
        let client = client(&mut options(server.base_url()));
        client.capture(Event::new("button_clicked", "user-1"));
        client.flush();
        client.shutdown();
        analytics.assert_calls(1);
        ai.assert_calls(0);
    }

    #[test]
    fn shutdown_drains_both_lanes_and_drops_later_ai_captures() {
        let server = MockServer::start();
        let ai = ok_mock(&server, AI_PATH);
        let analytics = ok_mock(&server, ANALYTICS_PATH);
        let client = client(&mut options(server.base_url()));
        client.capture(Event::new("button_clicked", "user-1"));
        client.capture_ai(ai_event("$ai_generation"));
        client.shutdown();
        analytics.assert_calls(1);
        ai.assert_calls(1);
        client.capture_ai(ai_event("$ai_generation"));
        client.flush();
        client.shutdown();
        ai.assert_calls(1);
    }

    #[test]
    fn drop_drains_the_ai_lane() {
        let server = MockServer::start();
        let ai = ok_mock(&server, AI_PATH);
        let client = client(&mut options(server.base_url()));
        client.capture_ai(ai_event("$ai_generation"));
        drop(client);
        ai.assert_calls(1);
    }

    #[test]
    fn background_lane_drops_events_over_eight_mib_locally() {
        let server = MockServer::start();
        let ai = ok_mock(&server, AI_PATH);
        let client = client(&mut options(server.base_url()));
        client.capture_ai(sized_ai_event(8 * MIB + 1));
        client.flush();
        ai.assert_calls(0);
        client.capture_ai(sized_ai_event(MIB));
        client.flush();
        ai.assert_calls(1);
    }

    #[test]
    fn capture_ai_batch_splits_at_the_byte_target_and_passes_the_flag() {
        let server = MockServer::start();
        let ai = server.mock(|when, then| {
            when.method(POST)
                .path(AI_PATH)
                .is_true(|req| zstd_body(req).contains("\"historical_migration\":true"));
            then.status(200).json_body(json!({ "results": {} }));
        });
        let client = client(&mut options(server.base_url()));
        let (_, events) = three_two_mib_events();
        client.capture_ai_batch(events, true);
        client.flush();
        ai.assert_calls(2);
    }

    #[test]
    fn on_error_reports_the_ai_endpoint_and_server_verdicts() {
        let server = MockServer::start();
        let misrouted = Uuid::now_v7();
        let ok = Uuid::now_v7();
        let _ai = ai_verdict_mock(
            &server,
            json!({
                misrouted.to_string(): { "result": "drop", "details": "non_ai_event" },
                ok.to_string(): { "result": "ok" }
            }),
        );
        let (failures, hook) = failure_sink();
        let mut builder = options(server.base_url());
        builder.on_error(hook);
        let client = client(&mut builder);
        client.capture_ai(ai_event_with_uuid("not_an_ai_event", misrouted));
        client.capture_ai(ai_event_with_uuid("$ai_generation", ok));
        client.flush();
        let failures = failures.lock().unwrap();
        assert_eq!(
            *failures,
            vec![(
                Endpoint::CaptureAi,
                vec![(misrouted, "non_ai_event".to_string())]
            )]
        );
    }

    #[test]
    fn capture_ai_immediate_returns_server_verdicts_including_too_big() {
        let server = MockServer::start();
        let too_big = Uuid::now_v7();
        let ai = server.mock(|when, then| {
            when.method(POST)
                .path(AI_PATH)
                .header("content-encoding", "zstd");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "results": {
                    too_big.to_string(): { "result": "drop", "details": "ai_event_too_big" }
                }}));
        });
        let client = client(&mut options(server.base_url()));
        let mut event = sized_ai_event(8 * MIB + 1);
        event.set_uuid(too_big);
        let summary = client.capture_ai_immediate(event).unwrap();
        ai.assert_calls(1);
        assert_eq!(summary.submitted(), 1);
        assert_eq!(summary.not_persisted(), 1);
        assert_eq!(
            summary.event_results()[&too_big].details.as_deref(),
            Some("ai_event_too_big")
        );
    }

    #[test]
    fn capture_ai_batch_immediate_chunks_by_bytes_and_merges_results() {
        let server = MockServer::start();
        let (uuids, events) = three_two_mib_events();
        let ai = ai_verdict_mock(&server, all_ok(&uuids));
        let client = client(&mut options(server.base_url()));
        let summary = client.capture_ai_batch_immediate(events, false).unwrap();
        ai.assert_calls(2);
        assert_eq!(summary.submitted(), 3);
        assert!(summary.all_persisted());
        assert_eq!(summary.event_results().len(), 3);
    }

    #[test]
    fn capture_ai_batch_immediate_returns_err_on_terminal_status() {
        let server = MockServer::start();
        let ai = server.mock(|when, then| {
            when.method(POST).path(AI_PATH);
            then.status(401)
                .json_body(json!({ "error": "invalid_api_token" }));
        });
        let client = client(&mut options(server.base_url()));
        let result = client.capture_ai_batch_immediate(vec![ai_event("$ai_generation")], false);
        ai.assert_calls(1);
        assert!(result.is_err());
    }
}
