use super::*;
use std::sync::{Arc, Mutex};

fn time() -> SessionTime {
    SessionTime {
        wall_ms: 1_700_000_000_000,
    }
}
fn later(now: SessionTime, elapsed: Duration) -> SessionTime {
    SessionTime {
        wall_ms: now.wall_ms + elapsed.as_millis() as u64,
    }
}
fn active(now: SessionTime) -> Sessions {
    let mut sessions = Sessions::default();
    sessions.apply(Operation::Activity, now, false);
    sessions
}

#[test]
fn exact_idle_boundary_and_read_only_inspection() {
    for elapsed in [
        IDLE - Duration::from_millis(1),
        IDLE,
        IDLE + Duration::from_millis(1),
    ] {
        let now = time();
        let mut sessions = active(now);
        let original = sessions.live.as_ref().unwrap().id;
        let before = sessions.clone();
        assert_eq!(
            sessions.active_id(later(now, elapsed)).is_some(),
            elapsed < IDLE
        );
        assert_eq!(sessions, before);
        sessions.apply(Operation::Maintain, later(now, elapsed), false);
        assert_eq!(
            sessions.live.as_ref().unwrap().id == original,
            elapsed < IDLE
        );
    }
}

#[test]
fn getters_do_not_extend_activity_and_continuous_activity_reaches_maximum() {
    let now = time();
    let mut sessions = active(now);
    let id = sessions.active_id(now);
    sessions.apply(Operation::Maintain, later(now, IDLE / 2), false);
    sessions.apply(Operation::Maintain, later(now, IDLE), false);
    assert_ne!(sessions.active_id(later(now, IDLE)), id);
    for boundary in [
        MAX_AGE - Duration::from_millis(1),
        MAX_AGE,
        MAX_AGE + Duration::from_millis(1),
    ] {
        let mut sessions = active(now);
        let original = sessions.active_id(now);
        for minute in 1..(24 * 60) {
            sessions.apply(
                Operation::Activity,
                later(now, Duration::from_secs(minute * 60)),
                false,
            );
        }
        sessions.apply(Operation::Activity, later(now, boundary), false);
        assert_eq!(
            sessions.active_id(later(now, boundary)) == original,
            boundary < MAX_AGE
        );
    }
}

#[test]
fn background_and_duplicate_lifecycle_signals_do_not_revive_sessions() {
    let now = time();
    let mut sessions = active(now);
    sessions.apply(Operation::Background(true), later(now, IDLE / 2), false);
    let stored = sessions.clone();
    sessions.apply(Operation::Background(true), later(now, IDLE), false);
    assert_eq!(sessions, stored);
    sessions.apply(Operation::Activity, later(now, IDLE * 2), false);
    assert!(sessions.live.is_none());
    sessions.apply(Operation::Maintain, later(now, IDLE * 2), false);
    assert!(sessions.live.is_none());
    sessions.apply(Operation::Background(false), later(now, IDLE * 2), false);
    assert!(sessions.live.is_some());
}

#[test]
fn manual_boundaries_are_explicit_and_start_is_idempotent() {
    let now = time();
    let mut sessions = Sessions {
        mode: SessionMode::Manual,
        ..Sessions::default()
    };
    sessions.apply(Operation::Activity, now, false);
    sessions.apply(Operation::Maintain, now, false);
    assert!(sessions.live.is_none());
    sessions.apply(Operation::Start, now, false);
    let original = sessions.clone();
    sessions.apply(Operation::Start, later(now, MAX_AGE * 3), false);
    assert_eq!(sessions, original);
    assert_eq!(
        sessions.active_id(later(now, MAX_AGE * 3)),
        original.active_id(now)
    );
    sessions.apply(Operation::Reset, now, false);
    assert_ne!(sessions.active_id(now), original.active_id(now));
    sessions.apply(Operation::End, now, false);
    assert!(sessions.live.is_none());
}

