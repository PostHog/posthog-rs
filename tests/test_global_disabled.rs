//! Separate process from test_global: disabling initialization cannot be reset.
use httpmock::prelude::*;

#[test]
fn disabling_global_prevents_initialization_and_delivery() {
    let server = MockServer::start();
    let requests = server.mock(|when, then| {
        when.any_request();
        then.status(200);
    });
    let options = posthog_rs::ClientOptionsBuilder::default()
        .api_key("phc_test".to_string())
        .host(server.base_url())
        .enable_local_evaluation(true)
        .secret_key("phx_test")
        .build()
        .unwrap();

    posthog_rs::disable_global();
    posthog_rs::disable_global();
    assert!(posthog_rs::global_is_disabled());
    #[cfg(feature = "async-client")]
    futures::executor::block_on(posthog_rs::init_global(options)).unwrap();
    #[cfg(not(feature = "async-client"))]
    posthog_rs::init_global(options).unwrap();
    posthog_rs::capture(posthog_rs::Event::new("not-sent", "user-1"));
    #[cfg(feature = "async-client")]
    futures::executor::block_on(posthog_rs::shutdown());
    #[cfg(not(feature = "async-client"))]
    posthog_rs::shutdown();
    requests.assert_calls(0);
}
