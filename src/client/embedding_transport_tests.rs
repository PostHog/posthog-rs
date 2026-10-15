use super::*;
use crate::embedding::{EmbeddingOptions, EventStore};
use std::io;
use std::sync::Condvar;

#[derive(Default)]
struct Store {
    events: Mutex<Vec<Event>>,
    block_put: Mutex<bool>,
    entered: Condvar,
    puts: AtomicUsize,
    release: Condvar,
    fail_purge: AtomicBool,
}

impl EventStore for Store {
    fn load(&self) -> io::Result<Vec<Event>> {
        Ok(self.events.lock().unwrap().clone())
    }
    fn put(&self, event: &Event) -> io::Result<()> {
        let mut block = self.block_put.lock().unwrap();
        self.puts.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_all();
        while *block {
            block = self.release.wait(block).unwrap();
        }
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
    fn remove(&self, ids: &[uuid::Uuid]) -> io::Result<()> {
        self.events
            .lock()
            .unwrap()
            .retain(|event| !ids.contains(&event.uuid()));
        Ok(())
    }
    fn purge(&self) -> io::Result<()> {
        if self.fail_purge.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "test"));
        }
        self.events.lock().unwrap().clear();
        Ok(())
    }
}

impl Store {
    fn wait_for_put(&self) {
        let mut block = self.block_put.lock().unwrap();
        while self.puts.load(Ordering::SeqCst) == 0 {
            let (next, timeout) = self
                .entered
                .wait_timeout(block, Duration::from_secs(5))
                .unwrap();
            assert!(!timeout.timed_out(), "worker did not start persistence");
            block = next;
        }
    }
    fn unblock(&self) {
        *self.block_put.lock().unwrap() = false;
        self.release.notify_all();
    }
}

fn options(store: Arc<dyn EventStore>) -> ClientOptions {
    options_at(store, "http://127.0.0.1:1".into())
}

fn options_at(store: Arc<dyn EventStore>, host: String) -> ClientOptions {
    crate::ClientOptionsBuilder::default()
        .api_key("phc_test".into())
        .host(host)
        .flush_at(100usize)
        .flush_interval_ms(60_000u64)
        .embedding(EmbeddingOptions {
            store,
            sdk_name: "posthog-unreal".into(),
            sdk_version: "0.0.1".into(),
            enabled: true,
        })
        .shutdown_timeout_ms(100u64)
        .build()
        .unwrap()
}

#[test]
fn stale_queued_resume_cannot_override_later_opt_out() {
    let store = Arc::new(Store::default());
    *store.block_put.lock().unwrap() = true;
    let handle = TransportHandle::spawn(options(store.clone()));
    handle.enqueue(Event::new("one", "user"));
    store.wait_for_put();
    let (tx, rx) = mpsc::channel();
    let generation = handle
        .gate
        .as_ref()
        .unwrap()
        .generation
        .load(Ordering::Acquire);
    handle.send_control(Control::Resume {
        generation,
        completion: tx,
    });
    handle.pause_and_purge();
    store.unblock();
    assert!(!rx.recv_timeout(Duration::from_secs(5)).unwrap());
    handle.checkpoint();
    assert_eq!(
        handle.enqueue_checked(Event::new("blocked", "user")),
        EnqueueResult::Disabled
    );
    assert!(store.events.lock().unwrap().is_empty());
    handle.shutdown_blocking();
}

#[test]
fn expired_shutdown_does_not_start_remaining_disk_writes() {
    let store = Arc::new(Store::default());
    *store.block_put.lock().unwrap() = true;
    let handle = TransportHandle::spawn(options(store.clone()));
    handle.enqueue(Event::new("first", "user"));
    store.wait_for_put();
    for _ in 0..10 {
        handle.enqueue(Event::new("later", "user"));
    }
    handle.request_close();
    // Drive the shared deadline directly; no wall-clock sleep is necessary.
    *handle.gate.as_ref().unwrap().deadline.lock().unwrap() = Some(Instant::now());
    store.unblock();
    handle.join();
    assert_eq!(store.puts.load(Ordering::SeqCst), 1);
    assert_eq!(store.events.lock().unwrap().len(), 1);
}

