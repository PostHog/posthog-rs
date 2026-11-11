//! One coalescing state writer. Producers update a bounded in-memory snapshot;
//! the worker owns blocking persistence and never holds the state lock during I/O.

use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use super::storage::{encode_snapshot, StateStore};
use super::Snapshot;
use crate::client::common::report_storage_error;
use crate::client::retry::exponential_backoff;
use crate::client::transport::{Clock, MAX_SHUTDOWN_TIMEOUT};
use crate::client::{OnErrorHook, StorageOperation};

pub(super) enum Backend {
    Memory,
    Unavailable,
    Persistent {
        store: Box<dyn StateStore>,
        restored: bool,
    },
}

#[derive(Clone, Copy)]
pub(super) struct WriterOptions {
    pub(super) max_bytes: usize,
    pub(super) coalesce: Duration,
    pub(super) retry_initial_ms: u64,
    pub(super) retry_max_ms: u64,
    pub(super) shutdown_timeout: Duration,
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024,
            coalesce: Duration::from_millis(250),
            retry_initial_ms: 100,
            retry_max_ms: 30_000,
            shutdown_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpdateError {
    Invalid,
    Full,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DrainOutcome {
    Persisted,
    MemoryOnly,
    Unavailable,
    WriteFailed,
    TimedOut,
    Deferred,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Memory,
    Unavailable,
    Persistent,
}

struct State {
    snapshot: Arc<Snapshot>,
    revision: u64,
    persisted: u64,
    failure: Option<u64>,
    failures: u32,
    due: Option<Instant>,
    closing: Option<Instant>,
    stopped: bool,
    mode: Mode,
    #[cfg(test)]
    tick_requested: u64,
    #[cfg(test)]
    tick_processed: u64,
    #[cfg(test)]
    in_flight: Option<u64>,
}

impl State {
    fn outcome(&self, revision: u64) -> Option<DrainOutcome> {
        match self.mode {
            Mode::Memory => Some(DrainOutcome::MemoryOnly),
            Mode::Unavailable => Some(DrainOutcome::Unavailable),
            Mode::Persistent if self.persisted >= revision => Some(DrainOutcome::Persisted),
            Mode::Persistent if self.failure.is_some_and(|failed| failed >= revision) => {
                Some(DrainOutcome::WriteFailed)
            }
            Mode::Persistent if self.stopped => Some(DrainOutcome::TimedOut),
            Mode::Persistent => None,
        }
    }
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    #[cfg(feature = "async-client")]
    progress: tokio::sync::watch::Sender<()>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Call outside the state lock: waking an executor may run caller code.
    fn notify(&self) {
        self.changed.notify_all();
        #[cfg(feature = "async-client")]
        self.progress.send_replace(());
    }

    #[cfg(test)]
    fn tick_observed(&self, state: &mut State) {
        state.tick_processed = state.tick_requested;
        self.changed.notify_all();
    }
}

/// Always wake waiters on thread exit, including an unexpected destructor panic.
struct WorkerExit {
    shared: Arc<Shared>,
    completed: bool,
}

impl Drop for WorkerExit {
    fn drop(&mut self) {
        {
            let mut state = self.shared.lock();
            if !self.completed {
                state.mode = Mode::Unavailable;
            }
            state.stopped = true;
        }
        self.shared.notify();
    }
}

pub(super) struct StateWriter {
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
    worker_id: Option<ThreadId>,
    clock: Arc<dyn Clock>,
    options: WriterOptions,
}

impl StateWriter {
    pub(super) fn new(
        snapshot: Snapshot,
        backend: Backend,
        options: WriterOptions,
        hooks: Vec<OnErrorHook>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, UpdateError> {
        if options.max_bytes == 0
            || options.retry_initial_ms == 0
            || options.retry_initial_ms > options.retry_max_ms
        {
            return Err(UpdateError::Invalid);
        }
        validate(&snapshot, options.max_bytes)?;
        let (mode, store, restored) = match backend {
            Backend::Memory => (Mode::Memory, None, false),
            Backend::Unavailable => (Mode::Unavailable, None, false),
            Backend::Persistent { store, restored } => (Mode::Persistent, Some(store), restored),
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                snapshot: Arc::new(snapshot),
                revision: 1,
                persisted: u64::from(restored),
                failure: None,
                failures: 0,
                due: (mode == Mode::Persistent && !restored).then(|| clock.now()),
                closing: None,
                stopped: mode != Mode::Persistent,
                mode,
                #[cfg(test)]
                tick_requested: 0,
                #[cfg(test)]
                tick_processed: 0,
                #[cfg(test)]
                in_flight: None,
            }),
            changed: Condvar::new(),
            #[cfg(feature = "async-client")]
            progress: tokio::sync::watch::channel(()).0,
        });
        let worker = if let Some(store) = store {
            let worker_state = shared.clone();
            let worker_clock = clock.clone();
            let worker_hooks = hooks.clone();
            match thread::Builder::new()
                .name("posthog-state".into())
                .spawn(move || {
                    let mut exit = WorkerExit {
                        shared: worker_state.clone(),
                        completed: false,
                    };
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_worker(
                            &worker_state,
                            store,
                            &worker_hooks,
                            worker_clock.as_ref(),
                            options,
                        );
                    }));
                    if result.is_err() {
                        // No backend remains after unwinding. Keep live memory usable.
                        worker_state.lock().mode = Mode::Unavailable;
                        report_storage_error(
                            &worker_hooks,
                            StorageOperation::Write,
                            &io::Error::other("State persistence worker panicked"),
                        );
                    }
                    // Finish callbacks/backend destruction before announcing shutdown.
                    drop(worker_hooks);
                    exit.completed = true;
                }) {
                Ok(worker) => Some(worker),
                Err(error) => {
                    {
                        let mut state = shared.lock();
                        state.mode = Mode::Unavailable;
                        state.stopped = true;
                    }
                    report_storage_error(&hooks, StorageOperation::Open, &error);
                    None
                }
            }
        } else {
            None
        };
        let worker_id = worker.as_ref().map(|worker| worker.thread().id());
        Ok(Self {
            shared,
            worker: Mutex::new(worker),
            worker_id,
            clock,
            options,
        })
    }

    pub(super) fn snapshot(&self) -> Arc<Snapshot> {
        self.shared.lock().snapshot.clone()
    }

    /// Domain-internal mutation only; do not call application code in `change`.
    /// Candidate validation happens before the authoritative state is replaced.
    #[cfg(test)]
    pub(super) fn update(&self, change: impl FnOnce(&mut Snapshot)) -> Result<u64, UpdateError> {
        self.mutate(change).map(|(revision, ())| revision)
    }

    pub(super) fn mutate<T>(
        &self,
        change: impl FnOnce(&mut Snapshot) -> T,
    ) -> Result<(u64, T), UpdateError> {
        let result = {
            let mut state = self.shared.lock();
            if state.closing.is_some() {
                return Err(UpdateError::Closed);
            }
            let mut candidate = (*state.snapshot).clone();
            let result = change(&mut candidate);
            if candidate.same_persisted_state(&state.snapshot) {
                if candidate != *state.snapshot {
                    state.snapshot = Arc::new(candidate);
                }
                return Ok((state.revision, result));
            }
            validate(&candidate, self.options.max_bytes)?;
            let revision = state.revision.checked_add(1).ok_or(UpdateError::Full)?;
            state.snapshot = Arc::new(candidate);
            state.revision = revision;
            if state.mode == Mode::Persistent && state.due.is_none() {
                state.due =
                    Some(self.clock.now() + self.options.coalesce.min(MAX_SHUTDOWN_TIMEOUT));
            }
            (revision, result)
        };
        self.shared.notify();
        Ok(result)
    }

    /// Capture one revision to drain, expediting coalescing but not retry cooldown.
    pub(super) fn checkpoint(&self) -> u64 {
        let revision = {
            let mut state = self.shared.lock();
            if state.mode == Mode::Persistent
                && !state.stopped
                && state.persisted < state.revision
                && state.failures == 0
            {
                state.due = Some(self.clock.now());
            }
            state.revision
        };
        self.shared.notify();
        revision
    }

    pub(super) fn wait_blocking(&self, revision: u64) -> DrainOutcome {
        if self.on_worker_thread() {
            return DrainOutcome::Deferred;
        }
        let mut state = self.shared.lock();
        loop {
            if let Some(outcome) = state.outcome(revision) {
                return outcome;
            }
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    #[cfg(feature = "async-client")]
    pub(super) async fn wait_async(&self, revision: u64) -> DrainOutcome {
        if self.on_worker_thread() {
            return DrainOutcome::Deferred;
        }
        // Subscribe before checking state so completion cannot be lost between
        // the check and awaiting a notification. No runtime/timer is owned here.
        let mut progress = self.shared.progress.subscribe();
        loop {
            if let Some(outcome) = self.shared.lock().outcome(revision) {
                return outcome;
            }
            let _ = progress.changed().await;
        }
    }

    #[cfg(test)]
    pub(super) fn is_closed(&self) -> bool {
        self.shared.lock().closing.is_some()
    }

    pub(super) fn on_worker_thread(&self) -> bool {
        self.worker_id == Some(thread::current().id())
    }

    /// Request every client's worker to close before waiting on any of them.
    pub(super) fn request_close(&self) {
        self.request_close_before(
            self.clock.now() + self.options.shutdown_timeout.min(MAX_SHUTDOWN_TIMEOUT),
        );
    }

    pub(super) fn request_close_before(&self, deadline: Instant) {
        {
            let mut state = self.shared.lock();
            if state.closing.is_none() {
                state.closing = Some(deadline);
                if state.mode == Mode::Persistent
                    && state.persisted < state.revision
                    && state.failures == 0
                {
                    state.due = Some(self.clock.now());
                }
            }
        }
        self.shared.notify();
    }

    pub(super) fn join_blocking(&self) -> DrainOutcome {
        if self.on_worker_thread() {
            return DrainOutcome::Deferred;
        }
        let outcome = {
            let mut state = self.shared.lock();
            while !state.stopped {
                state = self
                    .shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            state
                .outcome(state.revision)
                .unwrap_or(DrainOutcome::TimedOut)
        };
        self.join();
        outcome
    }

    #[cfg(feature = "async-client")]
    pub(super) async fn join_async(&self) -> DrainOutcome {
        if self.on_worker_thread() {
            return DrainOutcome::Deferred;
        }
        let mut progress = self.shared.progress.subscribe();
        let outcome = loop {
            {
                let state = self.shared.lock();
                if state.stopped {
                    break state
                        .outcome(state.revision)
                        .unwrap_or(DrainOutcome::TimedOut);
                }
            }
            let _ = progress.changed().await;
        };
        if self.on_worker_thread() {
            return DrainOutcome::Deferred;
        }
        self.join();
        outcome
    }

    /// Relinquish the join handle during client destruction on either of its
    /// workers. Drop requests closure; the worker retains its own state/backend
    /// until it finishes in-progress I/O and exits.
    pub(super) fn close_without_join(mut self) {
        self.worker
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    fn join(&self) {
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = worker.join();
        }
    }

    #[cfg(test)]
    fn tick(&self) {
        let mut state = self.shared.lock();
        state.tick_requested += 1;
        self.shared.changed.notify_all();
        let requested = state.tick_requested;
        while state.tick_processed < requested && !state.stopped {
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

impl Drop for StateWriter {
    fn drop(&mut self) {
        self.request_close();
        if !self.on_worker_thread() {
            self.join();
        }
    }
}

fn validate(snapshot: &Snapshot, max_bytes: usize) -> Result<(), UpdateError> {
    if !snapshot.is_valid() {
        return Err(UpdateError::Invalid);
    }
    // No candidate may make unrelated future state changes unpersistable.
    // Use the persistence codec itself, including its byte and decoder bounds.
    encode_snapshot(snapshot, max_bytes as u64).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidInput {
            UpdateError::Full
        } else {
            UpdateError::Invalid
        }
    })?;
    Ok(())
}

fn run_worker(
    shared: &Shared,
    mut store: Box<dyn StateStore>,
    hooks: &[OnErrorHook],
    clock: &dyn Clock,
    options: WriterOptions,
) {
    let mut state = shared.lock();
    loop {
        let now = clock.now();
        if state
            .closing
            .is_some_and(|deadline| now >= deadline || state.persisted >= state.revision)
        {
            break;
        }
        if state.persisted >= state.revision {
            state.due = None;
        }
        if state.due.is_some_and(|due| now >= due) {
            let snapshot = state.snapshot.clone();
            let revision = state.revision;
            state.due = None;
            #[cfg(test)]
            {
                state.in_flight = Some(revision);
                shared.tick_observed(&mut state);
            }
            drop(state);
            let result = store.save(&snapshot);
            state = shared.lock();
            #[cfg(test)]
            {
                state.in_flight = None;
            }
            match &result {
                Ok(()) => {
                    state.persisted = revision;
                    state.failure = None;
                    state.failures = 0;
                }
                Err(_) => {
                    state.failure = Some(revision);
                    state.failures = state.failures.saturating_add(1);
                    let delay = exponential_backoff(
                        options.retry_initial_ms,
                        options.retry_max_ms,
                        state.failures,
                    )
                    .min(MAX_SHUTDOWN_TIMEOUT);
                    state.due = Some(clock.now() + delay);
                }
            }
            drop(state);
            shared.notify();
            if let Err(error) = result {
                report_storage_error(hooks, StorageOperation::Write, &error);
            }
            state = shared.lock();
            continue;
        }
        #[cfg(test)]
        shared.tick_observed(&mut state);
        let wake_at = match (state.due, state.closing) {
            (Some(due), Some(close)) => Some(due.min(close)),
            (Some(at), None) | (None, Some(at)) => Some(at),
            (None, None) => None,
        };
        state = if let Some(at) = wake_at {
            shared
                .changed
                .wait_timeout(state, at.saturating_duration_since(now))
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0
        } else {
            shared
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        };
    }
    // Destructors and callbacks must run without the state lock, including a
    // backend whose destructor releases the last client reference.
    drop(state);
    drop(store);
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
