#![cfg(all(feature = "async-client", not(feature = "capture-v1")))]

use std::io::Read;
use std::time::{Duration, Instant};

use httpmock::prelude::*;
use posthog_rs::{CaptureCompression, Client, ClientOptionsBuilder, Event};

// A real v7 UUID so the event passes through unchanged; matched on the wire to
// prove retried attempts resend the same bytes.
const FIXED_UUID: &str = "01920000-0000-7000-8000-0000000000ff";

async fn create_v0_client(base_url: String, max_attempts: u32) -> Client {
    let options = ClientOptionsBuilder::default()
        .api_key("phc_test_token".to_string())
        .host(base_url)
        .max_capture_attempts(max_attempts)
        // Tiny backoffs keep the retry tests fast; the retry-after test below
        // relies on these being far smaller than the header value it asserts.
        .retry_initial_backoff_ms(1u64)
        .retry_max_backoff_ms(5u64)
        .build()
        .unwrap();
    posthog_rs::client(options).await
}

/// Drive whichever v0 path (single `/i/v0/e/` vs `/batch/`) the test wants,
/// so coverage of both stays DRY.
async fn capture(client: &Client, batch: bool) -> Result<(), posthog_rs::Error> {
    if batch {
        client
            .capture_batch(vec![Event::new("e", "user-1")], false)
            .await
    } else {
        client.capture(Event::new("e", "user-1")).await
    }
}

#[tokio::test]
async fn retryable_status_exhausts_attempts() {
    for status in [408u16, 500, 502, 503, 504] {
        for batch in [false, true] {
            let server = MockServer::start();
            let mock = server.mock(|when, then| {
                when.method(POST);
                then.status(status);
            });

            let client = create_v0_client(server.base_url(), 3).await;
            let result = capture(&client, batch).await;

            assert!(
                result.is_err(),
                "status {} (batch={}) should error after exhausting retries",
                status,
                batch
            );
            mock.assert_hits(3);
        }
    }
}

#[tokio::test]
async fn terminal_status_sends_once() {
    // 429 here carries no Retry-After, so it is terminal (RateLimit), not
    // retried — see `honors_retry_after_header` for the 429 + Retry-After case.
    for status in [400u16, 401, 402, 403, 413, 415, 429] {
        for batch in [false, true] {
            let server = MockServer::start();
            let mock = server.mock(|when, then| {
                when.method(POST);
                then.status(status);
            });

            let client = create_v0_client(server.base_url(), 3).await;
            let result = capture(&client, batch).await;

            assert!(result.is_err(), "status {} should be terminal", status);
            mock.assert_hits(1);
        }
    }
}

#[tokio::test]
async fn success_sends_once() {
    for batch in [false, true] {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST);
            then.status(200);
        });

        let client = create_v0_client(server.base_url(), 3).await;
        capture(&client, batch).await.unwrap();
        mock.assert_hits(1);
    }
}

#[tokio::test]
async fn retries_resend_identical_event() {
    let server = MockServer::start();
    // The mock only matches requests carrying the original UUID, so reaching
    // 3 hits proves every retry resent the same event identity.
    let mock = server.mock(|when, then| {
        when.method(POST).body_contains(FIXED_UUID);
        then.status(503);
    });

    let client = create_v0_client(server.base_url(), 3).await;
    let mut event = Event::new("e", "user-1");
    event.set_uuid(uuid::Uuid::parse_str(FIXED_UUID).unwrap());
    let _ = client.capture(event).await;

    mock.assert_hits(3);
}

/// Matcher: the request body is valid gzip that decodes to the expected event.
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
async fn gzip_sets_header_query_param_and_compresses_body() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .header("content-encoding", "gzip")
            .query_param("compression", "gzip")
            .matches(body_gunzips_to_user1);
        then.status(200);
    });

    let options = ClientOptionsBuilder::default()
        .api_key("phc_test_token".to_string())
        .host(server.base_url())
        .capture_compression(CaptureCompression::Gzip)
        .build()
        .unwrap();
    let client = posthog_rs::client(options).await;
    client
        .capture(Event::new("test_event", "user1"))
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn honors_retry_after_header() {
    let server = MockServer::start();
    // Mirrors the contract's `respects_retry_after_header`: a 429 carrying
    // Retry-After must delay the resend by the header value, not the (tiny)
    // exponential backoff.
    let mock = server.mock(|when, then| {
        when.method(POST);
        then.status(429).header("retry-after", "1");
    });

    let client = create_v0_client(server.base_url(), 2).await;
    let start = Instant::now();
    let _ = client.capture(Event::new("e", "user-1")).await;
    let elapsed = start.elapsed();

    mock.assert_hits(2);
    // Exponential backoff here would be ~1ms; only an honored Retry-After: 1
    // produces a gap this large.
    assert!(
        elapsed >= Duration::from_millis(900),
        "Retry-After header not honored: waited only {:?}",
        elapsed
    );
}
