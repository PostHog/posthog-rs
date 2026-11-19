use super::*;
use crate::Event;
use uuid::Uuid;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("posthog-event-store-{}", Uuid::now_v7())))
    }

    fn lease(&self) -> Arc<StorageDirectory> {
        StorageDirectory::open(&self.0).unwrap()
    }

    fn open(&self, limits: StoreLimits) -> FileEventStore {
        FileEventStore::open(self.lease(), limits).unwrap()
    }

    fn events(&self) -> PathBuf {
        self.0.join("events")
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

fn event(name: &str) -> CaptureEvent {
    let mut event = CaptureEvent::from_event(&Event::new(name, "original-user"));
    event.timestamp = "2025-01-01T00:00:00.123456Z".into();
    event.session_id = Some("original-session".into());
    event.window_id = Some("original-window".into());
    event
        .options
        .insert("process_person_profile".into(), false.into());
    event.properties = serde_json::json!({"original": {"array": [false, 0, null]}});
    event
}

fn json(event: &CaptureEvent) -> serde_json::Value {
    serde_json::to_value(event).unwrap()
}

#[cfg(unix)]
#[test]
fn directory_lease_ends_with_its_last_owner_despite_inherited_descriptors() {
    let directory = TestDirectory::new();
    let lease = directory.lease();
    // A child between fork and exec holds the same open file description.
    let inherited = lease._lock.try_clone().unwrap();
    let other_owner = Arc::clone(&lease);
    drop(lease);
    assert!(StorageDirectory::open(&directory.0).is_err());
    drop(other_owner);
    let reopened = directory.lease();
    drop(inherited);
    assert!(StorageDirectory::open(&directory.0).is_err());
    drop(reopened);
    assert!(StorageDirectory::open(&directory.0).is_ok());
}

#[test]
fn directory_lease_is_exclusive_and_shared_across_storage_components() {
    let directory = TestDirectory::new();
    let lease = directory.lease();
    assert!(StorageDirectory::open(&directory.0).is_err());
    let store = FileEventStore::open(Arc::clone(&lease), limits()).unwrap();
    drop(lease);
    assert!(StorageDirectory::open(&directory.0).is_err());
    drop(store);
    assert!(StorageDirectory::open(&directory.0).is_ok());
}

#[test]
fn restart_indexes_records_in_key_order_and_reads_the_complete_event() {
    let directory = TestDirectory::new();
    let first = event("first");
    // A reused event UUID never overwrites a record: keys are queue positions.
    let mut second = first.clone();
    second.event = "second".into();
    let (first_size, second_size) = {
        let mut store = directory.open(limits());
        assert!(store.take_index().is_empty());
        let second_size = store.write(StoreKey(7), &second).unwrap();
        let first_size = store.write(StoreKey(3), &first).unwrap();
        (first_size, second_size)
    };
    let mut store = directory.open(limits());
    assert_eq!(
        store.take_index(),
        vec![(StoreKey(3), first_size), (StoreKey(7), second_size)]
    );
    assert!(store.take_index().is_empty(), "the index is taken once");
    assert_eq!(json(&store.read(StoreKey(3)).unwrap()), json(&first));
    assert_eq!(json(&store.read(StoreKey(7)).unwrap()), json(&second));
    assert_eq!(first_size as usize, record_size(&first));

    store.delete(StoreKey(3)).unwrap();
    store.delete(StoreKey(3)).unwrap();
    assert!(matches!(store.read(StoreKey(3)), Err(ReadError::Lost(_))));
    drop(store);
    let index = directory.open(limits()).take_index();
    assert_eq!(index, vec![(StoreKey(7), second_size)]);
}

#[test]
fn nested_records_roundtrip_up_to_the_read_depth_limit() {
    // The record and event envelopes add two levels to the property depth.
    for (depth, supported) in [(4, true), (125, true), (126, false), (130, false)] {
        let directory = TestDirectory::new();
        let mut original = event("nested");
        let mut properties = serde_json::json!("private leaf");
        for _ in 0..depth {
            properties = serde_json::json!({"nested": properties});
        }
        original.properties = properties;
        let mut store = directory.open(limits());
        let written = store.write(StoreKey(1), &original);
        if supported {
            written.unwrap();
            assert_eq!(json(&store.read(StoreKey(1)).unwrap()), json(&original));
        } else {
            let error = written.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(error.to_string(), "Invalid persisted event record");
            assert_eq!(fs::read_dir(directory.events()).unwrap().count(), 0);
        }
    }
}

#[test]
fn computed_floats_survive_a_restart() {
    let directory = TestDirectory::new();
    let mut original = event("computed-floats");
    let mut state = 1u64;
    let values: Vec<f64> = (0..100)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 11) as f64 / ((1u64 << 53) as f64)
        })
        .collect();
    original.properties = serde_json::json!({"measurements": values});
    let snapshot = serde_json::to_string(&original).unwrap();
    directory
        .open(limits())
        .write(StoreKey(1), &original)
        .unwrap();
    let restored = directory.open(limits()).read(StoreKey(1)).unwrap();
    assert_eq!(serde_json::to_string(&restored).unwrap(), snapshot);
}

