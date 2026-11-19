use std::ffi::{c_char, CStr, CString};
use std::ptr;

use httpmock::prelude::*;
use posthog_ffi::*;
use serde_json::{json, Value};

const CAPTURE_PATH: &str = "/i/v1/analytics/events";

fn c(value: &str) -> CString {
    CString::new(value).unwrap()
}

fn last_error() -> Option<String> {
    let error = posthog_last_error();
    (!error.is_null()).then(|| {
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    })
}

unsafe fn take_string(value: *mut c_char) -> String {
    assert!(!value.is_null(), "expected a string, got NULL");
    let text = CStr::from_ptr(value).to_str().unwrap().to_owned();
    posthog_string_free(value);
    text
}

unsafe fn new_client(server: &MockServer) -> *mut PostHogClient {
    let options = posthog_options_new(c("phc_test").as_ptr());
    assert_eq!(
        posthog_options_set_host(options, c(&server.base_url()).as_ptr()),
        PostHogStatus::POSTHOG_STATUS_OK
    );
    posthog_options_set_feature_flags_request_max_retries(options, 0);
    let client = posthog_client_new(options);
    posthog_options_free(options);
    assert!(!client.is_null(), "{:?}", last_error());
    client
}

fn captured_batch(request: &HttpMockRequest) -> Vec<Value> {
    let body: Value = serde_json::from_slice(request.body_ref()).unwrap();
    body["batch"].as_array().cloned().unwrap_or_default()
}

#[test]
fn captures_event_with_typed_properties_and_ffi_lib() {
    let server = MockServer::start();
    let capture = server.mock(|when, then| {
        when.method(POST).path(CAPTURE_PATH).is_true(|request| {
            let batch = captured_batch(request);
            let [event] = batch.as_slice() else {
                return false;
            };
            let properties = &event["properties"];
            event["event"] == "app_started"
                && event["distinct_id"] == "user-1"
                && event["timestamp"] == "2024-01-02T03:04:05.006Z"
                && event["uuid"] == "01920000-0000-7000-8000-000000000001"
                && properties["edition"] == "community"
                && properties["plugins"] == 3
                && properties["ratio"] == 0.5
                && properties["beta"] == true
                && properties["$set"] == json!({"plan": "free"})
                && properties["$groups"] == json!({"company": "acme"})
                && properties["$lib"] == "posthog-rs-ffi"
                && properties["$lib_version"] == env!("CARGO_PKG_VERSION")
                && properties["$lib_version__major"]
                    == env!("CARGO_PKG_VERSION_MAJOR").parse::<u64>().unwrap()
                && properties["$lib_version__minor"]
                    == env!("CARGO_PKG_VERSION_MINOR").parse::<u64>().unwrap()
                && properties["$lib_version__patch"]
                    == env!("CARGO_PKG_VERSION_PATCH").parse::<u64>().unwrap()
        });
        then.status(200).json_body(json!({"results": {}}));
    });

    unsafe {
        let client = new_client(&server);
        let event = posthog_event_new(c("app_started").as_ptr(), c("user-1").as_ptr());
        let key = |k: &str| c(k);
        assert_eq!(
            posthog_event_set_property_string(
                event,
                key("edition").as_ptr(),
                c("community").as_ptr()
            ),
            PostHogStatus::POSTHOG_STATUS_OK
        );
        posthog_event_set_property_int(event, key("plugins").as_ptr(), 3);
        posthog_event_set_property_double(event, key("ratio").as_ptr(), 0.5);
        posthog_event_set_property_bool(event, key("beta").as_ptr(), true);
        posthog_event_set_property_json(
            event,
            key("$set").as_ptr(),
            c(r#"{"plan":"free"}"#).as_ptr(),
        );
        posthog_event_add_group(event, key("company").as_ptr(), c("acme").as_ptr());
        posthog_event_set_timestamp_ms(event, 1_704_164_645_006);
        assert_eq!(
            posthog_event_set_uuid(event, c("01920000-0000-7000-8000-000000000001").as_ptr()),
            PostHogStatus::POSTHOG_STATUS_OK
        );

        assert_eq!(
            posthog_capture(client, event),
            PostHogStatus::POSTHOG_STATUS_OK
        );
        posthog_event_free(event);
        assert_eq!(posthog_flush(client), PostHogStatus::POSTHOG_STATUS_OK);
        posthog_client_free(client);
    }

    capture.assert();
}

#[test]
fn reports_invalid_arguments_through_status_and_last_error() {
    unsafe {
        let event = posthog_event_new(c("e").as_ptr(), c("u").as_ptr());

        let status = posthog_event_set_property_json(event, c("k").as_ptr(), c("{").as_ptr());
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_INVALID_JSON);
        assert!(last_error().unwrap().contains("value_json"));

        let status = posthog_event_set_property_string(event, ptr::null(), c("v").as_ptr());
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_NULL_ARGUMENT);
        assert_eq!(last_error().unwrap(), "`key` is NULL");

        let invalid_utf8 = [0xffu8, 0];
        let status =
            posthog_event_set_property_string(event, invalid_utf8.as_ptr().cast(), c("v").as_ptr());
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_INVALID_UTF8);

        let status = posthog_event_set_uuid(event, c("not-a-uuid").as_ptr());
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_INVALID_ARGUMENT);

        let status = posthog_event_set_property_bool(event, c("ok").as_ptr(), true);
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_OK);
        assert_eq!(last_error(), None);

        posthog_event_free(event);
        assert!(posthog_event_new(ptr::null(), c("u").as_ptr()).is_null());
        assert!(posthog_client_new(ptr::null()).is_null());
    }
}

