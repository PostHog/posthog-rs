use super::*;
use crate::client::stateful::flag_state::{CachedFlag, FlagCache, FlagContext};
use crate::client::stateful::writer::{Backend, DrainOutcome, StateWriter, WriterOptions};
use crate::client::transport::SystemClock;
use crate::feature_flag_evaluations::EvaluatedFlagRecord;
use serde_json::{json, Value};
use std::collections::HashMap;

fn with_cache(payload: Option<Value>) -> Snapshot {
    let mut snapshot = Snapshot::anonymous("device".into());
    snapshot.distinct_id = "person".into();
    snapshot.identified = true;
    snapshot
        .flags
        .person_properties
        .insert("plan".into(), json!("pro"));
    snapshot.groups.insert("company".into(), "a".into());
    snapshot
        .flags
        .group_properties
        .insert("company".into(), HashMap::from([("tier".into(), json!(2))]));
    snapshot.flags.cache = Some(Arc::new(FlagCache {
        context: FlagContext::current(&snapshot, false),
        flags: HashMap::from([(
            "checkout".into(),
            CachedFlag {
                context: Arc::new(FlagContext::current(&snapshot, false)),
                revision: 0,
                partial: false,
                record: EvaluatedFlagRecord {
                    enabled: true,
                    variant: Some("blue".into()),
                    payload,
                    id: Some(1),
                    version: Some(3),
                    reason: Some("Matched".into()),
                    locally_evaluated: false,
                    has_experiment: Some(false),
                    minimal_flag_called_events: true,
                },
                request_id: Some("first-request".into()),
                evaluated_at: Some(123),
            },
        )]),
    }));
    snapshot
}

#[test]
fn cloning_snapshot_shares_unchanged_flag_cache() {
    let snapshot = with_cache(Some(json!({"nested": null})));
    let cloned = snapshot.clone();
    assert!(std::ptr::eq(
        &snapshot.flags.cache.as_ref().unwrap().flags,
        &cloned.flags.cache.as_ref().unwrap().flags,
    ));
    assert_eq!(snapshot, cloned);
}

#[test]
fn writer_mutations_share_unchanged_flag_cache() {
    let writer = StateWriter::new(
        with_cache(Some(json!({"nested": null}))),
        Backend::Memory,
        WriterOptions::default(),
        vec![],
        Arc::new(SystemClock),
    )
    .unwrap();
    let before = writer.snapshot();
    writer.mutate(|state| state.generation += 1).unwrap();
    let transient = writer.snapshot();
    assert_eq!(transient.generation, before.generation + 1);
    assert!(std::ptr::eq(
        &before.flags.cache.as_ref().unwrap().flags,
        &transient.flags.cache.as_ref().unwrap().flags,
    ));
    writer.mutate(|state| state.opt_out = Some(true)).unwrap();
    let persisted = writer.snapshot();
    assert_eq!(persisted.opt_out, Some(true));
    assert_eq!(before.opt_out, None);
    assert!(std::ptr::eq(
        &before.flags.cache.as_ref().unwrap().flags,
        &persisted.flags.cache.as_ref().unwrap().flags,
    ));
}

#[test]
fn reconciling_shared_cache_preserves_retained_snapshots() {
    let mut encoded = serde_json::to_value(with_cache(Some(json!({"nested": null})))).unwrap();
    encoded["flags"]["cache"]["flags"]["checkout"]["context"]["person_properties"]["plan"] =
        json!("previous-plan");
    let before: Snapshot = serde_json::from_value(encoded.clone()).unwrap();
    let mut restored = before.clone();
    assert!(restored.reconcile_flag_cache(false));
    assert!(restored.flags.cache.as_ref().unwrap().flags.is_empty());
    assert_eq!(serde_json::to_value(&before).unwrap(), encoded);

    let current = with_cache(None);
    let mut unchanged = current.clone();
    assert!(!unchanged.reconcile_flag_cache(false));
    assert!(std::ptr::eq(
        &current.flags.cache.as_ref().unwrap().flags,
        &unchanged.flags.cache.as_ref().unwrap().flags,
    ));
}

