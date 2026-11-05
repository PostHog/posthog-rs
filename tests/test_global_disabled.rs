//! Separate process from test_global: disabling initialization cannot be reset.
use httpmock::prelude::*;

#[test]
fn disabling_global_prevents_initialization_and_delivery() {
    let server = MockServer::start();
    let requests = server.mock(|when, then| {
        when.any_request();
        then.status(200);
    });
    let options = posthog::ClientOptionsBuilder::default()
        .api_key("phc_test".to_string())
        .host(server.base_url())
        .enable_local_evaluation(true)
        .secret_key("phx_test")
        .build()
        .unwrap();

    posthog::disable_global();
    posthog::disable_global();
    assert!(posthog::global_is_disabled());
    #[cfg(feature = "async-client")]
    futures::executor::block_on(posthog::init_global(options)).unwrap();
    #[cfg(not(feature = "async-client"))]
    posthog::init_global(options).unwrap();
    posthog::capture(posthog::Event::new("not-sent", "user-1"));
    #[cfg(feature = "async-client")]
    futures::executor::block_on(posthog::shutdown());
    #[cfg(not(feature = "async-client"))]
    posthog::shutdown();
    requests.assert_calls(0);
}