#[test]
fn opted_out_operations_preserve_session_but_end_and_environment_work() {
    let now = time();
    let mut sessions = active(now);
    let original = sessions.clone();
    for operation in [
        Operation::Start,
        Operation::Reset,
        Operation::Activity,
        Operation::Maintain,
    ] {
        sessions.apply(operation, later(now, MAX_AGE), true);
        assert_eq!(sessions, original);
    }
    sessions.apply(Operation::Background(true), later(now, IDLE), true);
    assert!(sessions.background);
    assert_eq!(sessions.live, original.live);
    sessions.apply(Operation::End, now, true);
    assert!(sessions.live.is_none());
}

#[test]
fn backward_clock_changes_do_not_expire_or_rewind_and_restart_rejects_future_timing() {
    let now = time();
    let mut sessions = active(now);
    let original = sessions.active_id(now);
    sessions.apply(Operation::Activity, later(now, IDLE / 2), false);
    let rewound = SessionTime { wall_ms: 0 };
    sessions.apply(Operation::Activity, rewound, false);
    assert_eq!(sessions.active_id(rewound), original);
    let live = sessions.live.as_ref().unwrap();
    assert_eq!(
        live.activity_ms,
        now.wall_ms + (IDLE / 2).as_millis() as u64
    );
    assert!(live.is_valid());

    let stored = active(now).live.unwrap();
    let rollback = SessionTime {
        wall_ms: now.wall_ms - 1,
    };
    assert!(Sessions::restored(
        Some(&stored),
        SessionMode::Automatic,
        SessionPersistence::Persistent,
        rollback
    )
    .live
    .is_none());
}

#[test]
fn restored_expiry_uses_wall_age_including_maximum() {
    let now = time();
    let stored = active(now).live.unwrap();
    for age in [
        IDLE - Duration::from_millis(1),
        IDLE,
        IDLE + Duration::from_millis(1),
    ] {
        let restart = SessionTime {
            wall_ms: now.wall_ms + age.as_millis() as u64,
        };
        let sessions = Sessions::restored(
            Some(&stored),
            SessionMode::Automatic,
            SessionPersistence::Persistent,
            restart,
        );
        assert_eq!(sessions.live.is_some(), age < IDLE);
        if age < IDLE {
            assert!(sessions.active_id(later(now, IDLE)).is_none());
        }
    }
    let stored = StoredSession {
        activity_ms: now.wall_ms + MAX_AGE.as_millis() as u64 - 1,
        ..stored
    };
    for age in [
        MAX_AGE - Duration::from_millis(1),
        MAX_AGE,
        MAX_AGE + Duration::from_millis(1),
    ] {
        let restart = SessionTime {
            wall_ms: now.wall_ms + age.as_millis() as u64,
        };
        let sessions = Sessions::restored(
            Some(&stored),
            SessionMode::Automatic,
            SessionPersistence::Persistent,
            restart,
        );
        assert_eq!(sessions.live.is_some(), age < MAX_AGE);
    }
    assert!(Sessions::restored(
        Some(&stored),
        SessionMode::Manual,
        SessionPersistence::Memory,
        later(now, MAX_AGE)
    )
    .live
    .is_none());
    assert!(Sessions::restored(
        Some(&stored),
        SessionMode::Manual,
        SessionPersistence::Persistent,
        later(now, MAX_AGE * 2)
    )
    .live
    .is_some());
}

#[test]
fn memory_activity_keeps_persistence_revision_while_persistent_boundaries_advance_it() {
    for persistence in [SessionPersistence::Memory, SessionPersistence::Persistent] {
        let mut config = super::super::Config::new("phc_test");
        config.preload_feature_flags = false;
        config.persistence = super::super::Persistence::Memory;
        config.session_persistence = persistence;
        let client = Client::init_blocking(config);
        let before = client.state.as_ref().unwrap().checkpoint();
        client.session_activity();
        client.reset_session();
        let after = client.state.as_ref().unwrap().checkpoint();
        assert_eq!(after == before, persistence == SessionPersistence::Memory);
        assert!(client.peek_session_id().is_some());
    }
}

