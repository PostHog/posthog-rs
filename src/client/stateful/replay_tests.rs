#[path = "replay_delivery_tests.rs"]
mod delivery;

use super::super::{Persistence, SessionMode};
use super::*;
use crate::client::transport::{Clock, TransportHandle};
use httpmock::prelude::*;
use serde_json::{json, Value};
use std::io::Read;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone)]
struct Reply {
    status: u16,
    split: bool,
    retry_after: Option<&'static str>,
    body: Value,
}
struct Backend {
    server: MockServer,
    bodies: Arc<Mutex<Vec<Value>>>,
    uploads: Mutex<mpsc::Receiver<Value>>,
    config: Arc<Mutex<Value>>,
    reply: Arc<Mutex<Reply>>,
}
impl Backend {
    fn new() -> Self {
        let server = MockServer::start();
        let config = Arc::new(Mutex::new(
            json!({"hasFeatureFlags":false,"sessionRecording":{}}),
        ));
        let current = config.clone();
        server.mock(|when, then| {
            when.method(GET).path("/array/phc_test/config");
            then.respond_with(move |_| {
                http::Response::builder()
                    .status(200)
                    .body(current.lock().unwrap().to_string())
                    .unwrap()
                    .into()
            });
        });
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(json!({"featureFlags":{}}));
        });
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = bodies.clone();
        let (uploaded_tx, uploaded_rx) = mpsc::channel();
        let reply = Arc::new(Mutex::new(Reply {
            status: 200,
            split: false,
            retry_after: None,
            body: json!({}),
        }));
        let response = reply.clone();
        server.mock(|when, then| {
            when.method(POST)
                .path("/s/")
                .header("content-encoding", "gzip")
                .header("content-type", "application/json");
            then.respond_with(move |request| {
                let mut bytes = Vec::new();
                flate2::read::GzDecoder::new(request.body_ref())
                    .read_to_end(&mut bytes)
                    .unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                let reply = response.lock().unwrap().clone();
                let status = if reply.split && body.as_array().unwrap().len() > 1 {
                    413
                } else {
                    reply.status
                };
                seen.lock().unwrap().push(body.clone());
                let _ = uploaded_tx.send(body);
                let mut builder = http::Response::builder().status(status);
                if let Some(value) = reply.retry_after {
                    builder = builder.header("retry-after", value);
                }
                builder.body(reply.body.to_string()).unwrap().into()
            });
        });
        Self {
            server,
            bodies,
            uploads: Mutex::new(uploaded_rx),
            config,
            reply,
        }
    }
    fn client(&self, configure: impl FnOnce(&mut Config)) -> Client {
        let mut config = Config::new("phc_test");
        config.host = self.server.base_url();
        config.persistence = Persistence::Memory;
        config.preload_feature_flags = false;
        config.request_timeout_seconds = 2;
        config.shutdown_timeout = Duration::from_secs(2);
        configure(&mut config);
        let client = Client::init_blocking(config);
        client.flush_blocking();
        client
    }
}
fn layout() -> FrameLayout {
    FrameLayout {
        width: 32,
        height: 24,
        row_stride_bytes: 32 * 4 + 16,
        format: PixelFormat::Bgra8888,
    }
}
fn frame(replay: &SessionReplay) -> ReplayFrame {
    let mut frame = replay.begin_frame(layout()).unwrap();
    // Blue image with a black masked right half; row padding is deliberately red.
    for row in frame.pixels.chunks_exact_mut(layout().row_stride_bytes) {
        row.fill(255);
        for (x, pixel) in row[..128].chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(if x < 16 {
                &[255, 0, 0, 7]
            } else {
                &[0, 0, 0, 255]
            });
        }
    }
    frame
}
struct ManualClock(Mutex<Instant>);
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
impl ManualClock {
    fn advance(&self, duration: Duration) {
        *self.0.lock().unwrap() += duration;
    }
}
fn with_clock(backend: &Backend) -> (Client, Arc<ManualClock>) {
    with_clock_config(backend, |_| {})
}
fn with_clock_config(
    backend: &Backend,
    configure: impl FnOnce(&mut Config),
) -> (Client, Arc<ManualClock>) {
    let mut client = backend.client(configure);
    let old = client.transport.take().unwrap();
    old.request_close();
    old.join();
    let clock = Arc::new(ManualClock(Mutex::new(Instant::now())));
    let mut config = Config::new("phc_test");
    config.host = backend.server.base_url();
    config.persistence = Persistence::Memory;
    config.request_timeout_seconds = 2;
    config.shutdown_timeout = Duration::from_secs(2);
    let transport = TransportHandle::spawn_with_clock(config.transport_options(), clock.clone());
    client.replay = Controller::new(
        client.state.as_ref().unwrap(),
        &transport,
        client.producer.as_ref().unwrap(),
        &config,
    );
    client.transport = Some(transport);
    (client, clock)
}