#[test]
fn evaluates_flags_and_records_flag_called_events() {
    let server = MockServer::start();
    let flags_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/flags/")
            .json_body_includes(r#"{"distinct_id":"user-1","groups":{"company":"acme"}}"#);
        then.status(200).json_body(json!({
            "flags": {
                "beta": {
                    "key": "beta", "enabled": true, "variant": null,
                    "reason": {"code": "condition_match"},
                    "metadata": {"id": 1, "version": 1, "payload": null}
                },
                "checkout": {
                    "key": "checkout", "enabled": true, "variant": "test",
                    "reason": {"code": "condition_match"},
                    "metadata": {"id": 2, "version": 1, "payload": {"color": "blue"}}
                },
                "off": {
                    "key": "off", "enabled": false, "variant": null,
                    "reason": {"code": "out_of_rollout_bound"},
                    "metadata": {"id": 3, "version": 1, "payload": null}
                }
            },
            "errorsWhileComputingFlags": false,
            "requestId": "req-1"
        }));
    });
    let capture = server.mock(|when, then| {
        when.method(POST).path(CAPTURE_PATH).is_true(|request| {
            let batch = captured_batch(request);
            let called: Vec<_> = batch
                .iter()
                .filter(|event| event["event"] == "$feature_flag_called")
                .map(|event| event["properties"]["$feature_flag"].clone())
                .collect();
            let purchase = batch.iter().find(|event| event["event"] == "purchase");
            called == [json!("beta"), json!("checkout"), json!("missing")]
                && purchase.is_some_and(|event| {
                    event["properties"]["$feature/beta"] == true
                        && event["properties"]["$feature/checkout"] == "test"
                        && event["properties"]["$active_feature_flags"]
                            == json!(["beta", "checkout"])
                })
        });
        then.status(200).json_body(json!({"results": {}}));
    });

    unsafe {
        let client = new_client(&server);
        let mut flags = ptr::null_mut();
        let options = c(r#"{"groups":{"company":"acme"}}"#);
        let status =
            posthog_evaluate_flags(client, c("user-1").as_ptr(), options.as_ptr(), &mut flags);
        assert_eq!(
            status,
            PostHogStatus::POSTHOG_STATUS_OK,
            "{:?}",
            last_error()
        );

        let mut enabled = false;
        posthog_flags_is_enabled(flags, c("beta").as_ptr(), &mut enabled);
        assert!(enabled);

        let mut state = PostHogFlagState::POSTHOG_FLAG_STATE_DISABLED;
        let mut variant = ptr::null_mut();
        posthog_flags_get_flag(flags, c("checkout").as_ptr(), &mut state, &mut variant);
        assert_eq!(state, PostHogFlagState::POSTHOG_FLAG_STATE_VARIANT);
        assert_eq!(take_string(variant), "test");

        posthog_flags_get_flag(flags, c("missing").as_ptr(), &mut state, &mut variant);
        assert_eq!(state, PostHogFlagState::POSTHOG_FLAG_STATE_NOT_FOUND);
        assert!(variant.is_null());

        let mut payload = ptr::null_mut();
        posthog_flags_get_payload(flags, c("checkout").as_ptr(), &mut payload);
        assert_eq!(take_string(payload), r#"{"color":"blue"}"#);

        let mut keys = ptr::null_mut();
        posthog_flags_keys(flags, &mut keys);
        assert_eq!(take_string(keys), r#"["beta","checkout","off"]"#);

        let accessed = posthog_flags_only_accessed(flags);
        let event = posthog_event_new(c("purchase").as_ptr(), c("user-1").as_ptr());
        posthog_event_set_flags(event, accessed);
        posthog_capture(client, event);
        posthog_event_free(event);
        posthog_flags_free(accessed);
        posthog_flags_free(flags);

        posthog_client_free(client);
    }

    flags_mock.assert();
    capture.assert();
}

#[test]
fn rejects_malformed_flag_options_without_a_request() {
    let server = MockServer::start();
    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"flags": {}}));
    });

    unsafe {
        let client = new_client(&server);
        let mut flags = ptr::null_mut();
        let status = posthog_evaluate_flags(
            client,
            c("user-1").as_ptr(),
            c(r#"{"flag_keys":"beta"}"#).as_ptr(),
            &mut flags,
        );
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_INVALID_JSON);
        assert!(flags.is_null());
        assert!(last_error().unwrap().contains("flag_keys"));
        posthog_client_free(client);
    }

    flags_mock.assert_calls(0);
}