#[test]
fn cached_records_round_trip_payload_presence_and_per_record_metadata() {
    for payload in [
        None,
        Some(Value::Null),
        Some(json!(false)),
        Some(json!(0)),
        Some(json!("")),
        Some(json!("123")),
        Some(json!("true")),
        Some(json!([1, null])),
        Some(json!({"nested":null})),
    ] {
        let mut snapshot = with_cache(payload);
        let cache = Arc::make_mut(snapshot.flags.cache.as_mut().unwrap());
        let mut second = cache.flags["checkout"].clone();
        second.request_id = Some("second-request".into());
        second.record.minimal_flag_called_events = false;
        second.record.enabled = false;
        second.record.variant = None;
        cache.flags.insert("other".into(), second);
        let directory = TestDirectory::new();
        let (mut store, _) = FileStateStore::open(directory.lease(), 8192).unwrap();
        store.save(&snapshot).unwrap();
        assert_eq!(store.load().unwrap(), Some(snapshot));
    }
}

#[test]
fn optional_cache_byte_pressure_does_not_displace_mandatory_state_or_live_cache() {
    let mut snapshot = with_cache(Some(json!("x".repeat(4096))));
    let capacity = snapshot_capacity(&snapshot);
    assert!(snapshot_size(&snapshot) > capacity);
    let original_cache = snapshot.flags.cache.clone();
    let encoded = encode_snapshot(&snapshot, capacity as u64).unwrap();
    let mut restored = FileStateStore::decode(&encoded).unwrap();
    assert!(restored.flags.cache.is_none());
    assert_eq!(
        restored.flags.person_properties,
        snapshot.flags.person_properties
    );
    assert_eq!(
        restored.flags.group_properties,
        snapshot.flags.group_properties
    );
    assert_eq!(snapshot.flags.cache, original_cache);
    for choice in [Some(false), Some(true), None] {
        snapshot.opt_out = choice;
        let bytes = encode_snapshot(&snapshot, capacity as u64).unwrap();
        assert!(bytes.len() <= capacity);
        restored = FileStateStore::decode(&bytes).unwrap();
        assert_eq!(restored.opt_out, choice);
        assert_eq!(restored.distinct_id, "person");
        assert!(restored.flags.cache.is_none());
    }
    snapshot
        .flags
        .person_properties
        .insert("oversized".into(), json!("x".repeat(capacity)));
    assert!(encode_snapshot(&snapshot, capacity as u64).is_err());
}

#[test]
fn optional_cache_codec_failure_isolated_from_mandatory_codec_failure() {
    let mut deep = json!(0);
    for _ in 0..150 {
        deep = json!({"nested":deep});
    }
    let mut snapshot = with_cache(Some(deep.clone()));
    let bytes = encode_snapshot(&snapshot, 16_384).unwrap();
    let restored = FileStateStore::decode(&bytes).unwrap();
    assert!(restored.flags.cache.is_none());
    assert_eq!(restored.distinct_id, "person");
    assert!(snapshot.flags.cache.is_some());
    // Optional omission must not hide invalid mandatory targeting.
    snapshot.flags.person_properties.insert("deep".into(), deep);
    assert!(encode_snapshot(&snapshot, 16_384).is_err());
}

#[test]
fn fitting_cache_is_evicted_from_disk_before_mandatory_updates_are_rejected() {
    let snapshot = with_cache(Some(json!("x".repeat(512))));
    let directory = TestDirectory::new();
    let limit = snapshot_size(&snapshot) + 1;
    let (store, _) = FileStateStore::open(directory.lease(), limit as u64).unwrap();
    let writer = StateWriter::new(
        snapshot.clone(),
        Backend::Persistent {
            store: Box::new(store),
            restored: false,
        },
        WriterOptions {
            max_bytes: limit,
            ..WriterOptions::default()
        },
        vec![],
        Arc::new(SystemClock),
    )
    .unwrap();
    assert_eq!(
        writer.wait_blocking(writer.checkpoint()),
        DrainOutcome::Persisted
    );
    let initial =
        FileStateStore::decode(&fs::read(directory.path().join("state.json")).unwrap()).unwrap();
    assert!(initial.flags.cache.is_some());
    writer
        .mutate(|state| {
            state
                .registered
                .insert("padding".into(), json!("x".repeat(512)));
            state.opt_out = Some(false);
            state
                .flags
                .person_properties
                .insert("team".into(), json!("next"));
        })
        .unwrap();
    assert_eq!(
        writer.wait_blocking(writer.checkpoint()),
        DrainOutcome::Persisted
    );
    let disk =
        FileStateStore::decode(&fs::read(directory.path().join("state.json")).unwrap()).unwrap();
    assert_eq!(disk.opt_out, Some(false));
    assert_eq!(disk.flags.person_properties["team"], "next");
    assert_eq!(disk.registered["padding"], json!("x".repeat(512)));
    assert!(disk.flags.cache.is_none());
    assert_eq!(writer.snapshot().flags.cache, snapshot.flags.cache);
}

