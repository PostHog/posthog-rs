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
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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

#[test]
fn stateful_sdk_attribution_is_in_the_record_and_survives_recovery() {
    let directory = TestDirectory::new();
    let mut input = Event::new("captured", "anonymous-id");
    input.set_uuid(Uuid::now_v7());
    input.insert_prop("$lib", "embedded-sdk").unwrap();
    input.insert_prop("$lib_version", "1.2.3").unwrap();
    let now = "2025-01-01T00:00:00.123456789Z".parse().unwrap();
    let original = CaptureEvent::from_stateful_event_at(&input, now);
    let expected = serde_json::to_value(&original).unwrap();
    {
        let mut store = directory.open(limits());
        let key = store.put(&original).unwrap();
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(store.path(key)).unwrap()).unwrap();
        assert_eq!(record["event"], expected);
        assert_eq!(record["event"]["properties"]["$lib"], "embedded-sdk");
        assert_eq!(record["event"]["properties"]["$lib_version"], "1.2.3");
    }
    let mut store = directory.open(limits());
    let recovered = store.recover(10, 64 * 1024).unwrap();
    assert!(recovered.errors.is_empty());
    assert_eq!(recovered.events.len(), 1);
    assert_eq!(
        serde_json::to_value(&recovered.events[0].event).unwrap(),
        expected
    );
}

#[test]
fn restart_preserves_the_complete_event_and_removes_by_entry_identity() {
    let directory = TestDirectory::new();
    let original = event("first");
    let mut duplicate = original.clone();
    duplicate.event = "second".into();
    let (first, second) = {
        let mut store = directory.open(limits());
        let first = store.put(&original).unwrap();
        let second = store.put(&duplicate).unwrap();
        assert_ne!(
            first, second,
            "reused event UUIDs must not overwrite records"
        );
        assert!(store.recover(10, 64 * 1024).unwrap().events.is_empty());
        (first, second)
    };
    let mut store = directory.open(limits());
    let recovered = store.recover(10, 64 * 1024).unwrap();
    assert!(recovered.errors.is_empty());
    assert!(!recovered.more);
    assert_eq!(recovered.events.len(), 2);
    assert_eq!(recovered.events[0].key, first);
    assert_eq!(recovered.events[1].key, second);
    assert_eq!(
        serde_json::to_value(&recovered.events[0].event).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&recovered.events[1].event).unwrap(),
        serde_json::to_value(duplicate).unwrap()
    );
    store.remove(first).unwrap();
    store.remove(first).unwrap();
    assert!(store.path(second).exists());
    assert_eq!(store.count, 1);
    drop(store);
    let recovered = directory.open(limits()).recover(10, 64 * 1024).unwrap();
    assert_eq!(recovered.events.len(), 1);
    assert_eq!(recovered.events[0].key, second);
}

#[test]
fn nested_records_roundtrip_up_to_the_recovery_depth_limit() {
    // The record and event envelopes add two levels to the property depth.
    for (depth, supported) in [(4, true), (125, true), (126, false), (130, false)] {
        let directory = TestDirectory::new();
        let mut original = event("nested");
        let mut properties = serde_json::json!("private leaf");
        for _ in 0..depth {
            properties = serde_json::json!({"nested": properties});
        }
        original.properties = properties;
        let snapshot = serde_json::to_string(&original).unwrap();
        {
            let mut store = directory.open(limits());
            let result = store.put(&original);
            if supported {
                result.unwrap();
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.error.kind(), io::ErrorKind::InvalidData);
                assert_eq!(error.error.to_string(), "Invalid persisted event record");
                assert_eq!(store.count, 0);
                assert_eq!(store.bytes, 0);
                assert_eq!(store.last_key, 0);
                assert_eq!(fs::read_dir(&store.events_path).unwrap().count(), 0);
            }
            assert_eq!(serde_json::to_string(&original).unwrap(), snapshot);
        }
        let recovered = directory.open(limits()).recover(10, 64 * 1024).unwrap();
        assert!(recovered.errors.is_empty(), "{:?}", recovered.errors);
        assert!(!recovered.more);
        assert_eq!(
            recovered.events.len(),
            usize::from(supported),
            "depth {depth}"
        );
        if supported {
            assert_eq!(
                serde_json::to_string(&recovered.events[0].event).unwrap(),
                snapshot,
                "depth {depth}"
            );
        }
    }
}