#[test]
fn remote_policy_neutral_controls_and_unsupported_restrictions() {
    for value in [
        json!(true),
        json!({}),
        json!({"version":1,"sampleRate":"1.0","linkedFlag":null,"minimumDurationMilliseconds":0,"eventTriggers":[],"urlTriggers":[],"urlBlocklist":[],"endpoint":"/s/","futureSetting":true}),
    ] {
        assert!(policy::eligible(Some(&value)), "{}", value);
    }
    for value in [
        json!(false),
        json!(null),
        json!({"linkedFlag":"record"}),
        json!({"linkedFlag":""}),
        json!({"sampleRate":"0.5"}),
        json!({"sampleRate":1}),
        json!({"sampleRate":"broken"}),
        json!({"minimumDurationMilliseconds":1}),
        json!({"eventTriggers":["open"]}),
        json!({"urlTriggers":[{"url":".*"}]}),
        json!({"urlBlocklist":[{"url":".*"}]}),
        json!({"version":2,"triggerGroups":[{"sampleRate":"1"}]}),
        json!({"triggerGroups":[{}]}),
        json!({"triggerMatchType":"unknown"}),
        json!({"endpoint":"https://other.example/s/"}),
        json!({"enabled":false}),
    ] {
        assert!(!policy::eligible(Some(&value)), "{}", value);
    }
    assert!(!policy::eligible(None));
}

#[test]
fn manual_permission_session_ownership_remote_updates_and_no_screenshot_activity() {
    let backend = Backend::new();
    let client = backend.client(|_| {});
    let replay = client.session_replay();
    assert!(!replay.is_active());
    assert!(replay.begin_frame(layout()).is_none());
    replay.start();
    let session = client.peek_session_id().unwrap();
    let sessions = client.state.as_ref().unwrap().snapshot().sessions.clone();
    assert!(replay.is_active());
    replay.start();
    assert_eq!(client.peek_session_id(), Some(session));
    let token = frame(&replay);
    assert_eq!(client.state.as_ref().unwrap().snapshot().sessions, sessions);
    backend.config.lock().unwrap()["sessionRecording"] = json!(false);
    client.load_remote_config(false);
    client.flush_blocking();
    assert!(!replay.is_active());
    assert!(replay.begin_frame(layout()).is_none());
    // Remote disable closes acquisition, not previously acquired valid work.
    assert_eq!(replay.submit_frame(token), SubmitOutcome::Accepted);
    replay.stop();
    backend.config.lock().unwrap()["sessionRecording"] = json!({});
    client.load_remote_config(false);
    client.flush_blocking();
    assert!(!replay.is_active());
    replay.start();
    assert!(replay.is_active());
    assert_eq!(client.state.as_ref().unwrap().snapshot().sessions, sessions);
    client.shutdown_blocking(None);
    assert!(!replay.is_active());
    assert!(replay.begin_frame(layout()).is_none());
}

