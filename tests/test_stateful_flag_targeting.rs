#![cfg(feature = "stateful")]

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use httpmock::{Method::POST, MockServer};
use posthog::stateful::{
    Client, Config, Identify, Persistence, PersistenceOutcome, PersonProfiles,
};
use serde_json::{json, Value};

fn properties(value: Value) -> HashMap<String, Value> {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn config(server: &MockServer) -> Config {
    let mut config = Config::new("phc_test");
    config.preload_feature_flags = false;
    config.host = server.base_url();
    config.persistence = Persistence::Memory;
    config.bootstrap_anonymous_id = Some("device".into());
    config.flush_interval = Duration::from_secs(3600);
    config.shutdown_timeout = Duration::from_secs(1);
    config
}

fn reload(
    server: &MockServer,
    client: &Client,
    id: &str,
    person: Value,
    groups: Value,
    group_properties: Value,
    action: impl FnOnce(),
) {
    let mut response = server.mock(|when, then| {
        when.method(POST)
            .path("/flags/")
            .query_param("v", "2")
            .json_body(json!({
                "api_key":"phc_test", "distinct_id":id,
                "$anon_distinct_id":"device", "$device_id":"device", "geoip_disable":false,
                "person_properties":person, "groups":groups, "group_properties":group_properties
            }));
        then.status(200).json_body(json!({"featureFlags": {}}));
    });
    action();
    client.flush_blocking();
    response.assert_calls(1);
    response.delete();
}

#[test]
fn targeting_setters_merge_and_reset_with_default_or_suppressed_reload_while_out_and_never() {
    let server = MockServer::start();
    let mut conf = config(&server);
    conf.opt_out = true;
    conf.person_profiles = PersonProfiles::Never;
    let hooks = Arc::new(AtomicUsize::new(0));
    let seen = hooks.clone();
    conf.before_send(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        None
    });
    let client = Client::init_blocking(conf);
    client.set_person_properties_for_flags(
        properties(json!({"plan":"free", "nullable":null})),
        Some(false),
    );
    client.set_group_properties_for_flags("company", properties(json!({"tier":1})), Some(false));
    client.set_group_properties_for_flags(
        "project",
        properties(json!({"active":true})),
        Some(false),
    );
    reload(
        &server,
        &client,
        "device",
        json!({"plan":"pro", "nullable":null}),
        json!({}),
        json!({"company":{"tier":1},"project":{"active":true}}),
        || {
            client.set_person_properties_for_flags(properties(json!({"plan":"pro"})), None);
        },
    );
    reload(
        &server,
        &client,
        "device",
        json!({"plan":"pro", "nullable":null}),
        json!({}),
        json!({"company":{"tier":2,"extra":true},"project":{"active":true}}),
        || {
            client.set_group_properties_for_flags(
                "company",
                properties(json!({"tier":2,"extra":true})),
                Some(true),
            );
        },
    );
    reload(
        &server,
        &client,
        "device",
        json!({}),
        json!({}),
        json!({"company":{"tier":2,"extra":true},"project":{"active":true}}),
        || {
            client.reset_person_properties_for_flags(None);
        },
    );
    reload(
        &server,
        &client,
        "device",
        json!({}),
        json!({}),
        json!({"project":{"active":true}}),
        || {
            client.reset_group_properties_for_flags(Some("company"), None);
        },
    );
    client.reset_group_properties_for_flags(None, Some(false));
    reload(
        &server,
        &client,
        "device",
        json!({}),
        json!({}),
        json!({}),
        || {
            client.reset_group_properties_for_flags(None, None);
        },
    );
    assert_eq!(hooks.load(Ordering::SeqCst), 0);
    assert!(!client.is_identified());
    assert_eq!(client.get_distinct_id(), "device");
}

#[test]
fn empty_or_invalid_setters_do_not_reload_but_unchanged_nonempty_setters_do() {
    let server = MockServer::start();
    let calls = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"featureFlags":{}}));
    });
    let client = Client::init_blocking(config(&server));
    client.set_person_properties_for_flags(HashMap::new(), None);
    client.set_group_properties_for_flags("company", HashMap::new(), None);
    client.set_group_properties_for_flags("  ", properties(json!({"p":1})), None);
    client.reset_group_properties_for_flags(Some(" "), None);
    client.flush_blocking();
    calls.assert_calls(0);
    for count in 1..=2 {
        client.set_person_properties_for_flags(properties(json!({"p":1})), None);
        client.flush_blocking();
        calls.assert_calls(count);
    }
    client.reset_person_properties_for_flags(None);
    client.flush_blocking();
    client.reset_person_properties_for_flags(None);
    client.flush_blocking();
    calls.assert_calls(4);
    client.shutdown_blocking(None);
    client.set_person_properties_for_flags(properties(json!({"p":2})), None);
    client.set_group_properties_for_flags("company", properties(json!({"p":2})), None);
    client.reset_person_properties_for_flags(None);
    client.reset_group_properties_for_flags(None, None);
    calls.assert_calls(4);
    let mut disabled = config(&server);
    disabled.disabled = true;
    let disabled = Client::init_blocking(disabled);
    disabled.set_person_properties_for_flags(properties(json!({"p":2})), None);
    disabled.reset_group_properties_for_flags(None, None);
    calls.assert_calls(4);
}