#[test]
fn computed_float_snapshot_survives_restart() {
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
    directory.open(limits()).put(&original).unwrap();
    let recovered = directory.open(limits()).recover(10, 64 * 1024).unwrap();
    assert!(recovered.errors.is_empty(), "{:?}", recovered.errors);
    assert_eq!(recovered.events.len(), 1);
    assert_eq!(
        serde_json::to_string(&recovered.events[0].event).unwrap(),
        snapshot
    );
}

#[test]
fn count_limit_evicts_oldest_and_late_acknowledgement_preserves_replacement() {
    let directory = TestDirectory::new();
    let mut store = directory.open(StoreLimits {
        max_events: 1,
        ..limits()
    });
    let first = store.put(&event("first")).unwrap();
    let second = store.put(&event("second")).unwrap();
    assert!(!store.contains(first).unwrap());
    store.remove(first).unwrap();
    assert!(store.contains(second).unwrap());
    assert_eq!(store.count, 1);
    assert_eq!(store.head(1, 4096).unwrap().events[0].key, second);
}

#[test]
fn byte_and_record_limits_are_enforced_without_partial_files() {
    let directory = TestDirectory::new();
    let record = event("event");
    let size = serde_json::to_vec(&Record {
        version: 1,
        event: record.clone(),
    })
    .unwrap()
    .len() as u64;
    let mut store = directory.open(StoreLimits {
        max_events: 10,
        max_bytes: size,
        max_record_bytes: size,
    });
    let key = store.put(&record).unwrap();
    assert_eq!(store.bytes, size);
    let replacement = store.put(&record).unwrap();
    store.remove(key).unwrap();
    let mut oversized = record.clone();
    oversized.properties = serde_json::json!({"large": "x".repeat(size as usize)});
    assert_eq!(
        store.put(&oversized).unwrap_err().error.kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(store.count, 1);
    assert_eq!(store.bytes, size);
    assert!(store.contains(replacement).unwrap());
    store.remove(replacement).unwrap();
    assert_eq!(fs::read_dir(&store.events_path).unwrap().count(), 0);
    assert!(store.put(&record).is_ok());
}

#[test]
fn reduced_limits_recover_oldest_records_in_bounded_pages_without_deleting_overflow() {
    let directory = TestDirectory::new();
    let mut keys = Vec::new();
    {
        let mut store = directory.open(limits());
        for index in 0..6 {
            keys.push(store.put(&event(&format!("event-{index}"))).unwrap());
        }
    }
    let mut store = directory.open(StoreLimits {
        max_events: 2,
        ..limits()
    });
    assert_eq!(store.count, 6);
    for page in 0..3 {
        let recovered = store.recover(100, 64 * 1024).unwrap();
        assert_eq!(recovered.events.len(), 2);
        assert_eq!(recovered.more, page < 2);
        assert_eq!(recovered.events[0].key, keys[page * 2]);
        assert_eq!(recovered.events[1].key, keys[page * 2 + 1]);
        for record in recovered.events {
            store.remove(record.key).unwrap();
        }
    }
    assert_eq!(store.count, 0);
    assert!(store.put(&event("new")).is_ok());
}

#[test]
fn recovery_byte_budget_defers_a_record_until_it_fits() {
    let directory = TestDirectory::new();
    let first;
    {
        let mut store = directory.open(limits());
        first = store.put(&event("event")).unwrap();
        store.put(&event("event")).unwrap();
    }
    let mut store = directory.open(limits());
    let size = fs::metadata(store.path(first)).unwrap().len();
    let empty = store.recover(2, size - 1).unwrap();
    assert!(empty.events.is_empty());
    assert!(empty.more);
    assert!(!empty.advanced);
    let one = store.recover(2, size).unwrap();
    assert_eq!(one.events.len(), 1);
    assert_eq!(one.events[0].key, first);
    assert_eq!(one.events[0].bytes, size);
    assert!(one.more);
    assert!(one.advanced);
    let two = store.recover(2, size).unwrap();
    assert_eq!(two.events.len(), 1);
    assert!(!two.more);
}

#[test]
fn recovery_never_reloads_newly_persisted_in_memory_events() {
    let directory = TestDirectory::new();
    {
        let mut store = directory.open(limits());
        store.put(&event("old")).unwrap();
    }
    let mut store = directory.open(limits());
    let new = store.put(&event("new")).unwrap();
    let recovered = store.recover(10, 64 * 1024).unwrap();
    assert_eq!(recovered.events.len(), 1);
    assert_eq!(recovered.events[0].event.event, "old");
    assert!(store.path(new).exists());
    assert!(store.recover(10, 64 * 1024).unwrap().events.is_empty());
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
fn corruption_and_future_versions_are_quarantined_without_payload_diagnostics() {
    let directory = TestDirectory::new();
    let path = {
        let store = directory.open(limits());
        store.events_path.clone()
    };
    fs::write(
        path.join("00000000000000000001.json"),
        b"{broken secret data",
    )
    .unwrap();
    fs::write(
        path.join("00000000000000000002.json"),
        br#"{"version":99,"future":"secret"}"#,
    )
    .unwrap();
    let mut store = directory.open(limits());
    let recovered = store.recover(2, 64 * 1024).unwrap();
    assert!(recovered.events.is_empty());
    assert_eq!(recovered.errors.len(), 2);
    assert!(recovered
        .errors
        .iter()
        .all(|error| !error.to_string().contains("secret")));
    assert!(path.join("00000000000000000001.corrupt").exists());
    assert!(path.join("00000000000000000002.unsupported").exists());
    assert_eq!(
        store.count, 2,
        "quarantined files still count against disk capacity"
    );
    assert!(store.recover(2, 64 * 1024).unwrap().errors.is_empty());
}

#[test]
fn oversized_records_are_not_read_into_memory() {
    let directory = TestDirectory::new();
    let path = {
        let store = directory.open(limits());
        store.path(StoreKey(1))
    };
    let file = File::create(&path).unwrap();
    file.set_len(100 * 1024 * 1024).unwrap();
    drop(file);
    let mut store = directory.open(limits());
    let recovered = store.recover(1, 64 * 1024).unwrap();
    assert!(recovered.events.is_empty());
    assert_eq!(recovered.errors.len(), 1);
    assert!(path.exists());
    let replacement = store.put(&event("new")).unwrap();
    assert!(!path.exists());
    assert!(store.contains(replacement).unwrap());
    assert_eq!(store.count, 1);
}

#[test]
fn lowering_record_limit_preserves_valid_records_for_a_later_restart() {
    let directory = TestDirectory::new();
    let original = event("old");
    let (key, size) = {
        let mut store = directory.open(limits());
        let key = store.put(&original).unwrap();
        (key, fs::metadata(store.path(key)).unwrap().len())
    };
    let mut store = directory.open(StoreLimits {
        max_record_bytes: size - 1,
        ..limits()
    });
    let recovered = store.recover(10, 64 * 1024).unwrap();
    assert!(recovered.events.is_empty());
    assert_eq!(recovered.errors.len(), 1);
    assert!(store.path(key).exists());
    drop(store);
    let recovered = directory.open(limits()).recover(10, 64 * 1024).unwrap();
    assert_eq!(recovered.events.len(), 1);
    assert_eq!(recovered.events[0].key, key);
    assert_eq!(
        serde_json::to_value(&recovered.events[0].event).unwrap(),
        serde_json::to_value(original).unwrap()
    );
}

#[test]
fn candidate_selection_is_bounded_before_payload_reads() {
    let directory = TestDirectory::new();
    let path = {
        let store = directory.open(limits());
        store.events_path.clone()
    };
    for key in 1..=RECOVERY_INDEX_LIMIT + 1 {
        fs::write(path.join(format!("{key:020}.json")), b"invalid").unwrap();
    }
    let mut store = directory.open(StoreLimits {
        max_events: 3,
        ..limits()
    });
    let (selected, total) = store.candidates(3).unwrap();
    assert_eq!(selected, vec![StoreKey(1), StoreKey(2), StoreKey(3)]);
    assert_eq!(total, RECOVERY_INDEX_LIMIT + 1);
    assert!(store.startup_index.is_none());
    let recovered = store.recover(200, 64 * 1024).unwrap();
    assert_eq!(recovered.errors.len(), 3);
    assert!(recovered.more);
    assert!(path.join("00000000000000000004.json").exists());
}

#[test]
fn indexed_recovery_reuses_one_bounded_filename_scan() {
    let directory = TestDirectory::new();
    {
        let mut store = directory.open(limits());
        for index in 0..7 {
            store.put(&event(&format!("event-{index}"))).unwrap();
        }
    }
    let mut store = directory.open(limits());
    assert_eq!(store.directory_scans.get(), 1);
    let mut recovered = Vec::new();
    loop {
        let page = store.recover(2, 64 * 1024).unwrap();
        assert!(page.errors.is_empty());
        assert!(!page.retry_pending);
        for record in page.events {
            recovered.push(record.event.event);
            store.remove(record.key).unwrap();
        }
        if !page.more {
            break;
        }
    }
    assert_eq!(
        recovered,
        (0..7).map(|i| format!("event-{i}")).collect::<Vec<_>>()
    );
    assert_eq!(store.directory_scans.get(), 1);
    assert!(store.recover(2, 64 * 1024).unwrap().events.is_empty());
}

#[test]
fn unreadable_record_retries_after_healthy_pages_without_reloading_owned_records() {
    let directory = TestDirectory::new();
    let first = {
        let mut store = directory.open(limits());
        let first = store.put(&event("temporarily-unreadable")).unwrap();
        store.put(&event("healthy-second")).unwrap();
        store.put(&event("healthy-third")).unwrap();
        first
    };
    let mut store = directory.open(limits());
    let path = store.path(first);
    let backup = path.with_extension("waiting");
    fs::rename(&path, &backup).unwrap();
    fs::create_dir(&path).unwrap();
    let failed = store.recover(1, 64 * 1024).unwrap();
    assert_eq!(failed.errors.len(), 1);
    assert!(failed.events.is_empty());
    assert!(failed.more && failed.retry_pending);
    let second = store.recover(1, 64 * 1024).unwrap();
    assert_eq!(second.events[0].event.event, "healthy-second");
    assert!(second.more);
    let third = store.recover(1, 64 * 1024).unwrap();
    assert_eq!(third.events[0].event.event, "healthy-third");
    assert!(!third.more && third.retry_pending);
    let still_failed = store.recover(1, 64 * 1024).unwrap();
    assert_eq!(still_failed.errors.len(), 1);
    assert!(!still_failed.more && still_failed.retry_pending);
    fs::remove_dir(&path).unwrap();
    fs::rename(backup, &path).unwrap();
    let retried = store.recover(1, 64 * 1024).unwrap();
    assert_eq!(retried.events.len(), 1);
    assert_eq!(retried.events[0].key, first);
    assert!(!retried.more && !retried.retry_pending);
    assert!(store.recover(10, 64 * 1024).unwrap().events.is_empty());
    assert_eq!(store.directory_scans.get(), 1);
}

#[test]
fn post_rename_sync_failure_retains_key_until_acknowledgement() {
    let directory = TestDirectory::new();
    let mut store = directory.open(StoreLimits {
        max_events: 1,
        ..limits()
    });
    store.fail_directory_sync.set(true);
    let failure = store.put(&event("uncertain")).unwrap_err();
    let key = failure.key.expect("the renamed record remains owned");
    assert_eq!(failure.error.kind(), io::ErrorKind::Other);
    assert!(store.path(key).is_file());
    assert_eq!(store.count, 1);
    assert!(store.bytes > 0);
    store.remove(key).unwrap();
    assert_eq!((store.count, store.bytes), (0, 0));
    assert!(store.put(&event("next")).is_ok());
}

#[test]
fn incomplete_temp_files_are_removed_without_touching_foreign_files() {
    let directory = TestDirectory::new();
    let path = {
        let store = directory.open(limits());
        store.events_path.clone()
    };
    fs::write(path.join("00000000000000000001.tmp"), b"partial").unwrap();
    fs::write(path.join("foreign.tmp"), b"keep").unwrap();
    fs::write(path.join("foreign.json"), b"keep").unwrap();
    let mut store = directory.open(limits());
    assert!(!path.join("00000000000000000001.tmp").exists());
    assert!(path.join("foreign.tmp").exists());
    assert!(path.join("foreign.json").exists());
    assert_eq!(store.put(&event("first")).unwrap(), StoreKey(1));
}

#[test]
fn write_and_remove_failures_leave_accounting_consistent() {
    let directory = TestDirectory::new();
    let mut store = directory.open(limits());
    let blocked = store.path(StoreKey(1)).with_extension("tmp");
    fs::create_dir(&blocked).unwrap();
    assert!(store.put(&event("blocked")).is_err());
    assert_eq!(store.count, 0);
    assert_eq!(store.bytes, 0);
    fs::remove_dir(blocked).unwrap();
    let key = store.put(&event("kept")).unwrap();
    let path = store.path(key);
    let encoded = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(store.remove(key).is_err());
    assert_eq!(store.count, 1);
    assert_eq!(store.bytes, encoded.len() as u64);
    fs::remove_dir(&path).unwrap();
    fs::write(path, encoded).unwrap();
    store.remove(key).unwrap();
    assert_eq!(store.count, 0);
    assert_eq!(store.bytes, 0);
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
        assert!(!directory.0.join("events").exists());
    }
}

#[cfg(unix)]
#[test]
fn files_and_directories_are_private_and_symlinks_are_not_recovered() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let directory = TestDirectory::new();
    let path;
    {
        let mut store = directory.open(limits());
        let key = store.put(&event("private")).unwrap();
        path = store.path(key);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&store.events_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    symlink(&path, path.with_file_name("00000000000000000002.json")).unwrap();
    let mut store = directory.open(limits());
    let recovered = store.recover(10, 64 * 1024).unwrap();
    assert_eq!(recovered.events.len(), 1);
    assert_eq!(store.count, 1);
}

#[test]
fn replacement_overage_stops_later_appends_until_eviction_recovers() {
    let directory = TestDirectory::new();
    let mut store = directory.open(StoreLimits {
        max_events: 1,
        ..limits()
    });
    let first = store.put(&event("first")).unwrap();
    store.fail_eviction.set(true);
    let failed = store.put(&event("replacement")).unwrap_err();
    let replacement = failed.key.unwrap();
    assert_eq!(store.count, 2);
    let bytes = store.bytes;
    assert!(bytes <= limits().max_bytes + limits().max_record_bytes);
    for _ in 0..3 {
        assert!(store.put(&event("blocked")).unwrap_err().key.is_none());
        assert_eq!(
            (store.count, store.bytes, store.last_key),
            (2, bytes, replacement.0)
        );
    }
    assert!(store.contains(first).unwrap());
    assert!(store.contains(replacement).unwrap());
    store.fail_eviction.set(false);
    let last = store.put(&event("last")).unwrap();
    assert_eq!(store.count, 1);
    assert!(!store.contains(first).unwrap());
    assert!(!store.contains(replacement).unwrap());
    assert_eq!(store.head(1, 4096).unwrap().events[0].key, last);
}

#[test]
fn failed_replacement_append_never_evicts_retained_records() {
    let directory = TestDirectory::new();
    let mut store = directory.open(StoreLimits {
        max_events: 1,
        ..limits()
    });
    let first = store.put(&event("first")).unwrap();
    let before = fs::read(store.path(first)).unwrap();
    fs::create_dir(store.path(StoreKey(first.0 + 1)).with_extension("tmp")).unwrap();
    assert!(store.put(&event("failed")).unwrap_err().key.is_none());
    assert_eq!(fs::read(store.path(first)).unwrap(), before);
    assert_eq!((store.count, store.bytes), (1, before.len() as u64));
}

#[test]
fn reduced_limits_and_quarantined_records_reclaim_oldest_with_exact_accounting() {
    let directory = TestDirectory::new();
    {
        let mut store = directory.open(limits());
        for index in 0..5 {
            store.put(&event(&format!("old-{index}"))).unwrap();
        }
        let path = store.path(StoreKey(1));
        fs::rename(&path, path.with_extension("unsupported")).unwrap();
    }
    let mut store = directory.open(StoreLimits {
        max_events: 2,
        ..limits()
    });
    assert_eq!(store.count, 5);
    let new = store.put(&event("new")).unwrap();
    let head = store.head(10, 64 * 1024).unwrap();
    assert_eq!(
        head.events
            .iter()
            .map(|record| record.key)
            .collect::<Vec<_>>(),
        vec![StoreKey(5), new]
    );
    assert_eq!(store.count, 2);
    assert_eq!(
        store.bytes,
        fs::metadata(store.path(StoreKey(5))).unwrap().len()
            + fs::metadata(store.path(new)).unwrap().len()
    );
}

#[test]
fn upload_head_respects_soft_batch_target_and_hard_record_window() {
    let directory = TestDirectory::new();
    let mut store = directory.open(limits());
    let mut large = event("large");
    large.properties = serde_json::json!({"data": "x".repeat(2048)});
    let first = store.put(&large).unwrap();
    let second = store.put(&event("small")).unwrap();
    let page = store.head_batch(10, 4096, 512).unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].key, first);
    assert!(page.events[0].bytes > 512 && page.events[0].bytes <= 4096);
    assert!(store
        .head_batch(10, page.events[0].bytes - 1, 512)
        .unwrap()
        .events
        .is_empty());
    store.remove(first).unwrap();
    assert_eq!(
        store.head_batch(10, 4096, 512).unwrap().events[0].key,
        second
    );
}