#[test]
fn pool_is_lazy_reuses_grows_and_frees_idle_and_outstanding_buffers_on_stop() {
    let backend = Backend::new();
    let client = backend.client(|_| {});
    let replay = client.session_replay();
    let controller = replay.0.upgrade().unwrap();
    assert!(controller.pool.lock().unwrap().buffers.is_empty());
    replay.start();
    let first = frame(&replay);
    let pointer = first.pixels.as_ptr();
    let capacity = first.pixels.capacity();
    drop(first);
    let smaller = replay
        .begin_frame(FrameLayout {
            height: 12,
            ..layout()
        })
        .unwrap();
    assert_eq!(smaller.pixels.as_ptr(), pointer);
    assert_eq!(smaller.pixels.capacity(), capacity);
    drop(smaller);
    let bigger = replay
        .begin_frame(FrameLayout {
            height: 48,
            ..layout()
        })
        .unwrap();
    assert!(bigger.pixels.capacity() >= 48 * layout().row_stride_bytes);
    let other = frame(&replay);
    assert!(replay.begin_frame(layout()).is_none());
    replay.stop();
    assert!(controller.pool.lock().unwrap().buffers.is_empty());
    drop(bigger);
    drop(other);
    let pool = controller.pool.lock().unwrap();
    assert_eq!(pool.in_flight, 0);
    assert!(pool.buffers.is_empty());
}

#[test]
fn invalid_layouts_do_not_reserve_or_allocate_and_wrong_recorder_releases() {
    let backend = Backend::new();
    let client = backend.client(|_| {});
    let other = backend.client(|_| {});
    let replay = client.session_replay();
    replay.start();
    for bad in [
        FrameLayout {
            width: 0,
            ..layout()
        },
        FrameLayout {
            height: 0,
            ..layout()
        },
        FrameLayout {
            width: u32::MAX,
            ..layout()
        },
        FrameLayout {
            row_stride_bytes: 1,
            ..layout()
        },
        FrameLayout {
            row_stride_bytes: usize::MAX,
            ..layout()
        },
    ] {
        assert!(replay.begin_frame(bad).is_none());
    }
    assert_eq!(
        other.session_replay().submit_frame(frame(&replay)),
        SubmitOutcome::WrongRecorder
    );
    assert_eq!(
        replay.0.upgrade().unwrap().pool.lock().unwrap().in_flight,
        0
    );
}

#[test]
fn identity_session_and_consent_transitions_reject_readbacks_without_time_expiry() {
    let backend = Backend::new();
    let client = backend.client(|c| c.session_mode = SessionMode::Manual);
    let replay = client.session_replay();
    replay.start();
    let session = client.peek_session_id();
    let stale = frame(&replay);
    client.identify("identified");
    assert_eq!(client.peek_session_id(), session);
    assert_eq!(replay.submit_frame(stale), SubmitOutcome::Stale);
    let stale = frame(&replay);
    client.opt_out();
    assert!(replay.begin_frame(layout()).is_none());
    client.opt_in();
    assert_eq!(replay.submit_frame(stale), SubmitOutcome::Stale);
    let stale = frame(&replay);
    client.reset_session();
    assert_eq!(replay.submit_frame(stale), SubmitOutcome::Stale);
    let mut old = frame(&replay);
    old.attribution.timestamp -= chrono::Duration::days(10);
    assert_eq!(replay.submit_frame(old), SubmitOutcome::Accepted);
    client.flush_blocking();
    let stale = frame(&replay);
    client.reset();
    assert_eq!(replay.submit_frame(stale), SubmitOutcome::Stale);
    replay.start();
    let closed = frame(&replay);
    client.shutdown_blocking(None);
    assert_eq!(replay.submit_frame(closed), SubmitOutcome::Closed);
}

