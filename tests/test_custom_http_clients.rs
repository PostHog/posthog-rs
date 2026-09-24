#![allow(deprecated)]

use httpmock::prelude::*;
use posthog_rs::{ClientOptionsBuilder, Event};
use reqwest::header::{HeaderMap, HeaderValue};
use std::time::Duration;

fn headers(value: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-custom-client", HeaderValue::from_static(value));
    headers
}

fn options(server: &MockServer) -> ClientOptionsBuilder {
    let mut builder = ClientOptionsBuilder::default();
    builder
        .api_key("phc_test".to_string())
        .host(server.base_url())
        .request_timeout_seconds(0)
        .feature_flags_request_timeout_seconds(5)
        .feature_flags_request_max_retries(0)
        .flush_interval_ms(60_000);
    builder
}

fn capture_mock<'a>(
    server: &'a MockServer,
    header: &'a str,
    event: &mut Event,
) -> httpmock::Mock<'a> {
    let id = uuid::Uuid::now_v7();
    event.set_uuid(id);
    server.mock(move |when, then| {
        when.method(POST)
            .header("x-custom-client", header)
            .path(if cfg!(feature = "capture-v1") {
                "/i/v1/analytics/events"
            } else {
                "/batch/"
            });
        then.status(200)
            .delay(Duration::from_millis(30))
            .json_body(serde_json::json!({"results": {id.to_string(): {"result": "ok"}}}));
    })
}

fn flags_mock<'a>(server: &'a MockServer, header: &'static str) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.method(POST)
            .path("/flags/")
            .header("x-custom-client", header);
        then.status(200)
            .delay(Duration::from_millis(30))
            .json_body(serde_json::json!({
                "featureFlags": {"flag": true}, "featureFlagPayloads": {"flag": "\"payload\""}
            }));
    })
}

fn definitions_mock<'a>(server: &'a MockServer, header: &'static str) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.method(GET)
            .path("/flags/definitions/")
            .header("x-custom-client", header);
        then.status(200)
            .delay(Duration::from_millis(30))
            .json_body(serde_json::json!({"flags": []}));
    })
}

#[cfg(feature = "async-client")]
mod asynchronous {
    use super::*;

