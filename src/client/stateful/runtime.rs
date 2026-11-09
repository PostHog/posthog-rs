use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use super::producer::Producer;
use super::storage::{namespace, RestoredStorage};
use super::writer::{Backend, DrainOutcome, StateWriter, WriterOptions};
use super::{Config, InitializationFailure, Persistence, Snapshot};
use crate::client::common::apply_on_error_hooks;
use crate::client::transport::{Completion, Control, StartupFailure, SystemClock, TransportHandle};
use crate::client::PostHogError;

/// Persistence result through a captured state revision, not an ingestion acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PersistenceOutcome {
    /// The revision has been written to the state store.
    Persisted,
    /// Persistence was intentionally disabled.
    MemoryOnly,
    /// Persistence is unavailable; live state remains in memory.
    Unavailable,
    /// A write attempt failed; dirty live state remains available for retry.
    WriteFailed,
    /// The writer stopped before persisting the requested revision.
    TimedOut,
    /// Waiting would block a client worker inside its callback.
    Deferred,
    /// The client is disabled.
    Disabled,
    /// The operation did not request state persistence because the client is closing.
    NotRequested,
}

impl From<DrainOutcome> for PersistenceOutcome {
    fn from(outcome: DrainOutcome) -> Self {
        match outcome {
            DrainOutcome::Persisted => Self::Persisted,
            DrainOutcome::MemoryOnly => Self::MemoryOnly,
            DrainOutcome::Unavailable => Self::Unavailable,
            DrainOutcome::WriteFailed => Self::WriteFailed,
            DrainOutcome::TimedOut => Self::TimedOut,
            DrainOutcome::Deferred => Self::Deferred,
        }
    }
}

/// Local delivery-worker progress. No variant guarantees server ingestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeliveryOutcome {
    /// A flush attempt completed; retries or persisted backlog may remain.
    Attempted,
    /// The worker has stopped. Undelivered persisted records may remain.
    Stopped,
    /// The delivery worker exited without completing this flush.
    Unavailable,
    /// The client is closing; no new flush was requested.
    Closed,
    /// Waiting would block a client worker inside its callback.
    Deferred,
    /// The client is disabled.
    Disabled,
}

/// Independent state-persistence and delivery progress for a drain operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DrainReport {
    /// Progress of the mandatory client-state snapshot.
    pub state: PersistenceOutcome,
    /// Progress of the delivery worker, not server ingestion success.
    pub delivery: DeliveryOutcome,
}

impl DrainReport {
    fn disabled() -> Self {
        Self {
            state: PersistenceOutcome::Disabled,
            delivery: DeliveryOutcome::Disabled,
        }
    }

    fn deferred() -> Self {
        Self {
            state: PersistenceOutcome::Deferred,
            delivery: DeliveryOutcome::Deferred,
        }
    }
}

/// A memory-first client whose initialization has finished.
///
/// Setup always returns this type. Invalid configuration or local delivery
/// startup failure disables the client and reports through `on_error`.
/// Storage failures instead allow enabled memory-backed operation. No network
/// request is required to establish readiness.
///
/// Dropping the client requests shutdown and blocks until its workers finish,
/// except from a worker callback where waiting would deadlock. In-flight I/O
/// and callbacks can exceed the shutdown scheduling budget.
pub struct Client {
    disabled: bool,
    shutdown_timeout: Duration,
    pub(super) producer: Option<Producer>,
    pub(super) state: Option<StateWriter>,
    pub(super) transport: Option<TransportHandle>,
    pub(super) closed: AtomicBool,
}

impl Client {
    /// Initialize on the current thread. Restores state before returning and
    /// waits for local delivery initialization, not network connectivity.
    /// Use `Client::init` with `async-client` to keep initialization I/O off the
    /// calling executor thread.
    pub fn init_blocking(mut config: Config) -> Self {
        config.normalize();
        Self::initialize(Arc::new(config))
    }

    /// Initialize on a short-lived standard thread, without owning or requiring
    /// an application runtime. Available with `async-client`.
    ///
    /// Cancelling this future does not interrupt filesystem work in progress;
    /// the initializer retains and shuts down an unclaimed client on its own
    /// thread, including cancellation after readiness was signalled.
    #[cfg(feature = "async-client")]
    pub async fn init(mut config: Config) -> Self {
        config.normalize();
        Self::init_with(Arc::new(config), Self::initialize).await
    }

    #[cfg(feature = "async-client")]
    async fn init_with(
        config: Arc<Config>,
        initialize: impl FnOnce(Arc<Config>) -> Self + Send + 'static,
    ) -> Self {
        let worker_config = config.clone();
        let (owner, pending) = super::initialization::handoff();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let worker = std::thread::Builder::new()
            .name("posthog-init".into())
            .spawn(move || {
                let client = initialize(worker_config);
                owner.publish(client, tx);
            });
        if worker.is_err() {
            return Self::disabled(&config, Some("Cannot start the initialization thread"));
        }
        match rx.await {
            Ok(()) => pending.claim().unwrap_or_else(|| {
                Self::disabled(&config, Some("Initialization ended without a ready client"))
            }),
            Err(_) => Self::disabled(
                &config,
                Some("Initialization worker stopped before readiness"),
            ),
        }
    }

