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
            4096,
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
        max_record_bytes: 4096,
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
fn namespace_has_stable_byte_encoding_and_zero_padded_hex() {
    for (host, token, instance, expected) in [
        (
            "https://us.i.posthog.com",
            "project",
            "default",
            "v1-ecfe5d95996e3a3cf5569de5efc4c54c3a614429",
        ),
        (
            "https://local",
            "project-µ",
            "instance-🎯",
            "v1-8c488bf868510430c13cc0c4adb367a7bbece7b3",
        ),
        (
            "https://local",
            "project",
            "11",
            "v1-0022e31390e2ad7a48e393177bf9818c9394fc3d",
        ),
    ] {
        assert_eq!(namespace(host, token, instance), expected);
    }
}

#[test]
fn namespace_is_stable_normalized_isolated_and_path_safe() {
    let scope = namespace("https://us.posthog.com/", "project", "default");
    assert_eq!(
        scope,
        namespace("https://us.i.posthog.com", "project", "default")
    );
    assert_ne!(
        scope,
        namespace("https://eu.posthog.com", "project", "default")
    );
    assert_ne!(
        scope,
        namespace("https://us.posthog.com", "other", "default")
    );
    assert_ne!(
        scope,
        namespace("https://us.posthog.com", "project", "other")
    );
    assert_ne!(
        namespace("https://local", "ab", "c"),
        namespace("https://local", "a", "bc")
    );
    assert_eq!(scope.len(), 43);
    assert!(scope.starts_with("v1-"));
    assert!(scope[3..].bytes().all(|byte| byte.is_ascii_hexdigit()));
    let hostile = namespace("https://local/../../", "private/credential", "../../other");
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
        let (mut store, loaded) = FileStateStore::open(directory.lease(), 4096).unwrap();
        assert!(loaded.is_none());
        store.save(&first).unwrap();
        assert_eq!(store.load().unwrap(), Some(first));
        store.save(&second).unwrap();
        assert!(!directory.path().join("state.json.tmp").exists());
    }
    let (_, loaded) = FileStateStore::open(directory.lease(), 4096).unwrap();
    assert_eq!(loaded, Some(second));
}

#[test]
fn serialization_limit_is_exact_and_failed_write_preserves_previous_state() {
    let directory = TestDirectory::new();
    let first = Snapshot::anonymous("用户".into());
    let exact_bytes = record(&first).len() as u64;
    {
        let (mut store, _) = FileStateStore::open(directory.lease(), exact_bytes - 1).unwrap();
        assert_eq!(
            store.save(&first).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!store.path().exists());
    }
    {
        let (mut store, _) = FileStateStore::open(directory.lease(), exact_bytes).unwrap();
        store.save(&first).unwrap();
        assert_eq!(fs::metadata(store.path()).unwrap().len(), exact_bytes);
        assert!(store.save(&Snapshot::anonymous("x".repeat(4096))).is_err());
        assert_eq!(store.load().unwrap(), Some(first.clone()));
    }
    assert!(FileStateStore::open(directory.lease(), exact_bytes - 1).is_err());
    assert_eq!(
        fs::read(directory.path().join("state.json")).unwrap(),
        record(&first)
    );
}

#[test]
fn invalid_identity_cannot_replace_persisted_state() {
    let directory = TestDirectory::new();
    let original = Snapshot::anonymous("original".into());
    let (mut store, _) = FileStateStore::open(directory.lease(), 4096).unwrap();
    store.save(&original).unwrap();
    for invalid in [
        Snapshot::anonymous(" \t".into()),
        Snapshot {
            anonymous_id: "valid".into(),
            distinct_id: String::new(),
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
        let (mut store, _) = FileStateStore::open(directory.lease(), 4096).unwrap();
        store.save(&original).unwrap();
        fs::write(
            directory.path().join("state.json.tmp"),
            b"interrupted write",
        )
        .unwrap();
    }
    let (mut store, loaded) = FileStateStore::open(directory.lease(), 4096).unwrap();
    assert_eq!(loaded, Some(original));
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
    let (mut store, _) = FileStateStore::open(directory.lease(), 4096).unwrap();
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
fn corrupt_oversized_and_future_records_are_preserved_and_never_writable() {
    for bytes in [
        b"truncated {".to_vec(),
        vec![b' '; 4097],
        br#"{"version":2,"state":{"anonymous_id":"private","distinct_id":"private"}}"#.to_vec(),
        br#"{"version":1,"state":{"anonymous_id":"private","distinct_id":"private","future":true}}"#.to_vec(),
        br#"{"version":1,"state":{"anonymous_id":"private","distinct_id":17}}"#.to_vec(),
        br#"{"version":1,"state":{"anonymous_id":"","distinct_id":"private"}}"#.to_vec(),
    ] {
        let directory = TestDirectory::new();
        let lease = directory.lease();
        let path = directory.path().join("state.json");
        fs::write(&path, &bytes).unwrap();
        let error = FileStateStore::open(lease, 4096).err().unwrap();
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
        4096,
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
        RestoredStorage::open(
            &Persistence::Directory(root),
            "scope",
            4096,
            limits(),
            &[hook],
        )
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
fn corrupt_state_reports_and_preserves_data_while_event_storage_remains_usable() {
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
    assert!(restored.state_store.is_none());
    assert!(restored.snapshot.is_none());
    assert!(restored
        .event_store
        .as_mut()
        .unwrap()
        .recover(10, 4096)
        .unwrap()
        .events
        .is_empty());
    assert_eq!(
        *errors.lock().unwrap(),
        vec![(
            StorageOperation::Read,
            "Invalid persisted client state".into()
        )]
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "private malformed state");
}

#[test]
fn event_store_open_failure_does_not_discard_restored_identity_or_state_writer() {
    let directory = TestDirectory::new();
    let original = Snapshot::anonymous("original".into());
    {
        let (mut store, _) = FileStateStore::open(directory.lease(), 4096).unwrap();
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
        let (mut store, _) = FileStateStore::open(directory.lease(), 4096).unwrap();
        store.save(&snapshot).unwrap();
        assert_eq!(
            fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::rename(store.path(), directory.path().join("other.json")).unwrap();
        symlink("other.json", store.path()).unwrap();
    }
    assert!(FileStateStore::open(directory.lease(), 4096).is_err());
    assert_eq!(
        fs::read(directory.path().join("other.json")).unwrap(),
        record(&snapshot)
    );
}