#[test]
fn capture_session_snapshot_survives_max_boundary_between_clock_reads() {
    use std::collections::VecDeque;
    struct SequenceClock(Mutex<VecDeque<SessionTime>>);
    impl SessionClock for SequenceClock {
        fn now(&self) -> SessionTime {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected clock read")
        }
    }
    let now = time();
    let mut config = super::super::Config::new("phc_test");
    config.preload_feature_flags = false;
    config.persistence = super::super::Persistence::Memory;
    config.host = "http://127.0.0.1:1".into();
    let seen = Arc::new(Mutex::new(None));
    let hook_seen = seen.clone();
    config.before_send(move |event| {
        *hook_seen.lock().unwrap() = Some(event);
        None
    });
    let mut client = Client::init_blocking(config);
    client
        .state
        .as_ref()
        .unwrap()
        .mutate(|snapshot| {
            snapshot.sessions = active(now);
            for minute in 1..(24 * 60) {
                snapshot.sessions.apply(
                    Operation::Activity,
                    later(now, Duration::from_secs(minute * 60)),
                    false,
                );
            }
        })
        .unwrap();
    let original = client.peek_session_id().unwrap();
    let clock = Arc::new(SequenceClock(Mutex::new(VecDeque::from([
        later(now, MAX_AGE - Duration::from_millis(1)),
        later(now, MAX_AGE),
    ]))));
    client.producer.as_mut().unwrap().session_clock = clock.clone();
    assert_eq!(
        client.capture("boundary"),
        super::super::CaptureOutcome::Dropped
    );
    assert_eq!(client.peek_session_id(), Some(original));
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.as_ref().unwrap().properties().get("$session_id"),
        Some(&serde_json::json!(original.to_string()))
    );
    assert_eq!(
        clock.0.lock().unwrap().len(),
        1,
        "preparation must not read the session clock again"
    );
}

struct TestClock(Mutex<SessionTime>);
impl SessionClock for TestClock {
    fn now(&self) -> SessionTime {
        *self.0.lock().unwrap()
    }
}

#[test]
fn admitted_passive_helpers_create_and_rotate_without_extending_idle() {
    let now = time();
    let clock = Arc::new(TestClock(Mutex::new(now)));
    let mut config = super::super::Config::new("phc_test");
    config.preload_feature_flags = false;
    config.persistence = super::super::Persistence::Memory;
    config.host = "http://127.0.0.1:1".into();
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    config.before_send(move |event| {
        seen.lock().unwrap().push(event.clone());
        Some(event)
    });
    let mut client = Client::init_blocking(config);
    client.producer.as_mut().unwrap().session_clock = clock.clone();
    client.group(("company", "a"));
    let original = client.peek_session_id().unwrap();
    client.capture("first");
    *clock.0.lock().unwrap() = later(now, IDLE / 2);
    client.group(("company", "a"));
    assert_eq!(client.get_session_id(), Some(original));
    *clock.0.lock().unwrap() = later(now, IDLE);
    client.group(("company", "a"));
    let next = client.peek_session_id().unwrap();
    assert_ne!(next, original);
    client.capture("next");
    let seen = events.lock().unwrap();
    assert_eq!(seen[0].properties()["$session_id"], original.to_string());
    assert_eq!(seen[1].properties()["$session_id"], original.to_string());
    assert_eq!(seen[2].properties()["$session_id"], original.to_string());
    assert_eq!(seen[3].properties()["$session_id"], next.to_string());
    assert_eq!(seen[4].properties()["$session_id"], next.to_string());
}