    fn http() -> reqwest::Client {
        reqwest::Client::builder()
            .default_headers(headers("async"))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    async fn blocking_http() -> reqwest::blocking::Client {
        tokio::task::spawn_blocking(|| {
            reqwest::blocking::Client::builder()
                .default_headers(headers("blocking"))
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap()
        })
        .await
        .unwrap()
    }

    #[test]
    fn constructs_with_custom_blocking_client_without_tokio() {
        for disabled in [false, true] {
            let options = ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .disabled(disabled)
                .blocking_http_client(reqwest::blocking::Client::new())
                .build()
                .unwrap();
            let client = futures::executor::block_on(posthog_rs::client(options));
            drop(client);
        }
    }

    #[tokio::test]
    async fn flags_deadline_applies_to_custom_client() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200)
                .delay(Duration::from_millis(1500))
                .json_body(serde_json::json!({"featureFlags": {"flag": true}}));
        });
        let client = posthog_rs::client(
            options(&server)
                .http_client(reqwest::Client::new())
                .feature_flags_request_timeout_seconds(1)
                .build()
                .unwrap(),
        )
        .await;
        assert!(client
            .get_feature_flag("flag", "user", None, None, None)
            .await
            .is_err());
        assert!(client
            .get_feature_flag_payload("flag", "user")
            .await
            .is_err());
        client.shutdown().await;
    }

    #[tokio::test]
    async fn sdk_timeout_applies_to_custom_immediate_capture() {
        let server = MockServer::start();
        let mut event = Event::new("test", "user");
        let _capture = capture_mock(&server, "async", &mut event);
        let client = posthog_rs::client(
            options(&server)
                .http_client(http())
                .max_capture_attempts(1)
                .build()
                .unwrap(),
        )
        .await;
        assert!(client.capture_immediate(event).await.is_err());
        client.shutdown().await;
    }

    #[tokio::test]
    async fn sdk_timeout_applies_to_custom_background_capture() {
        let server = MockServer::start();
        let mut event = Event::new("test", "user");
        let _capture = capture_mock(&server, "blocking", &mut event);
        let (tx, rx) = std::sync::mpsc::channel();
        let client = posthog_rs::client(
            options(&server)
                .blocking_http_client(blocking_http().await)
                .max_capture_attempts(1)
                .on_error(move |_| {
                    tx.send(()).unwrap();
                })
                .build()
                .unwrap(),
        )
        .await;
        client.capture(event);
        client.flush().await;
        assert!(
            rx.try_recv().is_ok(),
            "background timeout must report a failure"
        );
        client.shutdown().await;
    }

    #[tokio::test]
    async fn sdk_timeout_applies_to_custom_polling() {
        let server = MockServer::start();
        let _definitions = definitions_mock(&server, "async");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let client = posthog_rs::client(
            options(&server)
                .http_client(http())
                .enable_local_evaluation(true)
                .secret_key("phx_test")
                .poll_interval_seconds(1)
                .on_error(move |_| {
                    tx.send(()).unwrap();
                })
                .build()
                .unwrap(),
        )
        .await;
        assert!(rx.try_recv().is_ok(), "initial polling must time out");
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        client.shutdown().await;
    }

    #[tokio::test]
    async fn routes_all_requests_with_custom_clients() {
        let server = MockServer::start();
        let mut immediate = Event::new("immediate", "user");
        let mut queued = Event::new("queued", "user");
        let capture = capture_mock(&server, "async", &mut immediate);
        let background = capture_mock(&server, "blocking", &mut queued);
        let flags = flags_mock(&server, "async");
        let definitions = definitions_mock(&server, "async");
        let http = http();
        let blocking = blocking_http().await;
        let client = posthog_rs::client(
            options(&server)
                .request_timeout_seconds(5)
                .http_client(http.clone())
                .blocking_http_client(blocking.clone())
                .enable_local_evaluation(true)
                .secret_key("phx_test")
                .build()
                .unwrap(),
        )
        .await;
        assert!(client
            .capture_immediate(immediate)
            .await
            .unwrap()
            .all_persisted());
        client.capture(queued);
        client.flush().await;
        client
            .get_feature_flag("flag", "user", None, None, None)
            .await
            .unwrap();
        client
            .get_feature_flag_payload("flag", "user")
            .await
            .unwrap();
        capture.assert_hits(1);
        background.assert_hits(1);
        assert!(flags.calls() >= 2);
        definitions.assert_hits(1);
        client.shutdown().await;
        drop(client);
        assert!(http.get(server.url("/still-alive")).send().await.is_ok());
        tokio::task::spawn_blocking(move || {
            assert!(blocking.get(server.url("/still-alive")).send().is_ok());
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn only_async_client_uses_default_background_client() {
        let server = MockServer::start();
        let background = server.mock(|when, then| {
            when.method(POST);
            then.status(200)
                .json_body(serde_json::json!({"results": {}}));
        });
        let client = posthog_rs::client(
            options(&server)
                .request_timeout_seconds(5)
                .http_client(http())
                .build()
                .unwrap(),
        )
        .await;
        client.capture(Event::new("queued", "user"));
        client.flush().await;
        background.assert_hits(1);
        client.shutdown().await;
    }

    #[tokio::test]
    async fn only_blocking_client_uses_default_async_client() {
        let server = MockServer::start();
        let flags = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200)
                .json_body(serde_json::json!({"featureFlags": {"flag": true}}));
        });
        let client = posthog_rs::client(
            options(&server)
                .request_timeout_seconds(5)
                .feature_flags_request_timeout_seconds(5)
                .blocking_http_client(blocking_http().await)
                .build()
                .unwrap(),
        )
        .await;
        client
            .get_feature_flag("flag", "user", None, None, None)
            .await
            .unwrap();
        flags.assert_hits(1);
        client.shutdown().await;
    }

    #[tokio::test]
    async fn last_blocking_handle_is_safe_on_drop_and_disabled_initialization() {
        for disabled in [false, true] {
            let mut builder = ClientOptionsBuilder::default();
            builder
                .api_key("phc_test".to_string())
                .disabled(disabled)
                .blocking_http_client(blocking_http().await);
            let options = builder.build().unwrap();
            drop(builder);
            let client = posthog_rs::client(options).await;
            drop(client);
        }
    }

    #[tokio::test]
    async fn shutdown_deadline_still_bounds_custom_background_requests() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST);
            then.status(200).delay(Duration::from_secs(2));
        });
        let client = posthog_rs::client(
            options(&server)
                .blocking_http_client(blocking_http().await)
                .request_timeout_seconds(5)
                .shutdown_timeout_ms(50)
                .build()
                .unwrap(),
        )
        .await;
        client.capture(Event::new("queued", "user"));
        let start = std::time::Instant::now();
        client.shutdown().await;
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn initialization_can_be_cancelled_with_last_blocking_handle() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/flags/definitions/");
            then.status(200)
                .delay(Duration::from_secs(2))
                .json_body(serde_json::json!({"flags": []}));
        });
        let mut builder = options(&server);
        builder
            .request_timeout_seconds(5)
            .http_client(http())
            .blocking_http_client(blocking_http().await)
            .enable_local_evaluation(true)
            .secret_key("phx_test");
        let options = builder.build().unwrap();
        drop(builder);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), posthog_rs::client(options))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn flags_deadline_overrides_supplied_short_timeout() {
        let server = MockServer::start();
        let flags = flags_mock(&server, "async");
        let http = reqwest::Client::builder()
            .default_headers(headers("async"))
            .timeout(Duration::from_millis(5))
            .build()
            .unwrap();
        let client = posthog_rs::client(
            options(&server)
                .http_client(http)
                .feature_flags_request_timeout_seconds(5)
                .build()
                .unwrap(),
        )
        .await;
        assert!(client
            .get_feature_flag("flag", "user", None, None, None)
            .await
            .is_ok());
        flags.assert_calls(1);
        client.shutdown().await;
    }
}

