use super::*;

#[test]
fn analytics_deadline_applies_while_encoder_is_blocked() {
    for closing in [false, true] {
        let backend = Backend::new();
        let analytics = backend.server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            // Counting a started request is sufficient; no successful
            // analytics acknowledgment is needed for this assertion.
            then.status(400).json_body(json!({}));
        });

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let (client, clock) = with_clock_config(&backend, |config| {
            config.before_send(move |event| {
                if event.event_name() == "$snapshot" {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10))
                        .unwrap();
                }
                Some(event)
            });
        });
        let client = Arc::new(client);
        assert!(matches!(
            client.capture("pending analytics"),
            crate::stateful::CaptureOutcome::Accepted { .. }
        ));
        let replay = client.session_replay();
        replay.start();
        assert_eq!(replay.submit_frame(frame(&replay)), SubmitOutcome::Accepted);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let shutdown = if closing {
            let other = client.clone();
            Some(std::thread::spawn(move || {
                other.shutdown_blocking(Some(Duration::ZERO))
            }))
        } else {
            None
        };

        let gate = replay.0.upgrade().unwrap().gate.clone();
        if closing {
            let limit = Instant::now() + Duration::from_secs(5);
            while !client.state.as_ref().unwrap().is_closed() && Instant::now() < limit {
                std::thread::yield_now();
            }
        }
        let closed_observed =
            !closing || (gate.is_closed() && client.state.as_ref().unwrap().is_closed());

        // No new capture or encoder completion: exercise the existing
        // delivery timer servicing against an already-expired drain.
        clock.advance(Duration::from_secs(60));
        client.transport.as_ref().unwrap().tick();
        let requests_before_encoder_release = analytics.calls();

        release_tx.send(()).unwrap();
        if let Some(shutdown) = shutdown {
            shutdown.join().unwrap();
        } else {
            client.shutdown_blocking(Some(Duration::ZERO));
        }

        assert!(closed_observed, "shutdown setup failed");
        assert_eq!(
            requests_before_encoder_release,
            if closing { 0 } else { 1 },
            "closing={}",
            closing
        );
    }
}

#[test]
fn replay_only_shutdown_abandonment_reports_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let backend = Backend::new();
    let diagnostics = Arc::new(AtomicUsize::new(0));
    let seen = diagnostics.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);

    let client = Arc::new(backend.client(|config| {
        config.on_error(move |error| {
            if matches!(error, crate::client::PostHogError::Replay(_)) {
                seen.fetch_add(1, Ordering::SeqCst);
            }
        });
        config.before_send(move |event| {
            if event.event_name() == "$snapshot" {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
            }
            Some(event)
        });
    }));
    let replay = client.session_replay();
    replay.start();
    assert_eq!(replay.submit_frame(frame(&replay)), SubmitOutcome::Accepted);
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(replay.submit_frame(frame(&replay)), SubmitOutcome::Accepted);

    let other = client.clone();
    let shutdown = std::thread::spawn(move || other.shutdown_blocking(Some(Duration::ZERO)));
    let gate = replay.0.upgrade().unwrap().gate.clone();
    let limit = Instant::now() + Duration::from_secs(5);
    while !gate.is_closed() && Instant::now() < limit {
        std::thread::yield_now();
    }
    let closed = gate.is_closed();
    release_tx.send(()).unwrap();
    shutdown.join().unwrap();

    assert!(closed, "shutdown setup failed");
    assert!(backend.bodies.lock().unwrap().is_empty());
    assert_eq!(diagnostics.load(Ordering::SeqCst), 1);
}

#[test]
fn configured_default_opt_out_prevents_replay_until_opt_in() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let backend = Backend::new();
    let hooks = Arc::new(AtomicUsize::new(0));
    let seen = hooks.clone();
    let client = backend.client(|config| {
        config.opt_out = true;
        config.before_send(move |event| {
            if event.event_name() == "$snapshot" {
                seen.fetch_add(1, Ordering::SeqCst);
            }
            Some(event)
        });
    });
    let replay = client.session_replay();
    replay.start();
    assert_eq!(client.peek_session_id(), None);
    assert!(!replay.is_active());
    assert!(replay.begin_frame(layout()).is_none());
    client.flush_blocking();
    assert_eq!(hooks.load(Ordering::SeqCst), 0);
    assert!(backend.bodies.lock().unwrap().is_empty());

    client.opt_in();
    replay.start();
    assert!(replay.is_active());
    assert_eq!(replay.submit_frame(frame(&replay)), SubmitOutcome::Accepted);
    client.flush_blocking();
    assert_eq!(hooks.load(Ordering::SeqCst), 1);
    assert_eq!(backend.bodies.lock().unwrap().len(), 1);
}

#[test]
fn idle_worker_sends_single_frames_and_retries_without_external_wakeups() {
    let backend = Backend::new();
    backend.reply.lock().unwrap().status = 503;
    let client = backend.client(|_| {});
    let replay = client.session_replay();
    replay.start();
    assert_eq!(replay.submit_frame(frame(&replay)), SubmitOutcome::Accepted);
    let first = backend
        .uploads
        .lock()
        .unwrap()
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    backend.reply.lock().unwrap().status = 200;
    let retried = backend
        .uploads
        .lock()
        .unwrap()
        .recv_timeout(Duration::from_secs(15))
        .unwrap();
    assert_eq!(first, retried);
    // Both uploads were observed before any explicit flush.
    client.flush_blocking();
    assert!(replay.begin_frame(layout()).is_some());
}