#[test]
fn wall_time_rotates_at_idle_and_max_age_in_memory_and_persistent_modes() {
    for persistence in [SessionPersistence::Memory, SessionPersistence::Persistent] {
        for limit in [IDLE, MAX_AGE] {
            for boundary in [
                limit - Duration::from_millis(1),
                limit,
                limit + Duration::from_millis(1),
            ] {
                let now = time();
                let clock = Arc::new(TestClock(Mutex::new(now)));
                let mut config = super::super::Config::new("phc_test");
                config.persistence = super::super::Persistence::Memory;
                config.session_persistence = persistence;
                let mut client = Client::init_blocking(config);
                client.producer.as_mut().unwrap().session_clock = clock.clone();
                let original = client.get_session_id();
                if limit == MAX_AGE {
                    // Keep idle below 30 minutes until the final interval.
                    for minute in 1..(24 * 60) {
                        *clock.0.lock().unwrap() = later(now, Duration::from_secs(minute * 60));
                        client.session_activity();
                    }
                }
                *clock.0.lock().unwrap() = later(now, boundary);
                assert_eq!(client.get_session_id() == original, boundary < limit);
                let snapshot = client.state.as_ref().unwrap().snapshot();
                assert_eq!(
                    snapshot.session.is_some(),
                    persistence == SessionPersistence::Persistent
                );
                if let Some(stored) = &snapshot.session {
                    assert_eq!(Some(stored.id), client.peek_session_id());
                }
            }
        }
    }
}

#[test]
fn activity_updates_persisted_timing_and_restored_expiry() {
    let now = time();
    let mut sessions = Sessions {
        persistence: SessionPersistence::Persistent,
        ..active(now)
    };
    let original = sessions.active_id(now);
    let resumed = later(now, IDLE / 2);
    sessions.apply(Operation::Activity, resumed, false);
    assert_eq!(sessions.active_id(resumed), original);
    let stored = sessions.stored().unwrap();
    assert_eq!(stored.started_ms, now.wall_ms);
    assert_eq!(
        stored.activity_ms,
        now.wall_ms + (IDLE / 2).as_millis() as u64
    );

    let restart = later(now, IDLE);
    let mut restored = Sessions::restored(
        Some(&stored),
        SessionMode::Automatic,
        SessionPersistence::Persistent,
        restart,
    );
    assert_eq!(restored.active_id(restart), original);
    let expired = later(restart, IDLE / 2);
    restored.apply(Operation::Maintain, expired, false);
    assert_ne!(restored.active_id(expired), original);
}

#[test]
fn system_session_clock_samples_wall_time() {
    let wall_ms = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    };
    let before = wall_ms();
    let now = SystemSessionClock.now();
    let after = wall_ms();
    assert!((before..=after).contains(&now.wall_ms));
}

#[test]
fn dropped_capture_after_idle_previews_without_committing_in_both_modes() {
    for persistence in [SessionPersistence::Memory, SessionPersistence::Persistent] {
        let now = time();
        let clock = Arc::new(TestClock(Mutex::new(now)));
        let seen = Arc::new(Mutex::new(None));
        let hook_seen = seen.clone();
        let mut config = super::super::Config::new("phc_test");
        config.persistence = super::super::Persistence::Memory;
        config.session_persistence = persistence;
        config.before_send(move |event| {
            *hook_seen.lock().unwrap() = Some(event);
            None
        });
        let mut client = Client::init_blocking(config);
        client.producer.as_mut().unwrap().session_clock = clock.clone();
        let original = client.get_session_id();
        *clock.0.lock().unwrap() = later(now, IDLE);
        assert_eq!(
            client.capture("resumed"),
            super::super::CaptureOutcome::Dropped
        );
        assert_eq!(client.peek_session_id(), original);
        assert_ne!(
            seen.lock().unwrap().as_ref().unwrap().properties()["$session_id"],
            original.unwrap().to_string()
        );
    }
}

#[test]
fn provisional_attribution_is_shared_bounded_and_committed_only_in_current_epoch() {
    let now = time();
    let mut sessions = Sessions::default();
    let a = sessions.prepare_admission(now);
    let b = sessions.prepare_admission(now);
    assert_eq!(a.id, b.id);
    assert!(sessions.live.is_none());
    assert!(sessions.stored().is_none());
    sessions.commit_admission(b, later(now, IDLE / 2), true);
    sessions.commit_admission(a, now, true);
    assert_eq!(sessions.active_id(later(now, IDLE)), a.id);
    assert_eq!(
        sessions.live.as_ref().unwrap().activity_ms,
        later(now, IDLE / 2).wall_ms
    );
    sessions.apply(Operation::End, now, false);
    sessions.commit_admission(a, later(now, IDLE), true);
    assert!(sessions.live.is_none());
    assert!(sessions.provisional.is_none());
    let c = sessions.prepare_admission(later(now, IDLE));
    assert_ne!(c.id, a.id);
    sessions.apply(Operation::Background(true), later(now, IDLE), false);
    sessions.commit_admission(c, later(now, IDLE), true);
    assert!(sessions.live.is_none());
}

