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
/// the user's panic-site frame is kept and marked `in_app = true`, and the
/// frames obey the canonical wire order (outermost first, crash site last) with
/// the SDK's own capture plumbing stripped from the crash-site tail.
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
    let Some(frames) = body
        .pointer("/batch/0/properties/$exception_list/0/stacktrace/frames")
        .and_then(|v| v.as_array())
    else {
        return false;
    };
    let function_names: Vec<&str> = frames
        .iter()
        .map(|frame| frame["function"].as_str().unwrap_or_default())
        .collect();
    let panic_site_in_app = frames.iter().any(|frame| {
        frame["function"]
            .as_str()
            .is_some_and(|name| name.contains("integration_panic_site"))
            && frame["in_app"] == true
    });
    // Canonical wire order puts the crash site last. The panic hook fires nested
    // inside the panic runtime, so a naive reverse would leave the SDK's own hook
    // plumbing (`posthog_rs::error_tracking::*`) as the tail. We strip everything
    // innermost of the panic dispatcher, so no posthog_rs frame survives at all,
    // and the tail is the crash-side panic runtime rather than an SDK frame.
    let no_sdk_frames = !function_names
        .iter()
        .any(|name| name.contains("posthog_rs::error_tracking"));
    let tail_is_panic_runtime = function_names
        .last()
        .is_some_and(|name| name.contains("panicking::") || name.contains("panic_with_hook"));
    // The user's panic site must come *before* (outermore than) the crash-side
    // panic runtime that follows it — i.e. it is not left innermost/first.
    let panic_site_before_runtime = function_names
        .iter()
        .position(|name| name.contains("integration_panic_site"))
        .zip(
            function_names
                .iter()
                .rposition(|name| name.contains("panic_with_hook")),
        )
        .is_some_and(|(site, runtime)| site < runtime);
    is_panic
        && panic_site_in_app
        && no_sdk_frames
        && tail_is_panic_runtime
        && panic_site_before_runtime
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