#[test]
fn a_record_over_the_limit_is_refused_without_a_file() {
    let directory = TestDirectory::new();
    let mut store = directory.open(limits());
    let mut oversized = event("oversized");
    oversized.properties = serde_json::json!({"large": "x".repeat(4096)});
    assert_eq!(
        store.write(StoreKey(1), &oversized).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(fs::read_dir(directory.events()).unwrap().count(), 0);
    assert!(store.write(StoreKey(1), &event("fits")).is_ok());
}

#[test]
fn a_record_over_a_lowered_limit_stays_on_disk_without_being_read() {
    let directory = TestDirectory::new();
    let path = directory.events().join(format!("{:020}.json", 1));
    let mut large = event("large");
    large.properties = serde_json::json!({"large": "x".repeat(2048)});
    let size = directory.open(limits()).write(StoreKey(1), &large).unwrap();
    let lowered = StoreLimits {
        max_record_bytes: size - 1,
        ..limits()
    };
    let mut store = directory.open(lowered);
    assert!(store.take_index().is_empty());
    assert!(matches!(store.read(StoreKey(1)), Err(ReadError::Lost(_))));
    assert!(path.exists());
    drop(store);
    let mut store = directory.open(limits());
    assert_eq!(store.take_index(), vec![(StoreKey(1), size)]);
    assert_eq!(json(&store.read(StoreKey(1)).unwrap()), json(&large));
}

#[test]
fn oversized_files_are_not_read_into_memory() {
    let directory = TestDirectory::new();
    drop(directory.open(limits()));
    let path = directory.events().join(format!("{:020}.json", 1));
    File::create(&path)
        .unwrap()
        .set_len(100 * 1024 * 1024)
        .unwrap();
    let mut store = directory.open(limits());
    assert!(store.take_index().is_empty());
    assert!(matches!(store.read(StoreKey(1)), Err(ReadError::Lost(_))));
}

#[test]
fn corrupt_and_future_records_move_aside_without_payload_diagnostics() {
    let directory = TestDirectory::new();
    drop(directory.open(limits()));
    let events = directory.events();
    fs::write(
        events.join("00000000000000000001.json"),
        b"{broken secret data",
    )
    .unwrap();
    fs::write(
        events.join("00000000000000000002.json"),
        br#"{"version":99,"future":"secret"}"#,
    )
    .unwrap();
    let mut store = directory.open(limits());
    assert_eq!(store.take_index().len(), 2);
    for key in [1, 2] {
        match store.read(StoreKey(key)) {
            Err(ReadError::Lost(error)) => assert!(!error.to_string().contains("secret")),
            other => panic!("record {}: {:?}", key, other),
        }
    }
    assert!(events.join("00000000000000000001.corrupt").exists());
    assert!(events.join("00000000000000000002.unsupported").exists());
    drop(store);
    assert!(directory.open(limits()).take_index().is_empty());
}

#[cfg(unix)]
#[test]
fn a_record_the_process_cannot_open_is_a_transient_failure() {
    use std::os::unix::fs::PermissionsExt;
    let directory = TestDirectory::new();
    let mut store = directory.open(limits());
    store.write(StoreKey(1), &event("blocked")).unwrap();
    let path = directory.events().join(format!("{:020}.json", 1));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(matches!(
        store.read(StoreKey(1)),
        Err(ReadError::Transient(_))
    ));
    assert!(path.exists());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(store.read(StoreKey(1)).is_ok());
}

#[test]
fn incomplete_temp_files_are_removed_without_touching_foreign_files() {
    let directory = TestDirectory::new();
    drop(directory.open(limits()));
    let events = directory.events();
    fs::write(events.join("00000000000000000001.tmp"), b"partial").unwrap();
    fs::write(events.join("foreign.tmp"), b"keep").unwrap();
    fs::write(events.join("foreign.json"), b"keep").unwrap();
    let mut store = directory.open(limits());
    assert!(!events.join("00000000000000000001.tmp").exists());
    assert!(events.join("foreign.tmp").exists());
    assert!(events.join("foreign.json").exists());
    assert!(store.take_index().is_empty());
}

#[test]
fn a_failed_write_leaves_no_record_and_a_failed_delete_keeps_it() {
    let directory = TestDirectory::new();
    let mut store = directory.open(limits());
    let blocked = directory.events().join(format!("{:020}.tmp", 1));
    fs::create_dir(&blocked).unwrap();
    assert!(store.write(StoreKey(1), &event("blocked")).is_err());
    fs::remove_dir(&blocked).unwrap();
    assert_eq!(fs::read_dir(directory.events()).unwrap().count(), 0);

    store.write(StoreKey(2), &event("kept")).unwrap();
    let path = directory.events().join(format!("{:020}.json", 2));
    let encoded = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(store.delete(StoreKey(2)).is_err());
    fs::remove_dir(&path).unwrap();
    fs::write(&path, encoded).unwrap();
    store.delete(StoreKey(2)).unwrap();
    assert!(!path.exists());
}

#[test]
fn invalid_limits_fail_before_creating_an_event_spool() {
    let directory = TestDirectory::new();
    for invalid in [
        StoreLimits {
            max_events: 0,
            ..limits()
        },
        StoreLimits {
            max_record_bytes: 0,
            ..limits()
        },
        StoreLimits {
            max_bytes: 1,
            ..limits()
        },
    ] {
        assert!(FileEventStore::open(directory.lease(), invalid).is_err());
        assert!(!directory.events().exists());
    }
}

#[cfg(unix)]
#[test]
fn files_and_directories_are_private_and_symlinks_are_not_indexed() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let directory = TestDirectory::new();
    let path = directory.events().join(format!("{:020}.json", 1));
    {
        let mut store = directory.open(limits());
        store.write(StoreKey(1), &event("private")).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(directory.events())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    symlink(&path, path.with_file_name("00000000000000000002.json")).unwrap();
    let index = directory.open(limits()).take_index();
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].0, StoreKey(1));
}

const FINAL_V1_RECORD: &str = r#"{"version":1,"event":{"event":"fixture","uuid":"018e23f4-1234-7000-8000-000000000001","distinct_id":"original-user","timestamp":"2025-01-01T00:00:00.123456Z","session_id":"original-session","window_id":"original-window","options":{"process_person_profile":false},"properties":{"original":{"array":[false,0,null]}}}}"#;

#[test]
fn final_v1_record_fixture_reads_the_complete_event_and_writes_the_same_format() {
    let directory = TestDirectory::new();
    drop(directory.open(limits()));
    let path = directory.events().join(format!("{:020}.json", 1));
    fs::write(&path, FINAL_V1_RECORD).unwrap();
    let mut store = directory.open(limits());
    let restored = store.read(StoreKey(1)).unwrap();
    let mut expected = event("fixture");
    expected.uuid = Uuid::parse_str("018e23f4-1234-7000-8000-000000000001").unwrap();
    assert_eq!(json(&restored), json(&expected));

    store.delete(StoreKey(1)).unwrap();
    store.write(StoreKey(1), &restored).unwrap();
    let written: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let fixture: serde_json::Value = serde_json::from_str(FINAL_V1_RECORD).unwrap();
    assert_eq!(written, fixture);
}