#[test]
fn passive_admission_never_touches_live_idle_and_rejected_previews_expire() {
    let now = time();
    let mut sessions = Sessions::default();
    let rejected = sessions.prepare_admission(now);
    let first = sessions.prepare_admission(later(now, IDLE));
    assert_ne!(first.id, rejected.id);
    sessions.commit_admission(first, later(now, IDLE), false);
    let passive = sessions.prepare_admission(later(now, IDLE + IDLE / 2));
    sessions.commit_admission(passive, later(now, IDLE + IDLE / 2), false);
    assert_eq!(sessions.active_id(later(now, IDLE * 2)), None);
}

#[test]
fn helper_events_from_before_a_reset_do_not_join_the_next_session() {
    let mut config = super::super::Config::new("phc_test");
    config.persistence = super::super::Persistence::Memory;
    config.host = "http://127.0.0.1:1".into();
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    config.before_send(move |event| {
        seen.lock().unwrap().push(event.clone());
        None
    });
    let client = Client::init_blocking(config);
    let before_reset = client.state.as_ref().unwrap().snapshot();
    client.reset();
    let next_session = client.get_session_id();
    assert!(next_session.is_some());
    client.capture_snapshot(super::super::Capture::new("old-user"), &before_reset);
    let seen = events.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(!seen[0].properties().contains_key("$session_id"));
}

#[test]
fn session_calls_between_prepare_and_commit_keep_the_events_session() {
    for operation in [
        || Operation::Maintain,
        || Operation::Activity,
        || Operation::Start,
    ] {
        let now = time();
        // First event after idle.
        let mut sessions = active(now);
        let resumed = later(now, IDLE);
        let event = sessions.prepare_admission(resumed);
        sessions.apply(operation(), resumed, false);
        sessions.commit_admission(event, resumed, true);
        assert_eq!(sessions.active_id(resumed), event.id);
        // First event of the process.
        let mut sessions = Sessions::default();
        let event = sessions.prepare_admission(now);
        sessions.apply(operation(), now, false);
        sessions.commit_admission(event, now, true);
        assert_eq!(sessions.active_id(now), event.id);
    }
}

#[test]
fn expired_provisional_session_is_not_adopted() {
    let now = time();
    let mut sessions = Sessions::default();
    let stale = sessions.prepare_admission(now);
    let resumed = later(now, IDLE);
    sessions.apply(Operation::Maintain, resumed, false);
    assert!(sessions.active_id(resumed).is_some());
    assert_ne!(sessions.active_id(resumed), stale.id);
}

#[test]
fn activity_touches_an_adopted_session() {
    let now = time();
    let mut sessions = Sessions::default();
    let event = sessions.prepare_admission(now);
    let touched = later(now, IDLE / 2);
    sessions.apply(Operation::Activity, touched, false);
    assert_eq!(sessions.active_id(touched), event.id);
    assert_eq!(sessions.live.as_ref().unwrap().activity_ms, touched.wall_ms);
}

#[test]
fn adopting_a_dropped_captures_session_records_the_call_time() {
    for operation in [|| Operation::Maintain, || Operation::Start] {
        let now = time();
        let mut sessions = Sessions::default();
        let dropped = sessions.prepare_admission(now);
        let call = later(now, IDLE - Duration::from_secs(60));
        sessions.apply(operation(), call, false);
        assert_eq!(sessions.active_id(call), dropped.id);
        assert_eq!(sessions.live.as_ref().unwrap().activity_ms, call.wall_ms);
        // The adopted session stays valid for a full idle period from the call.
        let next = later(call, IDLE / 3);
        assert_eq!(sessions.active_id(next), dropped.id);
    }
}

