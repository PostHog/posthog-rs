//! Confirmed-delivery capture: `capture_confirmed` / `capture_batch_confirmed`.
//!
//! Unlike fire-and-forget `capture`/`capture_batch` (which enqueue onto the
//! background worker and are driven with `flush()`), the `*_confirmed` methods
//! send inline and return a terminal [`CaptureSummary`] (or an `Err`). These
//! tests call them directly and assert on both the returned outcome and the
//! wire, with no `flush()` in the loop. Tiny backoffs keep the inline retry
//! paths fast.
//!
//! Exactly one of the four modules below compiles per feature combo
//! (async XOR blocking) × (capture-v1 XOR v0).

// ---------------------------------------------------------------------------
// Async, V1 capture
// ---------------------------------------------------------------------------
#[cfg(all(feature = "async-client", feature = "capture-v1"))]
mod async_v1 {
    use std::sync::{Arc, Mutex};

    use httpmock::prelude::*;
    use posthog_rs::{Client, ClientOptionsBuilder, Event, PostHogError};
    use serde_json::json;

    async fn v1_client(base_url: String) -> Client {
        posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test_token".to_string())
                .host(base_url)
                .max_capture_attempts(3u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(5u64)
                .build()
                .unwrap(),
        )
        .await
    }

    /// An `on_error` hook plus the counter it increments, to prove confirmed
    /// methods never fire it (the returned `Result` is the only signal).
    fn error_sink() -> (
        Arc<Mutex<usize>>,
        impl Fn(&PostHogError<'_>) + Send + Sync + 'static,
    ) {
        let count = Arc::new(Mutex::new(0usize));
        let sink = count.clone();
        let hook = move |_: &PostHogError<'_>| *sink.lock().unwrap() += 1;
        (count, hook)
    }

    #[tokio::test]
    async fn single_success_reports_all_persisted() {
        let server = MockServer::start();
        let uuid = uuid::Uuid::now_v7();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.status(200)
                .json_body(json!({ "results": { uuid.to_string(): { "result": "ok" } } }));
        });

        let client = v1_client(server.base_url()).await;
        let mut event = Event::new("test", "user-1");
        event.set_uuid(uuid);