#[test]
fn failed_purge_keeps_admission_closed_until_successful_resume() {
    let store = Arc::new(Store::default());
    let handle = TransportHandle::spawn(options(store.clone()));
    handle.enqueue(Event::new("event", "user"));
    handle.checkpoint();
    store.fail_purge.store(true, Ordering::SeqCst);
    handle.pause_and_purge();
    assert!(!handle.resume(handle.generation()));
    assert_eq!(
        handle.enqueue_checked(Event::new("blocked", "user")),
        EnqueueResult::Disabled
    );
    assert!(handle.checkpoint().persistence_errors > 0);
    store.fail_purge.store(false, Ordering::SeqCst);
    assert!(handle.resume(handle.generation()));
    assert!(store.events.lock().unwrap().is_empty());
    handle.shutdown_blocking();
}

#[test]
fn opt_in_epoch_captured_before_a_later_opt_out_cannot_resume() {
    let store = Arc::new(Store::default());
    let handle = TransportHandle::spawn(options(store));
    let generation = handle.generation();
    handle.pause_and_purge();
    assert!(!handle.resume(generation));
    assert_eq!(
        handle.enqueue_checked(Event::new("blocked", "user")),
        EnqueueResult::Disabled
    );
    handle.shutdown_blocking();
}

#[test]
fn mixed_v1_verdicts_retire_only_terminal_records() {
    use httpmock::prelude::*;
    let server = MockServer::start();
    let store = Arc::new(Store::default());
    let events: Vec<_> = ["ok", "warning", "drop", "retry"]
        .iter()
        .map(|name| Event::new(*name, "user"))
        .collect();
    let results: serde_json::Map<String, serde_json::Value> = events
        .iter()
        .map(|event| {
            (
                event.uuid().to_string(),
                serde_json::json!({"result": event.event_name()}),
            )
        })
        .collect();
    let mut response = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .header("posthog-sdk-info", "posthog-unreal/0.0.1");
        then.status(200)
            .json_body(serde_json::json!({"results": results}));
    });
    let mut config = options_at(store.clone(), server.base_url());
    config.max_capture_attempts = 1;
    config.max_queue_size = 4;
    let handle = TransportHandle::spawn(config);
    for event in &events {
        assert_eq!(
            handle.enqueue_checked(event.clone()),
            EnqueueResult::Accepted
        );
    }
    handle.flush_blocking();
    let report = handle.checkpoint();
    assert_eq!(
        (report.delivered, report.terminally_dropped, report.pending),
        (2, 1, 1)
    );
    assert_eq!(
        store
            .events
            .lock()
            .unwrap()
            .iter()
            .map(Event::uuid)
            .collect::<Vec<_>>(),
        vec![events[3].uuid()]
    );
    response.assert_calls(1);
    response.delete();
    let ack = server.mock(|when, then| {
        when.method(POST)
            .header("posthog-attempt", "1")
            .json_body_includes(serde_json::json!({"batch": [{"event": "retry"}]}).to_string());
        then.status(200).json_body(serde_json::json!({"results": {
            events[3].uuid().to_string(): {"result": "ok"}
        }}));
    });
    handle.flush_blocking();
    ack.assert_calls(1);
    let report = handle.checkpoint();
    assert_eq!(
        (report.delivered, report.terminally_dropped, report.pending),
        (3, 1, 0)
    );
    assert!(store.events.lock().unwrap().is_empty());
    handle.shutdown_blocking();
}

