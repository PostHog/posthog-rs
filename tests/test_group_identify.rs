//! Coverage for `Client::group_identify`, the `$groupidentify` helper.
//!
//! The emitted event keeps its group properties in the event properties map;
//! nothing is lifted into the V1 `options` object.
//!
//! Capture is a non-blocking enqueue drained by the background worker, so every
//! test `flush()`es before asserting.

use httpmock::prelude::*;
use serde::Serialize;
use serde_json::{json, Value};

const CAPTURE_PATH: &str = "/i/v1/analytics/events";

const GROUP_TYPE: &str = "company";
const GROUP_KEY: &str = "company_id_in_your_db";

#[derive(Serialize)]
struct CompanyProperties {
    name: String,
    employees: u32,
}

/// V1 reports per-event status in `results`; an empty map is a clean accept and
/// keeps the client to a single attempt.
fn ok_response() -> Value {
    json!({ "results": {} })
}

fn group_identify_event(body: &Value) -> Option<&Value> {
    body.get("batch")?
        .as_array()?
        .iter()
        .find(|event| event.get("event").and_then(Value::as_str) == Some("$groupidentify"))
}

/// The event is attributed to `$group_type_group_key` at the top level, and
/// `$group_type`, `$group_key`, and `$group_set` are set in `properties`.
fn request_has_well_formed_group_identify_event(req: &HttpMockRequest) -> bool {
    let Ok(body) = serde_json::from_slice::<Value>(req.body_ref()) else {
        return false;
    };
    let Some(event) = group_identify_event(&body) else {
        return false;
    };

    let expected_distinct_id = format!("${GROUP_TYPE}_{GROUP_KEY}");

    event.get("distinct_id").and_then(Value::as_str) == Some(&expected_distinct_id)
        && event
            .pointer("/properties/$group_type")
            .and_then(Value::as_str)
            == Some(GROUP_TYPE)
        && event
            .pointer("/properties/$group_key")
            .and_then(Value::as_str)
            == Some(GROUP_KEY)
        && event
            .pointer("/properties/$group_set/name")
            .and_then(Value::as_str)
            == Some("Awesome Inc.")
        && event
            .pointer("/properties/$group_set/employees")
            .and_then(Value::as_u64)
            == Some(11)
}

/// Group identification is personful, so the helper must not stamp the opt-out that
/// `Event::new_anon` sets.
fn request_does_not_disable_person_processing(req: &HttpMockRequest) -> bool {
    let Ok(body) = serde_json::from_slice::<Value>(req.body_ref()) else {
        return false;
    };
    let Some(event) = group_identify_event(&body) else {
        return false;
    };

    event
        .pointer("/properties/$process_person_profile")
        .and_then(Value::as_bool)
        != Some(false)
}

/// PostHog deduplicates on event UUID, so a constructor that reused one would
/// have every group identify after the first silently dropped server-side.
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

const BLANK_KEYS: [&str; 2] = ["", "   "];

#[cfg(not(feature = "async-client"))]
mod blocking {
    use super::*;

    fn create_test_client(base_url: String) -> posthog_rs::Client {
        let options: posthog_rs::ClientOptions = ("test_api_key", base_url.as_str()).into();
        posthog_rs::client(options)
    }