#[test]
fn null_only_evaluate_locally_is_rejected_without_a_request() {
    let server = MockServer::start();
    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"flags": {}}));
    });

    unsafe {
        let client = new_client(&server);
        let mut flags = ptr::null_mut();
        let options =
            c(r#"{"only_evaluate_locally":null,"person_properties":{"email":"a@example.com"}}"#);
        let status =
            posthog_evaluate_flags(client, c("user-1").as_ptr(), options.as_ptr(), &mut flags);
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_INVALID_JSON);
        assert!(flags.is_null());
        assert!(last_error().unwrap().contains("only_evaluate_locally"));
        posthog_client_free(client);
    }

    flags_mock.assert_calls(0);
}

#[test]
fn surfaces_flag_request_failures() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(500).body("boom");
    });

    unsafe {
        let client = new_client(&server);
        let mut flags = ptr::null_mut();
        let status = posthog_evaluate_flags(client, c("user-1").as_ptr(), ptr::null(), &mut flags);
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_REQUEST);
        assert!(flags.is_null());
        assert!(last_error().is_some());
        posthog_client_free(client);
    }
}

#[test]
fn alias_and_group_identify_capture_events() {
    let server = MockServer::start();
    let capture = server.mock(|when, then| {
        when.method(POST).path(CAPTURE_PATH).is_true(|request| {
            let batch = captured_batch(request);
            let names: Vec<_> = batch.iter().map(|event| event["event"].clone()).collect();
            names == [json!("$create_alias"), json!("$groupidentify")]
                && batch[1]["properties"]["$group_set"] == json!({"name": "Acme"})
        });
        then.status(200).json_body(json!({"results": {}}));
    });

    unsafe {
        let client = new_client(&server);
        let status = posthog_alias(client, c("anon-1").as_ptr(), c("user-1").as_ptr());
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_OK);
        let status = posthog_group_identify(
            client,
            c("company").as_ptr(),
            c("acme").as_ptr(),
            c(r#"{"name":"Acme"}"#).as_ptr(),
        );
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_OK);
        let status = posthog_group_identify(
            client,
            c("company").as_ptr(),
            c("acme").as_ptr(),
            c("[1]").as_ptr(),
        );
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_INVALID_JSON);
        posthog_client_free(client);
    }

    capture.assert();
}

