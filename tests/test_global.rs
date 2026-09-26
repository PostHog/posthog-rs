//! One test owns the process-wide singleton; other global scenarios use separate binaries.
use httpmock::prelude::*;
use posthog_rs::{ClientOptionsBuilder, Error, Event};
use serde_json::{json, Value};

#[cfg(feature = "async-client")]
macro_rules! call {
    ($expr:expr) => {
        futures::executor::block_on($expr)
    };
}
#[cfg(not(feature = "async-client"))]
macro_rules! call {
    ($expr:expr) => {
        $expr
    };
}

#[test]
fn global_client_lifecycle() {
    let server = MockServer::start();
    let capture = server.mock(|when, then| {
        when.method(POST).matches(|request| {
            let body: Value = serde_json::from_slice(request.body_ref()).unwrap();
            body["batch"].as_array().is_some_and(|batch| {
                batch.len() == 1
                    && batch[0]["event"] == "global-event"
                    && batch[0]["distinct_id"] == "user-1"
            })
        });
        then.status(200).json_body(json!({"results": {}}));
    });
    let options = ClientOptionsBuilder::default()
        .api_key("phc_test".to_string())
        .host(server.base_url())
        .flush_interval_ms(600_000)
        .build()
        .unwrap();

    assert!(!posthog_rs::global_is_disabled());
    posthog_rs::capture(Event::new("before-init", "user-1"));
    call!(posthog_rs::flush());
    call!(posthog_rs::shutdown());
    #[cfg(feature = "error-tracking")]
    {
        let error = std::io::Error::other("global error");
        assert!(matches!(
            call!(posthog_rs::capture_exception(&error)),
            Err(Error::NotInitialized)
        ));
        assert!(matches!(
            call!(posthog_rs::capture_exception_with(
                &error,
                posthog_rs::CaptureExceptionOptions::new()
            )),
            Err(Error::NotInitialized)
        ));
    }
    capture.assert_calls(0);

    call!(posthog_rs::init_global(options.clone())).unwrap();
    assert!(matches!(
        call!(posthog_rs::init_global(options)),
        Err(Error::AlreadyInitialized)
    ));
    posthog_rs::capture(Event::new("global-event", "user-1"));
    call!(posthog_rs::flush());
    capture.assert_calls(1);

    posthog_rs::disable_global();
    assert!(posthog_rs::global_is_disabled());
    posthog_rs::capture(Event::new("global-event", "user-1"));
    call!(posthog_rs::flush());
    capture.assert_calls(2);

    #[cfg(feature = "error-tracking")]
    {
        let exceptions = server.mock(|when, then| {
            when.method(POST).matches(|request| {
                let body: Value = serde_json::from_slice(request.body_ref()).unwrap();
                let batch = body["batch"].as_array().unwrap();
                batch.len() == 2
                    && batch.iter().all(|event| event["event"] == "$exception")
                    && batch[1]["distinct_id"] == "identified-user"
                    && batch[1]["properties"]["context"] == "global"
            });
            then.status(200).json_body(json!({"results": {}}));
        });
        let error = std::io::Error::other("global error");
        call!(posthog_rs::capture_exception(&error)).unwrap();
        call!(posthog_rs::capture_exception_with(
            &error,
            posthog_rs::CaptureExceptionOptions::new()
                .distinct_id("identified-user")
                .property("context", "global")
                .unwrap()
        ))
        .unwrap();
        call!(posthog_rs::flush());
        exceptions.assert_calls(1);
    }

    posthog_rs::capture(Event::new("global-event", "user-1"));
    call!(posthog_rs::shutdown());
    capture.assert_calls(3);
    posthog_rs::capture(Event::new("global-event", "user-1"));
    call!(posthog_rs::flush());
    call!(posthog_rs::shutdown());
    capture.assert_calls(3);
}
