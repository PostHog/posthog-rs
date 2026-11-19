//! The transport handle's stateful delivery: a coordinator thread that owns
//! the queue and an executor thread that sends requests. See
//! [`queue`](super::queue) for the design.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use tracing::warn;

use super::inbox::Inbox;
use super::queue::{Coordinator, Executor, Settings};
use super::table::Limits;
#[cfg(test)]
use super::SystemClock;
use super::{
    build_http, Clock, Completion, Control, LaneConfig, TransportHandle, MAX_SHUTDOWN_TIMEOUT,
};
use crate::capture_event::CaptureEvent;
use crate::client::event_store::{record_size, EventStore};
use crate::client::ClientOptions;

/// The immediate result of admitting an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Queued. Delivery and persistence happen in the background.
    Accepted,
    /// Larger than the per-event limit.
    Full,
    /// The client is closed or its delivery stopped.
    Closed,
}

/// Why stateful delivery could not start. Network reachability is never a
/// startup failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupFailure {
    /// A delivery thread could not be spawned.
    Spawn,
    /// The HTTP client could not be built.
    Http,
    /// A delivery thread stopped before reporting that it was ready.
    WorkerStopped,
}

/// An admission result whose diagnostic is reported later. Admission itself
/// never runs user code, so callers may admit while holding SDK locks and
/// report after releasing them: a tracing subscriber is user code.
#[must_use]
pub(crate) struct PreparedAdmission {
    pub(crate) admission: Admission,
    too_large: Option<usize>,
}

impl PreparedAdmission {
    fn new(admission: Admission) -> Self {
        Self {
            admission,
            too_large: None,
        }
    }

    /// Log any diagnostic and return the admission result.
    pub(crate) fn report(self) -> Admission {
        if let Some(bytes) = self.too_large {
            warn!("posthog-rs: dropping an event of {bytes} bytes, over the per-event size limit");
        }
        self.admission
    }
}

pub(super) struct Delivery {
    pub(super) inbox: Arc<Inbox>,
    /// The shutdown deadline. The first caller to select one wins.
    deadline: Arc<Mutex<Option<Instant>>>,
    pub(super) drain_timeout: Duration,
    executor: Mutex<Option<JoinHandle<()>>>,
    pub(super) executor_id: Option<ThreadId>,
    /// Latches the oversized-event warning until an event is accepted again.
    too_large_warned: AtomicBool,
}

impl Delivery {
    pub(super) fn select_deadline(&self, at: Instant) {
        let mut deadline = self.deadline.lock().unwrap_or_else(|p| p.into_inner());
        if deadline.is_none() {
            *deadline = Some(at);
        }
    }

    pub(super) fn join_executor(&self) {
        if self.executor_id == Some(thread::current().id()) {
            return;
        }
        if let Some(handle) = self
            .executor
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = handle.join();
        }
    }

    fn executor_stopped(&self) -> bool {
        self.executor
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map_or(true, JoinHandle::is_finished)
    }
}

/// The name of the thread that sends stateful delivery requests.
const EXECUTOR_THREAD_NAME: &str = "posthog-upload";

impl TransportHandle {
    /// Start stateful delivery: the coordinator, which owns the queue and
    /// `store`, and the executor, which sends requests. Returns once the
    /// executor has built its HTTP client; it does not wait for queued events
    /// from an earlier run. `deadline` is the shared shutdown deadline: once a
    /// caller selects it, delivery stops by then.
    pub(crate) fn spawn_stateful_with_deadline(
        options: ClientOptions,
        store: Option<Box<dyn EventStore>>,
        max_queue_bytes: usize,
        max_event_bytes: usize,
        deadline: Arc<Mutex<Option<Instant>>>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StartupFailure> {
        Self::spawn_stateful_with(
            options,
            store,
            max_queue_bytes,
            max_event_bytes,
            deadline,
            clock,
            build_http,
        )
    }

    /// [`Self::spawn_stateful_with_deadline`] with the HTTP client builder as
    /// a test seam.
    pub(super) fn spawn_stateful_with(
        options: ClientOptions,
        store: Option<Box<dyn EventStore>>,
        max_queue_bytes: usize,
        max_event_bytes: usize,
        deadline: Arc<Mutex<Option<Instant>>>,
        clock: Arc<dyn Clock>,
        build_http: fn(&ClientOptions) -> Option<reqwest::blocking::Client>,
    ) -> Result<Self, StartupFailure> {
        let lane = LaneConfig::analytics();
        let (tx, rx) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(0));
        let max_queue_size = lane.max_queue_size(&options);
        let settings = Settings {
            options: options.clone(),
            lane: lane.clone(),
            memory: Limits {
                events: max_queue_size.max(1),
                bytes: max_queue_bytes as u64,
            },
        };
        let memory_only = store.is_none();
        let (coordinator, inbox) = Coordinator::new(
            settings,
            store,
            max_event_bytes,
            clock.clone(),
            pending.clone(),
            deadline.clone(),
        );

        let (jobs_tx, jobs_rx) = mpsc::sync_channel(1);
        let (ready_tx, ready_rx) = mpsc::channel();
        let executor = {
            let options = options.clone();
            let lane = lane.clone();
            let clock = clock.clone();
            let deadline = deadline.clone();
            let results = tx.clone();
            thread::Builder::new()
                .name(EXECUTOR_THREAD_NAME.to_string())
                .spawn(move || {
                    let Some(http) = build_http(&options) else {
                        let _ = ready_tx.send(false);
                        return;
                    };
                    let _ = ready_tx.send(true);
                    Executor {
                        http,
                        options,
                        lane,
                        clock,
                        deadline,
                        memory_only,
                    }
                    .run(jobs_rx, results);
                })
                .map_err(|_| StartupFailure::Spawn)?
        };
        let ready = ready_rx.recv();
        if !matches!(ready, Ok(true)) {
            drop(jobs_tx);
            let _ = executor.join();
            return Err(match ready {
                Ok(_) => StartupFailure::Http,
                Err(_) => StartupFailure::WorkerStopped,
            });
        }
        let executor_id = Some(executor.thread().id());
        let worker = match thread::Builder::new()
            .name(lane.thread_name.to_string())
            .spawn(move || coordinator.run(jobs_tx, rx))
        {
            Ok(worker) => worker,
            Err(_) => {
                // The failed spawn dropped the job sender, so the executor stops.
                let _ = executor.join();
                return Err(StartupFailure::Spawn);
            }
        };
        let worker_id = Some(worker.thread().id());
        Ok(Self {
            tx,
            len: pending,
            closed: AtomicBool::new(false),
            worker: Mutex::new(Some(worker)),
            full_warned: AtomicBool::new(false),
            max_queue_size,
            endpoint: lane.endpoint,
            clock,
            worker_id,
            delivery: Some(Delivery {
                inbox,
                deadline,
                drain_timeout: Duration::from_millis(options.shutdown_timeout_ms)
                    .min(MAX_SHUTDOWN_TIMEOUT),
                executor: Mutex::new(Some(executor)),
                executor_id,
                too_large_warned: AtomicBool::new(false),
            }),
        })
    }