fn filtering_client(clock: Arc<TestClock>) -> (Client, Arc<Mutex<Vec<crate::Event>>>) {
    let mut config = super::super::Config::new("phc_test");
    config.persistence = super::super::Persistence::Memory;
    config.host = "http://127.0.0.1:1".into();
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    config.before_send(move |event| {
        if event.event_name() != "filtered" {
            seen.lock().unwrap().push(event.clone());
        }
        None
    });
    let mut client = Client::init_blocking(config);
    client.producer.as_mut().unwrap().session_clock = clock;
    (client, events)
}

#[test]
fn getter_id_stays_stable_within_idle_after_a_filtered_first_capture() {
    let now = time();
    let clock = Arc::new(TestClock(Mutex::new(now)));
    let (client, _) = filtering_client(clock.clone());
    assert_eq!(
        client.capture("filtered"),
        super::super::CaptureOutcome::Dropped
    );
    *clock.0.lock().unwrap() = later(now, Duration::from_secs(25 * 60));
    let first = client.get_session_id();
    assert!(first.is_some());
    *clock.0.lock().unwrap() = later(now, Duration::from_secs(35 * 60));
    assert_eq!(client.get_session_id(), first);
}

#[test]
fn started_session_is_used_by_the_next_capture_after_a_filtered_first_capture() {
    let now = time();
    let clock = Arc::new(TestClock(Mutex::new(now)));
    let (client, events) = filtering_client(clock.clone());
    client.capture("filtered");
    *clock.0.lock().unwrap() = later(now, Duration::from_secs(29 * 60));
    client.start_session();
    let started = client.peek_session_id().unwrap();
    *clock.0.lock().unwrap() = later(now, Duration::from_secs(30 * 60));
    client.capture("kept");
    let seen = events.lock().unwrap();
    assert_eq!(seen[0].properties()["$session_id"], started.to_string());
}

// Screenshot acquisition after a session expires or ends.
#[cfg(feature = "session-replay")]
mod replay_sessions {
    use super::super::super::{
        Config, FrameLayout, FrameUnavailable, Persistence, PixelFormat, SessionMode,
    };
    use super::*;
    use httpmock::prelude::*;
    use std::time::Instant;