        let summary = client.capture_confirmed(event).await.unwrap();
        mock.assert_hits(1);
        assert_eq!(summary.submitted(), 1);
        assert_eq!(summary.not_persisted(), 0);
        assert!(summary.all_persisted());
        assert_eq!(summary.event_results().len(), 1);
    }

    #[tokio::test]
    async fn partial_persist_is_ok_but_not_all_persisted() {
        let server = MockServer::start();
        let mut ok = Event::new("ok", "user-1");
        let mut dropped = Event::new("drop", "user-1");
        let uuid_ok = uuid::Uuid::now_v7();
        let uuid_drop = uuid::Uuid::now_v7();
        ok.set_uuid(uuid_ok);
        dropped.set_uuid(uuid_drop);

        let mock = server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.status(200).json_body(json!({ "results": {
                uuid_ok.to_string(): { "result": "ok" },
                uuid_drop.to_string(): { "result": "drop", "details": "billing_limit_exceeded" }
            } }));
        });

        let client = v1_client(server.base_url()).await;
        let summary = client
            .capture_batch_confirmed(vec![ok, dropped], false)
            .await
            .unwrap();
        mock.assert_hits(1);
        assert_eq!(summary.submitted(), 2);
        assert_eq!(summary.not_persisted(), 1);
        assert!(!summary.all_persisted());
        assert_eq!(summary.event_results().len(), 2);
    }

    #[tokio::test]
    async fn retryable_status_then_success_within_one_call() {
        let server = MockServer::start();
        let fail = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "1");
            then.status(503)
                .header("retry-after", "0")
                .json_body(json!({ "error": "service_unavailable" }));
        });
        let ok = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "2");
            then.status(200).json_body(json!({ "results": {} }));
        });

        let client = v1_client(server.base_url()).await;
        let summary = client
            .capture_confirmed(Event::new("test", "user-1"))
            .await
            .unwrap();
        fail.assert_hits(1);
        ok.assert_hits(1);
        // 200 with an empty results map: nothing was reported unpersisted, but
        // the one submitted event has no ok/warning verdict either.
        assert_eq!(summary.submitted(), 1);
    }

    #[tokio::test]
    async fn partial_retry_pruned_and_resent_within_one_call() {
        let server = MockServer::start();
        let uuid = uuid::Uuid::now_v7();
        let retry = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "1");
            then.status(200).json_body(
                json!({ "results": { uuid.to_string(): { "result": "retry", "details": "not_persisted" } } }),
            );
        });
        let ok = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .header("posthog-attempt", "2");
            then.status(200)
                .json_body(json!({ "results": { uuid.to_string(): { "result": "ok" } } }));
        });

        let client = v1_client(server.base_url()).await;
        let mut event = Event::new("test", "user-1");
        event.set_uuid(uuid);

        let summary = client.capture_confirmed(event).await.unwrap();
        retry.assert_hits(1);
        ok.assert_hits(1);
        assert!(summary.all_persisted());
        assert_eq!(summary.not_persisted(), 0);
    }

    #[tokio::test]
    async fn exhausting_retryable_status_returns_err() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.status(500)
                .json_body(json!({ "error": "internal_error" }));
        });

        let client = v1_client(server.base_url()).await;
        let err = client
            .capture_confirmed(Event::new("test", "user-1"))
            .await
            .expect_err("exhausted retry budget must surface an error");
        // max_capture_attempts = 3 → 3 inline attempts before giving up.
        mock.assert_hits(3);
        assert!(
            err.to_string().contains("500") || err.to_string().to_lowercase().contains("server")
        );
    }

    #[tokio::test]
    async fn terminal_billing_status_returns_err_without_retry() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.status(402)
                .json_body(json!({ "error": "billing_limit_exceeded" }));
        });

        let client = v1_client(server.base_url()).await;
        let result = client.capture_confirmed(Event::new("test", "user-1")).await;
        assert!(result.is_err());
        mock.assert_hits(1);
    }

    #[tokio::test]
    async fn historical_migration_flag_is_sent() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v1/analytics/events")
                .body_contains("\"historical_migration\":true");
            then.status(200).json_body(json!({ "results": {} }));
        });

        let client = v1_client(server.base_url()).await;
        client
            .capture_batch_confirmed(vec![Event::new("a", "u"), Event::new("b", "u")], true)
            .await
            .unwrap();
        mock.assert_hits(1);
    }

    #[tokio::test]
    async fn does_not_fire_on_error_on_terminal_failure() {
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.status(500)
                .json_body(json!({ "error": "internal_error" }));
        });

        let (count, hook) = error_sink();
        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test_token".to_string())
                .host(server.base_url())
                .max_capture_attempts(2u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(5u64)
                .on_error(hook)
                .build()
                .unwrap(),
        )
        .await;

        let _ = client.capture_confirmed(Event::new("test", "user-1")).await;
        assert_eq!(
            *count.lock().unwrap(),
            0,
            "confirmed capture must not fire on_error; the Result is the signal"
        );
    }

    #[tokio::test]
    async fn disabled_and_empty_are_noops() {
        // Disabled client: no request, default summary.
        let disabled = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .disabled(true)
                .build()
                .unwrap(),
        )
        .await;
        let summary = disabled
            .capture_confirmed(Event::new("test", "user-1"))
            .await
            .unwrap();
        assert_eq!(summary.submitted(), 0);
        assert!(summary.all_persisted());

        // Empty batch against a live server sends nothing.
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.status(200).json_body(json!({ "results": {} }));
        });
        let client = v1_client(server.base_url()).await;
        let summary = client.capture_batch_confirmed(vec![], false).await.unwrap();
        assert_eq!(summary.submitted(), 0);
        mock.assert_hits(0);
    }
}

// ---------------------------------------------------------------------------
// Async, V0 capture
// ---------------------------------------------------------------------------
#[cfg(all(feature = "async-client", not(feature = "capture-v1")))]
mod async_v0 {
    use std::io::Read;
    use std::sync::{Arc, Mutex};

    use httpmock::prelude::*;
    use posthog_rs::{CaptureCompression, Client, ClientOptionsBuilder, Event, PostHogError};

    async fn v0_client(base_url: String) -> Client {
        posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test_token".to_string())
                .host(base_url)
                .max_capture_attempts(3u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(5u64)
                .build()
                .unwrap(),
        )
        .await
    }

