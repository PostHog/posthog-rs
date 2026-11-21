//! The transport handle's stateful delivery: a coordinator thread that owns
//! the queue and an executor thread that sends requests. See
//! [`queue`](super::queue) for the design.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

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

pub(super) struct Delivery {
    pub(super) inbox: Arc<Inbox>,
    /// The shutdown deadline. The first caller to select one wins.
    deadline: Arc<Mutex<Option<Instant>>>,
    pub(super) drain_timeout: Duration,
    executor: Mutex<Option<JoinHandle<()>>>,
    pub(super) executor_id: Option<ThreadId>,
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
        finished(&self.executor)
    }
}

/// Whether the thread held in `handle` has finished, or was already joined.
fn finished(handle: &Mutex<Option<JoinHandle<()>>>) -> bool {
    handle
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map_or(true, JoinHandle::is_finished)
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
        deadline: Arc<Mutex<Option<Instant>>>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StartupFailure> {
        Self::spawn_stateful_with(options, store, max_queue_bytes, deadline, clock, build_http)
    }

    /// [`Self::spawn_stateful_with_deadline`] with the HTTP client builder as
    /// a test seam.
    pub(super) fn spawn_stateful_with(
        options: ClientOptions,
        store: Option<Box<dyn EventStore>>,
        max_queue_bytes: usize,
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
                events: max_queue_size,
                bytes: max_queue_bytes as u64,
            },
        };
        let (coordinator, inbox) = Coordinator::new(
            settings,
            store,
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
            Arc::new(Mutex::new(None)),
            Arc::new(SystemClock),
        )
    }

    /// Admit an event. Measuring it is the only work done before the inbox's
    /// short lock, and no user code runs, so callers may admit while holding
    /// SDK locks.
    pub(crate) fn enqueue_prepared(&self, event: CaptureEvent) -> Admission {
        let Some(delivery) = &self.delivery else {
            return Admission::Closed;
        };
        if self.is_closed() {
            return Admission::Closed;
        }
        let bytes = record_size(&event);
        let admission = delivery.inbox.admit(event, bytes);
        if admission == Admission::Accepted
            && delivery.inbox.arm()
            && self.tx.send(Control::QueueWake).is_err()
        {
            // Delivery already stopped: nothing will take the event.
            self.closed.store(true, Ordering::Release);
            let stranded = delivery.inbox.drain().len();
            super::dec_len(&self.len, stranded);
            return Admission::Closed;
        }
        admission
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

    /// Whether the coordinator has finished: admitted events are written to
    /// disk and storage is released. The executor may still be sending.
    /// Never waits.
    pub(crate) fn coordinator_stopped(&self) -> bool {
        finished(&self.worker)
    }

    /// Whether every delivery thread has finished. Never waits.
    pub(crate) fn all_stopped(&self) -> bool {
        finished(&self.worker)
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
