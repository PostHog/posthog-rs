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
    build_http, finished, Clock, Completion, Control, LaneConfig, TransportHandle,
    MAX_SHUTDOWN_TIMEOUT,
};
use crate::capture_event::CaptureEvent;
use crate::client::event_store::EventStore;
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
    /// One active feature flag request plus one replaceable pending request.
    flags: Arc<super::flags::Slot>,
    /// Sends feature flag requests, independent of event delivery.
    requests: Option<super::requests::Worker>,
    /// Session replay's encoder and delivery lane, once installed.
    #[cfg(feature = "session-replay")]
    replay: Mutex<Option<Replay>>,
}

#[cfg(feature = "session-replay")]
struct Replay {
    encoder: Arc<crate::client::stateful::replay::encoder::Worker>,
    lane: Arc<super::replay::Lane>,
}

impl Delivery {
    #[cfg(feature = "session-replay")]
    fn replay<T>(&self, read: impl FnOnce(&Replay) -> T) -> Option<T> {
        self.replay
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(read)
    }

    /// Route a flush, or a test tick, to both lanes: `control` builds the
    /// replay lane's copy. The returned completion fires once both finish.
    pub(super) fn split_flush(
        &self,
        completion: Completion,
        #[allow(unused_variables)] control: fn(Completion) -> Control,
    ) -> Completion {
        #[cfg(feature = "session-replay")]
        if let Some(gate) = self.replay(|replay| replay.encoder.gate.clone()) {
            let shared = super::SharedCompletion::split(completion, 2);
            match gate.control(control(Completion::Shared(shared.clone()))) {
                Err(Control::Flush(part)) => part.signal(),
                #[cfg(test)]
                Err(Control::Tick(part)) => part.signal(),
                _ => {}
            }
            return Completion::Shared(shared);
        }
        completion
    }

    /// Stop session replay with the selected deadline, after the frames
    /// already submitted.
    fn close_replay(&self) {
        #[cfg(feature = "session-replay")]
        if let Some(gate) = self.replay(|replay| replay.encoder.gate.clone()) {
            let (tx, _) = mpsc::channel();
            let _ = gate.control(Control::Shutdown(Completion::Blocking(tx)));
        }
    }

    pub(super) fn close_requests(&self) {
        self.close_replay();
        self.flags.close();
        if let Some(requests) = &self.requests {
            requests.request_close();
        }
    }

    pub(super) fn join_requests(&self) {
        if let Some(requests) = &self.requests {
            requests.join();
        }
        // The encoder stops first: the lane stops at its shutdown barrier.
        // Join outside the lock, which client calls from their hooks take.
        #[cfg(feature = "session-replay")]
        if let Some((encoder, lane)) =
            self.replay(|replay| (replay.encoder.clone(), replay.lane.clone()))
        {
            encoder.join();
            lane.join();
        }
    }

    pub(super) fn on_request_worker(&self) -> bool {
        #[cfg(feature = "session-replay")]
        if self
            .replay(|replay| replay.encoder.on_worker_thread() || replay.lane.on_thread())
            .unwrap_or(false)
        {
            return true;
        }
        self.requests
            .as_ref()
            .is_some_and(super::requests::Worker::on_worker_thread)
    }

