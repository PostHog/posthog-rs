#![cfg(feature = "session-replay")]

use posthog::stateful::{Client, Config, FrameLayout, Persistence, PixelFormat, SubmitOutcome};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone)]
struct Seen {
    encoding: Option<String>,
    raw: Vec<u8>,
    json: Value,
}

struct Host {
    url: String,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<Seen>>>,
    worker: Option<JoinHandle<()>>,
}

impl Host {
    fn new(status: u16, truncated: bool, split_pair: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stopping = stop.clone();
        let captured = seen.clone();

        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let (stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("accept: {}", error),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut first = String::new();
                if reader.read_line(&mut first).unwrap() == 0 {
                    continue;
                }
                let path = first.split_whitespace().nth(1).unwrap().to_string();
                let mut length = 0;
                let mut encoding = None;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse::<usize>().unwrap();
                        }
                        if name.eq_ignore_ascii_case("content-encoding") {
                            encoding = Some(value.trim().to_string());
                        }
                    }
                }
                let mut raw = vec![0; length];
                reader.read_exact(&mut raw).unwrap();
                let mut stream: TcpStream = reader.into_inner();

                let (response_status, body, bad_length) = if path == "/array/phc_test/config" {
                    (
                        200,
                        json!({
                            "hasFeatureFlags": false,
                            "sessionRecording": {}
                        })
                        .to_string(),
                        false,
                    )
                } else if path == "/s/" {
                    let decoded = if encoding.as_deref() == Some("gzip") {
                        let mut decoded = Vec::new();
                        let _ =
                            flate2::read::GzDecoder::new(raw.as_slice()).read_to_end(&mut decoded);
                        decoded
                    } else {
                        raw.clone()
                    };
                    let value: Value = serde_json::from_slice(&decoded).unwrap_or(Value::Null);
                    let count = value.as_array().map_or(0, Vec::len);
                    captured.lock().unwrap().push(Seen {
                        encoding,
                        raw,
                        json: value,
                    });
                    let code = if split_pair && count == 1 {
                        200
                    } else {
                        status
                    };
                    let bad = truncated && code == status;
                    (code, if bad { "{".into() } else { "{}".into() }, bad)
                } else {
                    (404, "{}".into(), false)
                };

                let declared = if bad_length {
                    body.len() + 20
                } else {
                    body.len()
                };
                write!(
                    stream,
                    "HTTP/1.1 {} Test\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_status, declared, body
                )
                .unwrap();
                stream.flush().unwrap();
                // Closing here makes a declared-but-missing response body observable.
            }
        });

        Self {
            url,
            stop,
            seen,
            worker: Some(worker),
        }
    }

    fn client(&self, frames: usize, disable_compression: bool) -> Client {
        let mut config = Config::new("phc_test");
        config.host = self.url.clone();
        config.persistence = Persistence::Memory;
        config.preload_feature_flags = false;
        config.request_timeout_seconds = 1;
        config.shutdown_timeout = Duration::ZERO;
        config.session_replay.max_frames_in_flight = frames;
        if disable_compression {
            config.capture_compression = None;
        }
        let client = Client::init_blocking(config);
        client.flush_blocking();
        client
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn layout() -> FrameLayout {
    FrameLayout {
        width: 2,
        height: 2,
        row_stride_bytes: 8,
        format: PixelFormat::Bgra8888,
    }
}

fn submit(client: &Client, count: usize) {
    let replay = client.session_replay();
    replay.start();
    assert!(replay.is_active(), "remote-config/setup control failed");
    for _ in 0..count {
        let mut frame = replay.begin_frame(layout()).unwrap();
        frame.pixels_mut().fill(0);
        assert_eq!(replay.submit_frame(frame), SubmitOutcome::Accepted);
    }
    client.flush_blocking();
}

#[test]
fn screenshot_id_is_outside_reserved_range() {
    let host = Host::new(200, false, false);
    let client = host.client(1, false);
    submit(&client, 1);
    let requests = host.requests();
    assert_eq!(requests.len(), 1);
    let id = requests[0].json[0]["properties"]["$snapshot_data"][1]["data"]["wireframes"][0]["id"]
        .as_u64()
        .unwrap();
    assert!(id >= 10_000_000, "reserved screenshot id: {}", id);
}

#[test]
fn replay_compression_default_and_opt_out() {
    for disabled in [false, true] {
        let host = Host::new(200, false, false);
        let client = host.client(1, disabled);
        submit(&client, 1);
        let requests = host.requests();
        assert_eq!(requests.len(), 1);
        if disabled {
            assert_eq!(requests[0].encoding, None);
            let plain: Value = serde_json::from_slice(&requests[0].raw).unwrap();
            assert_eq!(plain[0]["event"], "$snapshot");
        } else {
            assert_eq!(requests[0].encoding.as_deref(), Some("gzip"));
            assert_eq!(requests[0].json[0]["event"], "$snapshot");
        }
    }
}

fn status_case(
    status: u16,
    truncated: bool,
    split_pair: bool,
    frames: usize,
    expected_requests: usize,
    capacity_released: bool,
) {
    let host = Host::new(status, truncated, split_pair);
    let client = host.client(frames, false);
    submit(&client, frames);
    assert_eq!(host.requests().len(), expected_requests);
    let next = client.session_replay().begin_frame(layout());
    assert_eq!(next.is_some(), capacity_released);
    drop(next);
    client.shutdown_blocking(Some(Duration::ZERO));
}

#[test]
fn complete_400_releases_capacity_control() {
    status_case(400, false, false, 2, 1, true);
}

#[test]
fn truncated_400_releases_capacity() {
    status_case(400, true, false, 2, 1, true);
}

#[test]
fn complete_413_pair_splits_control() {
    status_case(413, false, true, 2, 3, true);
}

#[test]
fn truncated_413_pair_splits() {
    status_case(413, true, true, 2, 3, true);
}

#[test]
fn truncated_413_singleton_releases_capacity() {
    status_case(413, true, false, 1, 1, true);
}

#[test]
fn truncated_503_retains_capacity_control() {
    status_case(503, true, false, 2, 1, false);
}

fn hook_client<F>(host: &Host, hook: F) -> Client
where
    F: FnMut(posthog::Event) -> Option<posthog::Event> + Send + 'static,
{
    let mut config = Config::new("phc_test");
    config.host = host.url.clone();
    config.persistence = Persistence::Memory;
    config.preload_feature_flags = false;
    config.request_timeout_seconds = 1;
    config.shutdown_timeout = Duration::ZERO;
    config.before_send(hook);
    let client = Client::init_blocking(config);
    client.flush_blocking();
    client
}

#[test]
fn replay_preserves_hook_cookieless_option() {
    // First case is an ordinary legacy-property control. The other cases
    // require the explicit option to survive, including its precedence.
    for mode in 0..4 {
        let host = Host::new(200, false, false);
        let client = hook_client(&host, move |mut event| {
            if event.event_name() == "$snapshot" {
                if mode == 0 {
                    event.insert_prop("$cookieless_mode", true).unwrap();
                } else {
                    if mode == 2 {
                        event.insert_prop("$cookieless_mode", false).unwrap();
                    }
                    event
                        .insert_option(
                            "cookieless_mode",
                            if mode == 3 {
                                Value::Null
                            } else {
                                Value::Bool(true)
                            },
                        )
                        .unwrap();
                    if mode == 3 {
                        event.insert_prop("$cookieless_mode", true).unwrap();
                    }
                }
            }
            Some(event)
        });
        submit(&client, 1);
        let requests = host.requests();
        assert_eq!(requests.len(), 1, "upload/setup control");
        assert_eq!(
            requests[0].json[0]["properties"]["$cookieless_mode"],
            Value::Bool(true),
            "mode {}",
            mode
        );
    }
}

#[test]
fn window_id_is_session_scoped_after_reset() {
    let host = Host::new(200, false, false);
    let client = host.client(1, false);
    submit(&client, 1);
    client.reset();
    submit(&client, 1);

    let requests = host.requests();
    assert_eq!(requests.len(), 2, "upload/setup control");
    let before = &requests[0].json[0];
    let after = &requests[1].json[0];
    assert_ne!(before["distinct_id"], after["distinct_id"]);
    assert_ne!(
        before["properties"]["$device_id"],
        after["properties"]["$device_id"]
    );
    assert_ne!(
        before["properties"]["$session_id"],
        after["properties"]["$session_id"]
    );
    assert_ne!(
        before["properties"]["$window_id"],
        after["properties"]["$window_id"]
    );
    for event in [before, after] {
        assert_eq!(
            event["properties"]["$window_id"],
            event["properties"]["$session_id"]
        );
    }
}

#[test]
fn native_snapshot_wire_contract_uses_consistent_envelope_and_millisecond_timestamps() {
    let host = Host::new(200, false, false);
    let client = host.client(2, false);
    submit(&client, 2);
    let requests = host.requests();
    assert_eq!(requests.len(), 1);
    let batch = requests[0].json.as_array().unwrap();
    assert_eq!(batch.len(), 2);
    let mut screenshot_id = None;
    for event in batch {
        assert_eq!(event["event"], "$snapshot");
        assert_eq!(event["api_key"], "phc_test");
        assert_eq!(event["distinct_id"], client.get_distinct_id());
        uuid::Uuid::parse_str(event["uuid"].as_str().unwrap()).unwrap();
        let millis = chrono::DateTime::parse_from_rfc3339(event["timestamp"].as_str().unwrap())
            .unwrap()
            .timestamp_millis();
        let properties = &event["properties"];
        assert_eq!(properties["$snapshot_source"], "mobile");
        assert_eq!(properties["$process_person_profile"], false);
        assert_eq!(
            properties["$session_id"],
            client.peek_session_id().unwrap().to_string()
        );
        assert_eq!(properties["$window_id"], properties["$session_id"]);
        let snapshots = properties["$snapshot_data"].as_array().unwrap();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0]["type"], 4);
        assert_eq!(snapshots[1]["type"], 2);
        for snapshot in snapshots {
            assert_eq!(snapshot["timestamp"], millis);
        }
        assert_eq!(snapshots[0]["data"]["width"], layout().width);
        assert_eq!(snapshots[0]["data"]["height"], layout().height);
        assert_eq!(
            snapshots[1]["data"]["initialOffset"],
            json!({"top": 0, "left": 0})
        );
        let image = &snapshots[1]["data"]["wireframes"][0];
        assert_eq!(image["type"], "screenshot");
        assert_eq!(image["width"], layout().width);
        assert_eq!(image["height"], layout().height);
        let id = image["id"].as_u64().unwrap();
        assert!(id >= 10_000_000);
        assert_eq!(*screenshot_id.get_or_insert(id), id);
        let data = image["base64"]
            .as_str()
            .unwrap()
            .strip_prefix("data:image/jpeg;base64,")
            .unwrap();
        use base64::Engine;
        let jpeg = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        assert!(jpeg.starts_with(&[0xff, 0xd8, 0xff]));
    }
}