#[test]
fn admission_key_gaps_and_failed_serialization_preserve_exact_queue_identity() {
    let directory = TestDirectory::new();
    let mut store = directory.open(limits());
    assert_eq!(
        store.put_at(&event("first"), StoreKey(5)).unwrap(),
        StoreKey(5)
    );
    let before = fs::read(store.path(StoreKey(5))).unwrap();
    let mut invalid = event("invalid");
    invalid.properties = serde_json::json!({"data": "x".repeat(8192)});
    assert!(store
        .put_at(&invalid, StoreKey(7))
        .unwrap_err()
        .key
        .is_none());
    assert_eq!(fs::read(store.path(StoreKey(5))).unwrap(), before);
    assert_eq!(
        store.put_at(&event("next"), StoreKey(9)).unwrap(),
        StoreKey(9)
    );
    assert!(store.put_at(&event("stale"), StoreKey(5)).is_err());
    let page = store.head_batch(10, 4096, 4096).unwrap();
    assert_eq!(
        page.events
            .iter()
            .map(|record| record.key)
            .collect::<Vec<_>>(),
        vec![StoreKey(5), StoreKey(9)]
    );
}

#[test]
fn head_batch_continues_empty_pages_before_reselecting_eligible_fifo() {
    for window in [1, 3] {
        let directory = TestDirectory::new();
        let mut large = event("preserved");
        large.properties = serde_json::json!({"data": "x".repeat(2048)});
        let (prefix, healthy) = {
            let mut store = directory.open(limits());
            let prefix: Vec<_> = (0..3).map(|_| store.put(&large).unwrap()).collect();
            let healthy = store.put(&event("healthy")).unwrap();
            (prefix, healthy)
        };
        let small_limit = record_size(&event("healthy"));
        let mut store = directory.open(StoreLimits {
            max_record_bytes: small_limit as u64,
            ..limits()
        });
        let original_bytes = store.bytes;
        for _ in 0..2 {
            for key in &prefix {
                let page = store.head_batch(window, 4096, 4096).unwrap();
                assert!(page.events.is_empty());
                assert_eq!(page.errors.len(), 1);
                assert!(page.advanced && page.more);
                assert_eq!(
                    store.recovery_cursor, key.0,
                    "empty head page restarted its scan"
                );
            }
            let page = store.head_batch(window, 4096, 4096).unwrap();
            assert_eq!(page.events.len(), 1);
            assert_eq!(page.events[0].key, healthy);
            assert!(!page.more);
        }
        assert_eq!((store.count, store.bytes), (4, original_bytes));
        store.remove(healthy).unwrap();
        assert_eq!(store.count, 3);
        assert_eq!(
            store.bytes,
            prefix
                .iter()
                .map(|key| fs::metadata(store.path(*key)).unwrap().len())
                .sum::<u64>()
        );
        drop(store);
        let page = directory.open(limits()).recover(10, 64 * 1024).unwrap();
        assert_eq!(
            page.events
                .iter()
                .map(|event| event.key)
                .collect::<Vec<_>>(),
            prefix
        );
    }
}