    #[cfg(feature = "session-replay")]
    fn replay_stopped(&self) -> bool {
        self.replay(|replay| replay.encoder.is_stopped() && replay.lane.is_stopped())
            .unwrap_or(true)
    }

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
        let flags = Arc::new(super::flags::Slot::default());
        let mut handle = Self {
            tx,
            len: pending,
            closed: Arc::new(AtomicBool::new(false)),
            worker: Mutex::new(Some(worker)),
            full_warned: AtomicBool::new(false),
            max_queue_size,
            endpoint: lane.endpoint,
            clock,
            worker_id,
            delivery: Some(Delivery {
                inbox,
                deadline: deadline.clone(),
                drain_timeout: Duration::from_millis(options.shutdown_timeout_ms)
                    .min(MAX_SHUTDOWN_TIMEOUT),
                executor: Mutex::new(Some(executor)),
                executor_id,
                flags: flags.clone(),
                requests: None,
                #[cfg(feature = "session-replay")]
                replay: Mutex::new(None),
            }),
        };
        match super::requests::Worker::spawn(options, flags, handle.closed.clone(), deadline) {
            Ok(requests) => {
                handle.delivery.as_mut().expect("set above").requests = Some(requests);
                Ok(handle)
            }
            Err(failure) => {
                handle.request_close();
                handle.join();
                Err(failure)
            }
        }
    }

    /// Schedule a feature flag request: one runs at a time and one more waits,
    /// replaced by any later request. The completion must not retain this
    /// transport.
    pub(crate) fn schedule_flags(&self, task: super::flags::Task) -> bool {
        self.delivery.as_ref().is_some_and(|delivery| {
            delivery.requests.as_ref().is_some_and(|requests| {
                delivery
                    .flags
                    .enqueue(task, requests.sender(), &self.closed)
            })
        })
    }

    /// Load remote configuration on the request worker.
    pub(crate) fn schedule_remote_config(&self, task: super::remote_config::Task) -> bool {
        !self.is_closed()
            && self
                .delivery
                .as_ref()
                .and_then(|delivery| delivery.requests.as_ref())
                .is_some_and(|requests| requests.sender().send(Control::RemoteConfig(task)).is_ok())
    }

    /// Wait only for work preceding this FIFO position, not later requests.
    #[cfg(test)]
    pub(crate) fn request_barrier(&self) {
        let requests = self
            .delivery
            .as_ref()
            .and_then(|delivery| delivery.requests.as_ref())
            .expect("stateful request worker");
        assert!(
            !requests.on_worker_thread(),
            "request barrier cannot wait on itself"
        );
        let (tx, rx) = mpsc::channel();
        requests.sender().send(Control::RequestBarrier(tx)).unwrap();
        rx.recv_timeout(Duration::from_secs(5))
            .expect("request worker did not reach bounded completion barrier");
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

    /// Admit an event, measuring it here.
    #[cfg(test)]
    pub(crate) fn enqueue_prepared(&self, event: CaptureEvent) -> Admission {
        let bytes = crate::client::event_store::record_size(&event);
        self.enqueue_measured(event, bytes)
    }

    /// Admit an event whose serialized size the caller measured before taking
    /// an SDK lock. No user code runs, so callers may admit under SDK locks.
    pub(crate) fn enqueue_measured(&self, event: CaptureEvent, bytes: usize) -> Admission {
        let Some(delivery) = &self.delivery else {
            return Admission::Closed;
        };
        if self.is_closed() {
            return Admission::Closed;
        }
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

    /// Whether every delivery thread has finished. Never waits.
    /// Event delivery and the feature flag worker have both stopped.
    pub(crate) fn all_stopped(&self) -> bool {
        self.delivery_stopped()
            && self.delivery.as_ref().map_or(true, |delivery| {
                delivery
                    .requests
                    .as_ref()
                    .map_or(true, super::requests::Worker::is_stopped)
            })
    }

    /// Event delivery has stopped.
    pub(crate) fn delivery_stopped(&self) -> bool {
        finished(&self.worker)
            && self.delivery.as_ref().map_or(true, |delivery| {
                #[cfg(feature = "session-replay")]
                if !delivery.replay_stopped() {
                    return false;
                }
                delivery.executor_stopped()
            })
    }

    /// Start session replay: a delivery lane for screenshots and the encoder
    /// that feeds it. Returns the encoder's gate.
    #[cfg(feature = "session-replay")]
    pub(crate) fn install_replay(
        &self,
        producer: crate::client::stateful::producer::Producer,
        config: &crate::client::stateful::ReplayConfig,
        options: ClientOptions,
        token: String,
        shutdown_timeout: Duration,
    ) -> std::io::Result<Arc<crate::client::stateful::replay::encoder::Gate>> {
        let delivery = self
            .delivery
            .as_ref()
            .ok_or_else(|| std::io::Error::other("Replay needs stateful delivery"))?;
        let (lane, sender) =
            super::replay::Lane::spawn(options, self.clock.clone(), delivery.deadline.clone())?;
        let lane = Arc::new(lane);
        let encoder = Arc::new(crate::client::stateful::replay::encoder::Worker::spawn(
            producer,
            config,
            token,
            sender,
            shutdown_timeout,
            delivery.deadline.clone(),
        )?);
        let gate = encoder.gate.clone();
        *delivery.replay.lock().unwrap_or_else(|p| p.into_inner()) = Some(Replay { encoder, lane });
        Ok(gate)
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
