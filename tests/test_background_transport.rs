//! The `background_transport` option.
//!
//! With `background_transport(false)` the client starts no batching worker and
//! no blocking HTTP client, for embedders on an async runtime that only use
//! immediate capture and must not spawn threads or block on drop. These tests
//! pin the resulting contract: immediate capture and flags still work,
//! fire-and-forget capture is dropped, and teardown never blocks.

use std::time::{Duration, Instant};

use httpmock::prelude::*;

/// Teardown must not wait on a worker; allow generous slack over the expected
/// near-zero so the assertion isn't flaky on a loaded CI box, while still
/// failing loudly if a `shutdown_timeout_ms`-scale drain happens.
const TEARDOWN_BUDGET: Duration = Duration::from_secs(1);

/// Success body for the active capture pipeline: V1 reports per-event results,
/// V0 returns a plain acknowledgement.
#[cfg(feature = "async-client")]
fn success_body(uuid: uuid::Uuid) -> serde_json::Value {
    #[cfg(feature = "capture-v1")]
    {
        serde_json::json!({ "results": { uuid.to_string(): { "result": "ok" } } })
    }
    #[cfg(not(feature = "capture-v1"))]
    {
        let _ = uuid;
        serde_json::json!({ "status": 1 })
    }
}

#[cfg(feature = "async-client")]
mod async_client {
    use super::*;
    use posthog_rs::{ClientOptionsBuilder, Event};

    const MARKER_HEADER: &str = "x-custom-client-marker";
    const MARKER_VALUE: &str = "supplied-by-caller";

    fn marked_client() -> reqwest::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_static(MARKER_HEADER),
            reqwest::header::HeaderValue::from_static(MARKER_VALUE),
        );
        reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap()
    }

    /// (a) Immediate capture still works, and still goes through the supplied
    /// `http_client`, with no background worker in the picture.
    #[tokio::test]
    async fn capture_immediate_works_without_background_transport() {
        let server = MockServer::start();
        let uuid = uuid::Uuid::now_v7();
        let mock = server.mock(|when, then| {
            when.method(POST).header(MARKER_HEADER, MARKER_VALUE);
            then.status(200).json_body(success_body(uuid));
        });

        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .http_client(marked_client())
                .background_transport(false)
                .build()
                .unwrap(),
        )
        .await;

        let mut event = Event::new("immediate-no-worker", "user-123");
        event.set_uuid(uuid);
        client
            .capture_immediate(event)
            .await
            .expect("capture_immediate should succeed without a background worker");

        mock.assert_calls(1);
    }

    /// (b) Fire-and-forget capture is dropped: nothing reaches the server, even
    /// after an explicit flush.
    #[tokio::test]
    async fn fire_and_forget_capture_is_dropped_without_background_transport() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST);
            then.status(200).json_body(serde_json::json!({}));
        });

        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                // Would flush almost immediately if a worker existed.
                .flush_interval_ms(10u64)
                .background_transport(false)
                .build()
                .unwrap(),
        )
        .await;

        client.capture(Event::new("dropped", "user-123"));
        client.capture_batch(vec![Event::new("dropped-batch", "user-123")], false);
        client.alias("anon-1", "user-123");
        client.flush().await;

        // Give any worker that did get spawned a chance to send, so this fails
        // rather than passing on timing.
        tokio::time::sleep(Duration::from_millis(100)).await;

        mock.assert_calls(0);
    }

    /// (c) Teardown never blocks: shutdown and drop both return promptly
    /// because there is no worker to join.
    ///
    /// The mock server stalls every response, so a client that *did* spawn a
    /// worker would drain into that stall on shutdown/drop and blow the budget.
    /// That makes the timing assertion discriminating rather than incidental —
    /// a refused connection would fail fast and prove nothing.
    #[tokio::test]
    async fn shutdown_and_drop_do_not_block_without_background_transport() {
        let server = MockServer::start();
        let _stalled = server.mock(|when, then| {
            when.method(POST);
            then.status(200)
                .delay(Duration::from_secs(10))
                .json_body(serde_json::json!({}));
        });

        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .request_timeout_seconds(30u64)
                .shutdown_timeout_ms(30_000u64)
                .background_transport(false)
                .build()
                .unwrap(),
        )
        .await;

        client.capture(Event::new("queued-then-dropped", "user-123"));

        let started = Instant::now();
        client.shutdown().await;
        let shutdown_elapsed = started.elapsed();

        let started = Instant::now();
        drop(client);
        let drop_elapsed = started.elapsed();

        assert!(
            shutdown_elapsed < TEARDOWN_BUDGET,
            "shutdown should return immediately without a worker, took {shutdown_elapsed:?}"
        );
        assert!(
            drop_elapsed < TEARDOWN_BUDGET,
            "drop should return immediately without a worker, took {drop_elapsed:?}"
        );
    }

    /// The default is unchanged: a client built without the option still has a
    /// working background worker.
    #[tokio::test]
    async fn background_transport_is_on_by_default() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST);
            then.status(200).json_body(serde_json::json!({}));
        });

        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .flush_interval_ms(600_000u64)
                .build()
                .unwrap(),
        )
        .await;

        client.capture(Event::new("delivered", "user-123"));
        client.flush().await;

        mock.assert_calls(1);
    }
}

#[cfg(not(feature = "async-client"))]
mod blocking_client {
    use super::*;
    use posthog_rs::{ClientOptionsBuilder, Event};

    /// A blocking client with `background_transport(false)` never constructs a
    /// blocking transport: the event is dropped and teardown is immediate.
    #[test]
    fn blocking_client_builds_no_transport() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST);
            then.status(200).json_body(serde_json::json!({}));
        });

        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .flush_interval_ms(10u64)
                .background_transport(false)
                .build()
                .unwrap(),
        );

        client.capture(Event::new("dropped", "user-123"));
        client.flush();
        std::thread::sleep(Duration::from_millis(100));

        // No worker ever sent the event.
        mock.assert_calls(0);

        let started = Instant::now();
        client.shutdown();
        drop(client);
        let elapsed = started.elapsed();
        assert!(
            elapsed < TEARDOWN_BUDGET,
            "teardown should not block without a worker, took {elapsed:?}"
        );
    }

    /// Immediate capture still reaches the server with no worker present.
    #[test]
    fn capture_immediate_works_without_background_transport() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST);
            then.status(200).json_body(serde_json::json!({}));
        });

        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .background_transport(false)
                .build()
                .unwrap(),
        );

        client
            .capture_immediate(Event::new("immediate-no-worker", "user-123"))
            .expect("capture_immediate should succeed without a background worker");

        mock.assert_calls(1);
    }
}