    fn initialize(config: Arc<Config>) -> Self {
        if config.disabled {
            return Self::disabled(&config, None);
        }
        if let Some(reason) = config.invalid_reason() {
            return Self::disabled(&config, Some(reason));
        }
        let producer = Producer::new(&config);
        let scope = namespace(&config.host, &config.project_token, &config.instance_name);
        let restored = RestoredStorage::open(
            &config.persistence,
            &scope,
            config.max_state_bytes as u64,
            config.event_limits(),
            &config.on_error,
        );
        let was_restored = restored.snapshot.is_some();
        let snapshot = restored.snapshot.unwrap_or_else(|| {
            let mut snapshot = Snapshot::anonymous(
                config
                    .bootstrap_anonymous_id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
            );
            if let Some(id) = &config.bootstrap_identified_id {
                snapshot.distinct_id = id.clone();
                snapshot.identified = true;
                snapshot.person_processing = true;
            }
            snapshot
        });
        let backend = match restored.state_store {
            Some(store) => Backend::Persistent {
                store: Box::new(store),
                restored: was_restored,
            },
            None if matches!(config.persistence, Persistence::Memory) => Backend::Memory,
            None => Backend::Unavailable,
        };
        let transport = match TransportHandle::spawn_stateful(
            config.transport_options(),
            restored
                .event_store
                .map(|store| Box::new(store) as Box<dyn crate::client::event_store::EventStore>),
            config.max_queue_bytes,
        ) {
            Ok(transport) => transport,
            Err(failure) => {
                return Self::disabled(
                    &config,
                    Some(match failure {
                        StartupFailure::Spawn => "Cannot start the delivery thread",
                        StartupFailure::Http => {
                            "Cannot initialize the local delivery HTTP/TLS client"
                        }
                        StartupFailure::WorkerStopped => "Delivery worker stopped before readiness",
                    }),
                );
            }
        };
        let state = match StateWriter::new(
            snapshot,
            backend,
            WriterOptions {
                max_bytes: config.max_state_bytes,
                coalesce: config.state_coalesce,
                shutdown_timeout: config.shutdown_timeout,
                ..WriterOptions::default()
            },
            config.on_error.clone(),
            Arc::new(SystemClock),
        ) {
            Ok(state) => state,
            Err(_) => {
                // Configuration and restored records are validated before this
                // point; do not leak the started transport if that invariant fails.
                transport.request_close();
                transport.join();
                return Self::disabled(&config, Some("Invalid mandatory state configuration"));
            }
        };
        let client = Self {
            disabled: false,
            shutdown_timeout: config.shutdown_timeout,
            producer: Some(producer),
            state: Some(state),
            transport: Some(transport),
            closed: AtomicBool::new(false),
        };
        if was_restored {
            if let Some(id) = &config.bootstrap_identified_id {
                client.reconcile_identified_bootstrap(id);
            }
        }
        client
    }

    fn disabled(config: &Config, reason: Option<&'static str>) -> Self {
        if let Some(message) = reason {
            if config.on_error.is_empty() {
                tracing::warn!("posthog-rs: {message}; stateful client disabled");
            } else {
                apply_on_error_hooks(
                    &config.on_error,
                    &PostHogError::Initialization(InitializationFailure { message }),
                );
            }
        }
        Self {
            disabled: true,
            shutdown_timeout: config.shutdown_timeout,
            producer: None,
            state: None,
            transport: None,
            closed: AtomicBool::new(false),
        }
    }

    /// Whether configuration or local startup disabled this client.
    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Return the current anonymous identity from memory. Empty when disabled.
    pub fn get_anonymous_id(&self) -> String {
        if self.disabled {
            return String::new();
        }
        self.state.as_ref().unwrap().snapshot().anonymous_id.clone()
    }

    /// Return the current distinct identity from memory. Empty when disabled.
    pub fn get_distinct_id(&self) -> String {
        if self.disabled {
            return String::new();
        }
        self.state.as_ref().unwrap().snapshot().distinct_id.clone()
    }

    fn on_worker_thread(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(StateWriter::on_worker_thread)
            || self
                .transport
                .as_ref()
                .is_some_and(TransportHandle::on_worker_thread)
    }

