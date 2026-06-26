//! End-to-end check that `init_global` installs panic autocapture by default.
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

/// True when the request body carries a personless panic `$exception` event in
/// the transport's batch envelope (same shape for the V0 and V1 wire formats).
fn is_panic_exception(req: &HttpMockRequest) -> bool {
    let Some(body) = req.body.as_deref() else {
        return false;
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    body.pointer("/batch/0/event").and_then(|v| v.as_str()) == Some("$exception")
        && body
            .pointer("/batch/0/properties/$exception_list/0/type")
            .and_then(|v| v.as_str())
            == Some("Panic")
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
fn init_global_installs_panic_capture_by_default() {
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
        .build()
        .unwrap();
    init_global_for_test(options).expect("init_global should succeed");

    // `capture_panics` is on by default, so the installed hook routes this panic
    // through the global client. `catch_unwind` keeps the test process alive; the
    // hook's flush is synchronous, so the event has been sent by the time it
    // returns.
    let _ = panic::catch_unwind(AssertUnwindSafe(|| panic!("integration panic boom")));

    mock.assert_hits(1);
}
