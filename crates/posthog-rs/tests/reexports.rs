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

#[cfg(feature = "stateful")]
#[test]
fn reexports_stateful_client_and_its_public_entry_points() {
    use posthog_rs::stateful::{CaptureOutcome, Client, Config, DeliveryOutcome, Persistence};
    assert_same_type::<Client, posthog::stateful::Client>();
    assert_same_type::<Config, posthog::stateful::Config>();
    assert_same_type::<posthog_rs::stateful::Capture, posthog::stateful::Capture>();
    assert_same_type::<posthog_rs::stateful::DrainReport, posthog::stateful::DrainReport>();
    assert_same_type::<posthog_rs::stateful::Identify, posthog::stateful::Identify>();
    assert_same_type::<posthog_rs::stateful::PersonProfiles, posthog::stateful::PersonProfiles>();
    assert_same_type::<posthog_rs::stateful::SessionMode, posthog::stateful::SessionMode>();
    assert_same_type::<
        posthog_rs::stateful::SessionPersistence,
        posthog::stateful::SessionPersistence,
    >();
    assert_same_type::<
        posthog_rs::stateful::FeatureFlagContext,
        posthog::stateful::FeatureFlagContext,
    >();
    assert_same_type::<posthog_rs::stateful::FeatureFlagResult, posthog::stateful::FeatureFlagResult>(
    );
    assert_same_type::<
        posthog_rs::stateful::FeatureFlagsUpdate,
        posthog::stateful::FeatureFlagsUpdate,
    >();
    assert_same_type::<
        posthog_rs::stateful::FeatureFlagLoadError,
        posthog::stateful::FeatureFlagLoadError,
    >();
    assert_same_type::<
        posthog_rs::stateful::FeatureFlagsSubscription,
        posthog::stateful::FeatureFlagsSubscription,
    >();
    let mut config = Config::new("phc_test");
    config.preload_feature_flags = false;
    config.send_feature_flag_event = false;
    config.bootstrap_feature_flags = Some(std::collections::HashMap::from([(
        "ready".into(),
        posthog_rs::FlagValue::Boolean(true),
    )]));
    config.bootstrap_feature_flag_payloads = Some(Default::default());
    config.on_feature_flags(|_| {});
    config.session_mode = posthog_rs::stateful::SessionMode::Manual;
    config.session_persistence = posthog_rs::stateful::SessionPersistence::Persistent;
    config.persistence = Persistence::Memory;
    config.disabled = true;
    config.opt_out = true;
    let client = Client::init_blocking(config);
    client.reload_feature_flags();
    client.set_person_properties_for_flags(Default::default(), Some(false));
    client.reset_person_properties_for_flags(None);
    client.set_group_properties_for_flags("company", Default::default(), Some(false));
    client.reset_group_properties_for_flags(None, None);
    assert_eq!(client.get_feature_flag("ready", None), None);
    assert_eq!(client.is_feature_enabled("ready", Some(false)), None);
    assert_eq!(client.get_feature_flag_result("ready", None), None);
    assert_eq!(client.get_feature_flag_payload("ready"), None);
    assert_eq!(client.get_feature_flags(), None);
    assert_eq!(client.get_feature_flag_payloads(), None);
    let subscription = client.on_feature_flags(|_| {});
    subscription.clone().unsubscribe();
    subscription.unsubscribe();
    client.identify(posthog_rs::stateful::Identify::new("user"));
    client.alias("alias");
    client.set_person_properties(Default::default(), Default::default());
    client.create_person_profile();
    client.register(Default::default());
    client.unregister("key");
    client.group("company", "example", None);
    client.group_identify("company", "example", Default::default());
    client.remove_group("company");
    client.start_session();
    client.session_activity();
    client.set_app_in_background(true);
    client.reset_session();
    client.end_session();
    assert_eq!(client.session_id(), None);
    assert_eq!(client.peek_session_id(), None);
    client.reset();
    client.opt_out();
    assert!(client.is_opt_out());
    client.opt_in();
    assert!(client.is_opt_out());
    let _: CaptureOutcome = CaptureOutcome::OptedOut;
    assert!(!client.is_identified());
    assert_eq!(client.capture("disabled"), CaptureOutcome::Disabled);
    assert_eq!(client.flush_blocking().delivery, DeliveryOutcome::Disabled);
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        DeliveryOutcome::Disabled
    );
}

#[test]
fn shares_global_state() {
    assert!(!posthog::global_is_disabled());
    posthog_rs::disable_global();
    assert!(posthog::global_is_disabled());
}