#[test]
fn head_batch_finishes_preserved_scan_before_reselecting_an_appended_replacement() {
    let directory = TestDirectory::new();
    let mut large = event("preserved");
    large.properties = serde_json::json!({"data": "x".repeat(2048)});
    let (first, second) = {
        let mut store = directory.open(limits());
        (store.put(&large).unwrap(), store.put(&large).unwrap())
    };
    let mut store = directory.open(StoreLimits {
        max_events: 2,
        max_record_bytes: record_size(&event("replacement")) as u64,
        ..limits()
    });
    let first_page = store.head_batch(1, 4096, 4096).unwrap();
    assert!(first_page.events.is_empty() && first_page.more && first_page.advanced);
    let replacement = store.put(&event("replacement")).unwrap();
    assert!(!store.contains(first).unwrap());
    let last_page = store.head_batch(1, 4096, 4096).unwrap();
    assert!(last_page.events.is_empty() && last_page.advanced);
    assert!(
        !last_page.more && !last_page.retry_pending,
        "completed unreadable scan must yield"
    );
    assert_eq!(store.recovery_cursor, second.0);
    let restarted = store.head_batch(1, 4096, 4096).unwrap();
    assert!(restarted.events.is_empty() && restarted.more);
    let page = store.head_batch(1, 4096, 4096).unwrap();
    assert_eq!(page.events[0].key, replacement);
    assert_eq!(store.count, 2);
    assert_eq!(
        store.bytes,
        fs::metadata(store.path(second)).unwrap().len() + page.events[0].bytes
    );
}