    /// Test seam: stateful delivery on the system clock with its own deadline.
    #[cfg(test)]
    pub(crate) fn spawn_stateful(
        options: ClientOptions,
        store: Option<Box<dyn EventStore>>,
        max_queue_bytes: usize,
    ) -> Result<Self, StartupFailure> {
        Self::spawn_stateful_with_deadline(
            options,
            store,
            max_queue_bytes,
            max_queue_bytes,
            Arc::new(Mutex::new(None)),
            Arc::new(SystemClock),
        )
    }

    /// Admit an event whose serialized record size the caller measured before
    /// taking any SDK lock. Runs no user code.
    pub(crate) fn enqueue_measured_deferred(
        &self,
        event: CaptureEvent,
        bytes: usize,
    ) -> PreparedAdmission {
        let Some(delivery) = &self.delivery else {
            return PreparedAdmission::new(Admission::Closed);
        };
        if self.is_closed() {
            return PreparedAdmission::new(Admission::Closed);
        }
        match delivery.inbox.admit(event, bytes) {
            Admission::Accepted => {
                if delivery.inbox.arm() && self.tx.send(Control::QueueWake).is_err() {
                    // Delivery already stopped: nothing will take the event.
                    self.closed.store(true, Ordering::Release);
                    let stranded = delivery.inbox.drain().len();
                    super::dec_len(&self.len, stranded);
                    return PreparedAdmission::new(Admission::Closed);
                }
                delivery.too_large_warned.store(false, Ordering::Release);
                PreparedAdmission::new(Admission::Accepted)
            }
            refused => PreparedAdmission {
                admission: refused,
                too_large: (refused == Admission::Full
                    && !delivery.too_large_warned.swap(true, Ordering::AcqRel))
                .then_some(bytes),
            },
        }
    }

    /// Like [`Self::enqueue_measured_deferred`], measuring the event here.
    pub(crate) fn enqueue_prepared_deferred(&self, event: CaptureEvent) -> PreparedAdmission {
        let bytes = record_size(&event);
        self.enqueue_measured_deferred(event, bytes)
    }

    /// Admit an event and report any diagnostic at once.
    pub(crate) fn enqueue_prepared(&self, event: CaptureEvent) -> Admission {
        self.enqueue_prepared_deferred(event).report()
    }

    /// Close and stop delivery by `deadline` without waiting. An earlier
    /// selected deadline stands.
    pub(crate) fn request_close_before(&self, deadline: Instant) {
        let Some(delivery) = &self.delivery else {
            self.request_close();
            return;
        };
        delivery.select_deadline(deadline);
        if self.begin_close() {
            let (completion, _) = mpsc::channel();
            let _ = self
                .tx
                .send(Control::Shutdown(Completion::Blocking(completion)));
        } else {
            // Wake the coordinator to read the deadline.
            let _ = self.tx.send(Control::QueueWake);
        }
    }

    /// Whether every delivery thread has finished. Never waits.
    pub(crate) fn all_stopped(&self) -> bool {
        let worker_stopped = self
            .worker
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map_or(true, JoinHandle::is_finished);
        worker_stopped
            && self
                .delivery
                .as_ref()
                .map_or(true, Delivery::executor_stopped)
    }
}

impl Drop for TransportHandle {
    /// A stateful handle dropped without a shutdown, for example by a
    /// cancelled initialization, stops its threads at once. Queued events on
    /// disk stay for the next start.
    fn drop(&mut self) {
        if self.delivery.is_some() && !self.is_closed() {
            self.request_close_before(self.clock.now());
        }
    }
}
