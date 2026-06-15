//! Acceptance coverage for the background event transport (event-batcher,
//! retry-queue, http-client, flush, shutdown specs). Runs on the async client;
//! assertions are wire-format-agnostic (they hold for both the v0 `/batch/`
//! and v1 `/i/v1/analytics/events` body shapes, which both nest events under a
//! `batch` array).
#![cfg(feature = "async-client")]

use std::time::Duration;

use httpmock::prelude::*;
use posthog_rs::{Client, ClientOptionsBuilder, Event};

async fn client_with(host: String, flush_at: usize, max_attempts: u32) -> Client {
    let options = ClientOptionsBuilder::default()
        .api_key("phc_test".to_string())
        .host(host)
        .flush_at(flush_at)
        .max_batch_size(100usize)
        .flush_interval_ms(50u64)
        .max_capture_attempts(max_attempts)
        // Tiny backoffs keep scheduled retries fast in tests.
        .retry_initial_backoff_ms(1u64)
        .retry_max_backoff_ms(5u64)
        .build()
        .unwrap();
    posthog_rs::client(options).await
}

/// Poll a mock until it has been hit `want` times (for worker-timed paths such
/// as the size-threshold and interval flushes that aren't driven by `flush()`).
fn wait_for_hits(mock: &httpmock::Mock, want: usize) {
    for _ in 0..300 {
        if mock.hits() >= want {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(mock.hits(), want, "timed out waiting for {want} hits");
}

fn body_contains(req: &HttpMockRequest, needle: &str) -> bool {
    req.body
        .as_deref()
        .map(|b| String::from_utf8_lossy(b).contains(needle))
        .unwrap_or(false)
}

// --- flush -----------------------------------------------------------------

#[tokio::test]
async fn flush_immediately_sends_queued_events() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .matches(|r| body_contains(r, "\"event\":\"First\""))
            .matches(|r| body_contains(r, "\"event\":\"Second\""));
        then.status(200);
    });

    let client = client_with(server.base_url(), 100, 3).await;
    client
        .capture(Event::new("First", "user-123"))
        .await
        .unwrap();
    client
        .capture(Event::new("Second", "user-123"))
        .await
        .unwrap();
    client.flush().await;

    // Both events arrive in a single batch request.
    mock.assert_hits(1);
}

#[tokio::test]
async fn flush_is_safe_when_queue_is_empty() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST);
        then.status(200);
    });

    let client = client_with(server.base_url(), 100, 3).await;
    client.flush().await; // must not panic, must not send

    mock.assert_hits(0);
}

#[tokio::test]
async fn flush_keeps_events_retryable_then_delivers_after_success() {
    let server = MockServer::start();
    let mut fail = server.mock(|when, then| {
        when.method(POST);
        then.status(503);
    });

    let client = client_with(server.base_url(), 100, 5).await;
    client
        .capture(Event::new("Save", "user-123"))
        .await
        .unwrap();

    // First flush attempts once; the 503 keeps the event queued for retry.
    client.flush().await;
    fail.assert_hits(1);

    // Server recovers; the held event is delivered on the next flush.
    fail.delete();
    let ok = server.mock(|when, then| {
        when.method(POST)
            .matches(|r| body_contains(r, "\"event\":\"Save\""));
        then.status(200);
    });
    client.flush().await;
    ok.assert_hits(1);

    // Delivered: a third flush sends nothing more.
    client.flush().await;
    ok.assert_hits(1);
}

// --- shutdown --------------------------------------------------------------

#[tokio::test]
async fn shutdown_flushes_and_disables_future_capture() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .matches(|r| body_contains(r, "\"event\":\"Save\""));
        then.status(200);
    });

    let client = client_with(server.base_url(), 100, 3).await;
    client
        .capture(Event::new("Save", "user-123"))
        .await
        .unwrap();
    client.shutdown().await;
    mock.assert_hits(1);

    // Captures after shutdown are dropped, not enqueued.
    client
        .capture(Event::new("After Shutdown", "user-123"))
        .await
        .unwrap();
    client.flush().await;
    mock.assert_hits(1);
}

#[tokio::test]
async fn shutdown_is_idempotent() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST);
        then.status(200);
    });

    let client = client_with(server.base_url(), 100, 3).await;
    client.shutdown().await;
    client.shutdown().await; // second call must not panic or hang
}

#[tokio::test]
async fn shutdown_does_not_throw_on_delivery_failure() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST);
        then.status(503);
    });

    let client = client_with(server.base_url(), 100, 3).await;
    client
        .capture(Event::new("Save", "user-123"))
        .await
        .unwrap();
    client.shutdown().await; // best-effort: attempts once, drops, does not panic
    mock.assert_hits(1);
}

// --- event-batcher ---------------------------------------------------------

#[tokio::test]
async fn batcher_flushes_when_size_threshold_reached() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST);
        then.status(200);
    });

    // flush_at = 2: the second capture triggers an automatic flush (no explicit
    // flush() call).
    let client = client_with(server.base_url(), 2, 3).await;
    client
        .capture(Event::new("First", "user-123"))
        .await
        .unwrap();
    client
        .capture(Event::new("Second", "user-123"))
        .await
        .unwrap();

    wait_for_hits(&mock, 1);
}

#[tokio::test]
async fn batcher_preserves_fifo_order_within_a_batch() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).matches(|r| {
            let Some(body) = r.body.as_deref() else {
                return false;
            };
            let s = String::from_utf8_lossy(body);
            match (s.find("First"), s.find("Second"), s.find("Third")) {
                (Some(a), Some(b), Some(c)) => a < b && b < c,
                _ => false,
            }
        });
        then.status(200);
    });

    let client = client_with(server.base_url(), 3, 3).await;
    client
        .capture(Event::new("First", "user-123"))
        .await
        .unwrap();
    client
        .capture(Event::new("Second", "user-123"))
        .await
        .unwrap();
    client
        .capture(Event::new("Third", "user-123"))
        .await
        .unwrap();

    wait_for_hits(&mock, 1);
}

// --- http-client -----------------------------------------------------------

#[tokio::test]
async fn successful_status_drains_the_queue() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST);
        then.status(200);
    });

    let client = client_with(server.base_url(), 100, 3).await;
    client
        .capture(Event::new("Save", "user-123"))
        .await
        .unwrap();
    client.flush().await;
    mock.assert_hits(1);

    // Nothing left to send.
    client.flush().await;
    mock.assert_hits(1);
}