#[test]
fn cache_and_targeting_mutations_complete_while_state_io_is_blocked() {
    struct BlockedStore {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
        blocked: bool,
    }
    impl StateStore for BlockedStore {
        fn save(&mut self, _snapshot: &Snapshot) -> io::Result<SaveOutcome> {
            if !self.blocked {
                self.blocked = true;
                self.entered.send(()).unwrap();
                self.release.recv().unwrap();
            }
            Ok(SaveOutcome::default())
        }
    }
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let writer = Arc::new(
        StateWriter::new(
            Snapshot::anonymous("device".into()),
            Backend::Persistent {
                store: Box::new(BlockedStore {
                    entered: entered_tx,
                    release: release_rx,
                    blocked: false,
                }),
                restored: false,
            },
            WriterOptions::default(),
            vec![],
            Arc::new(SystemClock),
        )
        .unwrap(),
    );
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let updating = writer.clone();
    let thread = std::thread::spawn(move || {
        updating
            .mutate(|state| *state = with_cache(Some(json!("value"))))
            .unwrap();
        // Identity changes invalidate the old cache; installation is a separate
        // operation guarded by the request's resulting evaluation revision.
        updating
            .mutate(|state| state.flags.cache = with_cache(Some(json!("value"))).flags.cache)
            .unwrap();
        let snapshot = updating.snapshot();
        done_tx
            .send((
                snapshot.flags.person_properties["plan"].clone(),
                snapshot.flags.cache.is_some(),
            ))
            .unwrap();
    });
    let completed = done_rx.recv_timeout(std::time::Duration::from_secs(5));
    // Always release controlled I/O before an assertion can unwind and join.
    release_tx.send(()).unwrap();
    thread.join().unwrap();
    assert_eq!(completed.unwrap(), (json!("pro"), true));
    assert_eq!(
        writer.wait_blocking(writer.checkpoint()),
        DrainOutcome::Persisted
    );
}