    fn error_sink() -> (
        Arc<Mutex<usize>>,
        impl Fn(&PostHogError<'_>) + Send + Sync + 'static,
    ) {
        let count = Arc::new(Mutex::new(0usize));
        let sink = count.clone();
        let hook = move |_: &PostHogError<'_>| *sink.lock().unwrap() += 1;
        (count, hook)
    }

    #[tokio::test]
    async fn success_reports_whole_batch_persisted() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/batch/");
            then.status(200);
        });

        let client = v0_client(server.base_url()).await;
        let summary = client
            .capture_batch_confirmed(vec![Event::new("a", "u"), Event::new("b", "u")], false)
            .await
            .unwrap();
        mock.assert_hits(1);
        // v0 has no per-event verdicts: a 2xx persists the whole batch.
        assert_eq!(summary.submitted(), 2);
        assert_eq!(summary.not_persisted(), 0);
        assert!(summary.all_persisted());
    }

    #[tokio::test]
    async fn exhausting_retryable_status_returns_err() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/batch/");
            then.status(503);
        });

        let client = v0_client(server.base_url()).await;
        let err = client
            .capture_confirmed(Event::new("test", "user-1"))
            .await
            .expect_err("exhausted retry budget must surface an error");
        mock.assert_hits(3);
        assert!(!err.to_string().is_empty());
    }

    #[tokio::test]
    async fn terminal_status_returns_err_without_retry() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/batch/");
            then.status(401);
        });

        let client = v0_client(server.base_url()).await;
        let result = client.capture_confirmed(Event::new("test", "user-1")).await;
        assert!(result.is_err());
        mock.assert_hits(1);
    }

    #[tokio::test]
    async fn historical_migration_flag_is_sent() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/batch/")
                .body_contains("\"historical_migration\":true");
            then.status(200);
        });

        let client = v0_client(server.base_url()).await;
        client
            .capture_batch_confirmed(vec![Event::new("a", "u")], true)
            .await
            .unwrap();
        mock.assert_hits(1);
    }

    fn body_gunzips_to_user1(req: &HttpMockRequest) -> bool {
        let Some(body) = req.body.as_ref() else {
            return false;
        };
        let mut decoder = flate2::read::GzDecoder::new(&body[..]);
        let mut decoded = String::new();
        match decoder.read_to_string(&mut decoded) {
            Ok(_) => decoded.contains(r#""distinct_id":"user1""#),
            Err(_) => false,
        }
    }

    #[tokio::test]
    async fn gzip_sets_header_and_query_param() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/batch/")
                .header("content-encoding", "gzip")
                .query_param("compression", "gzip")
                .matches(body_gunzips_to_user1);
            then.status(200);
        });

        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test_token".to_string())
                .host(server.base_url())
                .capture_compression(CaptureCompression::Gzip)
                .build()
                .unwrap(),
        )
        .await;
        client
            .capture_confirmed(Event::new("test_event", "user1"))
            .await
            .unwrap();
        mock.assert_hits(1);
    }

    #[tokio::test]
    async fn does_not_fire_on_error_on_terminal_failure() {
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(POST).path("/batch/");
            then.status(500);
        });

        let (count, hook) = error_sink();
        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test_token".to_string())
                .host(server.base_url())
                .max_capture_attempts(2u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(5u64)
                .on_error(hook)
                .build()
                .unwrap(),
        )
        .await;

        let _ = client.capture_confirmed(Event::new("test", "user-1")).await;
        assert_eq!(*count.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn disabled_client_is_noop() {
        let disabled = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .disabled(true)
                .build()
                .unwrap(),
        )
        .await;
        let summary = disabled
            .capture_confirmed(Event::new("test", "user-1"))
            .await
            .unwrap();
        assert_eq!(summary.submitted(), 0);
        assert!(summary.all_persisted());
    }
}

// ---------------------------------------------------------------------------
// Blocking, V1 capture
// ---------------------------------------------------------------------------
#[cfg(all(not(feature = "async-client"), feature = "capture-v1"))]
mod blocking_v1 {
    use std::sync::{Arc, Mutex};

    use httpmock::prelude::*;
    use posthog_rs::{Client, ClientOptionsBuilder, Event, PostHogError};
    use serde_json::json;

    fn v1_client(base_url: String) -> Client {
        posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test_token".to_string())
                .host(base_url)
                .max_capture_attempts(3u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(5u64)
                .build()
                .unwrap(),
        )
    }