    /// Request persistence through the current state revision and attempt
    /// delivery of earlier queued events. Does not acknowledge ingestion.
    /// Failed writes/retry cooldowns may leave work pending. An in-flight I/O
    /// operation or user callback can delay the wait.
    pub fn flush_blocking(&self) -> DrainReport {
        if self.disabled {
            return DrainReport::disabled();
        }
        if self.closed.load(Ordering::Acquire) {
            return DrainReport {
                state: PersistenceOutcome::NotRequested,
                delivery: DeliveryOutcome::Closed,
            };
        }
        let state = self.state.as_ref().unwrap();
        let transport = self.transport.as_ref().unwrap();
        let revision = state.checkpoint();
        let (tx, rx) = mpsc::channel();
        let sent = transport.send_control(Control::Flush(Completion::Blocking(tx)));
        if self.on_worker_thread() || self.in_capture() {
            return DrainReport::deferred();
        }
        DrainReport {
            state: state.wait_blocking(revision).into(),
            delivery: if sent && rx.recv().is_ok() {
                DeliveryOutcome::Attempted
            } else {
                DeliveryOutcome::Unavailable
            },
        }
    }

    /// Async flush through the same captured state revision and delivery queue
    /// as `flush_blocking`. Does not require a Tokio runtime. Cancelling the
    /// wait does not cancel work already requested from the workers.
    #[cfg(feature = "async-client")]
    pub async fn flush(&self) -> DrainReport {
        if self.disabled {
            return DrainReport::disabled();
        }
        if self.closed.load(Ordering::Acquire) {
            return DrainReport {
                state: PersistenceOutcome::NotRequested,
                delivery: DeliveryOutcome::Closed,
            };
        }
        let state = self.state.as_ref().unwrap();
        let transport = self.transport.as_ref().unwrap();
        let revision = state.checkpoint();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sent = transport.send_control(Control::Flush(Completion::Async(tx)));
        if self.on_worker_thread() || self.in_capture() {
            return DrainReport::deferred();
        }
        DrainReport {
            state: state.wait_async(revision).await.into(),
            delivery: if sent && rx.await.is_ok() {
                DeliveryOutcome::Attempted
            } else {
                DeliveryOutcome::Unavailable
            },
        }
    }

    fn request_shutdown(&self, timeout: Option<Duration>) {
        let deadline = Instant::now()
            + timeout
                .unwrap_or(self.shutdown_timeout)
                .min(crate::client::transport::MAX_SHUTDOWN_TIMEOUT);
        // The first caller selects one deadline for both workers. Later calls,
        // including Drop, cannot restart or extend that drain budget.
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.transport
                .as_ref()
                .unwrap()
                .request_close_before(deadline);
            self.state.as_ref().unwrap().request_close_before(deadline);
        }
    }

    /// Close admission, request both workers to stop, then join them. `None`
    /// uses Config::shutdown_timeout; `Some` overrides it for this shutdown.
    /// Durations are clamped to one day. The first shutdown call selects the
    /// shared budget; repeated calls wait for the same teardown.
    ///
    /// This is a best-effort scheduling budget, not a hard return deadline.
    /// Already-running work, I/O, callbacks and final thread joins may exceed it.
    /// Calls from worker callbacks or synchronous capture hooks request closure
    /// but return Deferred instead of waiting.
    pub fn shutdown_blocking(&self, timeout: Option<Duration>) -> DrainReport {
        if self.disabled {
            return DrainReport::disabled();
        }
        self.request_shutdown(timeout);
        let transport = self.transport.as_ref().unwrap();
        let state = self.state.as_ref().unwrap();
        if self.on_worker_thread() || self.in_capture() {
            return DrainReport::deferred();
        }
        transport.join();
        DrainReport {
            state: state.join_blocking().into(),
            delivery: DeliveryOutcome::Stopped,
        }
    }

    /// Async shutdown with the same best-effort budget as `shutdown_blocking`.
    /// `None` uses Config::shutdown_timeout; `Some` overrides it (at most one day).
    /// Worker completion waits need no Tokio runtime, but final thread joins
    /// are synchronous and can block polling or exceed the requested budget.
    /// Cancelling a pending wait does not reopen admission or cancel teardown.
    /// Dropping Client still performs blocking cleanup.
    #[cfg(feature = "async-client")]
    pub async fn shutdown(&self, timeout: Option<Duration>) -> DrainReport {
        if self.disabled {
            return DrainReport::disabled();
        }
        self.request_shutdown(timeout);
        if self.on_worker_thread() || self.in_capture() {
            return DrainReport::deferred();
        }
        let transport = self.transport.as_ref().unwrap();
        let state = self.state.as_ref().unwrap();
        transport.wait_stopped().await;
        let persisted = state.join_async().await;
        transport.join();
        DrainReport {
            state: persisted.into(),
            delivery: DeliveryOutcome::Stopped,
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let on_worker = self.on_worker_thread();
        self.shutdown_blocking(None);
        if on_worker {
            // Field destruction must preserve the same no-join rule as explicit
            // shutdown from either worker, not only the state writer's thread.
            if let Some(state) = self.state.take() {
                state.close_without_join();
            }
        }
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