#[test]
fn idle_worker_sends_a_pair_without_an_explicit_flush() {
    let backend = Backend::new();
    let client = backend.client(|_| {});
    let replay = client.session_replay();
    replay.start();
    for _ in 0..2 {
        assert_eq!(replay.submit_frame(frame(&replay)), SubmitOutcome::Accepted);
    }
    let batch = backend
        .uploads
        .lock()
        .unwrap()
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(batch.as_array().unwrap().len(), 2);
}

#[test]
fn terminal_and_quota_diagnostics_reach_the_public_hook_without_sensitive_data() {
    for (status, quota, expected) in [
        (400, false, "Screenshot upload permanently rejected"),
        (413, false, "Screenshot upload permanently rejected"),
        (200, true, "Screenshot upload rejected by recording quota"),
    ] {
        let backend = Backend::new();
        *backend.reply.lock().unwrap() = Reply {
            status,
            split: false,
            retry_after: None,
            body: if quota {
                json!({"quotaLimited": ["recordings"], "error": "private-body-marker"})
            } else {
                json!({"error": "private-body-marker"})
            },
        };
        let (tx, rx) = mpsc::channel();
        let client = backend.client(|config| {
            config.bootstrap_anonymous_id = Some("private-attribution-marker".into());
            config.on_error(move |error| {
                if let crate::client::PostHogError::Replay(failure) = error {
                    tx.send((
                        failure.message().to_owned(),
                        failure.status(),
                        format!("{error:?}"),
                    ))
                    .unwrap();
                }
            });
            config.before_send(|mut event| {
                event
                    .insert_prop("private", "private-payload-marker")
                    .unwrap();
                Some(event)
            });
        });
        let replay = client.session_replay();
        replay.start();
        assert_eq!(replay.submit_frame(frame(&replay)), SubmitOutcome::Accepted);
        client.flush_blocking();
        let diagnostic = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(diagnostic.0, expected);
        assert_eq!(diagnostic.1, Some(status));
        assert!(rx.try_recv().is_err());
        for marker in [
            "phc_test",
            "private-attribution-marker",
            "private-payload-marker",
            "private-body-marker",
            "data:image/jpeg",
        ] {
            assert!(!diagnostic.0.contains(marker));
            assert!(!diagnostic.2.contains(marker));
        }
        assert!(replay.begin_frame(layout()).is_some());
    }
}

#[test]
fn nonzero_shutdown_budget_clamps_replay_http_and_stops_following_batches() {
    let backend = Backend::new();
    let (tx, rx) = mpsc::channel();
    let client = backend.client(|config| {
        config.request_timeout_seconds = 10;
        config.on_error(move |error| {
            if let crate::client::PostHogError::Replay(failure) = error {
                tx.send((failure.message().to_owned(), failure.status()))
                    .unwrap();
            }
        });
    });
    backend.server.reset();
    let stalled = backend.server.mock(|when, then| {
        when.method(POST).path("/s/");
        then.status(200).delay(Duration::from_secs(3));
    });
    let replay = client.session_replay();
    replay.start();
    assert_eq!(replay.submit_frame(frame(&replay)), SubmitOutcome::Accepted);
    client.end_session();
    replay.start();
    assert_eq!(replay.submit_frame(frame(&replay)), SubmitOutcome::Accepted);
    let start = Instant::now();
    client.shutdown_blocking(Some(Duration::from_millis(250)));
    assert!(start.elapsed() < Duration::from_secs(2));
    stalled.assert_calls(1);
    assert_eq!(
        replay.0.upgrade().unwrap().pool.lock().unwrap().in_flight,
        0
    );
    let diagnostic = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        diagnostic.0,
        "Screenshot delivery abandoned during shutdown"
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn shutdown_reports_cooldown_abandonment_without_bypassing_retry_after() {
    let backend = Backend::new();
    *backend.reply.lock().unwrap() = Reply {
        status: 503,
        split: false,
        retry_after: Some("60"),
        body: json!({}),
    };
    let (tx, rx) = mpsc::channel();
    let client = backend.client(|config| {
        config.on_error(move |error| {
            if let crate::client::PostHogError::Replay(failure) = error {
                tx.send(failure.message().to_owned()).unwrap();
            }
        });
    });
    let replay = client.session_replay();
    replay.start();
    assert_eq!(replay.submit_frame(frame(&replay)), SubmitOutcome::Accepted);
    client.flush_blocking();
    assert_eq!(backend.bodies.lock().unwrap().len(), 1);
    client.shutdown_blocking(Some(Duration::from_secs(1)));
    assert_eq!(backend.bodies.lock().unwrap().len(), 1);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        "Screenshot delivery abandoned during shutdown"
    );
    assert!(rx.try_recv().is_err());
    assert_eq!(
        replay.0.upgrade().unwrap().pool.lock().unwrap().in_flight,
        0
    );
}