#[test]
fn shares_one_client_across_threads() {
    let server = MockServer::start();
    let sent = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&sent);
    server.mock(|when, then| {
        when.method(POST)
            .path(CAPTURE_PATH)
            .is_true(move |request| {
                counter.fetch_add(
                    captured_batch(request).len(),
                    std::sync::atomic::Ordering::SeqCst,
                );
                true
            });
        then.status(200).json_body(json!({"results": {}}));
    });

    struct Shared(*mut PostHogClient);
    unsafe impl Send for Shared {}
    unsafe impl Sync for Shared {}

    let client = Shared(unsafe { new_client(&server) });
    std::thread::scope(|scope| {
        for thread in 0..8 {
            let client = &client;
            scope.spawn(move || unsafe {
                for i in 0..25 {
                    let event =
                        posthog_event_new(c("tick").as_ptr(), c(&format!("u{thread}")).as_ptr());
                    posthog_event_set_property_int(event, c("i").as_ptr(), i);
                    assert_eq!(
                        posthog_capture(client.0, event),
                        PostHogStatus::POSTHOG_STATUS_OK
                    );
                    posthog_event_free(event);
                }
            });
        }
    });
    unsafe { posthog_client_free(client.0) };

    assert_eq!(sent.load(std::sync::atomic::Ordering::SeqCst), 200);
}

#[test]
fn calls_from_thread_local_destructors_do_not_abort() {
    struct CallsOnDrop;
    impl Drop for CallsOnDrop {
        fn drop(&mut self) {
            unsafe {
                let event = posthog_event_new(ptr::null(), ptr::null());
                assert!(event.is_null());
                let _ = posthog_last_error();
            }
        }
    }
    thread_local! {
        static GUARD: CallsOnDrop = const { CallsOnDrop };
    }

    let result = std::thread::spawn(|| {
        // Registered before the SDK's error slot, so it is destroyed after it.
        GUARD.with(|_| {});
        unsafe { posthog_event_new(ptr::null(), ptr::null()) };
    })
    .join();
    assert!(result.is_ok());
}

#[test]
fn disabled_client_sends_nothing() {
    let server = MockServer::start();
    let any_request = server.mock(|when, then| {
        when.any_request();
        then.status(200)
            .json_body(json!({"flags": {}, "results": {}}));
    });

    unsafe {
        let options = posthog_options_new(c("phc_test").as_ptr());
        posthog_options_set_host(options, c(&server.base_url()).as_ptr());
        posthog_options_set_disabled(options, true);
        let client = posthog_client_new(options);
        posthog_options_free(options);
        assert!(!client.is_null(), "{:?}", last_error());

        let event = posthog_event_new(c("disabled").as_ptr(), c("user-1").as_ptr());
        assert_eq!(
            posthog_capture(client, event),
            PostHogStatus::POSTHOG_STATUS_OK
        );
        posthog_event_free(event);

        let mut flags = ptr::null_mut();
        let status = posthog_evaluate_flags(client, c("user-1").as_ptr(), ptr::null(), &mut flags);
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_OK);
        let mut keys = ptr::null_mut();
        posthog_flags_keys(flags, &mut keys);
        assert_eq!(take_string(keys), "[]");
        posthog_flags_free(flags);

        assert_eq!(posthog_flush(client), PostHogStatus::POSTHOG_STATUS_OK);
        posthog_client_free(client);
    }

    any_request.assert_calls(0);
}

