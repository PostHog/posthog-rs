mod common;

use common::default_user_agent;
use httpmock::prelude::*;
use posthog_rs::{ClientOptionsBuilder, Error};
use serde_json::{json, Value};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const FLAG_PATH: &str = "/api/projects/@current/feature_flags/test-flag/remote_config";

#[cfg(feature = "capture-v1")]
const CAPTURE_PATH: &str = "/i/v1/analytics/events";
#[cfg(not(feature = "capture-v1"))]
const CAPTURE_PATH: &str = "/batch/";

fn builder(host: String) -> ClientOptionsBuilder {
    let mut builder = ClientOptionsBuilder::default();
    builder
        .api_key("test_api_key".to_string())
        .host(host)
        .secret_key("phx_test_secret");
    builder
}

fn payload_mock<'a>(server: &'a MockServer, body: &str) -> httpmock::Mock<'a> {
    let body = body.to_string();
    server.mock(move |when, then| {
        when.method(GET).path(FLAG_PATH);
        then.status(200)
            .header("content-type", "application/json")
            .body(body);
    })
}

fn status_mock<'a>(server: &'a MockServer, status: u16, body: &str) -> httpmock::Mock<'a> {
    let body = body.to_string();
    server.mock(move |when, then| {
        when.method(GET).path(FLAG_PATH);
        then.status(status).body(body);
    })
}

fn catch_all_mock(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.any_request();
        then.status(200).body("{}");
    })
}

fn path_mock<'a>(server: &'a MockServer, expected_path: String) -> httpmock::Mock<'a> {
    server.mock(move |when, then| {
        when.method(GET)
            .is_true(move |request| request.uri().path() == expected_path)
            .is_true(|request| request.uri().query() == Some("token=test_api_key"));
        then.status(200).body("{}");
    })
}

fn unused_local_host() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

const RESERVED_KEYS: [(&str, &str); 6] = [
    ("a/b", "a%2Fb"),
    ("a?b=1", "a%3Fb=1"),
    ("a#b", "a%23b"),
    ("50%", "50%25"),
    ("sp ace", "sp%20ace"),
    ("é", "%C3%A9"),
];

// ---------- blocking ----------

#[cfg(not(feature = "async-client"))]
mod blocking {
    use super::*;

    fn create_client(options: posthog_rs::ClientOptions) -> posthog_rs::Client {
        posthog_rs::client(options)
    }

