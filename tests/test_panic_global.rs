//! End-to-end check that `init_global` installs panic autocapture when
//! `capture_panics` is enabled.
//!
//! This lives in its own integration-test binary on purpose: it initializes the
//! real process-wide global client (a set-once `OnceLock`) and installs the
//! process-wide panic hook. Its own process gives it a fresh `OnceLock`, so it
//! is isolated — the equivalent in-process unit test would be order-dependent
//! (the `OnceLock` can't be reset) and break as soon as another test touched the
//! global. The gate logic and the hook mechanism are covered by the in-process
//! unit tests; this only exercises the `init_global` -> hook wiring.
#![cfg(feature = "error-tracking")]

use httpmock::prelude::*;
use std::panic::{self, AssertUnwindSafe};

/// The panic site, named so the captured stack frame is matchable. Lives in
/// this test crate (not `posthog_rs`), so its frame classifies as in-app —
/// which the in-process unit tests can't show (a panic there originates inside
/// `posthog_rs`, which is classified out-of-app).
#[inline(never)]
fn integration_panic_site() {
    panic!("integration panic boom");
}

/// True when the request body carries a personless panic `$exception` event in
/// the transport's batch envelope (same shape for the V0 and V1 wire formats),
/// and the user's panic-site frame is kept and marked `in_app = true`.
fn is_panic_exception(req: &HttpMockRequest) -> bool {
    let Some(body) = req.body.as_deref() else {
        return false;
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let is_panic = body.pointer("/batch/0/event").and_then(|v| v.as_str()) == Some("$exception")
        && body
            .pointer("/batch/0/properties/$exception_list/0/type")
            .and_then(|v| v.as_str())
            == Some("Panic");
    let panic_site_in_app = body
        .pointer("/batch/0/properties/$exception_list/0/stacktrace/frames")
        .and_then(|v| v.as_array())
        .is_some_and(|frames| {
            frames.iter().any(|frame| {
                frame["function"]
                    .as_str()
                    .is_some_and(|name| name.contains("integration_panic_site"))
                    && frame["in_app"] == true
            })
        });
    is_panic && panic_site_in_app
}

#[cfg(feature = "async-client")]
fn init_global_for_test(options: posthog_rs::ClientOptions) -> Result<(), posthog_rs::Error> {
    // The async constructor only awaits under local evaluation, so a minimal
    // executor drives it with no Tokio runtime needed.
    futures::executor::block_on(posthog_rs::init_global(options))
}

#[cfg(not(feature = "async-client"))]
fn init_global_for_test(options: posthog_rs::ClientOptions) -> Result<(), posthog_rs::Error> {
    posthog_rs::init_global(options)
}

#[test]
fn init_global_installs_panic_capture_when_enabled() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).matches(is_panic_exception);
        then.status(200);
    });

    // Quiet the default hook (init_global chains whatever is installed now) so a
    // caught, expected panic doesn't print to the test output.
    panic::set_hook(Box::new(|_| {}));

    let options = posthog_rs::ClientOptionsBuilder::default()
        .api_key("test_api_key".to_string())
        .host(server.base_url())
        .error_tracking(
            posthog_rs::ErrorTrackingOptionsBuilder::default()
                .capture_panics(true)
                .build()
                .unwrap(),
        )
        .build()
        .unwrap();
    init_global_for_test(options).expect("init_global should succeed");

    // With `capture_panics` enabled, the installed hook routes this panic through
    // the global client. `catch_unwind` keeps the test process alive; the hook's
    // flush is synchronous, so the event has been sent by the time it returns.
    let _ = panic::catch_unwind(AssertUnwindSafe(integration_panic_site));

    mock.assert_hits(1);
}