#[cfg(not(feature = "async-client"))]
mod blocking {
    use super::*;

    #[test]
    fn flags_deadline_applies_to_custom_client() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200)
                .delay(Duration::from_millis(1500))
                .json_body(serde_json::json!({"featureFlags": {"flag": true}}));
        });
        let client = posthog_rs::client(
            options(&server)
                .blocking_http_client(reqwest::blocking::Client::new())
                .feature_flags_request_timeout_seconds(1)
                .build()
                .unwrap(),
        );
        assert!(client
            .get_feature_flag("flag", "user", None, None, None)
            .is_err());
        assert!(client.get_feature_flag_payload("flag", "user").is_err());
        client.shutdown();
    }

    #[test]
    fn flags_deadline_overrides_supplied_short_timeout() {
        let server = MockServer::start();
        let _flags = flags_mock(&server, "blocking");
        let http = reqwest::blocking::Client::builder()
            .default_headers(headers("blocking"))
            .timeout(Duration::from_millis(5))
            .build()
            .unwrap();
        let client = posthog_rs::client(
            options(&server)
                .blocking_http_client(http)
                .feature_flags_request_timeout_seconds(5)
                .build()
                .unwrap(),
        );
        assert!(client
            .get_feature_flag("flag", "user", None, None, None)
            .is_ok());
        client.shutdown();
    }

    fn http() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .default_headers(headers("blocking"))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    #[test]
    fn sdk_timeout_applies_to_custom_immediate_capture() {
        let server = MockServer::start();
        let mut event = Event::new("test", "user");
        let _capture = capture_mock(&server, "blocking", &mut event);
        let client = posthog_rs::client(
            options(&server)
                .blocking_http_client(http())
                .max_capture_attempts(1)
                .build()
                .unwrap(),
        );
        assert!(client.capture_immediate(event).is_err());
        client.shutdown();
    }

    #[test]
    fn sdk_timeout_applies_to_custom_background_capture() {
        let server = MockServer::start();
        let mut event = Event::new("test", "user");
        let _capture = capture_mock(&server, "blocking", &mut event);
        let (tx, rx) = std::sync::mpsc::channel();
        let client = posthog_rs::client(
            options(&server)
                .blocking_http_client(http())
                .max_capture_attempts(1)
                .on_error(move |_| {
                    tx.send(()).unwrap();
                })
                .build()
                .unwrap(),
        );
        client.capture(event);
        client.flush();
        assert!(
            rx.try_recv().is_ok(),
            "background timeout must report a failure"
        );
        client.shutdown();
    }

    #[test]
    fn sdk_timeout_applies_to_custom_polling() {
        let server = MockServer::start();
        let _definitions = definitions_mock(&server, "blocking");
        let (tx, rx) = std::sync::mpsc::channel();
        let client = posthog_rs::client(
            options(&server)
                .blocking_http_client(http())
                .enable_local_evaluation(true)
                .secret_key("phx_test")
                .poll_interval_seconds(1)
                .on_error(move |_| {
                    tx.send(()).unwrap();
                })
                .build()
                .unwrap(),
        );
        assert!(rx.try_recv().is_ok(), "initial polling must time out");
        rx.recv_timeout(Duration::from_secs(3)).unwrap();
        client.shutdown();
    }

    #[test]
    fn routes_all_requests_with_custom_clients() {
        let server = MockServer::start();
        let mut event = Event::new("test", "user");
        let capture = capture_mock(&server, "blocking", &mut event);
        let flags = flags_mock(&server, "blocking");
        let definitions = definitions_mock(&server, "blocking");
        let http = reqwest::blocking::Client::builder()
            .default_headers(headers("blocking"))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let client = posthog_rs::client(
            options(&server)
                .request_timeout_seconds(5)
                .blocking_http_client(http.clone())
                .enable_local_evaluation(true)
                .secret_key("phx_test")
                .build()
                .unwrap(),
        );
        assert!(client
            .capture_immediate(event.clone())
            .unwrap()
            .all_persisted());
        client.capture(event);
        client.flush();
        client
            .get_feature_flag("flag", "user", None, None, None)
            .unwrap();
        client.get_feature_flag_payload("flag", "user").unwrap();
        assert!(capture.calls() >= 2);
        assert!(flags.calls() >= 2);
        definitions.assert_hits(1);
        client.shutdown();
        drop(client);
        assert!(http.get(server.url("/still-alive")).send().is_ok());
    }
}