#[test]
fn current_flag_fields_are_required_and_invalid_records_are_preserved() {
    let valid: Value = serde_json::from_slice(&record(&with_cache(Some(json!(false))))).unwrap();
    for path in [
        vec!["flags"],
        vec!["flags", "person_properties"],
        vec!["flags", "group_properties"],
        vec!["flags", "cache"],
    ] {
        let mut invalid = valid.clone();
        let mut parent = &mut invalid["state"];
        for key in &path[..path.len() - 1] {
            parent = &mut parent[*key];
        }
        parent.as_object_mut().unwrap().remove(path[path.len() - 1]);
        assert!(FileStateStore::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
    for field in ["unknown", "cache"] {
        let mut invalid = valid.clone();
        invalid["state"]["flags"][field] = json!(42);
        let bytes = serde_json::to_vec(&invalid).unwrap();
        let directory = TestDirectory::new();
        let lease = directory.lease();
        fs::write(directory.path().join("state.json"), &bytes).unwrap();
        assert!(FileStateStore::open(lease, 8192).is_err());
        assert_eq!(
            fs::read(directory.path().join("state.json")).unwrap(),
            bytes
        );
    }
}

fn checkout_payload(snapshot: &Snapshot) -> Option<Value> {
    snapshot
        .flags
        .cache
        .as_ref()?
        .flags
        .get("checkout")?
        .record
        .payload
        .clone()
}

fn seed_client_cache(directory: &TestDirectory, identified: bool) -> crate::stateful::Config {
    let mut config = crate::stateful::Config::new("phc_test");
    config.preload_feature_flags = false;
    config.host = "http://127.0.0.1:1".into();
    config.persistence = Persistence::Directory(directory.0.clone());
    let scope = namespace(&config.host, &config.project_token, &config.instance_name);
    let mut restored = RestoredStorage::open(
        &config.persistence,
        &scope,
        config.max_state_bytes as u64,
        config.event_limits(),
        &[],
    );
    let mut snapshot = with_cache(Some(json!("bootstrap-cache")));
    snapshot.identified = identified;
    if !identified {
        snapshot.distinct_id = snapshot.anonymous_id.clone();
    }
    let context = FlagContext::current(&snapshot, config.disable_geoip);
    let cache = Arc::make_mut(snapshot.flags.cache.as_mut().unwrap());
    cache.context = context.clone();
    let context = Arc::new(context);
    for flag in cache.flags.values_mut() {
        flag.context = context.clone();
    }
    restored
        .state_store
        .as_mut()
        .unwrap()
        .save(&snapshot)
        .unwrap();
    config
}

#[test]
fn bootstrap_reconciles_cache_against_final_identity() {
    use crate::stateful::{Client, PersistenceOutcome, PersonProfiles};
    for opted_out in [false, true] {
        for (identified, bootstrap, policy, expected, keep_cache) in [
            (
                false,
                "new-person",
                PersonProfiles::IdentifiedOnly,
                "new-person",
                false,
            ),
            (
                false,
                "device",
                PersonProfiles::IdentifiedOnly,
                "device",
                true,
            ),
            (false, "new-person", PersonProfiles::Never, "device", true),
            (
                true,
                "new-person",
                PersonProfiles::IdentifiedOnly,
                "person",
                true,
            ),
        ] {
            let directory = TestDirectory::new();
            let mut config = seed_client_cache(&directory, identified);
            config.bootstrap_identified_id = Some(bootstrap.into());
            config.person_profiles = policy;
            config.opt_out = opted_out;
            config.before_send(|_| None);
            let scope = namespace(&config.host, &config.project_token, &config.instance_name);
            let client = Client::init_blocking(config);
            assert_eq!(client.get_distinct_id(), expected);
            let snapshot = client.state.as_ref().unwrap().snapshot();
            assert_eq!(snapshot.flags.cache.is_some(), keep_cache,
                "cache must match final bootstrap identity (out={opted_out}, bootstrap={bootstrap}, expected={expected})");
            if let Some(cache) = &snapshot.flags.cache {
                assert_eq!(cache.context, FlagContext::current(&snapshot, false));
                assert_eq!(checkout_payload(&snapshot), Some(json!("bootstrap-cache")));
            }
            assert_eq!(
                client.shutdown_blocking(None).state,
                PersistenceOutcome::Persisted
            );
            drop(client);
            let disk = FileStateStore::decode(
                &fs::read(directory.0.join(scope).join("state.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(disk.distinct_id, expected);
            assert_eq!(disk.flags.cache.is_some(), keep_cache);
            assert_eq!(
                checkout_payload(&disk),
                keep_cache.then(|| json!("bootstrap-cache"))
            );
        }
    }
}

#[test]
fn identify_invalidates_changed_identity_cache_before_running_hooks() {
    use crate::stateful::Client;
    for (new_id, keep_cache) in [("new-person", false), ("device", true)] {
        let directory = TestDirectory::new();
        let mut config = seed_client_cache(&directory, false);
        let holder = Arc::new(Mutex::new(std::sync::Weak::<Client>::new()));
        let in_hook = holder.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        config.before_send(move |_| {
            let client = in_hook.lock().unwrap().upgrade().unwrap();
            let snapshot = client.state.as_ref().unwrap().snapshot();
            tx.send((
                snapshot.distinct_id.clone(),
                snapshot.flags.cache.is_some(),
                checkout_payload(&snapshot),
            ))
            .unwrap();
            None
        });
        let client = Arc::new(Client::init_blocking(config));
        *holder.lock().unwrap() = Arc::downgrade(&client);
        client.identify(new_id);
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            (
                new_id.to_string(),
                keep_cache,
                keep_cache.then(|| json!("bootstrap-cache"))
            )
        );
        assert_eq!(
            checkout_payload(&client.state.as_ref().unwrap().snapshot()),
            keep_cache.then(|| json!("bootstrap-cache"))
        );
        assert_eq!(
            client
                .state
                .as_ref()
                .unwrap()
                .snapshot()
                .flags
                .cache
                .is_some(),
            keep_cache
        );
    }
}

#[test]
fn cache_restore_requires_exact_evaluation_context_but_ignores_analytics_and_consent() {
    let original = with_cache(Some(json!("true")));
    let mut same = original.clone();
    same.opt_out = Some(true);
    same.registered.insert("analytics-only".into(), json!(true));
    assert!(!same.reconcile_flag_cache(false));
    assert_eq!(same.flags.cache, original.flags.cache);
    for index in 0..6 {
        let mut changed = original.clone();
        match index {
            0 => changed.distinct_id = "other".into(),
            1 => changed.anonymous_id = "other-device".into(),
            2 => {
                changed.groups.insert("company".into(), "b".into());
            }
            3 => {
                changed
                    .flags
                    .person_properties
                    .insert("plan".into(), json!("free"));
            }
            4 => {
                changed.flags.group_properties.clear();
            }
            _ => {}
        }
        assert!(changed.reconcile_flag_cache(index == 5));
        assert!(changed.flags.cache.is_none());
        assert!(!changed.reconcile_flag_cache(index == 5));
    }
}