#[test]
fn head_batch_retains_healthy_fifo_after_skipped_prefix() {
    let directory = TestDirectory::new();
    let mut large = event("preserved");
    large.properties = serde_json::json!({"data": "x".repeat(8192)});
    let healthy = {
        let mut store = directory.open(StoreLimits {
            max_record_bytes: 16 * 1024,
            ..limits()
        });
        store.put(&large).unwrap();
        store.put(&large).unwrap();
        ["first", "second", "third"]
            .iter()
            .map(|name| store.put(&event(name)).unwrap())
            .collect::<Vec<_>>()
    };
    let mut store = directory.open(limits());
    for _ in 0..2 {
        let page = store.head_batch(3, 4096, 4096).unwrap();
        assert!(page.events.is_empty() && page.advanced && page.more);
        assert_eq!(page.errors.len(), 1);
    }
    let page = store.head_batch(3, 4096, 4096).unwrap();
    assert_eq!(
        page.events
            .iter()
            .map(|event| event.key)
            .collect::<Vec<_>>(),
        healthy
    );
    assert!(!page.more && !page.retry_pending);
    assert_eq!(store.count, 5);
}

#[test]
fn bounded_head_rollover_includes_its_checkpoint_but_not_later_appends() {
    let directory = TestDirectory::new();
    let mut large = event("preserved");
    large.properties = serde_json::json!({"data": "x".repeat(8192)});
    {
        let mut store = directory.open(StoreLimits {
            max_record_bytes: 16 * 1024,
            ..limits()
        });
        for _ in 0..3 {
            store.put(&large).unwrap();
        }
    }
    let mut store = directory.open(limits());
    let checkpoint = StoreKey(4);
    let first = store.head_batch_through(1, 4096, 4096, checkpoint).unwrap();
    assert!(first.events.is_empty() && first.more);
    assert_eq!(store.put(&event("checkpoint")).unwrap(), checkpoint);
    let later = store.put(&event("later")).unwrap();
    for _ in 0..2 {
        let page = store.head_batch_through(1, 4096, 4096, checkpoint).unwrap();
        assert!(page.events.is_empty() && page.advanced && page.more);
    }
    let page = store.head_batch_through(1, 4096, 4096, checkpoint).unwrap();
    assert_eq!(page.events[0].key, checkpoint);
    assert!(!page.more);
    store.remove(checkpoint).unwrap();
    for index in 0..3 {
        let page = store.head_batch_through(1, 4096, 4096, checkpoint).unwrap();
        assert!(page.events.is_empty() && page.advanced);
        assert_eq!(page.more, index < 2);
    }
    assert!(store.contains(later).unwrap());
    for _ in 0..3 {
        let page = store.head_batch_through(1, 4096, 4096, later).unwrap();
        assert!(page.events.is_empty() && page.more);
    }
    assert_eq!(
        store
            .head_batch_through(1, 4096, 4096, later)
            .unwrap()
            .events[0]
            .key,
        later
    );
    assert_eq!(store.count, 4);
}

// A literal version-1 record, independent of the current serializer: a format
// change must still recover records written by earlier releases.
const FINAL_V1_RECORD: &str = r#"{"version":1,"event":{"event":"fixture","uuid":"018e23f4-1234-7000-8000-000000000001","distinct_id":"original-user","timestamp":"2025-01-01T00:00:00.123456Z","session_id":"original-session","window_id":"original-window","options":{"process_person_profile":false},"properties":{"original":{"array":[false,0,null]}}}}"#;

#[test]
fn final_v1_record_fixture_recovers_the_complete_event() {
    let directory = TestDirectory::new();
    drop(directory.open(limits()));
    fs::write(
        directory.0.join("events").join(format!("{:020}.json", 1)),
        FINAL_V1_RECORD,
    )
    .unwrap();
    let mut store = directory.open(limits());
    let recovered = store.recover(10, 64 * 1024).unwrap();
    assert!(recovered.errors.is_empty(), "{:?}", recovered.errors);
    assert_eq!(recovered.events.len(), 1);
    let mut expected = event("fixture");
    expected.uuid = Uuid::parse_str("018e23f4-1234-7000-8000-000000000001").unwrap();
    assert_eq!(
        serde_json::to_value(&recovered.events[0].event).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
}