    #[test]
    fn returns_object_payload() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path(FLAG_PATH)
                .query_param("token", "test_api_key")
                .header("authorization", "Bearer phx_test_secret")
                .header("user-agent", default_user_agent());
            then.status(200).json_body(json!({"foo": ["bar", "baz"]}));
        });
        let client = create_client(builder(server.base_url()).build().unwrap());

        let payload = client.get_remote_config_payload("test-flag").unwrap();

        assert_eq!(payload, Some(json!({"foo": ["bar", "baz"]})));
        mock.assert_calls(1);
    }

    #[test]
    fn returns_empty_object() {
        let server = MockServer::start();
        let mock = payload_mock(&server, "{}");
        let client = create_client(builder(server.base_url()).build().unwrap());

        let payload = client.get_remote_config_payload("test-flag").unwrap();

        assert_eq!(payload, Some(json!({})));
        mock.assert_calls(1);
    }

    #[test]
    fn decodes_double_encoded_string_body() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path(FLAG_PATH);
            then.status(200)
                .json_body(Value::String(r#"{"foo":["bar","baz"]}"#.to_string()));
        });
        let client = create_client(builder(server.base_url()).build().unwrap());

        let payload = client.get_remote_config_payload("test-flag").unwrap();

        assert_eq!(payload, Some(json!({"foo": ["bar", "baz"]})));
        mock.assert_calls(1);
    }

    #[test]
    fn returns_plain_string_when_second_parse_fails() {
        let server = MockServer::start();
        let mock = payload_mock(&server, r#""not json""#);
        let client = create_client(builder(server.base_url()).build().unwrap());

        let payload = client.get_remote_config_payload("test-flag").unwrap();

        assert_eq!(payload, Some(Value::String("not json".to_string())));
        mock.assert_calls(1);
    }

    #[test]
    fn json_null_body_is_some_null() {
        let server = MockServer::start();
        let mock = payload_mock(&server, "null");
        let client = create_client(builder(server.base_url()).build().unwrap());

        let payload = client.get_remote_config_payload("test-flag").unwrap();

        assert_eq!(payload, Some(Value::Null));
        mock.assert_calls(1);
    }

    #[test]
    fn invalid_json_body_is_serialization_error() {
        let server = MockServer::start();
        let mock = payload_mock(&server, "not json at all");
        let client = create_client(builder(server.base_url()).build().unwrap());

        let err = client.get_remote_config_payload("test-flag").unwrap_err();

        assert!(matches!(err, Error::Serialization(_)), "{:?}", err);
        mock.assert_calls(1);
    }

    #[test]
    fn sends_request_with_expected_path_query_and_headers() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .is_true(|request| request.uri().path() == FLAG_PATH)
                .is_true(|request| request.uri().query() == Some("token=test_api_key"))
                .header("authorization", "Bearer phx_test_secret")
                .header("user-agent", default_user_agent())
                .header_missing("content-type");
            then.status(200).body("{}");
        });
        let client = create_client(builder(server.base_url()).build().unwrap());

        client.get_remote_config_payload("test-flag").unwrap();

        mock.assert_calls(1);
    }

    #[test]
    fn flag_key_with_reserved_characters_is_one_path_segment() {
        for (key, encoded) in RESERVED_KEYS {
            let server = MockServer::start();
            let mock = path_mock(
                &server,
                format!("/api/projects/@current/feature_flags/{encoded}/remote_config"),
            );
            let client = create_client(builder(server.base_url()).build().unwrap());

            let payload = client.get_remote_config_payload(key).unwrap();

            assert_eq!(payload, Some(json!({})), "key {:?}", key);
            mock.assert_calls(1);
        }
    }

    #[test]
    fn empty_and_dot_flag_keys_are_rejected_without_a_request() {
        let server = MockServer::start();
        let mock = catch_all_mock(&server);
        let client = create_client(builder(server.base_url()).build().unwrap());

        for key in ["", ".", ".."] {
            let err = client.get_remote_config_payload(key).unwrap_err();
            assert!(
                matches!(err, Error::BadRequest(_)),
                "key {:?}: {:?}",
                key,
                err
            );
        }

        mock.assert_calls(0);
    }

    #[test]
    fn host_path_prefix_is_kept() {
        for suffix in ["/proxy", "/proxy/"] {
            let server = MockServer::start();
            let mock = server.mock(|when, then| {
                when.method(GET)
                    .is_true(|request| {
                        request.uri().path()
                            == "/proxy/api/projects/@current/feature_flags/test-flag/remote_config"
                    })
                    .query_param("token", "test_api_key");
                then.status(200).body("{}");
            });
            let host = format!("{}{suffix}", server.base_url());
            let client = create_client(builder(host).build().unwrap());

            let payload = client.get_remote_config_payload("test-flag").unwrap();

            assert_eq!(payload, Some(json!({})), "host suffix {:?}", suffix);
            mock.assert_calls(1);
        }
    }

    #[test]
    fn missing_secret_key_returns_unauthorized_without_a_request() {
        let server = MockServer::start();
        let mock = catch_all_mock(&server);
        let options = ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .build()
            .unwrap();
        let client = create_client(options);

        let err = client.get_remote_config_payload("test-flag").unwrap_err();

        assert!(matches!(err, Error::Unauthorized), "{:?}", err);
        mock.assert_calls(0);
    }

    #[test]
    fn blank_secret_key_returns_unauthorized() {
        let server = MockServer::start();
        let mock = catch_all_mock(&server);
        let options = builder(server.base_url()).secret_key("  ").build().unwrap();
        let client = create_client(options);

        let err = client.get_remote_config_payload("test-flag").unwrap_err();

        assert!(matches!(err, Error::Unauthorized), "{:?}", err);
        mock.assert_calls(0);
    }

    #[test]
    fn disabled_client_returns_none_without_a_request() {
        let server = MockServer::start();
        let mock = catch_all_mock(&server);
        let options = builder(server.base_url()).disabled(true).build().unwrap();
        let client = create_client(options);

        let payload = client.get_remote_config_payload("test-flag").unwrap();

        assert_eq!(payload, None);
        mock.assert_calls(0);
    }

    #[test]
    fn disabled_check_runs_before_secret_key_check() {
        let server = MockServer::start();
        let mock = catch_all_mock(&server);
        let options = ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .disabled(true)
            .build()
            .unwrap();
        let client = create_client(options);

        let payload = client.get_remote_config_payload("test-flag").unwrap();

        assert_eq!(payload, None);
        mock.assert_calls(0);
    }

    #[test]
    fn blank_project_token_returns_none_without_a_request() {
        let server = MockServer::start();
        let mock = catch_all_mock(&server);
        let options = builder(server.base_url())
            .api_key("  ".to_string())
            .build()
            .unwrap();
        let client = create_client(options);

        let payload = client.get_remote_config_payload("test-flag").unwrap();

        assert_eq!(payload, None);
        mock.assert_calls(0);
    }

    #[test]
    fn status_401_returns_unauthorized() {
        let server = MockServer::start();
        let mock = status_mock(&server, 401, "nope");
        let client = create_client(builder(server.base_url()).build().unwrap());

        let err = client.get_remote_config_payload("test-flag").unwrap_err();

        assert!(matches!(err, Error::Unauthorized), "{:?}", err);
        mock.assert_calls(1);
    }

    #[test]
    fn status_404_returns_connection_error() {
        let server = MockServer::start();
        let mock = status_mock(&server, 404, "missing");
        let client = create_client(builder(server.base_url()).build().unwrap());

        let err = client.get_remote_config_payload("test-flag").unwrap_err();

        match err {
            Error::Connection(message) => assert!(message.contains("404"), "{}", message),
            other => panic!("expected Connection, got {:?}", other),
        }
        mock.assert_calls(1);
    }

    #[test]
    fn status_429_returns_rate_limit() {
        let server = MockServer::start();
        let mock = status_mock(&server, 429, "slow down");
        let client = create_client(builder(server.base_url()).build().unwrap());

        let err = client.get_remote_config_payload("test-flag").unwrap_err();

        assert!(matches!(err, Error::RateLimit), "{:?}", err);
        mock.assert_calls(1);
    }

    #[test]
    fn status_500_returns_server_error_and_does_not_retry() {
        let server = MockServer::start();
        let mock = status_mock(&server, 500, "boom");
        let options = builder(server.base_url())
            .feature_flags_request_max_retries(3u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = create_client(options);

        let err = client.get_remote_config_payload("test-flag").unwrap_err();

        match err {
            Error::ServerError { status, message } => {
                assert_eq!(status, 500);
                assert_eq!(message, "boom");
            }
            other => panic!("expected ServerError, got {:?}", other),
        }
        mock.assert_calls(1);
    }

    #[test]
    fn status_502_and_504_are_not_retried() {
        for status in [502u16, 504] {
            let server = MockServer::start();
            let mock = status_mock(&server, status, "gateway");
            let options = builder(server.base_url())
                .feature_flags_request_max_retries(3u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(1u64)
                .build()
                .unwrap();
            let client = create_client(options);

            let err = client.get_remote_config_payload("test-flag").unwrap_err();

            match err {
                Error::ServerError { status: got, .. } => assert_eq!(got, status),
                other => panic!("expected ServerError for {}, got {:?}", status, other),
            }
            mock.assert_calls(1);
        }
    }

    #[test]
    fn transport_error_returns_connection_error() {
        let options = builder(unused_local_host())
            .feature_flags_request_max_retries(3u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = create_client(options);

        let err = client.get_remote_config_payload("test-flag").unwrap_err();

        match err {
            Error::Connection(message) => {
                assert!(!message.contains("test_api_key"), "{}", message);
                assert!(!message.contains("remote_config"), "{}", message);
            }
            other => panic!("expected Connection, got {:?}", other),
        }
    }

    #[test]
    fn timeout_uses_feature_flags_request_timeout() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path(FLAG_PATH);
            then.status(200)
                .delay(Duration::from_millis(1500))
                .body("{}");
        });
        let options = builder(server.base_url())
            .feature_flags_request_timeout_seconds(1u64)
            .request_timeout_seconds(30u64)
            .build()
            .unwrap();
        let client = create_client(options);

        let started = Instant::now();
        let err = client.get_remote_config_payload("test-flag").unwrap_err();

        assert!(matches!(err, Error::Connection(_)), "{:?}", err);
        assert!(started.elapsed() < Duration::from_millis(1400));
    }

    #[test]
    fn sends_no_capture_event() {
        let server = MockServer::start();
        let payload_mock = payload_mock(&server, "{}");
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path(CAPTURE_PATH);
            then.status(200).json_body(json!({"results": {}}));
        });
        let client = create_client(builder(server.base_url()).build().unwrap());

        client.get_remote_config_payload("test-flag").unwrap();
        client.flush();

        payload_mock.assert_calls(1);
        capture_mock.assert_calls(0);
    }

    #[test]
    fn on_error_hook_is_not_called_on_failure() {
        let server = MockServer::start();
        let mock = status_mock(&server, 500, "boom");
        let hook_calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hook_calls);
        let options = builder(server.base_url())
            .on_error(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .build()
            .unwrap();
        let client = create_client(options);

        let result = client.get_remote_config_payload("test-flag");

        assert!(result.is_err());
        assert_eq!(hook_calls.load(Ordering::SeqCst), 0);
        mock.assert_calls(1);
    }

    #[test]
    fn works_without_local_evaluation() {
        let server = MockServer::start();
        let mock = payload_mock(&server, r#"{"enabled":true}"#);
        let definitions_mock = server.mock(|when, then| {
            when.path_includes("/flags/definitions");
            then.status(200).body("{}");
        });
        let options = builder(server.base_url())
            .enable_local_evaluation(false)
            .build()
            .unwrap();
        let client = create_client(options);

        let payload = client.get_remote_config_payload("test-flag").unwrap();

        assert_eq!(payload, Some(json!({"enabled": true})));
        mock.assert_calls(1);
        definitions_mock.assert_calls(0);
    }
}