#[test]
fn admission_spans_encoded_retries_and_retry_after_survives_flushes_beyond_four_attempts() {
    let backend = Backend::new();
    backend.reply.lock().unwrap().status = 503;
    backend.reply.lock().unwrap().retry_after = Some("2");
    let (client, clock) = with_clock(&backend);
    let replay = client.session_replay();
    replay.start();
    replay.submit_frame(frame(&replay));
    replay.submit_frame(frame(&replay));
    client.flush_blocking();
    assert_eq!(backend.bodies.lock().unwrap().len(), 1);
    assert!(replay.begin_frame(layout()).is_none());
    client.flush_blocking();
    assert_eq!(backend.bodies.lock().unwrap().len(), 1);
    clock.advance(Duration::from_secs(1));
    client.transport.as_ref().unwrap().tick();
    assert_eq!(backend.bodies.lock().unwrap().len(), 1);
    for _ in 0..6 {
        clock.advance(Duration::from_secs(600));
        client.transport.as_ref().unwrap().tick();
        assert!(replay.begin_frame(layout()).is_none());
    }
    let bodies = backend.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 7);
    assert!(bodies.iter().all(|b| b == &bodies[0]));
    drop(bodies);
    backend.reply.lock().unwrap().status = 200;
    clock.advance(Duration::from_secs(600));
    client.transport.as_ref().unwrap().tick();
    assert!(replay.begin_frame(layout()).is_some());
}

#[test]
fn cadence_out_of_order_completions_and_attribution_separated_batches() {
    let backend = Backend::new();
    let (client, clock) = with_clock(&backend);
    let replay = client.session_replay();
    replay.start();
    let first = frame(&replay);
    let first_uuid = first.attribution.uuid.to_string();
    let second = frame(&replay);
    replay.submit_frame(second);
    replay.submit_frame(first);
    client.flush_blocking();
    {
        let bodies = backend.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].as_array().unwrap().len(), 2);
        assert_eq!(bodies[0][0]["uuid"], first_uuid);
    }
    replay.submit_frame(frame(&replay));
    // An encoder FIFO barrier without a delivery flush.
    let (tx, rx) = mpsc::channel();
    replay
        .0
        .upgrade()
        .unwrap()
        .gate
        .control(Control::Tick(Completion::Blocking(tx)))
        .ok()
        .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let count = backend.bodies.lock().unwrap().len();
    clock.advance(Duration::from_millis(999));
    client.transport.as_ref().unwrap().tick();
    assert_eq!(backend.bodies.lock().unwrap().len(), count);
    clock.advance(Duration::from_millis(1));
    client.transport.as_ref().unwrap().tick();
    assert_eq!(backend.bodies.lock().unwrap().len(), count + 1);
    let first = frame(&replay);
    replay.submit_frame(first);
    let (tx, rx) = mpsc::channel();
    replay
        .0
        .upgrade()
        .unwrap()
        .gate
        .control(Control::Tick(Completion::Blocking(tx)))
        .ok()
        .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    client.identify("new-user");
    replay.submit_frame(frame(&replay));
    client.flush_blocking();
    let bodies = backend.bodies.lock().unwrap();
    let last = &bodies[bodies.len() - 2..];
    assert!(last.iter().all(|b| b.as_array().unwrap().len() == 1));
    assert_ne!(last[0][0]["distinct_id"], last[1][0]["distinct_id"]);
}

#[test]
fn split_413_terminal_rejection_and_quota_release_only_affected_work() {
    for (status, split, quota, expected) in [
        (200, true, false, 3),
        (413, false, false, 3),
        (400, false, false, 1),
        (200, false, true, 1),
    ] {
        let backend = Backend::new();
        {
            let mut reply = backend.reply.lock().unwrap();
            reply.status = status;
            reply.split = split;
            if quota {
                reply.body = json!({"quota_limited":["recordings"]});
            }
        }
        let client = backend.client(|_| {});
        let replay = client.session_replay();
        replay.start();
        replay.submit_frame(frame(&replay));
        replay.submit_frame(frame(&replay));
        client.flush_blocking();
        assert_eq!(backend.bodies.lock().unwrap().len(), expected);
        assert!(replay.begin_frame(layout()).is_some());
    }
}