    fn recording_client(mode: SessionMode, opt_out: bool) -> (MockServer, Client) {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/array/phc_test/config");
            then.status(200)
                .json_body(serde_json::json!({"hasFeatureFlags":false,"sessionRecording":true}));
        });
        server.mock(|when, then| {
            when.method(POST);
            then.status(200).json_body(serde_json::json!({}));
        });
        let mut config = Config::new("phc_test");
        config.host = server.base_url();
        config.persistence = Persistence::Memory;
        config.preload_feature_flags = false;
        config.session_mode = mode;
        config.opt_out = opt_out;
        config.session_replay.throttle_delay = Duration::ZERO;
        let client = Client::init_blocking(config);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = client.state.as_ref().unwrap().snapshot();
            if !snapshot.remote_config_loading && snapshot.remote_config.is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "remote config did not load");
            std::thread::yield_now();
        }
        (server, client)
    }

    fn layout() -> FrameLayout {
        FrameLayout::new(4, 4, 16, PixelFormat::Bgra8888)
    }

    /// Stand-in for 31 minutes without captures or `session_activity()`.
    fn idle_31_minutes(client: &Client) {
        client
            .state
            .as_ref()
            .unwrap()
            .mutate(|snapshot| {
                let live = snapshot.sessions.live.as_mut().unwrap();
                live.started_ms -= 31 * 60 * 1000;
                live.activity_ms -= 31 * 60 * 1000;
            })
            .unwrap();
    }

    #[test]
    fn foreground_idle_expiry_rotates_the_session_for_the_next_frame() {
        let (_server, client) = recording_client(SessionMode::Automatic, false);
        let replay = client.session_replay();
        replay.start();
        let first = client.peek_session_id().unwrap();
        drop(replay.begin_frame(layout()).unwrap());
        idle_31_minutes(&client);
        assert!(replay.is_active());
        let frame = replay.begin_frame(layout()).unwrap();
        let second = client.peek_session_id().unwrap();
        assert_ne!(first, second);
        // The new session is not extended by later frames.
        let started = client
            .state
            .as_ref()
            .unwrap()
            .snapshot()
            .sessions
            .live
            .clone()
            .unwrap();
        drop(frame);
        drop(replay.begin_frame(layout()).unwrap());
        assert_eq!(
            client
                .state
                .as_ref()
                .unwrap()
                .snapshot()
                .sessions
                .live
                .clone()
                .unwrap(),
            started
        );
        client.shutdown_blocking(Some(Duration::ZERO));
    }

    #[test]
    fn reset_in_automatic_mode_starts_a_session_for_the_next_frame() {
        let (_server, client) = recording_client(SessionMode::Automatic, false);
        let replay = client.session_replay();
        replay.start();
        let first = client.peek_session_id().unwrap();
        client.reset();
        assert!(replay.is_active());
        assert!(replay.begin_frame(layout()).is_ok());
        assert_ne!(client.peek_session_id(), Some(first));
        client.shutdown_blocking(Some(Duration::ZERO));
    }

    #[test]
    fn frames_do_not_start_sessions_in_manual_mode_the_background_or_before_start() {
        let (_server, client) = recording_client(SessionMode::Manual, false);
        let replay = client.session_replay();
        replay.start();
        client.end_session();
        assert!(!replay.is_active());
        assert_eq!(
            replay.begin_frame(layout()).err(),
            Some(FrameUnavailable::NoSession)
        );
        assert_eq!(client.peek_session_id(), None);

        let (_server, client) = recording_client(SessionMode::Automatic, false);
        let replay = client.session_replay();
        assert_eq!(
            replay.begin_frame(layout()).err(),
            Some(FrameUnavailable::NotRecording)
        );
        assert_eq!(client.peek_session_id(), None);
        replay.start();
        idle_31_minutes(&client);
        client.set_app_in_background(true);
        assert!(!replay.is_active());
        assert_eq!(
            replay.begin_frame(layout()).err(),
            Some(FrameUnavailable::NoSession)
        );
        client.shutdown_blocking(Some(Duration::ZERO));
    }

    #[test]
    fn start_while_opted_out_is_ignored() {
        let (_server, client) = recording_client(SessionMode::Automatic, true);
        let replay = client.session_replay();
        replay.start();
        assert!(!replay.is_active());
        assert_eq!(
            replay.begin_frame(layout()).err(),
            Some(FrameUnavailable::OptedOut)
        );
        client.opt_in();
        // As in posthog-ios and posthog-android, opting in doesn't revive a
        // start made while opted out; the app starts recording again.
        assert!(!replay.is_active());
        assert_eq!(
            replay.begin_frame(layout()).err(),
            Some(FrameUnavailable::NotRecording)
        );
        replay.start();
        assert!(replay.is_active());
        assert!(replay.begin_frame(layout()).is_ok());
        client.shutdown_blocking(Some(Duration::ZERO));
    }

    #[test]
    fn frames_after_stop_and_expiry_report_not_recording() {
        let (_server, client) = recording_client(SessionMode::Automatic, false);
        let replay = client.session_replay();
        replay.start();
        replay.stop();
        idle_31_minutes(&client);
        let expired = client.peek_session_id();
        assert_eq!(
            replay.begin_frame(layout()).err(),
            Some(FrameUnavailable::NotRecording)
        );
        // A frame that isn't recording doesn't rotate the session either.
        assert_eq!(client.peek_session_id(), expired);
        client.shutdown_blocking(Some(Duration::ZERO));
    }

    #[test]
    fn start_after_expiry_creates_a_new_session_and_records() {
        let (_server, client) = recording_client(SessionMode::Automatic, false);
        let replay = client.session_replay();
        replay.start();
        let first = client.peek_session_id().unwrap();
        idle_31_minutes(&client);
        replay.start();
        let second = client.peek_session_id().unwrap();
        assert_ne!(first, second);
        assert!(replay.begin_frame(layout()).is_ok());
        client.shutdown_blocking(Some(Duration::ZERO));
    }
}
