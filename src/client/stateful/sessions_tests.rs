use super::*;
use std::sync::{Arc, Mutex};

fn time() -> SessionTime {
    SessionTime {
        elapsed: SessionTick {
            monotonic: Instant::now(),
            boot_time: Some(Duration::from_secs(100)),
        },
        wall_ms: 1_700_000_000_000,
    }
}
fn later(now: SessionTime, elapsed: Duration) -> SessionTime {
    SessionTime {
        elapsed: SessionTick {
            monotonic: now.elapsed.monotonic + elapsed,
            boot_time: now.elapsed.boot_time.map(|time| time + elapsed),
        },
        wall_ms: now.wall_ms + elapsed.as_millis() as u64,
    }
}
// Full-system suspend advances boot time while the std monotonic clock is frozen.
fn suspended(now: SessionTime, elapsed: Duration) -> SessionTime {
    SessionTime {
        elapsed: SessionTick {
            monotonic: now.elapsed.monotonic,
            boot_time: now.elapsed.boot_time.map(|time| time + elapsed),
        },
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
            sessions.active_id(later(now, elapsed).elapsed).is_some(),
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
    let id = sessions.active_id(now.elapsed);
    sessions.apply(Operation::Maintain, later(now, IDLE / 2), false);
    sessions.apply(Operation::Maintain, later(now, IDLE), false);
    assert_ne!(sessions.active_id(later(now, IDLE).elapsed), id);
    for boundary in [
        MAX_AGE - Duration::from_nanos(1),
        MAX_AGE,
        MAX_AGE + Duration::from_nanos(1),
    ] {
        let mut sessions = active(now);
        let original = sessions.active_id(now.elapsed);
        for minute in 1..(24 * 60) {
            sessions.apply(
                Operation::Activity,
                later(now, Duration::from_secs(minute * 60)),
                false,
            );
        }
        sessions.apply(Operation::Activity, later(now, boundary), false);
        assert_eq!(
            sessions.active_id(later(now, boundary).elapsed) == original,
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
        sessions.active_id(later(now, MAX_AGE * 3).elapsed),
        original.active_id(now.elapsed)
    );
    sessions.apply(Operation::Reset, now, false);
    assert_ne!(
        sessions.active_id(now.elapsed),
        original.active_id(now.elapsed)
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
    for boot_time in [None, now.elapsed.boot_time] {
        let now = SessionTime {
            elapsed: SessionTick {
                boot_time,
                ..now.elapsed
            },
            ..now
        };
        for wall_ms in [0, now.wall_ms + 10 * MAX_AGE.as_millis() as u64] {
            let mut sessions = active(now);
            let original = sessions.active_id(now.elapsed);
            let jump = SessionTime {
                wall_ms,
                ..later(now, IDLE / 2)
            };
            sessions.apply(Operation::Activity, jump, false);
            assert_eq!(sessions.active_id(jump.elapsed), original);
            assert_eq!(
                sessions.live.as_ref().unwrap().stored.activity_ms,
                now.wall_ms + (IDLE / 2).as_millis() as u64
            );
        }
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
                .active_id(later(now, Duration::from_millis(1)).elapsed)
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

#[test]
fn suspended_elapsed_rotates_at_idle_and_max_age_in_memory_and_persistent_modes() {
    for persistence in [SessionPersistence::Memory, SessionPersistence::Persistent] {
        for limit in [IDLE, MAX_AGE] {
            for boundary in [
                limit - Duration::from_nanos(1),
                limit,
                limit + Duration::from_nanos(1),
            ] {
                let now = time();
                let clock = Arc::new(TestClock(Mutex::new(now)));
                let mut config = super::super::Config::new("phc_test");
                config.persistence = super::super::Persistence::Memory;
                config.session_persistence = persistence;
                let mut client = Client::init_blocking(config);
                client.producer.as_mut().unwrap().session_clock = clock.clone();
                let original = client.session_id();
                let mut before_suspend = now;
                if limit == MAX_AGE {
                    // Keep idle below 30 minutes, then suspend for the final minute.
                    for minute in 1..(24 * 60) {
                        before_suspend = later(now, Duration::from_secs(minute * 60));
                        *clock.0.lock().unwrap() = before_suspend;
                        client.session_activity();
                    }
                }
                let suspended_for = boundary - before_suspend.elapsed.elapsed_since(now.elapsed);
                let mut resumed = suspended(before_suspend, suspended_for);
                // Independently move wall time backwards: live expiry uses elapsed time.
                resumed.wall_ms = 0;
                *clock.0.lock().unwrap() = resumed;
                assert_eq!(client.session_id() == original, boundary < limit);
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
fn suspended_activity_keeps_logical_persisted_timing_and_restored_expiry() {
    let now = time();
    let mut sessions = Sessions {
        persistence: SessionPersistence::Persistent,
        ..active(now)
    };
    let original = sessions.active_id(now.elapsed);
    let resumed = SessionTime {
        wall_ms: 0,
        ..suspended(now, IDLE / 2)
    };
    sessions.apply(Operation::Activity, resumed, false);
    assert_eq!(sessions.active_id(resumed.elapsed), original);
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
    assert_eq!(restored.active_id(restart.elapsed), original);
    let expired = suspended(restart, IDLE / 2);
    restored.apply(Operation::Maintain, expired, false);
    assert_ne!(restored.active_id(expired.elapsed), original);
}

#[test]
fn unavailable_or_regressed_boot_clock_falls_back_to_std_elapsed_time() {
    let now = time();
    let later = later(now, IDLE);
    for (earlier_boot, later_boot) in [
        (None, None),
        (None, later.elapsed.boot_time),
        (now.elapsed.boot_time, None),
        (now.elapsed.boot_time, Some(Duration::ZERO)),
    ] {
        let earlier = SessionTick {
            boot_time: earlier_boot,
            ..now.elapsed
        };
        let later = SessionTick {
            boot_time: later_boot,
            ..later.elapsed
        };
        assert_eq!(later.elapsed_since(earlier), IDLE);
        let mut sessions = active(SessionTime {
            elapsed: earlier,
            ..now
        });
        let original = sessions.active_id(earlier);
        sessions.apply(
            Operation::Maintain,
            SessionTime {
                elapsed: later,
                ..now
            },
            false,
        );
        assert_ne!(sessions.active_id(later), original);
        assert_eq!(sessions.live.as_ref().unwrap().start_age, Duration::ZERO);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_system_session_clock_samples_boottime() {
    let before = boot_time().expect("Linux CLOCK_BOOTTIME must be available");
    let now = SystemSessionClock.now();
    let after = boot_time().unwrap();
    let sampled = now
        .elapsed
        .boot_time
        .expect("session clock must use CLOCK_BOOTTIME");
    assert!((before..=after).contains(&sampled));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn non_linux_system_session_clock_uses_std_elapsed_time() {
    let before = Instant::now();
    let now = SystemSessionClock.now();
    let after = Instant::now();
    assert!(now.elapsed.boot_time.is_none());
    assert!((before..=after).contains(&now.elapsed.monotonic));
}

#[test]
fn capture_after_suspend_rotates_and_attributes_in_both_persistence_modes() {
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
        let original = client.session_id();
        *clock.0.lock().unwrap() = suspended(now, IDLE);
        assert_eq!(
            client.capture("resumed"),
            super::super::CaptureOutcome::Dropped
        );
        let next = client.peek_session_id();
        assert_ne!(next, original);
        assert_eq!(
            seen.lock().unwrap().as_ref().unwrap().properties()["$session_id"],
            next.unwrap().to_string()
        );
    }
}