#[test]
fn encoding_and_hook_are_off_caller_flush_waits_and_stop_allows_late_frames() {
    let backend = Backend::new();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let client = Arc::new(backend.client(|config| {
        config.before_send(move |event| {
            if event.event_name() == "$snapshot" {
                entered_tx.send(std::thread::current().id()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            Some(event)
        });
    }));
    let replay = client.session_replay();
    replay.start();
    replay.submit_frame(frame(&replay));
    let thread = entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let late = frame(&replay);
    replay.stop();
    let waiting = client.clone();
    let (flushed_tx, flushed_rx) = mpsc::channel();
    let flush = std::thread::spawn(move || {
        let result = waiting.flush_blocking();
        flushed_tx.send(result).unwrap();
    });
    let pending = flushed_rx.try_recv().is_err();
    release_tx.send(()).unwrap();
    assert!(pending);
    assert_ne!(thread, std::thread::current().id());
    flushed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    flush.join().unwrap();
    assert_eq!(replay.submit_frame(late), SubmitOutcome::Accepted);
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    release_tx.send(()).unwrap();
    client.flush_blocking();
    assert_eq!(backend.bodies.lock().unwrap().len(), 2);
    assert!(!replay.is_active());
}

#[test]
fn hooks_redact_drop_fail_closed_and_cannot_reattribute_frames() {
    for mode in 0..13 {
        let backend = Backend::new();
        let client = backend.client(|config| {
            config.before_send(move |mut event| {
                if event.event_name() != "$snapshot" {
                    return Some(event);
                }
                match mode {
                    0 => {
                        event.insert_prop("redacted", true).unwrap();
                        Some(event)
                    }
                    1 => None,
                    2 => panic!("synthetic hook failure"),
                    mode => {
                        match mode {
                            3 => {
                                event.insert_prop("$session_id", "wrong-session").unwrap();
                            }
                            4 | 5 => {
                                event = crate::Event::from_parts(
                                    if mode == 4 {
                                        "renamed"
                                    } else {
                                        event.event_name()
                                    }
                                    .to_owned(),
                                    if mode == 5 {
                                        "other-person"
                                    } else {
                                        event.distinct_id()
                                    }
                                    .to_owned(),
                                    event.properties().clone(),
                                    event.options().clone(),
                                    event.groups().clone(),
                                    event.timestamp(),
                                    event.uuid(),
                                );
                            }
                            6 => event.set_uuid(Uuid::now_v7()),
                            7 => {
                                event
                                    .set_timestamp(Utc::now() + chrono::Duration::seconds(1))
                                    .unwrap();
                            }
                            8 => {
                                event.insert_prop("$window_id", "wrong-window").unwrap();
                            }
                            9 => {
                                event.remove_prop("$snapshot_data");
                            }
                            10 => {
                                event.insert_prop("$snapshot_data", json!([])).unwrap();
                            }
                            11 => {
                                event.insert_prop("$snapshot_data", json!({})).unwrap();
                            }
                            12 => {
                                event.insert_prop("$snapshot_data", Value::Null).unwrap();
                            }
                            _ => unreachable!(),
                        }
                        Some(event)
                    }
                }
            });
        });
        let replay = client.session_replay();
        replay.start();
        replay.submit_frame(frame(&replay));
        client.flush_blocking();
        assert_eq!(backend.bodies.lock().unwrap().len(), usize::from(mode == 0));
        if mode == 0 {
            assert_eq!(
                backend.bodies.lock().unwrap()[0][0]["properties"]["redacted"],
                true
            );
        }
        assert!(replay.begin_frame(layout()).is_some());
    }
}

#[test]
fn encoder_callbacks_reject_recursive_capture_and_defer_flush_and_shutdown() {
    let backend = Backend::new();
    let slot = Arc::new(Mutex::new(std::sync::Weak::<Client>::new()));
    let callback_slot = slot.clone();
    let (tx, rx) = mpsc::channel();
    let client = Arc::new(backend.client(|config| {
        config.before_send(move |event| {
            if event.event_name() == "$snapshot" {
                let client = callback_slot.lock().unwrap().upgrade().unwrap();
                let nested = client.capture("recursive");
                let no_frame = client.session_replay().begin_frame(layout()).is_none();
                let flush = client.flush_blocking();
                let shutdown = client.shutdown_blocking(None);
                tx.send((nested, no_frame, flush, shutdown)).unwrap();
            }
            Some(event)
        });
    }));
    *slot.lock().unwrap() = Arc::downgrade(&client);
    let replay = client.session_replay();
    replay.start();
    replay.submit_frame(frame(&replay));
    let (nested, no_frame, flush, shutdown) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(nested, super::super::CaptureOutcome::Reentrant);
    assert!(no_frame);
    assert_eq!(flush.delivery, super::super::DeliveryOutcome::Deferred);
    assert_eq!(shutdown.delivery, super::super::DeliveryOutcome::Deferred);
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        super::super::DeliveryOutcome::Stopped
    );
    assert_eq!(backend.bodies.lock().unwrap().len(), 1);
}

#[test]
fn admitted_frames_keep_attribution_after_consent_changes_and_session_boundaries_split() {
    let backend = Backend::new();
    let client = backend.client(|_| {});
    let replay = client.session_replay();
    replay.start();
    let old = frame(&replay);
    let old_id = old.attribution.distinct_id.clone();
    let old_session = old.attribution.session_id.to_string();
    replay.submit_frame(old);
    // Wait for encoding, but not the one-frame batching deadline.
    let (tx, rx) = mpsc::channel();
    replay
        .0
        .upgrade()
        .unwrap()
        .gate
        .control(Control::Tick(Completion::Blocking(tx)))
        .ok()
        .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    client.opt_out();
    client.opt_in();
    client.reset_session();
    replay.submit_frame(frame(&replay));
    client.flush_blocking();
    let bodies = backend.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(bodies
        .iter()
        .all(|body| body.as_array().unwrap().len() == 1));
    assert_eq!(bodies[0][0]["distinct_id"], old_id);
    assert_eq!(bodies[0][0]["properties"]["$session_id"], old_session);
    assert_ne!(bodies[1][0]["properties"]["$session_id"], old_session);
}

#[test]
fn encoding_errors_and_panics_release_capacity_without_stopping_the_worker() {
    struct Broken(bool);
    impl encoder::ImageEncoder for Broken {
        fn encode(&self, _: FrameLayout, _: &[u8], _: u8) -> Result<Vec<u8>, &'static str> {
            if self.0 {
                panic!("synthetic codec panic");
            }
            Err("synthetic codec failure")
        }
    }
    for panic in [false, true] {
        let backend = Backend::new();
        let client = backend.client(|_| {});
        let replay = client.session_replay();
        replay.start();
        let (tx, rx) = mpsc::channel();
        let worker = encoder::Worker::spawn_with_encoder(
            client.producer.as_ref().unwrap().clone(),
            &ReplayConfig::default(),
            "phc_test".into(),
            tx,
            Duration::from_secs(2),
            Box::new(Broken(panic)),
        )
        .unwrap();
        for _ in 0..2 {
            assert!(worker.gate.frame(frame(&replay), false));
            assert!(matches!(
                rx.recv_timeout(Duration::from_secs(5)).unwrap(),
                Control::ReplayFailure(_)
            ));
            assert_eq!(
                replay.0.upgrade().unwrap().pool.lock().unwrap().in_flight,
                0
            );
        }
        let (tx, _) = mpsc::channel();
        worker
            .gate
            .control(Control::Shutdown {
                completion: Completion::Blocking(tx),
                deadline: Some(Instant::now()),
            })
            .ok()
            .unwrap();
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Control::Shutdown { .. }
        ));
        worker.join();
    }
}