#[test]
fn only_evaluate_locally_never_calls_flags() {
    let server = MockServer::start();
    let flags_mock = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"flags": {
            "beta": {
                "key": "beta", "enabled": true, "variant": null,
                "reason": {"code": "condition_match"},
                "metadata": {"id": 1, "version": 1, "payload": null}
            }
        }}));
    });

    unsafe {
        let client = new_client(&server);
        let mut flags = ptr::null_mut();
        let options =
            c(r#"{"only_evaluate_locally":true,"person_properties":{"email":"a@example.com"}}"#);
        let status =
            posthog_evaluate_flags(client, c("user-1").as_ptr(), options.as_ptr(), &mut flags);
        assert_eq!(
            status,
            PostHogStatus::POSTHOG_STATUS_OK,
            "{:?}",
            last_error()
        );
        let mut keys = ptr::null_mut();
        posthog_flags_keys(flags, &mut keys);
        assert_eq!(take_string(keys), "[]");
        posthog_flags_free(flags);
        posthog_client_free(client);
    }

    flags_mock.assert_calls(0);
}

#[test]
fn capture_requests_identify_the_bindings() {
    let server = MockServer::start();
    let sdk_info = format!("posthog-rs-ffi/{}", env!("CARGO_PKG_VERSION"));
    let capture = server.mock(|when, then| {
        when.method(POST)
            .path(CAPTURE_PATH)
            .header("posthog-sdk-info", &sdk_info)
            .header("user-agent", &sdk_info);
        then.status(200).json_body(json!({"results": {}}));
    });

    unsafe {
        let client = new_client(&server);
        let event = posthog_event_new(c("identity").as_ptr(), c("user-1").as_ptr());
        assert_eq!(
            posthog_capture(client, event),
            PostHogStatus::POSTHOG_STATUS_OK
        );
        posthog_event_free(event);
        assert_eq!(posthog_flush(client), PostHogStatus::POSTHOG_STATUS_OK);
        posthog_client_free(client);
    }

    capture.assert();
}

#[test]
fn shutdown_stops_flag_requests_and_the_poller() {
    let server = MockServer::start();
    let definitions = server.mock(|when, then| {
        when.method(GET).path("/flags/definitions/");
        then.status(200)
            .json_body(json!({"flags": [], "group_type_mapping": {}, "cohorts": {}}));
    });
    let remote = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"flags": {}}));
    });

    unsafe {
        let options = posthog_options_new(c("phc_test").as_ptr());
        posthog_options_set_host(options, c(&server.base_url()).as_ptr());
        posthog_options_set_secret_key(options, c("phs_test").as_ptr());
        posthog_options_set_enable_local_evaluation(options, true);
        posthog_options_set_poll_interval_seconds(options, 1);
        posthog_options_set_feature_flags_request_max_retries(options, 0);
        let client = posthog_client_new(options);
        posthog_options_free(options);
        assert!(!client.is_null(), "{:?}", last_error());
        assert_eq!(definitions.calls(), 1, "initial definitions load");

        assert_eq!(posthog_shutdown(client), PostHogStatus::POSTHOG_STATUS_OK);
        let definitions_at_shutdown = definitions.calls();
        let remote_at_shutdown = remote.calls();

        let mut flags = ptr::null_mut();
        let status = posthog_evaluate_flags(client, c("user-1").as_ptr(), ptr::null(), &mut flags);
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_INVALID_ARGUMENT);
        assert!(flags.is_null());
        assert!(last_error().unwrap().contains("shut down"));

        let status = posthog_reload_feature_flags(client);
        assert_eq!(status, PostHogStatus::POSTHOG_STATUS_INVALID_ARGUMENT);

        // Longer than the poll interval, so a running poller would have fetched again.
        std::thread::sleep(std::time::Duration::from_millis(1600));
        assert_eq!(
            definitions.calls(),
            definitions_at_shutdown,
            "poller kept running"
        );
        assert_eq!(
            remote.calls(),
            remote_at_shutdown,
            "flags requested after shutdown"
        );

        posthog_client_free(client);
    }
}
