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
    frame_padded(session, PADDING)
}

fn frame_padded(session: &str, padding: usize) -> Event {
    let mut properties = HashMap::new();
    properties.insert("$session_id".to_string(), json!(session));
    properties.insert(
        "$snapshot_data".to_string(),
        json!([{"type": 4, "timestamp": 1, "data": {"href": "x".repeat(padding)}}]),
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
    spawn_with_budget(options, limits, Duration::from_millis(2_000))
}

fn spawn_with_budget(
    options: &ClientOptions,
    limits: ReplayOptions,
    budget: Duration,
) -> ReplayHandle {
    let config = options.embedding.as_ref().unwrap();
    let gate = Arc::new(Gate::new(config, budget));
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
fn oversized_batch_is_resent_one_frame_at_a_time() {
    let server = MockServer::start();
    let pair = server.mock(|when, then| {
        when.method(POST)
            .path("/s/")
            .is_true(|req| sessions(req) == ["a", "a"]);
        then.status(413);
    });
    let single_a = snapshot_mock(&server, |req| sessions(req) == ["a"]);
    let handle = spawn(
        &options(&server.base_url(), None),
        limits(64 * PADDING, 64 * PADDING),
    );
    assert_eq!(handle.enqueue(frame("a")), EnqueueResult::Accepted);
    assert_eq!(handle.enqueue(frame("a")), EnqueueResult::Accepted);
    close(&handle);
    pair.assert_calls(1);
    single_a.assert_calls(2);
    assert_eq!(handle.report(), (0, 0));
}

#[test]
fn opt_out_and_opt_in_during_backoff_does_not_resend_stale_frames() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/s/");
        then.status(503).header("Retry-After", "1");
    });
    let replay = ReplayOptions {
        flush_interval_ms: 100,
        ..ReplayOptions::default()
    };
    let client = crate::client(options_with(&server.base_url(), Some(replay), 1_000));
    assert_eq!(
        client.capture_replay_snapshot(frame("a")),
        EnqueueResult::Accepted
    );
    let started = Instant::now();
    while mock.calls() == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "no first attempt"
        );
        thread::sleep(Duration::from_millis(10));
    }
    client.pause_and_purge();
    assert!(client.resume_delivery(client.delivery_generation()));
    // Outlast the backoff so the retry runs while delivery is enabled again.
    thread::sleep(Duration::from_millis(1_200));
    client.shutdown();
    mock.assert_calls(1);
    let report = client.checkpoint_delivery();
    assert_eq!((report.replay_pending, report.replay_dropped), (0, 0));
}

#[test]
fn opt_out_during_a_retry_after_backoff_frees_the_queue_for_fresh_frames() {
    let server = MockServer::start();
    let stale = server.mock(|when, then| {
        when.method(POST)
            .path("/s/")
            .is_true(|req| sessions(req) == ["stale"]);
        then.status(503).header("Retry-After", "60");
    });
    let fresh = snapshot_mock(&server, |req| sessions(req) == ["fresh"]);
    // Two of these frames overflow the minimum 1 MiB queue, so the fresh one is
    // only admitted once the stale in-flight batch has released its bytes.
    let padding = 600 * 1024;
    let replay = ReplayOptions {
        max_queue_bytes: 1024 * 1024,
        max_batch_bytes: 64 * 1024,
        flush_interval_ms: 100,
    };
    let client = crate::client(options_with(&server.base_url(), Some(replay), 60_000));
    assert_eq!(
        client.capture_replay_snapshot(frame_padded("stale", padding)),
        EnqueueResult::Accepted
    );
    let started = Instant::now();
    while stale.calls() == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "no first attempt"
        );
        thread::sleep(Duration::from_millis(10));
    }
    // Let the worker settle into the 60 s backoff before opting out.
    thread::sleep(Duration::from_millis(200));
    client.pause_and_purge();
    assert!(client.resume_delivery(client.delivery_generation()));
    let resumed = Instant::now();
    let accepted = loop {
        match client.capture_replay_snapshot(frame_padded("fresh", padding)) {
            EnqueueResult::QueueFull if resumed.elapsed() < Duration::from_secs(1) => {
                thread::sleep(Duration::from_millis(10));
            }
            result => break result,
        }
    };
    assert_eq!(accepted, EnqueueResult::Accepted);
    while fresh.calls() == 0 {
        assert!(
            resumed.elapsed() < Duration::from_secs(3),
            "fresh frame stuck behind the stale backoff"
        );
        thread::sleep(Duration::from_millis(10));
    }
    client.shutdown();
    stale.assert_calls(1);
    fresh.assert_calls(1);
    let report = client.checkpoint_delivery();
    assert_eq!((report.replay_pending, report.replay_dropped), (0, 0));
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
fn close_requested_during_a_request_bounds_the_retry_after_backoff() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/s/");
        then.status(503)
            .header("Retry-After", "60")
            .delay(Duration::from_millis(500));
    });
    let replay = ReplayOptions {
        flush_interval_ms: 50,
        ..limits(64 * PADDING, 64 * PADDING)
    };
    let handle = spawn(&options_with(&server.base_url(), None, 60_000), replay);
    assert_eq!(handle.enqueue(frame("a")), EnqueueResult::Accepted);
    let started = Instant::now();
    while handle.shared.lock().in_flight == 0 {
        assert!(started.elapsed() < Duration::from_secs(2), "never sent");
        thread::sleep(Duration::from_millis(10));
    }
    // Close while the delayed response is still pending.
    thread::sleep(Duration::from_millis(200));
    let closed = Instant::now();
    close(&handle);
    assert!(closed.elapsed() < Duration::from_secs(3));
    mock.assert_calls(1);
    assert_eq!(handle.report(), (0, 1));
}