#[test]
fn exhausted_request_failures_keep_capacity_and_replay_original_event() {
    use httpmock::prelude::*;
    let server = MockServer::start();
    let store = Arc::new(Store::default());
    let mut failure = server.mock(|when, then| {
        when.method(POST);
        then.status(503);
    });
    let mut config = options_at(store.clone(), server.base_url());
    config.max_capture_attempts = 1;
    config.max_queue_size = 1;
    let timestamp = chrono::DateTime::parse_from_rfc3339("2025-01-02T03:04:05.123Z")
        .unwrap()
        .with_timezone(&Utc);
    let mut event = Event::from_prepared(
        "prepared".into(),
        "original-user".into(),
        serde_json::from_value(serde_json::json!({
            "nested": [null, false, {"number": 123, "text": "00123"}],
            "$process_person_profile": false,
            "$session_id": "01900000-0000-7000-8000-000000000001",
            "$lib": "ignored-property", "$lib_version": "ignored-version"
        }))
        .unwrap(),
        timestamp,
        uuid::Uuid::now_v7(),
    );
    event.insert_option("custom_option", "unchanged").unwrap();
    let handle = TransportHandle::spawn(config.clone());
    assert_eq!(
        handle.enqueue_checked(event.clone()),
        EnqueueResult::Accepted
    );
    handle.flush_blocking();
    assert_eq!(handle.checkpoint().pending, 1);
    assert_eq!(
        handle.enqueue_checked(Event::new("overflow", "new-user")),
        EnqueueResult::QueueFull
    );
    assert_eq!(store.events.lock().unwrap().as_slice(), &[event.clone()]);
    handle.shutdown_blocking();
    failure.delete();
    assert_eq!(store.events.lock().unwrap().as_slice(), &[event.clone()]);
    let ack = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events")
            .is_true(|request| {
                serde_json::from_slice::<serde_json::Value>(request.body_ref()).is_ok_and(|body| {
                    body["batch"][0]["properties"] == serde_json::json!({"nested": [null, false, {"number": 123, "text": "00123"}]})
                })
            })
            .header("posthog-sdk-info", "posthog-unreal/0.0.1")
            .json_body_includes(serde_json::json!({"batch": [{
                "uuid": event.uuid(), "event": "prepared", "distinct_id": "original-user",
                "timestamp": "2025-01-02T03:04:05.123Z",
                "session_id": "01900000-0000-7000-8000-000000000001",
                "options": {"process_person_profile": false, "custom_option": "unchanged"},
                "properties": {"nested": [null, false, {"number": 123, "text": "00123"}]}
            }]}).to_string());
        then.status(200).json_body(serde_json::json!({"results": {
            event.uuid().to_string(): {"result": "ok"}
        }}));
    });
    let restarted = TransportHandle::spawn(config);
    restarted.flush_blocking();
    ack.assert_calls(1);
    assert_eq!(restarted.checkpoint().delivered, 1);
    assert!(store.events.lock().unwrap().is_empty());
    restarted.shutdown_blocking();
}

#[test]
fn exhausted_sequence_waits_for_timer_and_opt_out_purges_retries() {
    use httpmock::prelude::*;
    let server = MockServer::start();
    let store = Arc::new(Store::default());
    let event = Event::new("retry", "user");
    let retry = server.mock(|when, then| {
        when.method(POST).header("posthog-attempt", "1");
        then.status(200)
            .header("retry-after", "60")
            .json_body(serde_json::json!({"results": {
                event.uuid().to_string(): {"result": "retry"}
            }}));
    });
    let mut config = options_at(store.clone(), server.base_url());
    config.max_capture_attempts = 1;
    let clock = Arc::new(super::tests::ManualClock::new());
    let handle = TransportHandle::spawn_with_clock(config, clock.clone());
    handle.enqueue(event);
    handle.flush_blocking();
    handle.tick();
    retry.assert_calls(1);
    clock.advance(Duration::from_secs(59));
    handle.tick();
    retry.assert_calls(1);
    clock.advance(Duration::from_secs(1));
    handle.tick();
    retry.assert_calls(2);
    assert_eq!(handle.checkpoint().pending, 1);
    assert_eq!(store.events.lock().unwrap().len(), 1);
    handle.pause_and_purge();
    assert_eq!(handle.checkpoint().pending, 0);
    clock.advance(Duration::from_secs(61));
    handle.tick();
    retry.assert_calls(2);
    assert!(store.events.lock().unwrap().is_empty());
    handle.shutdown_blocking();
}

