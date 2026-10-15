use super::*;

#[test]
fn healthy_records_replay_beside_truncated_records_and_lock_is_exclusive() {
    let path = std::env::temp_dir().join(format!("posthog-store-{}", Uuid::now_v7()));
    let store = FileEventStore::open(&path).unwrap();
    assert!(FileEventStore::open(&path).is_err());
    let event = Event::new("saved", "user");
    store.put(&event).unwrap();
    fs::write(path.join(format!("{}.json", Uuid::now_v7())), b"{").unwrap();
    fs::write(path.join(format!("{}.json", Uuid::now_v7())), b"").unwrap();
    assert_eq!(store.load().unwrap(), vec![event.clone()]);
    assert_eq!(
        fs::read_dir(&path)
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|e| e == "corrupt"))
            .count(),
        2
    );
    drop(store);
    let store = FileEventStore::open(&path).unwrap();
    assert_eq!(store.load().unwrap(), vec![event]);
    store.purge().unwrap();
    assert!(store.load().unwrap().is_empty());
    drop(store);
    fs::remove_dir_all(path).unwrap();
}