#[test]
fn shutdown_stops_system_clock_analytics_timers_while_a_screenshot_hook_is_blocked() {
    use httpmock::prelude::*;
    use std::sync::mpsc;

    struct Closed(mpsc::Sender<()>);
    impl Drop for Closed {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    for closing in [false, true] {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/array/phc_test/config");
            then.status(200)
                .json_body(json!({"hasFeatureFlags": false, "sessionRecording": {}}));
        });
        let (sent_tx, sent_rx) = mpsc::channel();
        server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.respond_with(move |_| {
                let _ = sent_tx.send(());
                http::Response::builder()
                    .status(400)
                    .body(String::new())
                    .unwrap()
                    .into()
            });
        });
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let (closed_tx, closed_rx) = mpsc::channel();
        let closed = Closed(closed_tx);
        let mut config = Config::new("phc_test");
        config.host = server.base_url();
        config.persistence = Persistence::Memory;
        config.preload_feature_flags = false;
        config.flush_interval = Duration::from_millis(250);
        config.request_timeout_seconds = 2;
        config.on_feature_flags(move |_| {
            let _retain_until_closed = &closed;
        });
        config.before_send(move |event| {
            if event.event_name() == "$snapshot" {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            Some(event)
        });
        let client = Arc::new(Client::init_blocking(config));
        client.flush_blocking();
        assert!(matches!(
            client.capture("pending analytics"),
            posthog::stateful::CaptureOutcome::Accepted { .. }
        ));
        let replay = client.session_replay();
        replay.start();
        let mut frame = replay.begin_frame(layout()).unwrap();
        frame.pixels_mut().fill(0);
        assert_eq!(replay.submit_frame(frame), SubmitOutcome::Accepted);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let shutdown = if closing {
            let other = client.clone();
            Some(thread::spawn(move || {
                other.shutdown_blocking(Some(Duration::ZERO))
            }))
        } else {
            None
        };
        let closed_observed = if closing {
            closed_rx.recv_timeout(Duration::from_secs(5)).is_ok()
        } else {
            true
        };
        // Observe the real timer before releasing the encoder or sending a flush.
        let sent = sent_rx.recv_timeout(Duration::from_millis(750)).is_ok();
        release_tx.send(()).unwrap();
        if let Some(shutdown) = shutdown {
            shutdown.join().unwrap();
        } else {
            client.shutdown_blocking(Some(Duration::ZERO));
        }
        assert!(closed_observed);
        assert_eq!(sent, !closing, "closing={}", closing);
    }
}