#[test]
fn serialization_limits_split_large_pairs_and_reject_oversized_hook_output() {
    let backend = Backend::new();
    let client = backend.client(|_| {});
    let replay = client.session_replay();
    replay.start();
    for _ in 0..2 {
        let token = frame(&replay);
        let bytes = serde_json::to_vec(
            &json!({"padding":"p".repeat(crate::client::transport::replay::MAX_BODY_BYTES / 2)}),
        )
        .unwrap();
        let encoded = token.finish(bytes);
        client
            .transport
            .as_ref()
            .unwrap()
            .send_control(Control::ReplayFrame(Box::new(encoded)));
    }
    client.flush_blocking();
    assert_eq!(backend.bodies.lock().unwrap().len(), 2);
    drop(client);
    let backend = Backend::new();
    let client = backend.client(|config| {
        config.before_send(|mut event| {
            if event.event_name() == "$snapshot" {
                event
                    .insert_prop(
                        "expanded",
                        "p".repeat(crate::client::transport::replay::MAX_BODY_BYTES),
                    )
                    .unwrap();
            }
            Some(event)
        });
    });
    let replay = client.session_replay();
    replay.start();
    replay.submit_frame(frame(&replay));
    client.flush_blocking();
    assert!(backend.bodies.lock().unwrap().is_empty());
    assert!(replay.begin_frame(layout()).is_some());
}

