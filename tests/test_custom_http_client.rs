//! Caller-supplied reqwest clients (`http_client` / `blocking_http_client`).
//!
//! These prove the SDK actually sends its requests through a client the caller
//! built, rather than one it constructed itself: each test configures a default
//! header on the supplied client and asserts the header arrives at the mock
//! server. A default header is used as the marker rather than `user_agent`
//! because the capture pipelines set `User-Agent` explicitly per request, which
//! overrides the client-level default.

use std::time::Duration;

use httpmock::prelude::*;

const MARKER_HEADER: &str = "x-custom-client-marker";
const MARKER_VALUE: &str = "supplied-by-caller";

fn marker_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::HeaderName::from_static(MARKER_HEADER),
        reqwest::header::HeaderValue::from_static(MARKER_VALUE),
    );
    headers
}

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

/// Poll a mock until it has been hit `want` times, for worker-driven paths.
#[cfg(not(feature = "async-client"))]
fn wait_for_hits(mock: &httpmock::Mock, want: usize) {
    for _ in 0..300 {
        if mock.calls() >= want {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(mock.calls(), want, "timed out waiting for {want} hits");
}

/// (a) The async client routes `capture_immediate` through a supplied
/// `reqwest::Client`.
#[cfg(feature = "async-client")]
mod async_client {
    use super::*;
    use posthog_rs::{ClientOptionsBuilder, Event};

    #[tokio::test]
    async fn capture_immediate_uses_supplied_http_client() {
        let server = MockServer::start();
        let uuid = uuid::Uuid::now_v7();
        let mock = server.mock(|when, then| {
            when.method(POST).header(MARKER_HEADER, MARKER_VALUE);
            then.status(200).json_body(success_body(uuid));
        });

        let http_client = reqwest::Client::builder()
            .default_headers(marker_headers())
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();

        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .http_client(http_client)
                .build()
                .unwrap(),
        )
        .await;

        let mut event = Event::new("custom-client", "user-123");
        event.set_uuid(uuid);

        client
            .capture_immediate(event)
            .await
            .expect("capture_immediate should succeed against the mock");

        // The request reached the server carrying the caller's default header,
        // so it went through the supplied client.
        mock.assert_calls(1);
    }

    /// Omitting the option keeps the previous behaviour: requests still go out,
    /// just without the caller's marker header.
    #[tokio::test]
    async fn default_client_is_unchanged_when_option_is_absent() {
        let server = MockServer::start();
        let uuid = uuid::Uuid::now_v7();
        let mock = server.mock(|when, then| {
            when.method(POST);
            then.status(200).json_body(success_body(uuid));
        });
        let marked = server.mock(|when, then| {
            when.method(POST).header(MARKER_HEADER, MARKER_VALUE);
            then.status(200).json_body(success_body(uuid));
        });

        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .build()
                .unwrap(),
        )
        .await;

        let mut event = Event::new("default-client", "user-123");
        event.set_uuid(uuid);

        client
            .capture_immediate(event)
            .await
            .expect("capture_immediate should succeed against the mock");

        mock.assert_calls(1);
        // ...and it carries no caller marker header, since no client was supplied.
        marked.assert_calls(0);
    }
}

/// (b) The background capture transport routes batches through a supplied
/// `reqwest::blocking::Client`. The transport is blocking on both clients, but
/// only the blocking build lets us drive capture + flush without an async
/// runtime, mirroring how the other blocking tests are gated.
#[cfg(not(feature = "async-client"))]
mod blocking_client {
    use super::*;
    use posthog_rs::{ClientOptionsBuilder, Event};

    #[test]
    fn background_transport_uses_supplied_blocking_client() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).header(MARKER_HEADER, MARKER_VALUE);
            then.status(200).json_body(serde_json::json!({}));
        });

        let blocking_client = reqwest::blocking::Client::builder()
            .default_headers(marker_headers())
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();

        let client = posthog_rs::client(
            ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .host(server.base_url())
                .blocking_http_client(blocking_client)
                // Keep the worker from flushing on its own so the hit count is
                // driven only by the explicit flush() below.
                .flush_interval_ms(600_000u64)
                .build()
                .unwrap(),
        );

        client.capture(Event::new("custom-blocking-client", "user-123"));
        client.flush();

        wait_for_hits(&mock, 1);
    }
}