#[test]
fn identity_mirroring_merges_set_once_and_set_and_only_anonymous_transitions_reload() {
    for same_id in [false, true] {
        let server = MockServer::start();
        let mut conf = config(&server);
        conf.opt_out = true;
        let client = Client::init_blocking(conf);
        client.set_person_properties_for_flags(
            properties(json!({"keep":"old", "nullable":null})),
            Some(false),
        );
        let id = if same_id { "device" } else { "person" };
        let mut input = Identify::new(id);
        input.set_once =
            properties(json!({"keep":"ignored", "new":1, "both":"once", "nullable":"ignored"}));
        input.set = properties(json!({"both":"set"}));
        let expected = json!({"keep":"old", "new":1, "both":"set", "nullable":null});
        reload(
            &server,
            &client,
            id,
            expected.clone(),
            json!({}),
            json!({}),
            || client.identify(input.clone()),
        );
        let unexpected = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(json!({"featureFlags":{}}));
        });
        client.identify(input);
        client.identify("different-identified-user");
        client.identify(" ");
        client.set_person_properties(
            properties(json!({"later":true})),
            properties(json!({"keep":"ignored"})),
        );
        client.flush_blocking();
        unexpected.assert_calls(0);
        let mut unexpected = unexpected;
        unexpected.delete();
        let mut expected = expected;
        expected["later"] = json!(true);
        reload(&server, &client, id, expected, json!({}), json!({}), || {
            client.reload_feature_flags()
        });
    }
}

#[test]
fn analytics_dedup_does_not_prevent_restoring_explicitly_cleared_targeting() {
    let server = MockServer::start();
    let mut conf = config(&server);
    let hooks = Arc::new(AtomicUsize::new(0));
    let seen = hooks.clone();
    conf.before_send(move |event| {
        seen.fetch_add(1, Ordering::SeqCst);
        Some(event)
    });
    let _analytics = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(200);
    });
    let client = Client::init_blocking(conf);
    let set = properties(json!({"plan":"pro"}));
    client.set_person_properties(set.clone(), HashMap::new());
    assert_eq!(hooks.load(Ordering::SeqCst), 1);
    client.reset_person_properties_for_flags(Some(false));
    client.set_person_properties(set.clone(), HashMap::new());
    assert_eq!(hooks.load(Ordering::SeqCst), 1);
    reload(
        &server,
        &client,
        "device",
        json!({"plan":"pro"}),
        json!({}),
        json!({}),
        || client.reload_feature_flags(),
    );
    client.identify("device"); // Promotion without properties does not refresh.
    let mut input = Identify::new("device");
    input.set = set;
    client.identify(input.clone());
    let before = hooks.load(Ordering::SeqCst);
    client.reset_person_properties_for_flags(Some(false));
    client.identify(input);
    assert_eq!(hooks.load(Ordering::SeqCst), before);
    reload(
        &server,
        &client,
        "device",
        json!({"plan":"pro"}),
        json!({}),
        json!({}),
        || client.reload_feature_flags(),
    );
}

#[test]
fn group_mirroring_is_scoped_to_membership_and_only_membership_changes_refresh() {
    let server = MockServer::start();
    let mut conf = config(&server);
    conf.before_send(|_| None);
    let client = Client::init_blocking(conf);
    client.set_group_properties_for_flags("company", properties(json!({"old":true})), Some(false));
    reload(
        &server,
        &client,
        "device",
        json!({}),
        json!({"company":"a"}),
        json!({"company":{"tier":1}}),
        || {
            client.group("company", "a", Some(properties(json!({"tier":1}))));
        },
    );
    let mut unexpected = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"featureFlags":{}}));
    });
    client.group("company", "a", Some(properties(json!({"tier":2}))));
    client.group_identify("company", "other", properties(json!({"wrong":true})));
    client.group_identify("company", "a", properties(json!({"direct":true})));
    client.group_identify("absent", "x", properties(json!({"wrong":true})));
    client.remove_group("missing");
    client.flush_blocking();
    unexpected.assert_calls(0);
    unexpected.delete();
    reload(
        &server,
        &client,
        "device",
        json!({}),
        json!({"company":"a"}),
        json!({"company":{"tier":2,"direct":true}}),
        || client.reload_feature_flags(),
    );
    reload(
        &server,
        &client,
        "device",
        json!({}),
        json!({"company":"b"}),
        json!({"company":{"new":null}}),
        || {
            client.group("company", "b", Some(properties(json!({"new":null}))));
        },
    );
    client.opt_out();
    client.group(
        "company",
        "ignored",
        Some(properties(json!({"wrong":true}))),
    );
    reload(
        &server,
        &client,
        "device",
        json!({}),
        json!({}),
        json!({}),
        || client.remove_group("company"),
    );
}

