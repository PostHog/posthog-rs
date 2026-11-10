use super::*;
use std::sync::{Arc, Mutex};

fn time() -> SessionTime {
    SessionTime {
        monotonic: Instant::now(),
        wall_ms: 1_700_000_000_000,
    }
}
fn later(now: SessionTime, elapsed: Duration) -> SessionTime {
    SessionTime {
        monotonic: now.monotonic + elapsed,
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
        IDLE - Duration::from_nanos(1),
        IDLE,
        IDLE + Duration::from_nanos(1),
    ] {
        let now = time();
        let mut sessions = active(now);
        let original = sessions.live.as_ref().unwrap().stored.id;
        let before = sessions.clone();
        assert_eq!(
            sessions.active_id(later(now, elapsed).monotonic).is_some(),
            elapsed < IDLE
        );
        assert_eq!(sessions, before);
        sessions.apply(Operation::Maintain, later(now, elapsed), false);
        assert_eq!(
            sessions.live.as_ref().unwrap().stored.id == original,
            elapsed < IDLE
        );
    }
}

#[test]
fn getters_do_not_extend_activity_and_continuous_activity_reaches_maximum() {
    let now = time();
    let mut sessions = active(now);
    let id = sessions.active_id(now.monotonic);
    sessions.apply(Operation::Maintain, later(now, IDLE / 2), false);
    sessions.apply(Operation::Maintain, later(now, IDLE), false);
    assert_ne!(sessions.active_id(later(now, IDLE).monotonic), id);
    for boundary in [
        MAX_AGE - Duration::from_nanos(1),
        MAX_AGE,
        MAX_AGE + Duration::from_nanos(1),
    ] {
        let mut sessions = active(now);
        let original = sessions.active_id(now.monotonic);
        for minute in 1..(24 * 60) {
            sessions.apply(
                Operation::Activity,
                later(now, Duration::from_secs(minute * 60)),
                false,
            );
        }
        sessions.apply(Operation::Activity, later(now, boundary), false);
        assert_eq!(
            sessions.active_id(later(now, boundary).monotonic) == original,
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
        sessions.active_id(later(now, MAX_AGE * 3).monotonic),
        original.active_id(now.monotonic)
    );
    sessions.apply(Operation::Reset, now, false);
    assert_ne!(
        sessions.active_id(now.monotonic),
        original.active_id(now.monotonic)
    );
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
fn wall_clock_jumps_do_not_change_live_expiry_and_restart_rejects_future_timing() {
    let now = time();
    for wall_ms in [0, now.wall_ms + 10 * MAX_AGE.as_millis() as u64] {
        let mut sessions = active(now);
        let original = sessions.active_id(now.monotonic);
        let jump = SessionTime {
            wall_ms,
            ..later(now, IDLE / 2)
        };
        sessions.apply(Operation::Activity, jump, false);
        assert_eq!(sessions.active_id(jump.monotonic), original);
        assert_eq!(
            sessions.live.as_ref().unwrap().stored.activity_ms,
            now.wall_ms + (IDLE / 2).as_millis() as u64
        );
    }
    let stored = active(now).live.unwrap().stored;
    let rollback = SessionTime {
        wall_ms: now.wall_ms - 1,
        ..now
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
fn restored_expiry_uses_wall_age_then_monotonic_time_including_maximum() {
    let now = time();
    let stored = active(now).live.unwrap().stored;
    for age in [
        IDLE - Duration::from_millis(1),
        IDLE,
        IDLE + Duration::from_millis(1),
    ] {
        let restart = SessionTime {
            wall_ms: now.wall_ms + age.as_millis() as u64,
            ..now
        };
        let sessions = Sessions::restored(
            Some(&stored),
            SessionMode::Automatic,
            SessionPersistence::Persistent,
            restart,
        );
        assert_eq!(sessions.live.is_some(), age < IDLE);
        if age < IDLE {
            assert!(sessions
                .active_id(now.monotonic + Duration::from_millis(1))
                .is_none());
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
            ..now
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
        later(now, MAX_AGE - Duration::from_nanos(1)),
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
fn capture_uses_injected_clock_and_passive_helpers_never_extend_or_create() {
    let now = time();
    let clock = Arc::new(TestClock(Mutex::new(now)));
    let mut config = super::super::Config::new("phc_test");
    config.preload_feature_flags = false;
    config.persistence = super::super::Persistence::Memory;
    config.host = "http://127.0.0.1:1".into();
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    config.before_send(move |event| {
        seen.lock().unwrap().push(event);
        None
    });
    let mut client = Client::init_blocking(config);
    client.producer.as_mut().unwrap().session_clock = clock.clone();
    client.group("company", "a", None);
    assert_eq!(client.peek_session_id(), None);
    client.capture("first");
    let original = client.peek_session_id().unwrap();
    *clock.0.lock().unwrap() = later(now, IDLE / 2);
    client.group("company", "a", None);
    assert_eq!(client.session_id(), Some(original));
    *clock.0.lock().unwrap() = later(now, IDLE);
    client.group("company", "a", None);
    assert_eq!(client.peek_session_id(), Some(original));
    client.capture("next");
    let next = client.peek_session_id().unwrap();
    assert_ne!(next, original);
    let seen = events.lock().unwrap();
    assert!(!seen[0].properties().contains_key("$session_id"));
    assert_eq!(seen[1].properties()["$session_id"], original.to_string());
    assert_eq!(seen[2].properties()["$session_id"], original.to_string());
    assert!(!seen[3].properties().contains_key("$session_id"));
    assert_eq!(seen[4].properties()["$session_id"], next.to_string());
}