fn check_truncated_response(status: u16) {
    use crate::embedding::FileEventStore;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{Shutdown, TcpListener};

    let path = std::env::temp_dir().join(format!("posthog-truncated-{}", Uuid::now_v7()));
    let store = Arc::new(FileEventStore::open(&path).unwrap());
    let event = Event::from_prepared(
        "body-failure".into(),
        "original-user".into(),
        HashMap::new(),
        Utc::now(),
        Uuid::now_v7(),
    );
    let id = event.uuid();
    // An HTTP fixture library normally repairs Content-Length. Use a raw
    // socket here to exercise an actual premature response-body EOF.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (requests_tx, requests_rx) = mpsc::channel();
    let terminal = (400..500).contains(&status);
    let request_count = if terminal { 1 } else { 3 };
    let server = std::thread::spawn(move || {
        for attempt in 0..request_count {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "request did not arrive");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("accept failed: {}", error),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut length = None;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = Some(value.trim().parse::<usize>().unwrap());
                    }
                }
            }
            let mut body = vec![0; length.unwrap()];
            reader.read_exact(&mut body).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            requests_tx.send(body["batch"].clone()).unwrap();
            drop(reader);
            if attempt < 2 {
                write!(socket, "HTTP/1.1 {} Fixture\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{{\"results\":", status).unwrap();
            } else {
                let body =
                    serde_json::json!({"results": {id.to_string(): {"result": "ok"}}}).to_string();
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
            socket.shutdown(Shutdown::Write).unwrap();
        }
    });
    let mut config = options_at(store.clone(), format!("http://{address}"));
    config.max_capture_attempts = 2;
    config.max_queue_size = 1;
    config.shutdown_timeout_ms = 1000;
    let handle = TransportHandle::spawn(config);
    assert_eq!(
        handle.enqueue_checked(event.clone()),
        EnqueueResult::Accepted
    );
    for _ in 0..if terminal { 0 } else { 2 } {
        handle.flush_blocking();
        let report = handle.checkpoint();
        assert_eq!(
            (report.pending, report.delivered, report.terminally_dropped),
            (1, 0, 0)
        );
        assert_eq!(store.load().unwrap(), vec![event.clone()]);
        assert_eq!(
            handle.enqueue_checked(Event::new("overflow", "user")),
            EnqueueResult::QueueFull
        );
    }
    handle.flush_blocking();
    let report = handle.checkpoint();
    assert_eq!(
        (report.pending, report.delivered, report.terminally_dropped),
        if terminal { (0, 0, 1) } else { (0, 1, 0) }
    );
    assert!(store.load().unwrap().is_empty());
    // A terminal response must not schedule another request.
    handle.flush_blocking();
    server.join().unwrap();
    let requests: Vec<_> = requests_rx.into_iter().collect();
    assert_eq!(requests.len(), request_count);
    assert_eq!(requests[0][0]["uuid"], id.to_string());
    assert!(requests.windows(2).all(|pair| pair[0] == pair[1]));
    handle.shutdown_blocking();
    drop(handle);
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn truncated_success_body_retains_spool_and_capacity_through_retry_exhaustion() {
    check_truncated_response(200);
}

#[test]
fn truncated_terminal_body_retires_spool_without_retries() {
    check_truncated_response(400);
    check_truncated_response(429);
}

#[test]
fn truncated_retryable_error_body_retains_spool() {
    check_truncated_response(503);
}

#[test]
fn terminal_http_failure_removes_only_its_batch() {
    use httpmock::prelude::*;
    let server = MockServer::start();
    let store = Arc::new(Store::default());
    let rejected = Event::new("reject", "user");
    let retry = Event::new("retry", "user");
    let reject = server.mock(|when, then| {
        when.method(POST).body_includes("reject");
        then.status(400);
    });
    let transient = server.mock(|when, then| {
        when.method(POST).body_includes("retry");
        then.status(503);
    });
    let mut config = options_at(store.clone(), server.base_url());
    config.max_batch_size = 1;
    config.max_capture_attempts = 1;
    let handle = TransportHandle::spawn(config);
    handle.enqueue(rejected);
    handle.enqueue(retry.clone());
    handle.flush_blocking();
    reject.assert_calls(1);
    transient.assert_calls(1);
    assert_eq!(
        store
            .events
            .lock()
            .unwrap()
            .iter()
            .map(Event::uuid)
            .collect::<Vec<_>>(),
        vec![retry.uuid()]
    );
    let report = handle.checkpoint();
    assert_eq!((report.pending, report.terminally_dropped), (1, 1));
    handle.pause_and_purge();
    assert_eq!(handle.checkpoint().pending, 0);
    assert!(store.events.lock().unwrap().is_empty());
    handle.shutdown_blocking();
}

#[test]
fn restored_events_count_against_admission_bound() {
    let store = Arc::new(Store::default());
    store
        .events
        .lock()
        .unwrap()
        .push(Event::new("restored", "user"));
    let mut options = options(store);
    options.max_queue_size = 1;
    let handle = TransportHandle::spawn(options);
    assert_eq!(
        handle.enqueue_checked(Event::new("overflow", "user")),
        EnqueueResult::QueueFull
    );
    handle.shutdown_blocking();
}
