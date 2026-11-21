use super::*;
use crate::client::event_store::EventStore;
use crate::client::PostHogError;
use std::sync::Mutex;
use uuid::Uuid;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("posthog-state-store-{}", Uuid::now_v7())))
    }

    fn path(&self) -> PathBuf {
        self.0.join("scope")
    }

    fn lease(&self) -> Arc<StorageDirectory> {
        StorageDirectory::open(self.path()).unwrap()
    }

    fn restore(&self, hooks: &[OnErrorHook]) -> RestoredStorage {
        RestoredStorage::open(
            &Persistence::Directory(self.0.clone()),
            "scope",
            limits(),
            hooks,
        )
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn limits() -> StoreLimits {
    StoreLimits {
        max_events: 10,
        max_bytes: 64 * 1024,
    }
}

fn record(snapshot: &Snapshot) -> Vec<u8> {
    serde_json::to_vec(&StateRecord {
        version: 1,
        state: snapshot,
    })
    .unwrap()
}

#[test]
fn consent_choices_round_trip_and_an_unreadable_choice_keeps_the_identity() {
    let mut snapshot = Snapshot::anonymous("anonymous".into());
    for choice in [None, Some(true), Some(false)] {
        snapshot.opt_out = choice;
        let directory = TestDirectory::new();
        let mut store = FileStateStore::open(directory.lease()).unwrap().store;
        store.save(&snapshot).unwrap();
        assert_eq!(store.load().unwrap(), Some(snapshot.clone()));
    }
    snapshot.opt_out = None;
    for consent in [
        None,
        Some(serde_json::json!("true")),
        Some(serde_json::json!(0)),
    ] {
        let mut value: serde_json::Value = serde_json::from_slice(&record(&snapshot)).unwrap();
        match consent {
            Some(consent) => value["state"]["opt_out"] = consent,
            None => {
                value["state"].as_object_mut().unwrap().remove("opt_out");
            }
        }
        let directory = TestDirectory::new();
        let lease = directory.lease();
        fs::write(
            directory.path().join("state.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let opened = FileStateStore::open(lease).unwrap();
        // The configured default applies, as when no choice was made.
        assert_eq!(opened.snapshot, Some(snapshot.clone()));
        assert!(opened.read_error.is_none());
    }
}

#[test]
fn namespace_has_stable_byte_encoding_and_zero_padded_hex() {
    for (token, instance, expected) in [
        (
            "project",
            "default",
            "v1-158e7eddb459563fe83b100c75c7e2274ae6a0be",
        ),
        (
            "project-µ",
            "instance-🎯",
            "v1-2a07fec3d2138dd6951f4325da387214ad7e915c",
        ),
        (
            "project",
            "11",
            "v1-57b16285117db1fa578f63958169c6ef1ce4da2e",
        ),
    ] {
        assert_eq!(namespace(token, instance), expected);
    }
}

#[test]
fn namespace_is_stable_isolated_and_path_safe() {
    let scope = namespace("project", "default");
    assert_eq!(scope, namespace("project", "default"));
    assert_ne!(scope, namespace("other", "default"));
    assert_ne!(scope, namespace("project", "other"));
    assert_ne!(namespace("ab", "c"), namespace("a", "bc"));
    assert_eq!(scope.len(), 43);
    assert!(scope.starts_with("v1-"));
    assert!(scope[3..].bytes().all(|byte| byte.is_ascii_hexdigit()));
    let hostile = namespace("private/credential", "../../other");
    assert_eq!(Path::new(&hostile).components().count(), 1);
    assert!(!hostile.contains("private"));
}

#[test]
fn automatic_uses_local_data_and_overrides_do_not_resolve_the_os_path() {
    let root = std::env::temp_dir().join("posthog-default-location-test");
    assert_eq!(
        Persistence::default()
            .path("scope", || Some(root.clone()))
            .unwrap(),
        Some(root.join("posthog").join("scope"))
    );
    assert_eq!(
        Persistence::Directory(root.clone())
            .path("scope", || panic!("OS lookup"))
            .unwrap(),
        Some(root.join("scope"))
    );
    assert_eq!(
        Persistence::Memory
            .path("scope", || panic!("OS lookup"))
            .unwrap(),
        None
    );
}

#[test]
fn missing_or_relative_os_location_does_not_guess_a_directory() {
    for root in [None, Some(PathBuf::from("relative")), Some(PathBuf::new())] {
        let error = Persistence::Automatic.path("scope", || root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}

#[test]
fn platform_default_matches_the_os_local_data_directory_without_creating_it() {
    let actual = Persistence::Automatic.path("scope", dirs::data_local_dir);
    match dirs::data_local_dir().filter(|path| path.is_absolute()) {
        Some(root) => assert_eq!(actual.unwrap(), Some(root.join("posthog").join("scope"))),
        None => assert_eq!(actual.unwrap_err().kind(), io::ErrorKind::NotFound),
    }
}

#[test]
fn fresh_state_roundtrips_and_atomic_replacement_keeps_latest_snapshot() {
    let directory = TestDirectory::new();
    let first = Snapshot::anonymous("anonymous-用户".into());
    let second = Snapshot::anonymous("replacement".into());
    {
        let opened = FileStateStore::open(directory.lease()).unwrap();
        assert!(opened.snapshot.is_none());
        let mut store = opened.store;
        store.save(&first).unwrap();
        assert_eq!(store.load().unwrap(), Some(first));
        store.save(&second).unwrap();
        assert!(!directory.path().join("state.json.tmp").exists());
    }
    let opened = FileStateStore::open(directory.lease()).unwrap();
    assert_eq!(opened.snapshot, Some(second));
}

#[test]
fn context_record_roundtrips_and_an_unreadable_or_missing_field_restores_empty() {
    let mut snapshot = Snapshot::anonymous("anonymous".into());
    snapshot.registered = serde_json::from_value(serde_json::json!({
        "number": 1.0_f64 / 3.0, "null": null, "array": [null, {"nested":false}]
    }))
    .unwrap();
    snapshot.groups.insert("company".into(), "a".into());
    let directory = TestDirectory::new();
    let mut store = FileStateStore::open(directory.lease()).unwrap().store;
    store.save(&snapshot).unwrap();
    assert_eq!(store.load().unwrap(), Some(snapshot.clone()));
    let bytes = fs::read(directory.path().join("state.json")).unwrap();
    let mut deep = serde_json::json!(1);
    for _ in 0..=super::super::MAX_VALUE_DEPTH {
        deep = serde_json::Value::Array(vec![deep]);
    }
    // A record written before the field existed, or a value this build can't
    // read or accept, restores the field empty and keeps the rest.
    for (field, malformed) in [
        ("registered", None),
        ("registered", Some(serde_json::json!([]))),
        ("registered", Some(serde_json::Value::Null)),
        ("registered", Some(serde_json::json!({ "deep": deep }))),
        ("groups", None),
        ("groups", Some(serde_json::json!([]))),
        ("groups", Some(serde_json::Value::Null)),
        ("groups", Some(serde_json::json!({"company": 1}))),
        ("groups", Some(serde_json::json!({" ": "key"}))),
    ] {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        match malformed {
            Some(malformed) => value["state"][field] = malformed,
            None => {
                value["state"].as_object_mut().unwrap().remove(field);
            }
        }
        let mut expected = snapshot.clone();
        match field {
            "registered" => expected.registered.clear(),
            _ => expected.groups.clear(),
        }
        assert_eq!(
            FileStateStore::decode(&serde_json::to_vec(&value).unwrap()).unwrap(),
            expected
        );
    }
}

#[test]
fn invalid_state_cannot_replace_persisted_state() {
    let directory = TestDirectory::new();
    let original = Snapshot::anonymous("original".into());
    let mut store = FileStateStore::open(directory.lease()).unwrap().store;
    store.save(&original).unwrap();
    for invalid in [
        Snapshot::anonymous(" \t".into()),
        Snapshot {
            anonymous_id: "valid".into(),
            distinct_id: String::new(),
            ..Snapshot::anonymous("valid".into())
        },
        Snapshot {
            groups: std::collections::HashMap::from([(" ".into(), "key".into())]),
            ..Snapshot::anonymous("valid".into())
        },
    ] {
        assert_eq!(
            store.save(&invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(store.load().unwrap(), Some(original.clone()));
    }
}

#[test]
fn uncommitted_temporary_is_replaced_without_displacing_last_committed_state() {
    let directory = TestDirectory::new();
    let original = Snapshot::anonymous("original".into());
    {
        let mut store = FileStateStore::open(directory.lease()).unwrap().store;
        store.save(&original).unwrap();
        fs::write(
            directory.path().join("state.json.tmp"),
            b"interrupted write",
        )
        .unwrap();
    }
    let opened = FileStateStore::open(directory.lease()).unwrap();
    assert_eq!(opened.snapshot, Some(original));
    let mut store = opened.store;
    let next = Snapshot::anonymous("next".into());
    store.save(&next).unwrap();
    assert_eq!(store.load().unwrap(), Some(next));
    assert!(!directory.path().join("state.json.tmp").exists());
}

#[test]
fn failed_replacement_can_retry_and_does_not_destroy_previous_state() {
    let directory = TestDirectory::new();
    let original = Snapshot::anonymous("original".into());
    let next = Snapshot::anonymous("next".into());
    let mut store = FileStateStore::open(directory.lease()).unwrap().store;
    store.save(&original).unwrap();
    let temporary = directory.path().join("state.json.tmp");
    fs::create_dir(&temporary).unwrap();
    assert!(store.save(&next).is_err());
    assert_eq!(store.load().unwrap(), Some(original));
    fs::remove_dir(temporary).unwrap();
    store.save(&next).unwrap();
    assert_eq!(store.load().unwrap(), Some(next));
}

#[test]
fn future_records_are_preserved_and_never_writable() {
    for bytes in [
        br#"{"version":99,"state":{"anonymous_id":"private","distinct_id":"private"}}"#.to_vec(),
        br#"{"version":99}"#.to_vec(),
        br#"{"version":4294967296,"state":false}"#.to_vec(),
    ] {
        let directory = TestDirectory::new();
        let lease = directory.lease();
        let path = directory.path().join("state.json");
        fs::write(&path, &bytes).unwrap();
        let error = FileStateStore::open(lease).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!error.to_string().contains("private"));
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn state_and_event_store_share_one_lease_until_both_are_dropped() {
    let directory = TestDirectory::new();
    let mut restored = directory.restore(&[]);
    assert!(restored.snapshot.is_none());
    assert!(restored.state_store.is_some());
    assert!(restored.event_store.is_some());
    assert!(StorageDirectory::open(directory.path()).is_err());
    drop(restored.state_store.take());
    assert!(StorageDirectory::open(directory.path()).is_err());
    drop(restored.event_store.take());
    assert!(StorageDirectory::open(directory.path()).is_ok());
}

#[test]
fn memory_mode_has_no_stores_and_never_reports_a_storage_error() {
    let restored = RestoredStorage::open(
        &Persistence::Memory,
        "scope",
        limits(),
        &[OnErrorHook::new(|_| {
            panic!("memory mode has no storage operation")
        })],
    );
    assert!(restored.snapshot.is_none());
    assert!(restored.state_store.is_none());
    assert!(restored.event_store.is_none());
}

#[test]
fn lock_failure_degrades_to_memory_and_reports_on_the_initialization_thread() {
    let directory = TestDirectory::new();
    let owner = directory.lease();
    let (tx, rx) = std::sync::mpsc::channel();
    let hook = OnErrorHook::new(move |error| {
        let PostHogError::Storage(error) = error else {
            panic!("expected storage failure")
        };
        tx.send((error.operation(), std::thread::current().id()))
            .unwrap();
    });
    let root = directory.0.clone();
    let worker = std::thread::spawn(move || {
        RestoredStorage::open(&Persistence::Directory(root), "scope", limits(), &[hook])
    });
    let id = worker.thread().id();
    let restored = worker.join().unwrap();
    assert_eq!(rx.recv().unwrap(), (StorageOperation::Open, id));
    assert!(restored.state_store.is_none());
    assert!(restored.event_store.is_none());
    assert!(restored.snapshot.is_none());
    assert!(StorageDirectory::open(directory.path()).is_err());
    drop(owner);
    assert!(StorageDirectory::open(directory.path()).is_ok());
}

#[test]
fn corrupt_state_reports_and_allows_replacement_while_event_storage_remains_usable() {
    let directory = TestDirectory::new();
    drop(directory.lease());
    let path = directory.path().join("state.json");
    fs::write(&path, "private malformed state").unwrap();
    let errors = Arc::new(Mutex::new(Vec::new()));
    let captured = errors.clone();
    let hook = OnErrorHook::new(move |error| {
        if let PostHogError::Storage(error) = error {
            captured
                .lock()
                .unwrap()
                .push((error.operation(), error.error().to_string()));
        }
    });
    let mut restored = directory.restore(&[hook]);
    assert!(restored.state_store.is_some());
    assert!(restored.snapshot.is_none());
    assert!(restored
        .event_store
        .as_mut()
        .unwrap()
        .take_index()
        .is_empty());
    assert_eq!(
        *errors.lock().unwrap(),
        vec![(
            StorageOperation::Read,
            "Invalid persisted client state".into()
        )]
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "private malformed state"
    );
    let replacement = Snapshot::anonymous("replacement".into());
    restored
        .state_store
        .as_mut()
        .unwrap()
        .save(&replacement)
        .unwrap();
    drop(restored);
    assert_eq!(directory.restore(&[]).snapshot, Some(replacement));
}

#[test]
fn event_store_open_failure_does_not_discard_restored_identity_or_state_writer() {
    let directory = TestDirectory::new();
    let original = Snapshot::anonymous("original".into());
    {
        let mut store = FileStateStore::open(directory.lease()).unwrap().store;
        store.save(&original).unwrap();
    }
    fs::write(directory.path().join("events"), b"not a directory").unwrap();
    let errors = Arc::new(Mutex::new(Vec::new()));
    let captured = errors.clone();
    let restored = directory.restore(&[OnErrorHook::new(move |error| {
        if let PostHogError::Storage(error) = error {
            captured.lock().unwrap().push(error.operation());
        }
    })]);
    assert_eq!(restored.snapshot, Some(original));
    assert!(restored.state_store.is_some());
    assert!(restored.event_store.is_none());
    assert_eq!(*errors.lock().unwrap(), vec![StorageOperation::Open]);
}

#[test]
fn error_hook_panics_do_not_prevent_degraded_initialization_or_later_hooks() {
    let directory = TestDirectory::new();
    let _owner = directory.lease();
    let called = Arc::new(Mutex::new(false));
    let observed = called.clone();
    let restored = directory.restore(&[
        OnErrorHook::new(|_| panic!("test observer panic")),
        OnErrorHook::new(move |_| *observed.lock().unwrap() = true),
    ]);
    assert!(*called.lock().unwrap());
    assert!(restored.state_store.is_none());
    assert!(restored.event_store.is_none());
}

#[cfg(unix)]
#[test]
fn state_files_are_private_and_symlink_state_is_not_followed() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let directory = TestDirectory::new();
    let snapshot = Snapshot::anonymous("private-id".into());
    {
        let mut store = FileStateStore::open(directory.lease()).unwrap().store;
        store.save(&snapshot).unwrap();
        assert_eq!(
            fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::rename(store.path(), directory.path().join("other.json")).unwrap();
        symlink("other.json", store.path()).unwrap();
    }
    assert!(FileStateStore::open(directory.lease()).is_err());
    assert_eq!(
        fs::read(directory.path().join("other.json")).unwrap(),
        record(&snapshot)
    );
}

// Literal final-v1 record: independent of the current Rust serializers.
const FINAL_V1: &str = r#"{
  "version":1,
  "state":{
    "anonymous_id":"anonymous", "device_id":"device", "distinct_id":"person",
    "identified":true, "person_processing":true, "opt_out":true,
    "registered":{"source":"fixture"}, "groups":{"company":"a"},
    "flags":{
      "person_properties":{"plan":"pro"},
      "group_properties":{"company":{"tier":2}},
      "cache":{
        "contexts":[{"distinct_id":"person","anonymous_id":"anonymous","device_id":"device",
          "groups":{"company":"a"},"person_properties":{"plan":"pro"},
          "group_properties":{"company":{"tier":2}},"disable_geoip":false}],
        "flags":{"checkout":{
          "context":0,
          "partial":false,"request_id":"request","evaluated_at":123,
          "record":{"enabled":true,"variant":"blue","payload":[null],"id":1,"version":3,
            "reason":"Matched","locally_evaluated":false,"has_experiment":false,
            "minimal_flag_called_events":true}
        }}
      }
    },
    "remote_config":{"has_feature_flags":true,"session_recording":{"sampleRate":1}},
    "session":{"id":"018e23f4-1234-7000-8000-000000000001","started_ms":100,"activity_ms":200}
  }
}"#;

fn fixture() -> serde_json::Value {
    serde_json::from_str(FINAL_V1).unwrap()
}

#[test]
fn final_v1_fixture_restores_identity_consent_and_all_caches() {
    let decoded = FileStateStore::decode_restored(FINAL_V1.as_bytes()).unwrap();
    assert!(!decoded.rewrite_required);
    let snapshot = decoded.snapshot;
    assert_eq!(snapshot.anonymous_id, "anonymous");
    assert_eq!(snapshot.device_id, "device");
    assert_eq!(snapshot.distinct_id, "person");
    assert_eq!(snapshot.opt_out, Some(true));
    assert_eq!(snapshot.registered["source"], "fixture");
    let cache = snapshot.flags.cache.as_ref().unwrap();
    assert_eq!(
        cache.flags["checkout"].record.payload,
        Some(serde_json::Value::Null)
    );
    assert_eq!(
        cache.flags["checkout"].request_id.as_deref(),
        Some("request")
    );
    assert!(snapshot.remote_config.is_some());
    assert!(snapshot.session.is_some());
    let encoded = encode_snapshot(&snapshot).unwrap();
    assert_eq!(FileStateStore::decode(&encoded).unwrap(), snapshot);
    let written: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        written,
        fixture(),
        "the writer must keep producing the v1 format"
    );
}

#[test]
fn additive_unknown_fields_are_tolerated_at_each_persisted_depth() {
    let expected = FileStateStore::decode_restored(FINAL_V1.as_bytes())
        .unwrap()
        .snapshot;
    for path in [
        "",
        "/state",
        "/state/flags",
        "/state/flags/cache",
        "/state/flags/cache/contexts/0",
        "/state/flags/cache/flags/checkout",
        "/state/flags/cache/flags/checkout/record",
        "/state/session",
    ] {
        let mut value = fixture();
        value
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                "additional".into(),
                serde_json::json!({"nested":[true,null]}),
            );
        let decoded =
            FileStateStore::decode_restored(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(decoded.snapshot, expected, "unknown field at {path}");
    }
}

#[test]
fn additive_remote_config_fields_preserve_the_cached_config() {
    let expected = FileStateStore::decode_restored(FINAL_V1.as_bytes())
        .unwrap()
        .snapshot;
    let mut value = fixture();
    value["state"]["remote_config"]["additional"] = serde_json::json!({"nested":[true,null]});
    let decoded = FileStateStore::decode_restored(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(decoded.snapshot, expected);
}

#[test]
fn optional_sections_recover_independently_without_losing_targeting_or_consent() {
    let original = FileStateStore::decode_restored(FINAL_V1.as_bytes())
        .unwrap()
        .snapshot;
    for (path, invalid) in [
        ("/state/flags/cache", serde_json::json!(42)),
        (
            "/state/flags/cache/flags/checkout/record/payload",
            serde_json::json!([1, 2]),
        ),
        (
            "/state/flags/cache/contexts/0/device_id",
            serde_json::json!(false),
        ),
        (
            "/state/flags/cache/flags/checkout/context",
            serde_json::json!(1),
        ),
        ("/state/remote_config", serde_json::json!([])),
        (
            "/state/remote_config/session_recording",
            serde_json::json!(42),
        ),
        ("/state/session", serde_json::json!(true)),
        ("/state/session/started_ms", serde_json::json!(201)),
        (
            "/state/session/id",
            serde_json::json!("00000000-0000-0000-0000-000000000000"),
        ),
    ] {
        let mut value = fixture();
        *value.pointer_mut(path).unwrap() = invalid;
        let decoded =
            FileStateStore::decode_restored(&serde_json::to_vec(&value).unwrap()).unwrap();
        let mut expected = original.clone();
        if path.starts_with("/state/flags") {
            expected.flags.cache = None;
        }
        if path.starts_with("/state/remote_config") {
            expected.remote_config = None;
        }
        if path.starts_with("/state/session") {
            expected.session = None;
        }
        assert!(decoded.rewrite_required, "{}", path);
        assert_eq!(decoded.snapshot, expected, "{path}");
    }
    let mut value = fixture();
    value["state"]["flags"]["cache"] = serde_json::json!(false);
    value["state"]["remote_config"] = serde_json::json!(false);
    value["state"]["session"] = serde_json::json!(false);
    let decoded = FileStateStore::decode_restored(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(decoded.snapshot.flags.cache.is_none());
    assert!(decoded.snapshot.remote_config.is_none());
    assert!(decoded.snapshot.session.is_none());
    assert_eq!(
        decoded.snapshot.flags.person_properties,
        original.flags.person_properties
    );
    assert_eq!(decoded.snapshot.opt_out, Some(true));
}

// Like `registered` and `groups`, flag targeting this build can't read or
// accept restores empty, so it never discards the identity and consent.
#[test]
fn unreadable_flag_targeting_restores_empty_and_keeps_identity_and_consent() {
    let original = FileStateStore::decode_restored(FINAL_V1.as_bytes())
        .unwrap()
        .snapshot;
    let mut deep = serde_json::json!(1);
    for _ in 0..=super::super::MAX_VALUE_DEPTH {
        deep = serde_json::json!([deep]);
    }
    for (path, invalid) in [
        ("/state/flags", serde_json::Value::Null),
        ("/state/flags", serde_json::json!(false)),
        ("/state/flags/person_properties", serde_json::json!([1])),
        (
            "/state/flags/person_properties",
            serde_json::json!({"deep": deep}),
        ),
        (
            "/state/flags/group_properties",
            serde_json::json!({"company": 5}),
        ),
        (
            "/state/flags/group_properties",
            serde_json::json!({" ": {"tier": 1}}),
        ),
        (
            "/state/flags/group_properties",
            serde_json::json!({"company": {"deep": deep}}),
        ),
    ] {
        let mut value = fixture();
        *value.pointer_mut(path).unwrap() = invalid;
        let decoded =
            FileStateStore::decode_restored(&serde_json::to_vec(&value).unwrap()).unwrap();
        let mut expected = original.clone();
        match path {
            "/state/flags" => expected.flags = Default::default(),
            "/state/flags/person_properties" => expected.flags.person_properties.clear(),
            _ => expected.flags.group_properties.clear(),
        }
        assert_eq!(decoded.snapshot, expected, "{path}");
    }
    for field in ["person_properties", "group_properties", "cache"] {
        let mut value = fixture();
        value["state"]["flags"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        let decoded =
            FileStateStore::decode_restored(&serde_json::to_vec(&value).unwrap()).unwrap();
        let mut expected = original.clone();
        match field {
            "person_properties" => expected.flags.person_properties.clear(),
            "group_properties" => expected.flags.group_properties.clear(),
            _ => expected.flags.cache = None,
        }
        assert_eq!(decoded.snapshot, expected, "missing {field}");
    }
}

#[test]
fn malformed_identity_is_core_corruption() {
    for (path, invalid) in [
        ("/state/device_id", serde_json::json!(" ")),
        ("/state/device_id", serde_json::json!(false)),
        ("/state/anonymous_id", serde_json::json!("")),
    ] {
        let mut value = fixture();
        *value.pointer_mut(path).unwrap() = invalid;
        assert!(
            matches!(
                FileStateStore::decode_restored(&serde_json::to_vec(&value).unwrap()),
                Err(RestoreFailure::Corrupt(_))
            ),
            "{}",
            path
        );
    }
}

#[test]
fn additive_fields_still_obey_whole_json_depth_bounds() {
    let mut value = fixture();
    let mut deep = serde_json::json!(0);
    for _ in 0..150 {
        deep = serde_json::json!({"nested":deep});
    }
    value["additional"] = deep;
    assert!(matches!(
        FileStateStore::decode_restored(&serde_json::to_vec(&value).unwrap()),
        Err(RestoreFailure::Corrupt(_))
    ));
}

#[path = "flag_storage_tests.rs"]
mod flag_tests;
