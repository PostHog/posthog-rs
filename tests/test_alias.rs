//! Coverage for `Client::alias`, the `$create_alias` helper.
//!
//! The emitted event is identical on both capture pipelines — nothing in it is
//! lifted into the V1 `options` object — so a single `CAPTURE_PATH` switch
//! covers V0 and V1 rather than the split-file treatment error tracking needs.
//!
//! Capture is a non-blocking enqueue drained by the background worker, so every
//! test `flush()`es before asserting.

use httpmock::prelude::*;
use serde_json::{json, Value};

#[cfg(feature = "capture-v1")]
const CAPTURE_PATH: &str = "/i/v1/analytics/events";
#[cfg(not(feature = "capture-v1"))]
const CAPTURE_PATH: &str = "/batch/";

const PREVIOUS_ID: &str = "anon-abc123";
const DISTINCT_ID: &str = "user-42";

/// V1 reports per-event status in `results`; an empty map is a clean accept and
/// keeps the client to a single attempt. V0 ignores the body entirely.
fn ok_response() -> Value {
    json!({ "results": {} })
}

fn alias_event(body: &Value) -> Option<&Value> {
    body.get("batch")?
        .as_array()?
        .iter()
        .find(|event| event.get("event").and_then(Value::as_str) == Some("$create_alias"))
}

/// The event is attributed to the *previous* ID at the top level, and that ID is
/// mirrored into `properties.distinct_id` while the merge target goes to
/// `properties.alias`. Both property keys are required: posthog-python,
/// posthog-js-lite, and posthog-php all write the pair, and issue #149's JSON
/// snippet omits `properties.distinct_id`.
fn request_has_well_formed_alias_event(req: &HttpMockRequest) -> bool {
    let Ok(body) = serde_json::from_slice::<Value>(req.body_ref()) else {
        return false;
    };
    let Some(event) = alias_event(&body) else {
        return false;
    };

    event.get("distinct_id").and_then(Value::as_str) == Some(PREVIOUS_ID)
        && event
            .pointer("/properties/distinct_id")
            .and_then(Value::as_str)
            == Some(PREVIOUS_ID)
        && event.pointer("/properties/alias").and_then(Value::as_str) == Some(DISTINCT_ID)
}

/// A merge cannot be personless, so the helper must not stamp the opt-out that
/// `Event::new_anon` sets.
fn request_does_not_disable_person_processing(req: &HttpMockRequest) -> bool {
    let Ok(body) = serde_json::from_slice::<Value>(req.body_ref()) else {
        return false;
    };
    let Some(event) = alias_event(&body) else {
        return false;
    };

    event
        .pointer("/properties/$process_person_profile")
        .and_then(Value::as_bool)
        != Some(false)
}

/// PostHog deduplicates on event UUID, so a constructor that reused one would
/// have every alias after the first silently dropped server-side.
fn request_has_two_distinct_event_uuids(req: &HttpMockRequest) -> bool {
    let Ok(body) = serde_json::from_slice::<Value>(req.body_ref()) else {
        return false;
    };
    let Some(batch) = body.get("batch").and_then(Value::as_array) else {
        return false;
    };
    if batch.len() != 2 {
        return false;
    }

    let uuid_at = |i: usize| {
        batch[i]
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let (first, second) = (uuid_at(0), uuid_at(1));

    const NIL_UUID: &str = "00000000-0000-0000-0000-000000000000";
    !first.is_empty() && first != second && first != NIL_UUID && second != NIL_UUID
}

const BLANK_IDS: [&str; 2] = ["", "   "];

#[cfg(not(feature = "async-client"))]
mod blocking {
    use super::*;

    fn create_test_client(base_url: String) -> posthog_rs::Client {
        let options: posthog_rs::ClientOptions = ("test_api_key", base_url.as_str()).into();
        posthog_rs::client(options)
    }

    #[test]
    fn alias_sends_create_alias_event() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .matches(request_has_well_formed_alias_event)
                .matches(request_does_not_disable_person_processing);
            then.status(200).json_body(ok_response());
        });

        let client = create_test_client(server.base_url());
        client.alias(PREVIOUS_ID, DISTINCT_ID);
        client.flush();

        capture_mock.assert();
    }

    #[test]
    fn alias_accepts_owned_and_borrowed_ids() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .matches(request_has_well_formed_alias_event);
            then.status(200).json_body(ok_response());
        });

        let client = create_test_client(server.base_url());
        // Mixing `&str` and `String` only compiles with two generic parameters.
        client.alias(PREVIOUS_ID, String::from(DISTINCT_ID));
        client.flush();

        capture_mock.assert();
    }

    #[test]
    fn repeated_aliases_get_distinct_event_uuids() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .matches(request_has_two_distinct_event_uuids);
            then.status(200).json_body(ok_response());
        });

        let client = create_test_client(server.base_url());
        client.alias(PREVIOUS_ID, DISTINCT_ID);
        client.alias("anon-def456", "user-99");
        client.flush();

        capture_mock.assert();
    }

    #[test]
    fn alias_with_blank_previous_id_is_dropped() {
        for blank in BLANK_IDS {
            let server = MockServer::start();
            let capture_mock = server.mock(|when, then| {
                when.method(POST).path(CAPTURE_PATH);
                then.status(200).json_body(ok_response());
            });

            let client = create_test_client(server.base_url());
            client.alias(blank, DISTINCT_ID);
            client.flush();

            capture_mock.assert_hits(0);
        }
    }

    #[test]
    fn alias_with_blank_distinct_id_is_dropped() {
        for blank in BLANK_IDS {
            let server = MockServer::start();
            let capture_mock = server.mock(|when, then| {
                when.method(POST).path(CAPTURE_PATH);
                then.status(200).json_body(ok_response());
            });

            let client = create_test_client(server.base_url());
            client.alias(PREVIOUS_ID, blank);
            client.flush();

            capture_mock.assert_hits(0);
        }
    }

    #[test]
    fn alias_is_noop_for_disabled_client() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path(CAPTURE_PATH);
            then.status(200).json_body(ok_response());
        });

        let options = posthog_rs::ClientOptionsBuilder::default()
            .host(server.base_url())
            .build()
            .unwrap();
        assert!(options.is_disabled());

        let client = posthog_rs::client(options);
        client.alias(PREVIOUS_ID, DISTINCT_ID);
        client.flush();

        capture_mock.assert_hits(0);
    }
}

