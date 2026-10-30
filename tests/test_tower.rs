#![cfg(feature = "tower")]

use axum::{body::Body, routing::get, Router};
use http::{Request, Response};
use posthog_rs::{tower::PostHogLayer, RequestContext};
use std::convert::Infallible;
use tower::{service_fn, Layer, ServiceExt};

#[tokio::test]
async fn axum_accepts_sanitized_headers_and_existing_application_context() {
    for (header, expected) in [
        ("  user  ", Some("user")),
        (" ", None),
        ("\tuser\t", Some("user")),
    ] {
        let app = Router::new()
            .route(
                "/work",
                get(
                    move |axum::Extension(context): axum::Extension<RequestContext>| async move {
                        assert_eq!(context.get_distinct_id(), expected);
                        assert_eq!(RequestContext::current().unwrap(), context);
                        assert_eq!(context.get_session_id(), Some("session"));
                        "ok"
                    },
                ),
            )
            .layer(PostHogLayer::new());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/work?secret=omit")
                    .header("X-PostHog-Distinct-Id", header)
                    .header("x-posthog-session-id", "session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(RequestContext::current().is_none());
    }
    let app = PostHogLayer::new().layer(service_fn(|request: Request<()>| async move {
        let context = request.extensions().get::<RequestContext>().unwrap();
        assert_eq!(context.get_distinct_id(), Some("authenticated-user"));
        assert_eq!(context.get_session_id(), Some("header-session"));
        let event = serde_json::to_value(context.event("work")).unwrap();
        assert_eq!(event["properties"]["$request_path"], "/work");
        assert!(event["properties"].get("$current_url").is_none());
        assert_eq!(event["options"]["cookieless_mode"], true);
        Ok::<_, Infallible>(Response::new(()))
    }));
    let mut request = Request::builder()
        .uri("/work?secret=omit")
        .header("x-posthog-distinct-id", "header-user")
        .header("x-posthog-session-id", "header-session")
        .body(())
        .unwrap();
    request.extensions_mut().insert(
        RequestContext::new()
            .distinct_id("authenticated-user")
            .option("cookieless_mode", true)
            .unwrap(),
    );
    app.oneshot(request).await.unwrap();
}

#[tokio::test]
async fn duplicate_invalid_and_long_headers_do_not_fail_requests() {
    let long = "a".repeat(1200);
    for (values, expected) in [
        (vec!["", "valid"], Some("valid".to_string())),
        (vec![long.as_str()], Some("a".repeat(1000))),
        (vec![""], None),
    ] {
        let app = PostHogLayer::new().layer(service_fn(move |_: Request<()>| {
            assert_eq!(
                RequestContext::current().unwrap().get_distinct_id(),
                expected.as_deref()
            );
            async { Ok::<_, Infallible>(Response::new(())) }
        }));
        let mut request = Request::new(());
        for value in values {
            request
                .headers_mut()
                .append("x-posthog-distinct-id", value.parse().unwrap());
        }
        app.oneshot(request).await.unwrap();
    }
    let app = PostHogLayer::new().layer(service_fn(|_: Request<()>| async {
        assert!(RequestContext::current()
            .unwrap()
            .get_distinct_id()
            .is_none());
        Ok::<_, Infallible>(Response::new(()))
    }));
    let mut request = Request::new(());
    request.headers_mut().insert(
        "x-posthog-distinct-id",
        http::HeaderValue::from_bytes(&[0xff]).unwrap(),
    );
    app.oneshot(request).await.unwrap();
}

#[tokio::test]
async fn middleware_preserves_body_response_errors_and_concurrent_context() {
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let app = PostHogLayer::new().layer(service_fn(move |request: Request<&'static str>| {
        let barrier = barrier.clone();
        async move {
            barrier.wait().await;
            assert_eq!(
                RequestContext::current().unwrap().get_distinct_id(),
                Some(*request.body())
            );
            if *request.body() == "failure" {
                Err("application error")
            } else {
                Ok(Response::new(request.into_body()))
            }
        }
    }));
    let make_request = |identity| {
        Request::builder()
            .header("x-posthog-distinct-id", identity)
            .body(identity)
            .unwrap()
    };
    let (success, error) = tokio::join!(
        app.clone().oneshot(make_request("success")),
        app.oneshot(make_request("failure"))
    );
    assert_eq!(success.unwrap().into_body(), "success");
    assert_eq!(error.unwrap_err(), "application error");
    assert!(RequestContext::current().is_none());
}
