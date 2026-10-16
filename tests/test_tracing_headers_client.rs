#![cfg(feature = "async-client")]

use httpmock::prelude::*;
use posthog_rs::{RequestContext, TracingHeadersClient};
use reqwest::{header::HeaderMap, Method};

#[tokio::test]
async fn exact_hosts_replace_stale_headers_and_preserve_http_requests() {
    let server = MockServer::start_async().await;
    for (allowed, matched) in [
        ("127.0.0.1", true),
        (" 127.0.0.1 ", true),
        ("localhost", false),
        ("0.0.1", false),
        ("*.0.0.1", false),
        ("127.0.0.1:80", false),
        ("http://127.0.0.1", false),
        ("127.0.0.1/work", false),
        ("", false),
    ] {
        let expected = if matched { "current" } else { "original" };
        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/work")
                    .body("payload")
                    .header("authorization", "Bearer example")
                    .header("cookie", "example=value")
                    .header("x-application", "default")
                    .header("x-posthog-distinct-id", expected)
                    .header("x-posthog-session-id", expected);
                then.status(201).body("result");
            })
            .await;
        let mut defaults = HeaderMap::new();
        defaults.insert("x-application", "default".parse().unwrap());
        let client = TracingHeadersClient::new(
            reqwest::Client::builder().default_headers(defaults),
            [allowed],
        )
        .unwrap();
        let request = client
            .client()
            .post(server.url("/work"))
            .bearer_auth("example")
            .header("cookie", "example=value")
            .header("x-posthog-distinct-id", "original")
            .header("x-posthog-session-id", "original")
            .body("payload")
            .build()
            .unwrap();
        let response = RequestContext::new()
            .distinct_id("current")
            .session_id("current")
            .scope(client.execute(request))
            .await
            .unwrap();
        assert_eq!(response.status(), 201);
        assert_eq!(response.text().await.unwrap(), "result");
        mock.assert_async().await;
        mock.delete_async().await;
    }
}

#[tokio::test]
async fn context_is_read_when_each_request_runs_and_missing_ids_are_removed() {
    let server = MockServer::start_async().await;
    let client = TracingHeadersClient::new(reqwest::Client::builder(), ["127.0.0.1"]).unwrap();
    for (distinct_id, session_id) in [
        (Some("first"), Some("one")),
        (Some("second"), None),
        (None, Some("two")),
        (None, None),
    ] {
        let mock = server
            .mock_async(|when, then| {
                let when = when.method(GET);
                let when = match distinct_id {
                    Some(value) => when.header("x-posthog-distinct-id", value),
                    None => when.header_missing("x-posthog-distinct-id"),
                };
                match session_id {
                    Some(value) => when.header("x-posthog-session-id", value),
                    None => when.header_missing("x-posthog-session-id"),
                };
                then.status(204);
            })
            .await;
        let request = client
            .client()
            .get(server.url("/"))
            .header("x-posthog-distinct-id", "stale")
            .header("x-posthog-session-id", "stale")
            .build()
            .unwrap();
        let pending = client.execute(request);
        let mut context = RequestContext::new();
        if let Some(value) = distinct_id {
            context = context.distinct_id(value);
        }
        if let Some(value) = session_id {
            context = context.session_id(value);
        }
        assert_eq!(context.scope(pending).await.unwrap().status(), 204);
        mock.assert_async().await;
        mock.delete_async().await;
    }
}

#[tokio::test]
async fn redirects_are_returned_without_forwarding_context() {
    let destination = MockServer::start_async().await;
    let redirected = destination
        .mock_async(|when, then| {
            when.any_request();
            then.status(200);
        })
        .await;
    let origin = MockServer::start_async().await;
    for cross_host in [false, true] {
        for status in [301, 302, 303, 307, 308] {
            let target = if cross_host {
                destination
                    .url("/destination")
                    .replace("127.0.0.1", "localhost")
            } else {
                origin.url("/destination")
            };
            let response = origin
                .mock_async(|when, then| {
                    when.method(POST)
                        .path("/start")
                        .header("x-posthog-distinct-id", "person");
                    then.status(status).header("location", &target);
                })
                .await;
            let client =
                TracingHeadersClient::new(reqwest::Client::builder(), ["127.0.0.1"]).unwrap();
            let request = client
                .client()
                .request(Method::POST, origin.url("/start"))
                .body("payload")
                .build()
                .unwrap();
            let result = RequestContext::new()
                .distinct_id("person")
                .scope(client.execute(request))
                .await
                .unwrap();
            assert_eq!(result.status().as_u16(), status);
            assert_eq!(result.headers()["location"], target);
            response.assert_async().await;
            response.delete_async().await;
        }
    }
    redirected.assert_calls_async(0).await;
}

#[tokio::test]
async fn invalid_context_headers_do_not_break_application_requests() {
    let server = MockServer::start_async().await;
    let client = TracingHeadersClient::new(reqwest::Client::builder(), [" LOCALHOST "]).unwrap();
    for invalid in ["person\nvalue", "person\rvalue", "person\0value", ""] {
        let mock = server
            .mock_async(|when, then| {
                when.method(GET)
                    .header_missing("x-posthog-distinct-id")
                    .header("x-posthog-session-id", "session");
                then.status(200);
            })
            .await;
        let request = client
            .client()
            .get(server.url("/").replace("127.0.0.1", "localhost"))
            .header("x-posthog-distinct-id", "stale")
            .build()
            .unwrap();
        let result = RequestContext::new()
            .distinct_id(invalid)
            .session_id("session")
            .scope(client.execute(request))
            .await
            .unwrap();
        assert_eq!(result.status(), 200);
        mock.assert_async().await;
        mock.delete_async().await;
    }
}

#[tokio::test]
async fn default_configuration_does_not_add_headers_and_transport_errors_reach_the_caller() {
    let server = MockServer::start_async().await;
    let client = TracingHeadersClient::new(reqwest::Client::builder(), Vec::<&str>::new()).unwrap();
    let mock = server
        .mock_async(|when, then| {
            when.method(GET)
                .path("/")
                .header_missing("x-posthog-distinct-id")
                .header_missing("x-posthog-session-id");
            then.status(418);
        })
        .await;
    let request = client.client().get(server.url("/")).build().unwrap();
    let result = RequestContext::new()
        .distinct_id("person")
        .session_id("session")
        .scope(client.execute(request))
        .await
        .unwrap();
    assert_eq!(result.status(), 418);
    mock.assert_async().await;

    let _delayed = server
        .mock_async(|when, then| {
            when.method(GET).path("/slow");
            then.status(200)
                .delay(std::time::Duration::from_millis(100));
        })
        .await;
    let request = client
        .client()
        .get(server.url("/slow"))
        .timeout(std::time::Duration::from_millis(1))
        .build()
        .unwrap();
    assert!(client.execute(request).await.unwrap_err().is_timeout());
}