#[test]
fn raw_high_water_capacity_is_retained_only_until_stop() {
    let backend = Backend::new();
    let client = backend.client(|_| {});
    let replay = client.session_replay();
    replay.start();
    for _ in 0..6 {
        replay.submit_frame(frame(&replay));
        replay.submit_frame(frame(&replay));
        client.flush_blocking();
    }
    let controller = replay.0.upgrade().unwrap();
    let retained: usize = controller
        .pool
        .lock()
        .unwrap()
        .buffers
        .iter()
        .map(Vec::capacity)
        .sum();
    assert_eq!(
        retained,
        layout().row_stride_bytes * layout().height as usize * 2
    );
    assert_eq!(controller.pool.lock().unwrap().in_flight, 0);
    replay.stop();
    assert_eq!(
        controller
            .pool
            .lock()
            .unwrap()
            .buffers
            .iter()
            .map(Vec::capacity)
            .sum::<usize>(),
        0
    );
}

#[test]
fn shutdown_budget_covers_waiting_images_and_encoding_that_finishes_late() {
    let backend = Backend::new();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let mut count = 0;
    let (client, clock) = with_clock_config(&backend, |config| {
        config.before_send(move |event| {
            if event.event_name() == "$snapshot" {
                count += 1;
                if count == 2 {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                }
            }
            Some(event)
        });
    });
    let client = Arc::new(client);
    let replay = client.session_replay();
    replay.start();
    replay.submit_frame(frame(&replay));
    let (tx, rx) = mpsc::channel();
    replay
        .0
        .upgrade()
        .unwrap()
        .gate
        .control(Control::Tick(Completion::Blocking(tx)))
        .ok()
        .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    replay.submit_frame(frame(&replay));
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // Drive the delivery deadline on its injected clock. Client shutdown uses
    // the system clock, independently covered by the public timer regression.
    client
        .transport
        .as_ref()
        .unwrap()
        .send_control(Control::DeliveryClosing(clock.now()));
    client.transport.as_ref().unwrap().tick();
    let closing = client.clone();
    let shutdown = std::thread::spawn(move || closing.shutdown_blocking(Some(Duration::ZERO)));
    let gate = replay.0.upgrade().unwrap().gate.clone();
    let limit = Instant::now() + Duration::from_secs(5);
    while !gate.is_closed() && Instant::now() < limit {
        std::thread::yield_now();
    }
    let closed = gate.is_closed();
    clock.advance(Duration::from_secs(2));
    client.transport.as_ref().unwrap().tick();
    let requests = backend.bodies.lock().unwrap().len();
    release_tx.send(()).unwrap();
    shutdown.join().unwrap();
    assert!(closed);
    assert_eq!(requests, 0);
    assert!(backend.bodies.lock().unwrap().is_empty());
}

#[test]
fn replay_handles_do_not_keep_client_operational_or_namespace_owned() {
    let backend = Backend::new();
    let client = backend.client(|_| {});
    let replay = client.session_replay();
    replay.start();
    let held = frame(&replay);
    drop(client);
    assert!(!replay.is_active());
    assert_eq!(replay.submit_frame(held), SubmitOutcome::Closed);
}