// ---------- async ----------

#[cfg(feature = "async-client")]
mod async_tests {
    use super::*;

    async fn create_client(options: posthog_rs::ClientOptions) -> posthog_rs::Client {
        posthog_rs::client(options).await
    }

    #[tokio::test]
    async fn returns_object_payload() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path(FLAG_PATH)
                .query_param("token", "test_api_key")
                .header("authorization", "Bearer phx_test_secret")
                .header("user-agent", default_user_agent());
            then.status(200).json_body(json!({"foo": ["bar", "baz"]}));
        });
        let client = create_client(builder(server.base_url()).build().unwrap()).await;

        let payload = client.get_remote_config_payload("test-flag").await.unwrap();

        assert_eq!(payload, Some(json!({"foo": ["bar", "baz"]})));
        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn returns_empty_object() {
        let server = MockServer::start();
        let mock = payload_mock(&server, "{}");
        let client = create_client(builder(server.base_url()).build().unwrap()).await;

        let payload = client.get_remote_config_payload("test-flag").await.unwrap();

        assert_eq!(payload, Some(json!({})));
        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn decodes_double_encoded_string_body() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path(FLAG_PATH);
            then.status(200)
                .json_body(Value::String(r#"{"foo":["bar","baz"]}"#.to_string()));
        });
        let client = create_client(builder(server.base_url()).build().unwrap()).await;

        let payload = client.get_remote_config_payload("test-flag").await.unwrap();

        assert_eq!(payload, Some(json!({"foo": ["bar", "baz"]})));
        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn returns_plain_string_when_second_parse_fails() {
        let server = MockServer::start();
        let mock = payload_mock(&server, r#""not json""#);
        let client = create_client(builder(server.base_url()).build().unwrap()).await;

        let payload = client.get_remote_config_payload("test-flag").await.unwrap();

        assert_eq!(payload, Some(Value::String("not json".to_string())));
        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn json_null_body_is_some_null() {
        let server = MockServer::start();
        let mock = payload_mock(&server, "null");
        let client = create_client(builder(server.base_url()).build().unwrap()).await;

        let payload = client.get_remote_config_payload("test-flag").await.unwrap();

        assert_eq!(payload, Some(Value::Null));
        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn invalid_json_body_is_serialization_error() {
        let server = MockServer::start();
        let mock = payload_mock(&server, "not json at all");
        let client = create_client(builder(server.base_url()).build().unwrap()).await;

        let err = client
            .get_remote_config_payload("test-flag")
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Serialization(_)), "{:?}", err);
        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn sends_request_with_expected_path_query_and_headers() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .is_true(|request| request.uri().path() == FLAG_PATH)
                .is_true(|request| request.uri().query() == Some("token=test_api_key"))
                .header("authorization", "Bearer phx_test_secret")
                .header("user-agent", default_user_agent())
                .header_missing("content-type");
            then.status(200).body("{}");
        });
        let client = create_client(builder(server.base_url()).build().unwrap()).await;

        client.get_remote_config_payload("test-flag").await.unwrap();

        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn flag_key_with_reserved_characters_is_one_path_segment() {
        for (key, encoded) in RESERVED_KEYS {
            let server = MockServer::start();
            let mock = path_mock(
                &server,
                format!("/api/projects/@current/feature_flags/{encoded}/remote_config"),
            );
            let client = create_client(builder(server.base_url()).build().unwrap()).await;

            let payload = client.get_remote_config_payload(key).await.unwrap();

            assert_eq!(payload, Some(json!({})), "key {:?}", key);
            mock.assert_calls(1);
        }
    }

    #[tokio::test]
    async fn empty_and_dot_flag_keys_are_rejected_without_a_request() {
        let server = MockServer::start();
        let mock = catch_all_mock(&server);
        let client = create_client(builder(server.base_url()).build().unwrap()).await;

        for key in ["", ".", ".."] {
            let err = client.get_remote_config_payload(key).await.unwrap_err();
            assert!(
                matches!(err, Error::BadRequest(_)),
                "key {:?}: {:?}",
                key,
                err
            );
        }

        mock.assert_calls(0);
    }

    #[tokio::test]
    async fn host_path_prefix_is_kept() {
        for suffix in ["/proxy", "/proxy/"] {
            let server = MockServer::start();
            let mock = server.mock(|when, then| {
                when.method(GET)
                    .is_true(|request| {
                        request.uri().path()
                            == "/proxy/api/projects/@current/feature_flags/test-flag/remote_config"
                    })
                    .query_param("token", "test_api_key");
                then.status(200).body("{}");
            });
            let host = format!("{}{suffix}", server.base_url());
            let client = create_client(builder(host).build().unwrap()).await;

            let payload = client.get_remote_config_payload("test-flag").await.unwrap();

            assert_eq!(payload, Some(json!({})), "host suffix {:?}", suffix);
            mock.assert_calls(1);
        }
    }

    #[tokio::test]
    async fn missing_secret_key_returns_unauthorized_without_a_request() {
        let server = MockServer::start();
        let mock = catch_all_mock(&server);
        let options = ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .build()
            .unwrap();
        let client = create_client(options).await;

        let err = client
            .get_remote_config_payload("test-flag")
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Unauthorized), "{:?}", err);
        mock.assert_calls(0);
    }

    #[tokio::test]
    async fn blank_secret_key_returns_unauthorized() {
        let server = MockServer::start();
        let mock = catch_all_mock(&server);
        let options = builder(server.base_url()).secret_key("  ").build().unwrap();
        let client = create_client(options).await;

        let err = client
            .get_remote_config_payload("test-flag")
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Unauthorized), "{:?}", err);
        mock.assert_calls(0);
    }

    #[tokio::test]
    async fn disabled_client_returns_none_without_a_request() {
        let server = MockServer::start();
        let mock = catch_all_mock(&server);
        let options = builder(server.base_url()).disabled(true).build().unwrap();
        let client = create_client(options).await;

        let payload = client.get_remote_config_payload("test-flag").await.unwrap();

        assert_eq!(payload, None);
        mock.assert_calls(0);
    }

    #[tokio::test]
    async fn disabled_check_runs_before_secret_key_check() {
        let server = MockServer::start();
        let mock = catch_all_mock(&server);
        let options = ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .disabled(true)
            .build()
            .unwrap();
        let client = create_client(options).await;

        let payload = client.get_remote_config_payload("test-flag").await.unwrap();

        assert_eq!(payload, None);
        mock.assert_calls(0);
    }

    #[tokio::test]
    async fn blank_project_token_returns_none_without_a_request() {
        let server = MockServer::start();
        let mock = catch_all_mock(&server);
        let options = builder(server.base_url())
            .api_key("  ".to_string())
            .build()
            .unwrap();
        let client = create_client(options).await;

        let payload = client.get_remote_config_payload("test-flag").await.unwrap();

        assert_eq!(payload, None);
        mock.assert_calls(0);
    }

    #[tokio::test]
    async fn status_401_returns_unauthorized() {
        let server = MockServer::start();
        let mock = status_mock(&server, 401, "nope");
        let client = create_client(builder(server.base_url()).build().unwrap()).await;

        let err = client
            .get_remote_config_payload("test-flag")
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Unauthorized), "{:?}", err);
        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn status_404_returns_connection_error() {
        let server = MockServer::start();
        let mock = status_mock(&server, 404, "missing");
        let client = create_client(builder(server.base_url()).build().unwrap()).await;

        let err = client
            .get_remote_config_payload("test-flag")
            .await
            .unwrap_err();

        match err {
            Error::Connection(message) => assert!(message.contains("404"), "{}", message),
            other => panic!("expected Connection, got {:?}", other),
        }
        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn status_429_returns_rate_limit() {
        let server = MockServer::start();
        let mock = status_mock(&server, 429, "slow down");
        let client = create_client(builder(server.base_url()).build().unwrap()).await;

        let err = client
            .get_remote_config_payload("test-flag")
            .await
            .unwrap_err();

        assert!(matches!(err, Error::RateLimit), "{:?}", err);
        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn status_500_returns_server_error_and_does_not_retry() {
        let server = MockServer::start();
        let mock = status_mock(&server, 500, "boom");
        let options = builder(server.base_url())
            .feature_flags_request_max_retries(3u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = create_client(options).await;

        let err = client
            .get_remote_config_payload("test-flag")
            .await
            .unwrap_err();

        match err {
            Error::ServerError { status, message } => {
                assert_eq!(status, 500);
                assert_eq!(message, "boom");
            }
            other => panic!("expected ServerError, got {:?}", other),
        }
        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn status_502_and_504_are_not_retried() {
        for status in [502u16, 504] {
            let server = MockServer::start();
            let mock = status_mock(&server, status, "gateway");
            let options = builder(server.base_url())
                .feature_flags_request_max_retries(3u32)
                .retry_initial_backoff_ms(1u64)
                .retry_max_backoff_ms(1u64)
                .build()
                .unwrap();
            let client = create_client(options).await;

            let err = client
                .get_remote_config_payload("test-flag")
                .await
                .unwrap_err();

            match err {
                Error::ServerError { status: got, .. } => assert_eq!(got, status),
                other => panic!("expected ServerError for {}, got {:?}", status, other),
            }
            mock.assert_calls(1);
        }
    }

    #[tokio::test]
    async fn transport_error_returns_connection_error() {
        let options = builder(unused_local_host())
            .feature_flags_request_max_retries(3u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(1u64)
            .build()
            .unwrap();
        let client = create_client(options).await;

        let err = client
            .get_remote_config_payload("test-flag")
            .await
            .unwrap_err();

        match err {
            Error::Connection(message) => {
                assert!(!message.contains("test_api_key"), "{}", message);
                assert!(!message.contains("remote_config"), "{}", message);
            }
            other => panic!("expected Connection, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn timeout_uses_feature_flags_request_timeout() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path(FLAG_PATH);
            then.status(200)
                .delay(Duration::from_millis(1500))
                .body("{}");
        });
        let options = builder(server.base_url())
            .feature_flags_request_timeout_seconds(1u64)
            .request_timeout_seconds(30u64)
            .build()
            .unwrap();
        let client = create_client(options).await;

        let started = Instant::now();
        let err = client
            .get_remote_config_payload("test-flag")
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Connection(_)), "{:?}", err);
        assert!(started.elapsed() < Duration::from_millis(1400));
    }

    #[tokio::test]
    async fn sends_no_capture_event() {
        let server = MockServer::start();
        let payload_mock = payload_mock(&server, "{}");
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path(CAPTURE_PATH);
            then.status(200).json_body(json!({"results": {}}));
        });
        let client = create_client(builder(server.base_url()).build().unwrap()).await;

        client.get_remote_config_payload("test-flag").await.unwrap();
        client.flush().await;

        payload_mock.assert_calls(1);
        capture_mock.assert_calls(0);
    }

    #[tokio::test]
    async fn on_error_hook_is_not_called_on_failure() {
        let server = MockServer::start();
        let mock = status_mock(&server, 500, "boom");
        let hook_calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hook_calls);
        let options = builder(server.base_url())
            .on_error(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .build()
            .unwrap();
        let client = create_client(options).await;

        let result = client.get_remote_config_payload("test-flag").await;

        assert!(result.is_err());
        assert_eq!(hook_calls.load(Ordering::SeqCst), 0);
        mock.assert_calls(1);
    }

    #[tokio::test]
    async fn works_without_local_evaluation() {
        let server = MockServer::start();
        let mock = payload_mock(&server, r#"{"enabled":true}"#);
        let definitions_mock = server.mock(|when, then| {
            when.path_includes("/flags/definitions");
            then.status(200).body("{}");
        });
        let options = builder(server.base_url())
            .enable_local_evaluation(false)
            .build()
            .unwrap();
        let client = create_client(options).await;

        let payload = client.get_remote_config_payload("test-flag").await.unwrap();

        assert_eq!(payload, Some(json!({"enabled": true})));
        mock.assert_calls(1);
        definitions_mock.assert_calls(0);
    }
}
