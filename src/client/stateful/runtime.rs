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

#[path = "shutdown.rs"]
mod shutdown;

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
    /// The revision was not confirmed persisted before the shutdown deadline.
    /// An already-started write may still finish afterward.
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
    /// With session-replay, includes preceding submitted encoding and eligible
    /// screenshot delivery, but not frames held by the host.
    Attempted,
    /// The worker has stopped. Undelivered persisted records may remain.
    Stopped,
    /// The selected shutdown deadline expired before all delivery work stopped;
    /// already-started work may finish afterward.
    TimedOut,
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
/// Persistent namespaces permit only one live owner. A second client with the
/// same storage scope uses a generated or bootstrap identity in memory instead
/// of restoring the first client's identity. Share an `Arc<Client>` when the
/// identity should be shared; see [`Persistence`] for namespace ownership and
/// retrying persistent initialization. A shutdown timeout does not release a
/// namespace lease while already-started storage I/O can still access it.
///
/// Dropping the client requests shutdown. SDK-controlled waiting ends by the
/// first selected shutdown deadline; already-started work retains its resources
/// until completion. Arbitrary destructors of application objects captured by
/// callbacks may run synchronously and block outside that waiting bound.
pub struct Client {
    disabled: bool,
    shutdown_timeout: Duration,
    shutdown: Option<shutdown::Shutdown>,
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
        let shutdown = match shutdown::Shutdown::new() {
            Ok(shutdown) => shutdown,
            Err(_) => return Self::disabled(&config, Some("Cannot start the shutdown notifier")),
        };
        let producer = Producer::new(&config);
        let scope = namespace(&config.project_token, &config.instance_name);
        let restored = RestoredStorage::open(
            &config.persistence,
            &scope,
            config.event_limits(),
            &config.on_error,
        );
        let was_restored = restored.snapshot.is_some();
        let snapshot = restored.snapshot.unwrap_or_else(|| {
            Snapshot::anonymous(
                config
                    .bootstrap_anonymous_id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
            )
        });
        let backend = match restored.state_store {
            Some(store) => Backend::Persistent {
                store: Box::new(store),
                restored: was_restored && !restored.rewrite_required,
            },
            None if matches!(config.persistence, Persistence::Memory) => Backend::Memory,
            None => Backend::Unavailable,
        };
        let transport = match TransportHandle::spawn_stateful_with_deadline(
            config.transport_options(),
            restored
                .event_store
                .map(|store| Box::new(store) as Box<dyn crate::client::event_store::EventStore>),
            config.max_queue_bytes,
            shutdown.deadline_handle(),
            Arc::new(SystemClock),
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
        let state = match StateWriter::new_with_deadline(
            snapshot,
            backend,
            WriterOptions {
                coalesce: config.state_write_delay,
                shutdown_timeout: config.shutdown_timeout,
                ..WriterOptions::default()
            },
            config.on_error.clone(),
            Arc::new(SystemClock),
            shutdown.deadline_handle(),
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
        Self {
            disabled: false,
            shutdown: Some(shutdown),
            shutdown_timeout: config.shutdown_timeout,
            producer: Some(producer),
            state: Some(state),
            transport: Some(transport),
            closed: AtomicBool::new(false),
        }
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
            shutdown: None,
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
        self.shutdown
            .as_ref()
            .is_some_and(shutdown::Shutdown::on_worker_thread)
            || self
                .state
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
        let (deadline, first) = self.shutdown.as_ref().unwrap().select(deadline);
        // Publish the selected deadline and closure before queueing any work.
        self.closed.store(true, Ordering::Release);
        if first {
            self.state.as_ref().unwrap().request_close_before(deadline);
            self.transport
                .as_ref()
                .unwrap()
                .request_close_before(deadline);
        }
    }

    /// Close admission and wait for SDK workers until the first selected deadline.
    /// `None` uses Config::shutdown_timeout; `Some` overrides it (at most one day).
    /// Repeated calls share that deadline. Already-started work owns its resources
    /// through completion; return does not promise every event reached disk.
    /// Application callback-capture destructors may run synchronously and block
    /// outside the SDK-controlled waiting bound. Worker callbacks and synchronous
    /// capture hooks request closure but return Deferred instead of waiting.
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
        self.shutdown
            .as_ref()
            .unwrap()
            .wait_blocking(|| transport.all_stopped() && state.is_stopped());
        if transport.all_stopped() && state.is_stopped() {
            self.shutdown.as_ref().unwrap().complete();
        }
        DrainReport {
            state: state.shutdown_outcome().into(),
            delivery: if transport.all_stopped() {
                DeliveryOutcome::Stopped
            } else {
                DeliveryOutcome::TimedOut
            },
        }
    }

    /// Runtime-independent async shutdown with the same deadline and application
    /// destructor boundary as `shutdown_blocking`. Cancelling a waiter does not
    /// reopen admission, extend the deadline, or cancel already-requested teardown.
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
        self.shutdown
            .as_ref()
            .unwrap()
            .wait_async(|| transport.all_stopped() && state.is_stopped())
            .await;
        if self.on_worker_thread() {
            return DrainReport::deferred();
        }
        if transport.all_stopped() && state.is_stopped() {
            self.shutdown.as_ref().unwrap().complete();
        }
        DrainReport {
            state: state.shutdown_outcome().into(),
            delivery: if transport.all_stopped() {
                DeliveryOutcome::Stopped
            } else {
                DeliveryOutcome::TimedOut
            },
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