    #[test]
    fn single_success_reports_all_persisted() {
        let server = MockServer::start();
        let uuid = uuid::Uuid::now_v7();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.status(200)
                .json_body(json!({ "results": { uuid.to_string(): { "result": "ok" } } }));
        });

        let client = v1_client(server.base_url());
        let mut event = Event::new("test", "user-1");
        event.set_uuid(uuid);
        let summary = client.capture_confirmed(event).unwrap();
        mock.assert_hits(1);
        assert!(summary.all_persisted());
        assert_eq!(summary.submitted(), 1);
    }

    #[test]
    fn partial_persist_is_ok_but_not_all_persisted() {
        let server = MockServer::start();
        let mut ok = Event::new("ok", "user-1");
        let mut dropped = Event::new("drop", "user-1");
        let uuid_ok = uuid::Uuid::now_v7();
        let uuid_drop = uuid::Uuid::now_v7();
        ok.set_uuid(uuid_ok);
        dropped.set_uuid(uuid_drop);

        let mock = server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.status(200).json_body(json!({ "results": {
                uuid_ok.to_string(): { "result": "ok" },
                uuid_drop.to_string(): { "result": "drop", "details": "billing_limit_exceeded" }
            } }));
        });

        let client = v1_client(server.base_url());
        let summary = client
            .capture_batch_confirmed(vec![ok, dropped], false)
            .unwrap();
        mock.assert_hits(1);
        assert_eq!(summary.not_persisted(), 1);
        assert!(!summary.all_persisted());
    }

    #[test]
    fn exhausting_retryable_status_returns_err() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.status(500)
                .json_body(json!({ "error": "internal_error" }));
        });

        let client = v1_client(server.base_url());
        let result = client.capture_confirmed(Event::new("test", "user-1"));
        assert!(result.is_err());
        mock.assert_hits(3);
    }

    #[test]
    fn does_not_fire_on_error_on_terminal_failure() {
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(POST).path("/i/v1/analytics/events");
            then.status(500)
                .json_body(json!({ "error": "internal_error" }));
        });

        let count = Arc::new(Mutex::new(0usize));
        let sink = count.clone();
        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test_token".to_string())
                .host(server.base_url())
                .max_capture_attempts(2u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(5u64)
                .on_error(move |_: &PostHogError<'_>| *sink.lock().unwrap() += 1)
                .build()
                .unwrap(),
        );
        let _ = client.capture_confirmed(Event::new("test", "user-1"));
        assert_eq!(*count.lock().unwrap(), 0);
    }
}

// ---------------------------------------------------------------------------
// Blocking, V0 capture
// ---------------------------------------------------------------------------
#[cfg(all(not(feature = "async-client"), not(feature = "capture-v1")))]
mod blocking_v0 {
    use std::sync::{Arc, Mutex};

    use httpmock::prelude::*;
    use posthog_rs::{Client, ClientOptionsBuilder, Event, PostHogError};

    fn v0_client(base_url: String) -> Client {
        posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test_token".to_string())
                .host(base_url)
                .max_capture_attempts(3u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(5u64)
                .build()
                .unwrap(),
        )
    }

    #[test]
    fn success_reports_whole_batch_persisted() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/batch/");
            then.status(200);
        });

        let client = v0_client(server.base_url());
        let summary = client
            .capture_batch_confirmed(vec![Event::new("a", "u"), Event::new("b", "u")], false)
            .unwrap();
        mock.assert_hits(1);
        assert_eq!(summary.submitted(), 2);
        assert!(summary.all_persisted());
    }

    #[test]
    fn exhausting_retryable_status_returns_err() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/batch/");
            then.status(503);
        });

        let client = v0_client(server.base_url());
        let result = client.capture_confirmed(Event::new("test", "user-1"));
        assert!(result.is_err());
        mock.assert_hits(3);
    }

    #[test]
    fn terminal_status_returns_err_without_retry() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/batch/");
            then.status(401);
        });

        let client = v0_client(server.base_url());
        let result = client.capture_confirmed(Event::new("test", "user-1"));
        assert!(result.is_err());
        mock.assert_hits(1);
    }

    #[test]
    fn does_not_fire_on_error_on_terminal_failure() {
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(POST).path("/batch/");
            then.status(500);
        });

        let count = Arc::new(Mutex::new(0usize));
        let sink = count.clone();
        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test_token".to_string())
                .host(server.base_url())
                .max_capture_attempts(2u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(5u64)
                .on_error(move |_: &PostHogError<'_>| *sink.lock().unwrap() += 1)
                .build()
                .unwrap(),
        );
        let _ = client.capture_confirmed(Event::new("test", "user-1"));
        assert_eq!(*count.lock().unwrap(), 0);
    }
}