#[test]
fn queue_left_at_the_shutdown_deadline_is_dropped_at_once() {
    // Never answers; joined below, so no worker outlives the test.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let handle = spawn_with_budget(
        &options(&format!("http://{}", silent.local_addr().unwrap()), None),
        limits(64 * PADDING, 3 * PADDING / 2),
        Duration::from_millis(300),
    );
    const FRAMES: usize = 20;
    for _ in 0..FRAMES {
        assert_eq!(handle.enqueue(frame("a")), EnqueueResult::Accepted);
    }
    let started = Instant::now();
    close(&handle);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(handle.report(), (0, FRAMES));
    let queue = handle.shared.lock();
    assert_eq!((queue.bytes, queue.since), (0, None));
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

/// Drops the client from the transport worker when it persists an event, like
/// a host store that tears down its owner.
struct DropOnPut {
    client: Mutex<Option<crate::Client>>,
    dropped: Mutex<std::sync::mpsc::Sender<()>>,
}

impl EventStore for DropOnPut {
    fn load(&self) -> io::Result<Vec<Event>> {
        Ok(Vec::new())
    }
    fn put(&self, _event: &Event) -> io::Result<()> {
        let client = self.client.lock().unwrap_or_else(|e| e.into_inner()).take();
        if client.is_some() {
            drop(client);
            let _ = self
                .dropped
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .send(());
        }
        Ok(())
    }
    fn remove(&self, _ids: &[Uuid]) -> io::Result<()> {
        Ok(())
    }
    fn purge(&self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn drop_from_transport_worker_does_not_wait_for_replay_drain() {
    // Never answers, so a replay drain runs to the shutdown deadline. Not an
    // httpmock server: the detached replay worker must not hit a pooled server
    // that a later test reuses.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    let store = Arc::new(DropOnPut {
        client: Mutex::new(None),
        dropped: Mutex::new(dropped_tx),
    });
    let options = crate::ClientOptionsBuilder::default()
        .api_key("phc_test".into())
        .host(format!("http://{}", silent.local_addr().unwrap()))
        .flush_at(1usize)
        .shutdown_timeout_ms(2_000u64)
        .embedding(EmbeddingOptions {
            store: Arc::clone(&store) as Arc<dyn EventStore>,
            sdk_name: "posthog-unreal".into(),
            sdk_version: "0.0.1".into(),
            enabled: true,
            replay: Some(ReplayOptions::default()),
        })
        .build()
        .unwrap();
    let client = crate::client(options);
    assert_eq!(
        client.capture_replay_snapshot(frame("a")),
        EnqueueResult::Accepted
    );
    let mut slot = store.client.lock().unwrap_or_else(|e| e.into_inner());
    slot.insert(client)
        .capture(Event::new("drop-in-callback", "user"));
    drop(slot);
    dropped_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("client drop waited for the replay worker");
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