#[test]
fn mandatory_targeting_bounds_reject_whole_mutations_and_survive_restart() {
    let server = MockServer::start();
    let directory =
        std::env::temp_dir().join(format!("posthog-targeting-{}", uuid::Uuid::now_v7()));
    let mut unexpected = server.mock(|when, then| {
        when.method(POST).path("/flags/");
        then.status(200).json_body(json!({"featureFlags":{}}));
    });
    let conf = || {
        let mut conf = config(&server);
        conf.persistence = Persistence::Directory(directory.clone());
        conf.max_state_bytes = 2048;
        conf.before_send(|_| None);
        conf
    };
    let client = Client::init_blocking(conf());
    client.set_person_properties_for_flags(properties(json!({"plan":"pro"})), Some(false));
    client.set_group_properties_for_flags("company", properties(json!({"tier":1})), Some(false));
    client.set_person_properties_for_flags(
        properties(json!({"plan":"bad", "large":"x".repeat(4096)})),
        None,
    );
    let mut deep = json!(1);
    for _ in 0..150 {
        deep = json!({"child":deep});
    }
    client.set_group_properties_for_flags("company", HashMap::from([("deep".into(), deep)]), None);
    let mut rejected_identity = Identify::new("person");
    rejected_identity.set = properties(json!({"large":"x".repeat(4096)}));
    client.identify(rejected_identity);
    assert_eq!(client.get_distinct_id(), "device");
    client.flush_blocking();
    unexpected.assert_calls(0);
    unexpected.delete();
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    let client = Client::init_blocking(conf());
    reload(
        &server,
        &client,
        "device",
        json!({"plan":"pro"}),
        json!({}),
        json!({"company":{"tier":1}}),
        || client.reload_feature_flags(),
    );
    client.reset_person_properties_for_flags(Some(false));
    client.reset_group_properties_for_flags(None, Some(false));
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    drop(client);
    let client = Client::init_blocking(conf());
    reload(
        &server,
        &client,
        "device",
        json!({}),
        json!({}),
        json!({}),
        || client.reload_feature_flags(),
    );
    client.shutdown_blocking(None);
    drop(client);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn mirrored_targeting_is_visible_to_reentrant_hooks_and_survives_event_drop() {
    let server = MockServer::start();
    let mut conf = config(&server);
    let holder = Arc::new(std::sync::Mutex::new(std::sync::Weak::<Client>::new()));
    let inside = holder.clone();
    conf.before_send(move |_| {
        inside
            .lock()
            .unwrap()
            .upgrade()
            .unwrap()
            .reload_feature_flags();
        None
    });
    let client = Arc::new(Client::init_blocking(conf));
    *holder.lock().unwrap() = Arc::downgrade(&client);
    reload(
        &server,
        &client,
        "device",
        json!({"plan":"pro"}),
        json!({}),
        json!({}),
        || {
            client.set_person_properties(properties(json!({"plan":"pro"})), HashMap::new());
        },
    );
}

#[test]
fn mirrored_targeting_survives_full_analytics_queue_without_reloading_automatically() {
    use posthog::stateful::CaptureOutcome;
    let server = MockServer::start();
    let mut conf = config(&server);
    conf.max_queue_size = 1;
    let client = Client::init_blocking(conf);
    assert!(matches!(
        client.capture("occupy"),
        CaptureOutcome::Accepted { .. }
    ));
    client.set_person_properties(properties(json!({"plan":"pro"})), HashMap::new());
    assert_eq!(client.capture("still-full"), CaptureOutcome::Full);
    let _analytics = server.mock(|when, then| {
        when.method(POST).path("/i/v1/analytics/events");
        then.status(200);
    });
    reload(
        &server,
        &client,
        "device",
        json!({"plan":"pro"}),
        json!({}),
        json!({}),
        || client.reload_feature_flags(),
    );
}

#[cfg(feature = "async-client")]
#[test]
fn asynchronous_initialization_exposes_the_same_synchronous_targeting_api() {
    let server = MockServer::start();
    let client = futures::executor::block_on(Client::init(config(&server)));
    reload(
        &server,
        &client,
        "device",
        json!({"plan":"pro"}),
        json!({}),
        json!({}),
        || {
            client.set_person_properties_for_flags(properties(json!({"plan":"pro"})), None);
        },
    );
    futures::executor::block_on(client.shutdown(None));
}
