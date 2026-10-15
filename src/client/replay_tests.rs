use super::*;
use crate::embedding::{EmbeddingOptions, EventStore};
use chrono::Utc;
use httpmock::prelude::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, Read};
use uuid::Uuid;

struct NullStore;

impl EventStore for NullStore {
    fn load(&self) -> io::Result<Vec<Event>> {
        Ok(Vec::new())
    }
    fn put(&self, _event: &Event) -> io::Result<()> {
        Ok(())
    }
    fn remove(&self, _ids: &[Uuid]) -> io::Result<()> {
        Ok(())
    }
    fn purge(&self) -> io::Result<()> {
        Ok(())
    }
}

const PADDING: usize = 1000;

/// `replay` is validated by the builder, so tests needing tiny limits pass
/// them to [`spawn`] instead.
fn options(host: &str, replay: Option<ReplayOptions>) -> ClientOptions {
    options_with(host, replay, 1)
}

fn options_with(host: &str, replay: Option<ReplayOptions>, backoff_ms: u64) -> ClientOptions {
    crate::ClientOptionsBuilder::default()
        .api_key("phc_test".into())
        .host(host.to_string())
        .flush_interval_ms(60_000u64)
        .shutdown_timeout_ms(2_000u64)
        .max_capture_attempts(3u32)
        .retry_initial_backoff_ms(backoff_ms)
        .retry_max_backoff_ms(backoff_ms)
        .embedding(EmbeddingOptions {
            store: Arc::new(NullStore),
            sdk_name: "posthog-unreal".into(),
            sdk_version: "0.0.1".into(),
            enabled: true,
            replay,
        })
        .build()
        .unwrap()
}

fn limits(max_queue_bytes: usize, max_batch_bytes: usize) -> ReplayOptions {
    ReplayOptions {
        max_queue_bytes,
        max_batch_bytes,
        flush_interval_ms: 60_000,
    }
}

fn frame(session: &str) -> Event {
    let mut properties = HashMap::new();
    properties.insert("$session_id".to_string(), json!(session));
    properties.insert(
        "$snapshot_data".to_string(),
        json!([{"type": 4, "timestamp": 1, "data": {"href": "x".repeat(PADDING)}}]),
    );
    Event::from_prepared(
        "$snapshot".into(),
        "user".into(),
        properties,
        Utc::now(),
        Uuid::now_v7(),
    )
}

fn spawn(options: &ClientOptions, limits: ReplayOptions) -> ReplayHandle {
    let config = options.embedding.as_ref().unwrap();
    let gate = Arc::new(Gate::new(config, Duration::from_millis(2_000)));
    ReplayHandle::spawn(options, limits, gate).unwrap()
}

fn close(handle: &ReplayHandle) {
    handle.request_close();
    handle.join();
}

/// Session ids of a gzipped `/s/` body whose frames all carry the wire shape.
fn sessions(req: &HttpMockRequest) -> Vec<String> {
    let mut text = String::new();
    if flate2::read::GzDecoder::new(req.body_ref())
        .read_to_string(&mut text)
        .is_err()
    {
        return Vec::new();
    }
    serde_json::from_str::<Vec<Value>>(&text)
        .unwrap_or_default()
        .iter()
        .filter(|f| {
            f["api_key"] == "phc_test"
                && f["event"] == "$snapshot"
                && f["distinct_id"] == "user"
                && f["uuid"].is_string()
                && f["timestamp"].is_string()
                && f["properties"]["$snapshot_data"].is_array()
        })
        .map(|f| f["properties"]["$session_id"].as_str().unwrap().to_owned())
        .collect()
}

fn snapshot_mock<'a>(
    server: &'a MockServer,
    matcher: fn(&HttpMockRequest) -> bool,
) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.method(POST)
            .path("/s/")
            .header("content-encoding", "gzip")
            .header("content-type", "application/json")
            .header("user-agent", "posthog-unreal/0.0.1")
            .is_true(matcher);
        then.status(200);
    })
}

#[test]
fn admission_is_bounded_by_serialized_bytes_and_closes_on_shutdown() {
    let handle = spawn(
        &options("http://127.0.0.1:1", None),
        limits(3 * PADDING, 3 * PADDING),
    );
    assert_eq!(handle.enqueue(frame("a")), EnqueueResult::Accepted);
    assert_eq!(handle.enqueue(frame("a")), EnqueueResult::Accepted);
    assert_eq!(handle.enqueue(frame("a")), EnqueueResult::QueueFull);
    assert_eq!(handle.report(), (2, 0));
    close(&handle);
    assert_eq!(handle.enqueue(frame("a")), EnqueueResult::Closed);
    // The unreachable host exhausts every attempt; the worker still exits.
    assert_eq!(handle.report(), (0, 2));
}

#[test]
fn consecutive_frames_of_one_session_share_a_gzipped_request() {
    let server = MockServer::start();
    let pair = snapshot_mock(&server, |req| sessions(req) == ["a", "a"]);
    let single_a = snapshot_mock(&server, |req| sessions(req) == ["a"]);
    let single_b = snapshot_mock(&server, |req| sessions(req) == ["b"]);
    let handle = spawn(
        &options(&server.base_url(), None),
        limits(64 * PADDING, 64 * PADDING),
    );
    for session in ["a", "a", "b", "a"] {
        assert_eq!(handle.enqueue(frame(session)), EnqueueResult::Accepted);
    }
    close(&handle);
    pair.assert_calls(1);
    single_b.assert_calls(1);
    single_a.assert_calls(1);
    assert_eq!(handle.report(), (0, 0));
}