#[cfg(feature = "async-client")]
mod async_client {
    use super::*;

    async fn create_test_client(base_url: String) -> posthog_rs::Client {
        let options: posthog_rs::ClientOptions = ("test_api_key", base_url.as_str()).into();
        posthog_rs::client(options).await
    }

    #[tokio::test]
    async fn alias_sends_create_alias_event() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .matches(request_has_well_formed_alias_event)
                .matches(request_does_not_disable_person_processing);
            then.status(200).json_body(ok_response());
        });

        let client = create_test_client(server.base_url()).await;
        client.alias(PREVIOUS_ID, DISTINCT_ID);
        client.flush().await;

        capture_mock.assert();
    }

    #[tokio::test]
    async fn alias_accepts_owned_and_borrowed_ids() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .matches(request_has_well_formed_alias_event);
            then.status(200).json_body(ok_response());
        });

        let client = create_test_client(server.base_url()).await;
        // Mixing `&str` and `String` only compiles with two generic parameters.
        client.alias(PREVIOUS_ID, String::from(DISTINCT_ID));
        client.flush().await;

        capture_mock.assert();
    }

    #[tokio::test]
    async fn repeated_aliases_get_distinct_event_uuids() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .matches(request_has_two_distinct_event_uuids);
            then.status(200).json_body(ok_response());
        });

        let client = create_test_client(server.base_url()).await;
        client.alias(PREVIOUS_ID, DISTINCT_ID);
        client.alias("anon-def456", "user-99");
        client.flush().await;

        capture_mock.assert();
    }

    #[tokio::test]
    async fn alias_with_blank_previous_id_is_dropped() {
        for blank in BLANK_IDS {
            let server = MockServer::start();
            let capture_mock = server.mock(|when, then| {
                when.method(POST).path(CAPTURE_PATH);
                then.status(200).json_body(ok_response());
            });

            let client = create_test_client(server.base_url()).await;
            client.alias(blank, DISTINCT_ID);
            client.flush().await;

            capture_mock.assert_hits(0);
        }
    }

    #[tokio::test]
    async fn alias_with_blank_distinct_id_is_dropped() {
        for blank in BLANK_IDS {
            let server = MockServer::start();
            let capture_mock = server.mock(|when, then| {
                when.method(POST).path(CAPTURE_PATH);
                then.status(200).json_body(ok_response());
            });

            let client = create_test_client(server.base_url()).await;
            client.alias(PREVIOUS_ID, blank);
            client.flush().await;

            capture_mock.assert_hits(0);
        }
    }

    #[tokio::test]
    async fn alias_is_noop_for_disabled_client() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path(CAPTURE_PATH);
            then.status(200).json_body(ok_response());
        });

        let options = posthog_rs::ClientOptionsBuilder::default()
            .host(server.base_url())
            .build()
            .unwrap();
        assert!(options.is_disabled());

        let client = posthog_rs::client(options).await;
        client.alias(PREVIOUS_ID, DISTINCT_ID);
        client.flush().await;

        capture_mock.assert_hits(0);
    }
}
