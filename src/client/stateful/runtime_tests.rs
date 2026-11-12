use std::fs;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::stateful::{Client, Config, DeliveryOutcome, Persistence, PersistenceOutcome};
use crate::{PostHogError, StorageOperation};
use uuid::Uuid;

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("posthog-stateful-client-{}", Uuid::now_v7())))
    }

    fn config(&self) -> Config {
        let mut config = memory_config();
        config.persistence = Persistence::Directory(self.0.clone());
        config
    }

    fn scope(&self) -> PathBuf {
        let config = self.config();
        self.0.join(super::namespace(
            &config.host,
            &config.project_token,
            &config.instance_name,
        ))
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if self.0.is_file() {
            let _ = fs::remove_file(&self.0);
        } else {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn memory_config() -> Config {
    let mut config = Config::new("phc_test");
    config.preload_feature_flags = false;
    config.host = "http://127.0.0.1:1".into();
    config.persistence = Persistence::Memory;
    config.shutdown_timeout = Duration::from_millis(50);
    config
}

#[test]
fn client_defaults_map_to_shared_delivery_without_changing_server_defaults() {
    let mut config = Config::new(" phc_test ");
    config.host = " https://eu.posthog.com/ ".into();
    config.normalize();
    assert!(config.invalid_reason().is_none());
    assert!(matches!(config.persistence, Persistence::Automatic));
    assert_eq!(config.project_token, "phc_test");
    assert_eq!(config.host, "https://eu.i.posthog.com");
    assert_eq!(config.max_queue_bytes, 8 * 1024 * 1024);
    assert_eq!(config.max_stored_bytes, 64 * 1024 * 1024);
    assert_eq!(config.max_stored_events, 10_000);
    assert_eq!(config.max_record_bytes, 1024 * 1024);
    assert_eq!(config.max_state_bytes, 1024 * 1024);
    assert_eq!(config.state_coalesce, Duration::from_millis(250));
    let options = config.transport_options();
    assert_eq!(
        (
            options.flush_at,
            options.max_batch_size,
            options.max_queue_size
        ),
        (20, 100, 1_000)
    );
    assert_eq!(options.flush_interval_ms, 10_000);
    assert_eq!(
        options.capture_compression,
        Some(crate::CaptureCompression::Gzip)
    );
    assert!(!options.capture_defaults().is_server);
    assert!(!options.capture_defaults().disable_geoip);
    assert_eq!(options.endpoints().api_host(), "https://eu.i.posthog.com");
    let server = crate::ClientOptions::from("phc_test");
    assert_eq!(server.flush_at, 100);
    assert_eq!(server.max_queue_size, 10_000);
    assert!(server.capture_defaults().is_server);
}

#[test]
fn invalid_token_returns_disabled_client_and_sanitized_diagnostic_without_storage() {
    for token in ["", " \t\n", "phx_not-a-project-token", "phc_bad\nheader"] {
        let directory = Directory::new();
        let mut config = directory.config();
        config.project_token = token.into();
        let messages = Arc::new(Mutex::new(Vec::new()));
        let output = messages.clone();
        config.on_error(move |error| {
            if let PostHogError::Initialization(error) = error {
                output.lock().unwrap().push(error.message().to_owned());
            } else {
                panic!("invalid credentials should be checked before storage");
            }
        });
        let client = Client::init_blocking(config);
        assert!(client.is_disabled());
        assert_eq!(client.get_anonymous_id(), "");
        assert_eq!(client.get_distinct_id(), "");
        assert_eq!(client.flush_blocking().delivery, DeliveryOutcome::Disabled);
        assert_eq!(
            client.shutdown_blocking(None).state,
            PersistenceOutcome::Disabled
        );
        assert_eq!(messages.lock().unwrap().len(), 1);
        if !token.trim().is_empty() {
            assert!(!messages.lock().unwrap()[0].contains(token));
        }
        assert!(!directory.0.exists());
    }
}

#[test]
fn explicitly_disabled_client_is_quiet_and_error_hook_panic_is_isolated() {
    let mut config = Config::new("");
    config.disabled = true;
    config.on_error(|_| panic!("an explicitly disabled client needs no token"));
    let client = Client::init_blocking(config);
    assert!(client.is_disabled());
    assert!(client.state.is_none());
    assert!(client.transport.is_none());

    let mut config = Config::new("");
    let (tx, rx) = mpsc::channel();
    config.on_error(|_| panic!("application callback failure"));
    config.on_error(move |_| tx.send(()).unwrap());
    assert!(Client::init_blocking(config).is_disabled());
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn incoherent_limits_disable_before_creating_storage() {
    let changes: &[fn(&mut Config)] = &[
        |c| c.flush_at = 0,
        |c| c.max_batch_size = 0,
        |c| c.max_queue_bytes = 0,
        |c| c.max_record_bytes = c.max_queue_bytes as u64 + 1,
        |c| c.max_stored_bytes = c.max_record_bytes - 1,
        |c| c.max_stored_events = 0,
        |c| c.max_state_bytes = 1,
        |c| c.flush_interval = Duration::ZERO,
        |c| c.shutdown_timeout = Duration::MAX,
        |c| c.state_coalesce = Duration::MAX,
        |c| c.request_timeout_seconds = 0,
        |c| c.max_capture_attempts = 0,
        |c| c.bootstrap_anonymous_id = Some(" ".into()),
        |c| c.bootstrap_anonymous_id = Some("x".repeat(c.max_state_bytes)),
        |c| c.host = "file:///tmp/events".into(),
        |c| c.host = "https://name:password@example.test".into(),
    ];
    for change in changes {
        let directory = Directory::new();
        let mut config = directory.config();
        change(&mut config);
        let client = Client::init_blocking(config);
        assert!(client.is_disabled());
        assert!(!directory.0.exists());
    }
}

#[test]
fn memory_identity_is_ready_stable_and_visible_to_concurrent_getters() {
    let client = Arc::new(Client::init_blocking(memory_config()));
    assert!(!client.is_disabled());
    let id = client.get_distinct_id();
    assert_eq!(Uuid::parse_str(&id).unwrap().get_version_num(), 7);
    assert_eq!(client.get_anonymous_id(), id);
    let readers: Vec<_> = (0..16)
        .map(|_| {
            let client = client.clone();
            std::thread::spawn(move || (client.get_distinct_id(), client.get_anonymous_id()))
        })
        .collect();
    for reader in readers {
        assert_eq!(reader.join().unwrap(), (id.clone(), id.clone()));
    }
    let report = client.flush_blocking();
    assert_eq!(report.state, PersistenceOutcome::MemoryOnly);
    assert_eq!(report.delivery, DeliveryOutcome::Attempted);
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        DeliveryOutcome::Stopped
    );
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        DeliveryOutcome::Stopped
    );
    assert_eq!(client.flush_blocking().delivery, DeliveryOutcome::Closed);
    assert_eq!(client.get_distinct_id(), id);
}

#[test]
fn anonymous_bootstrap_persists_and_does_not_replace_restored_identity() {
    let directory = Directory::new();
    let mut config = directory.config();
    config.bootstrap_anonymous_id = Some("fresh-bootstrap".into());
    let first = Client::init_blocking(config);
    assert_eq!(first.get_distinct_id(), "fresh-bootstrap");
    assert_eq!(first.flush_blocking().state, PersistenceOutcome::Persisted);
    assert_eq!(
        first.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );

    let mut config = directory.config();
    config.bootstrap_anonymous_id = Some("other-bootstrap".into());
    let second = Client::init_blocking(config);
    assert_eq!(second.get_anonymous_id(), "fresh-bootstrap");
    assert_eq!(second.get_distinct_id(), "fresh-bootstrap");
    second.shutdown_blocking(None);
    let mut separate = directory.config();
    separate.instance_name = "another-instance".into();
    let separate = Client::init_blocking(separate);
    assert_ne!(separate.get_distinct_id(), "fresh-bootstrap");
    separate.shutdown_blocking(None);
}

#[test]
fn unreadable_state_is_preserved_and_client_remains_enabled() {
    for original in [
        b"not json".as_slice(),
        b"{\"version\":99,\"state\":{}}".as_slice(),
    ] {
        let directory = Directory::new();
        fs::create_dir_all(directory.scope()).unwrap();
        let path = directory.scope().join("state.json");
        fs::write(&path, original).unwrap();
        let mut config = directory.config();
        let (tx, rx) = mpsc::channel();
        config.on_error(move |error| {
            if let PostHogError::Storage(error) = error {
                tx.send(error.operation()).unwrap();
            }
        });
        let client = Client::init_blocking(config);
        assert!(!client.is_disabled());
        assert!(!client.get_distinct_id().is_empty());
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            StorageOperation::Read
        );
        let report = client.flush_blocking();
        assert_eq!(report.state, PersistenceOutcome::Unavailable);
        assert_eq!(report.delivery, DeliveryOutcome::Attempted);
        client.shutdown_blocking(None);
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}

#[test]
fn namespace_contention_is_enabled_memory_fallback_without_overwriting_owner() {
    let directory = Directory::new();
    let first = Client::init_blocking(directory.config());
    let id = first.get_distinct_id();
    assert_eq!(first.flush_blocking().state, PersistenceOutcome::Persisted);
    let second = Client::init_blocking(directory.config());
    assert!(!second.is_disabled());
    assert_ne!(second.get_distinct_id(), id);
    assert_eq!(
        second.flush_blocking().state,
        PersistenceOutcome::Unavailable
    );
    second.shutdown_blocking(None);
    first.shutdown_blocking(None);
    let restored = Client::init_blocking(directory.config());
    assert_eq!(restored.get_distinct_id(), id);
    restored.shutdown_blocking(None);
}

#[test]
fn unavailable_storage_root_keeps_local_state_and_delivery_enabled() {
    let directory = Directory::new();
    fs::write(&directory.0, b"not a directory").unwrap();
    let client = Client::init_blocking(directory.config());
    assert!(!client.is_disabled());
    assert!(!client.get_distinct_id().is_empty());
    let report = client.flush_blocking();
    assert_eq!(report.state, PersistenceOutcome::Unavailable);
    assert_eq!(report.delivery, DeliveryOutcome::Attempted);
    client.shutdown_blocking(None);
    assert_eq!(fs::read(&directory.0).unwrap(), b"not a directory");
}

#[test]
fn shutdown_from_delivery_callback_defers_then_external_shutdown_joins_both_workers() {
    use crate::client::event_store::{EventStore, FileEventStore, StorageDirectory};
    let directory = Directory::new();
    let mut config = directory.config();
    let mut store = FileEventStore::open(
        StorageDirectory::open(directory.scope()).unwrap(),
        config.event_limits(),
    )
    .unwrap();
    store
        .put(&crate::capture_event::CaptureEvent::from_event(
            &crate::Event::new("old", "old-user"),
        ))
        .unwrap();
    drop(store);
    let path = fs::read_dir(directory.scope().join("events"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap();
    fs::write(path, b"broken record").unwrap();
    let holder = Arc::new(Mutex::new(None::<std::sync::Weak<Client>>));
    let callback_holder = holder.clone();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let (done_tx, done_rx) = mpsc::channel();
    config.on_error(move |error| {
        if matches!(error, PostHogError::Storage(error) if error.operation() == StorageOperation::Read) {
            release_rx.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
            let client = callback_holder.lock().unwrap().as_ref().unwrap().upgrade().unwrap();
            done_tx.send(client.shutdown_blocking(None)).unwrap();
        }
    });
    let client = Arc::new(Client::init_blocking(config));
    *holder.lock().unwrap() = Some(Arc::downgrade(&client));
    release_tx.send(()).unwrap();
    let report = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(report.delivery, DeliveryOutcome::Deferred);
    assert_eq!(report.state, PersistenceOutcome::Deferred);
    assert_eq!(
        client.shutdown_blocking(None).delivery,
        DeliveryOutcome::Stopped
    );
    drop(StorageDirectory::open(directory.scope()).unwrap());
}

#[test]
fn concurrent_flush_and_shutdown_finish_and_release_namespace() {
    let directory = Directory::new();
    let client = Arc::new(Client::init_blocking(directory.config()));
    let calls: Vec<_> = (0..16)
        .map(|index| {
            let client = client.clone();
            std::thread::spawn(move || {
                if index % 2 == 0 {
                    client.flush_blocking()
                } else {
                    client.shutdown_blocking(None)
                }
            })
        })
        .collect();
    for call in calls {
        call.join().unwrap();
    }
    client.shutdown_blocking(None);
    drop(crate::client::event_store::StorageDirectory::open(directory.scope()).unwrap());
}

#[cfg(feature = "async-client")]
#[test]
fn cancelling_initialization_cleans_up_without_blocking_the_polling_thread() {
    use std::future::Future;
    use std::task::{Context, Poll};

    struct Dropped(mpsc::Sender<std::thread::ThreadId>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            let _ = self.0.send(std::thread::current().id());
        }
    }
    let directory = Directory::new();
    fs::write(&directory.0, b"not a directory").unwrap();
    let mut config = directory.config();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let dropped = Dropped(dropped_tx);
    config.on_error(move |failure| {
        let _ = &dropped;
        if !matches!(failure, PostHogError::Storage(_)) {
            return;
        }
        entered_tx.send(std::thread::current().id()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
    });
    let mut future = Box::pin(Client::init(config));
    let waker = futures::task::noop_waker();
    let mut context = Context::from_waker(&waker);
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
    let initialization_thread = entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_ne!(initialization_thread, std::thread::current().id());
    drop(future);
    release_tx.send(()).unwrap();
    // The unclaimed client closes its transport before the last callback owner
    // is destroyed. No executor runtime or caller-side cleanup is needed.
    let drop_thread = dropped_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_ne!(drop_thread, std::thread::current().id());
}

#[cfg(feature = "async-client")]
#[test]
fn async_initialization_works_without_tokio_and_matches_blocking_restoration() {
    let directory = Directory::new();
    let client = futures::executor::block_on(Client::init(directory.config()));
    let id = client.get_distinct_id();
    assert!(!id.is_empty());
    assert_eq!(
        client.shutdown_blocking(None).state,
        PersistenceOutcome::Persisted
    );
    let restored = Client::init_blocking(directory.config());
    assert_eq!(restored.get_anonymous_id(), id);
    restored.shutdown_blocking(None);
    let disabled = futures::executor::block_on(Client::init(Config::new("")));
    assert!(disabled.is_disabled());
}