#[test]
fn batches_are_split_at_max_batch_bytes() {
    let server = MockServer::start();
    let single_a = snapshot_mock(&server, |req| sessions(req) == ["a"]);
    let handle = spawn(
        &options(&server.base_url(), None),
        limits(64 * PADDING, 3 * PADDING / 2),
    );
    assert_eq!(handle.enqueue(frame("a")), EnqueueResult::Accepted);
    assert_eq!(handle.enqueue(frame("a")), EnqueueResult::Accepted);
    close(&handle);
    single_a.assert_calls(2);
}

#[test]
fn payload_too_large_is_dropped_without_retry_and_server_errors_use_the_attempt_budget() {
    for (status, attempts) in [(413, 1), (500, 3)] {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/s/");
            then.status(status);
        });
        let handle = spawn(
            &options(&server.base_url(), None),
            limits(64 * PADDING, 64 * PADDING),
        );
        assert_eq!(handle.enqueue(frame("a")), EnqueueResult::Accepted);
        close(&handle);
        mock.assert_calls(attempts);
        assert_eq!(handle.report(), (0, 1));
    }
}

#[test]
fn backoff_never_waits_past_the_shutdown_deadline() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/s/");
        then.status(503);
    });
    let handle = spawn(
        &options_with(&server.base_url(), None, 60_000),
        limits(64 * PADDING, 64 * PADDING),
    );
    assert_eq!(handle.enqueue(frame("a")), EnqueueResult::Accepted);
    let started = Instant::now();
    close(&handle);
    assert!(started.elapsed() < Duration::from_secs(2));
    mock.assert_calls(1);
    assert_eq!(handle.report(), (0, 1));
}

#[test]
fn opt_out_purges_queued_frames_and_resume_reopens_admission() {
    let server = MockServer::start();
    let after_resume = snapshot_mock(&server, |req| sessions(req) == ["b"]);
    let client = crate::client(options(&server.base_url(), Some(ReplayOptions::default())));
    assert_eq!(
        client.capture_replay_snapshot(frame("a")),
        EnqueueResult::Accepted
    );
    assert_eq!(client.checkpoint_delivery().replay_pending, 1);
    client.pause_and_purge();
    let report = client.checkpoint_delivery();
    assert_eq!((report.replay_pending, report.replay_dropped), (0, 0));
    assert_eq!(
        client.capture_replay_snapshot(frame("a")),
        EnqueueResult::Disabled
    );
    assert!(client.resume_delivery(client.delivery_generation()));
    assert_eq!(
        client.capture_replay_snapshot(frame("b")),
        EnqueueResult::Accepted
    );
    client.shutdown();
    after_resume.assert_calls(1);
    assert_eq!(client.checkpoint_delivery().replay_dropped, 0);
}

#[test]
fn replay_is_disabled_without_replay_options() {
    let client = crate::client(options("http://127.0.0.1:1", None));
    assert_eq!(
        client.capture_replay_snapshot(frame("a")),
        EnqueueResult::Disabled
    );
    let report = client.checkpoint_delivery();
    assert_eq!((report.replay_pending, report.replay_dropped), (0, 0));
    client.shutdown();
}

#[test]
fn invalid_replay_limits_are_rejected() {
    let result = crate::ClientOptionsBuilder::default()
        .api_key("phc_test".into())
        .embedding(EmbeddingOptions {
            store: Arc::new(NullStore),
            sdk_name: "posthog-unreal".into(),
            sdk_version: "0.0.1".into(),
            enabled: true,
            replay: Some(limits(PADDING, 2 * PADDING)),
        })
        .build();
    assert!(result.is_err());
}

#[test]
fn replay_options_deserialize_with_defaults_and_validate_bounds() {
    const MIB: usize = 1024 * 1024;
    let parse = |json: Value| serde_json::from_value::<ReplayOptions>(json);
    assert_eq!(parse(json!({})).unwrap(), ReplayOptions::default());
    assert!(parse(json!({"unknown": 1})).is_err());
    assert!(ReplayOptions::default().validate().is_ok());
    for (max_queue_bytes, max_batch_bytes, flush_interval_ms, valid) in [
        (MIB, 64 * 1024, 100, true),
        (256 * MIB, 256 * MIB, 60_000, true),
        (MIB - 1, 64 * 1024, 1000, false),
        (256 * MIB + 1, MIB, 1000, false),
        (MIB, 64 * 1024 - 1, 1000, false),
        (MIB, MIB + 1, 1000, false),
        (MIB, MIB, 99, false),
        (MIB, MIB, 60_001, false),
    ] {
        let options = ReplayOptions {
            max_queue_bytes,
            max_batch_bytes,
            flush_interval_ms,
        };
        assert_eq!(options.validate().is_ok(), valid, "{options:?}");
    }
}