    #[test]
    fn group_identify_sends_groupidentify_event() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .matches(request_has_well_formed_group_identify_event)
                .matches(request_does_not_disable_person_processing);
            then.status(200).json_body(ok_response());
        });

        let client = create_test_client(server.base_url());
        client
            .group_identify(
                GROUP_TYPE,
                GROUP_KEY,
                json!({
                    "name": "Awesome Inc.",
                    "employees": 11,
                }),
            )
            .unwrap();
        client.flush();

        capture_mock.assert();
    }

    #[test]
    fn group_identify_accepts_owned_and_borrowed_keys_and_struct_properties() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .matches(request_has_well_formed_group_identify_event);
            then.status(200).json_body(ok_response());
        });

        let client = create_test_client(server.base_url());
        client
            .group_identify(
                GROUP_TYPE,
                String::from(GROUP_KEY),
                CompanyProperties {
                    name: "Awesome Inc.".to_string(),
                    employees: 11,
                },
            )
            .unwrap();
        client.flush();

        capture_mock.assert();
    }

    #[test]
    fn repeated_group_identifies_get_distinct_event_uuids() {
        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path(CAPTURE_PATH)
                .matches(request_has_two_distinct_event_uuids);
            then.status(200).json_body(ok_response());
        });

        let client = create_test_client(server.base_url());
        client
            .group_identify(GROUP_TYPE, GROUP_KEY, json!({ "name": "Awesome Inc." }))
            .unwrap();
        client
            .group_identify("organization", "org_99", json!({ "name": "Org 99" }))
            .unwrap();
        client.flush();

        capture_mock.assert();
    }

    #[test]
    fn group_identify_with_blank_group_type_is_dropped() {
        for blank in BLANK_KEYS {
            let server = MockServer::start();
            let capture_mock = server.mock(|when, then| {
                when.method(POST).path(CAPTURE_PATH);
                then.status(200).json_body(ok_response());
            });

            let client = create_test_client(server.base_url());
            client
                .group_identify(blank, GROUP_KEY, json!({ "name": "Awesome Inc." }))
                .unwrap();
            client.flush();

            capture_mock.assert_hits(0);
        }
    }

    #[test]
    fn group_identify_with_blank_group_key_is_dropped() {
        for blank in BLANK_KEYS {
            let server = MockServer::start();
            let capture_mock = server.mock(|when, then| {
                when.method(POST).path(CAPTURE_PATH);
                then.status(200).json_body(ok_response());
            });

            let client = create_test_client(server.base_url());
            client
                .group_identify(GROUP_TYPE, blank, json!({ "name": "Awesome Inc." }))
                .unwrap();
            client.flush();

            capture_mock.assert_hits(0);
        }
    }

    #[test]
    fn group_identify_is_noop_for_disabled_client() {
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
        client
            .group_identify(GROUP_TYPE, GROUP_KEY, json!({ "name": "Awesome Inc." }))
            .unwrap();
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
    async fn group_identify_sends_groupidentify_event() {
        let server = MockServer::start_async().await;
        let capture_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(CAPTURE_PATH)
                    .matches(request_has_well_formed_group_identify_event)
                    .matches(request_does_not_disable_person_processing);
                then.status(200).json_body(ok_response());
            })
            .await;

        let client = create_test_client(server.base_url()).await;
        client
            .group_identify(
                GROUP_TYPE,
                GROUP_KEY,
                json!({
                    "name": "Awesome Inc.",
                    "employees": 11,
                }),
            )
            .unwrap();
        client.flush().await;

        capture_mock.assert_async().await;
    }

    #[tokio::test]
    async fn group_identify_accepts_owned_and_borrowed_keys_and_struct_properties() {
        let server = MockServer::start_async().await;
        let capture_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(CAPTURE_PATH)
                    .matches(request_has_well_formed_group_identify_event);
                then.status(200).json_body(ok_response());
            })
            .await;

        let client = create_test_client(server.base_url()).await;
        client
            .group_identify(
                GROUP_TYPE,
                String::from(GROUP_KEY),
                CompanyProperties {
                    name: "Awesome Inc.".to_string(),
                    employees: 11,
                },
            )
            .unwrap();
        client.flush().await;

        capture_mock.assert_async().await;
    }

    #[tokio::test]
    async fn repeated_group_identifies_get_distinct_event_uuids() {
        let server = MockServer::start_async().await;
        let capture_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(CAPTURE_PATH)
                    .matches(request_has_two_distinct_event_uuids);
                then.status(200).json_body(ok_response());
            })
            .await;

        let client = create_test_client(server.base_url()).await;
        client
            .group_identify(GROUP_TYPE, GROUP_KEY, json!({ "name": "Awesome Inc." }))
            .unwrap();
        client
            .group_identify("organization", "org_99", json!({ "name": "Org 99" }))
            .unwrap();
        client.flush().await;

        capture_mock.assert_async().await;
    }

    #[tokio::test]
    async fn group_identify_with_blank_group_type_is_dropped() {
        for blank in BLANK_KEYS {
            let server = MockServer::start_async().await;
            let capture_mock = server
                .mock_async(|when, then| {
                    when.method(POST).path(CAPTURE_PATH);
                    then.status(200).json_body(ok_response());
                })
                .await;

            let client = create_test_client(server.base_url()).await;
            client
                .group_identify(blank, GROUP_KEY, json!({ "name": "Awesome Inc." }))
                .unwrap();
            client.flush().await;

            capture_mock.assert_hits_async(0).await;
        }
    }

    #[tokio::test]
    async fn group_identify_with_blank_group_key_is_dropped() {
        for blank in BLANK_KEYS {
            let server = MockServer::start_async().await;
            let capture_mock = server
                .mock_async(|when, then| {
                    when.method(POST).path(CAPTURE_PATH);
                    then.status(200).json_body(ok_response());
                })
                .await;

            let client = create_test_client(server.base_url()).await;
            client
                .group_identify(GROUP_TYPE, blank, json!({ "name": "Awesome Inc." }))
                .unwrap();
            client.flush().await;

            capture_mock.assert_hits_async(0).await;
        }
    }

    #[tokio::test]
    async fn group_identify_is_noop_for_disabled_client() {
        let server = MockServer::start_async().await;
        let capture_mock = server
            .mock_async(|when, then| {
                when.method(POST).path(CAPTURE_PATH);
                then.status(200).json_body(ok_response());
            })
            .await;

        let options = posthog_rs::ClientOptionsBuilder::default()
            .host(server.base_url())
            .build()
            .unwrap();
        assert!(options.is_disabled());

        let client = posthog_rs::client(options).await;
        client
            .group_identify(GROUP_TYPE, GROUP_KEY, json!({ "name": "Awesome Inc." }))
            .unwrap();
        client.flush().await;

        capture_mock.assert_hits_async(0).await;
    }
}
