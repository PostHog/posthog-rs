#![cfg(all(not(feature = "async-client"), not(feature = "capture-v1")))]

//! V0 retry behavior (blocking client) under the background transport. `capture`
//! is a non-blocking enqueue, so these tests drive delivery with `flush()` (each
//! flush makes one attempt per pending batch, ignoring backoff). Exhausting the
//! attempt budget takes `max_attempts` flushes; the Retry-After test instead
//! lets the worker retry on its own schedule so the header delay is observable.

use std::io::Read;
use std::time::{Duration, Instant};

use httpmock::prelude::*;
use posthog_rs::{CaptureCompression, Client, ClientOptionsBuilder, Event};

// A real v7 UUID so the event passes through unchanged; matched on the wire to
// prove retried attempts resend the same bytes.
const FIXED_UUID: &str = "01920000-0000-7000-8000-0000000000ff";

fn create_v0_client(base_url: String, max_attempts: u32) -> Client {
    let options = ClientOptionsBuilder::default()
        .api_key("phc_test_token".to_string())
        .host(base_url)
        .max_capture_attempts(max_attempts)
        .retry_initial_backoff_ms(1u64)
        .retry_max_backoff_ms(5u64)
        .build()
        .unwrap();
    posthog_rs::client(options)
}

/// Drive whichever capture entry point the test wants. Both now enqueue onto the
/// same worker and send through `/batch/`.
fn capture(client: &Client, batch: bool) {
    if batch {
        client
            .capture_batch(vec![Event::new("e", "user-1")], false)
            .unwrap();
    } else {
        client.capture(Event::new("e", "user-1")).unwrap();
    }
}

/// Poll until a mock has been hit `want` times (for the autonomous retry path
/// that isn't driven by `flush()`).
fn wait_for_hits(mock: &httpmock::Mock, want: usize) {
    for _ in 0..400 {
        if mock.hits() >= want {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(mock.hits(), want, "timed out waiting for {want} hits");
}

#[test]
fn retryable_status_keeps_retrying_until_attempts_exhausted() {
    for status in [408u16, 500, 502, 503, 504] {
        for batch in [false, true] {
            let server = MockServer::start();
            let mock = server.mock(|when, then| {
                when.method(POST);
                then.status(status);
            });

            let client = create_v0_client(server.base_url(), 3);
            capture(&client, batch);
            client.flush();
            client.flush();
            client.flush();

            mock.assert_hits(3);

            // Budget exhausted: the event is dropped, so a further flush is a no-op.
            client.flush();
            mock.assert_hits(3);
        }
    }
}

#[test]
fn terminal_status_sends_once() {
    // 429 here carries no Retry-After, so it is terminal, not retried — see
    // `honors_retry_after_header` for the 429 + Retry-After case.
    for status in [400u16, 401, 402, 403, 413, 415, 429] {
        for batch in [false, true] {
            let server = MockServer::start();
            let mock = server.mock(|when, then| {
                when.method(POST);
                then.status(status);
            });

            let client = create_v0_client(server.base_url(), 3);
            capture(&client, batch);
            client.flush();
            mock.assert_hits(1);

            // Terminal: dropped, not retried.
            client.flush();
            mock.assert_hits(1);
        }
    }
}

#[test]
fn success_sends_once() {
    for batch in [false, true] {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST);
            then.status(200);
        });

        let client = create_v0_client(server.base_url(), 3);
        capture(&client, batch);
        client.flush();
        mock.assert_hits(1);
    }
}

#[test]
fn retries_resend_identical_event() {
    let server = MockServer::start();
    // The mock only matches requests carrying the original UUID, so reaching
    // 3 hits proves every retry resent the same event identity.
    let mock = server.mock(|when, then| {
        when.method(POST).body_contains(FIXED_UUID);
        then.status(503);
    });

    let client = create_v0_client(server.base_url(), 3);
    let mut event = Event::new("e", "user-1");
    event.set_uuid(uuid::Uuid::parse_str(FIXED_UUID).unwrap());
    client.capture(event).unwrap();
    client.flush();
    client.flush();
    client.flush();

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

#[test]
fn gzip_sets_header_query_param_and_compresses_body() {
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
    let client = posthog_rs::client(options);
    client.capture(Event::new("test_event", "user1")).unwrap();
    client.flush();

    mock.assert();
}

#[test]
fn honors_retry_after_header() {
    let server = MockServer::start();
    // A 429 carrying Retry-After must delay the resend by the header value.
    // `flush()` forces an immediate attempt and would bypass the backoff, so we
    // let the worker retry on its own schedule and observe the gap.
    let mock = server.mock(|when, then| {
        when.method(POST);
        then.status(429).header("retry-after", "1");
    });

    let client = create_v0_client(server.base_url(), 2);
    client.capture(Event::new("e", "user-1")).unwrap();

    let start = Instant::now();
    client.flush();
    mock.assert_hits(1);
    wait_for_hits(&mock, 2);
    let elapsed = start.elapsed();

    mock.assert_hits(2);
    assert!(
        elapsed >= Duration::from_millis(900),
        "Retry-After header not honored: second attempt after only {:?}",
        elapsed
    );
}
