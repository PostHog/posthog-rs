//! `posthog_rs::capture_ai` on the global client rides the AI lane.
//!
//! Own integration-test binary on purpose: `init_global` sets a process-wide
//! `OnceLock`, so this must not share a process with any other global test.

use httpmock::prelude::*;
use posthog_rs::{ClientOptionsBuilder, Event};
use serde_json::json;

fn ok_mock<'a>(server: &'a MockServer, path: &str) -> httpmock::Mock<'a> {
    let path = path.to_string();
    server.mock(move |when, then| {
        when.method(POST).path(path);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    })
}

#[cfg(feature = "async-client")]
fn init_and_flush(options: posthog_rs::ClientOptions, event: Event) {
    futures::executor::block_on(async {
        posthog_rs::init_global(options).await.unwrap();
        posthog_rs::capture_ai(event);
        posthog_rs::flush().await;
    });
}

#[cfg(not(feature = "async-client"))]
fn init_and_flush(options: posthog_rs::ClientOptions, event: Event) {
    posthog_rs::init_global(options).unwrap();
    posthog_rs::capture_ai(event);
    posthog_rs::flush();
}

#[test]
fn global_capture_ai_posts_to_the_ai_endpoint_only() {
    let server = MockServer::start();
    let ai = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/ai/events")
            .header("content-encoding", "zstd");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({ "results": {} }));
    });
    let analytics = ok_mock(&server, "/i/v1/analytics/events");
    let options = ClientOptionsBuilder::default()
        .api_key("phc_test_token".to_string())
        .host(server.base_url())
        .flush_interval_ms(60_000u64)
        .build()
        .unwrap();

    init_and_flush(options, Event::new("$ai_generation", "user-1"));

    ai.assert_calls(1);
    analytics.assert_calls(0);
}
