use std::any::TypeId;

fn assert_same_type<T: 'static, U: 'static>() {
    assert_eq!(TypeId::of::<T>(), TypeId::of::<U>());
}

#[test]
fn reexports_canonical_types() {
    assert_same_type::<posthog_rs::Event, posthog::Event>();
    assert_same_type::<posthog_rs::Client, posthog::Client>();
    assert_same_type::<posthog_rs::RequestContext, posthog::RequestContext>();

    #[cfg(feature = "async-client")]
    assert_same_type::<posthog_rs::PostHogReqwestClient, posthog::PostHogReqwestClient>();

    #[cfg(feature = "error-tracking")]
    assert_same_type::<posthog_rs::CaptureExceptionOptions, posthog::CaptureExceptionOptions>();

    #[cfg(feature = "tower")]
    assert_same_type::<posthog_rs::tower::PostHogLayer, posthog::tower::PostHogLayer>();

    #[cfg(feature = "mcp")]
    assert_same_type::<posthog_rs::mcp::McpOptions, posthog::mcp::McpOptions>();
}

#[cfg(not(any(
    feature = "default",
    feature = "async-client",
    feature = "tower",
    feature = "mcp"
)))]
#[test]
fn exposes_blocking_client() {
    let _: fn(posthog_rs::ClientOptions) -> posthog_rs::Client = posthog_rs::client;
}

#[test]
fn shares_global_state() {
    assert!(!posthog::global_is_disabled());
    posthog_rs::disable_global();
    assert!(posthog::global_is_disabled());
}
