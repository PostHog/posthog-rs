//! One runtime-independent deadline notifier. It owns no client/backend and
//! invokes executor wakers only on its own thread, never the selecting caller.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

struct State {
    abandoned: bool,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    deadline: Arc<Mutex<Option<Instant>>>,
    #[cfg(feature = "async-client")]
    progress: tokio::sync::watch::Sender<()>,
}
pub(super) struct Shutdown {
    shared: Arc<Shared>,
    notifier: std::thread::ThreadId,
}
impl Shutdown {
    pub(super) fn new() -> std::io::Result<Self> {
        Self::new_with_deadline(Arc::new(Mutex::new(None)))
    }
    pub(super) fn new_with_deadline(
        deadline: Arc<Mutex<Option<Instant>>>,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State { abandoned: false }),
            deadline,
            changed: Condvar::new(),
            #[cfg(feature = "async-client")]
            progress: tokio::sync::watch::channel(()).0,
        });
        let worker = shared.clone();
        let notifier = std::thread::Builder::new()
            .name("posthog-deadline".into())
            .spawn(move || {
                let mut state = worker.state.lock().unwrap_or_else(|p| p.into_inner());
                loop {
                    if state.abandoned {
                        drop(state);
                        worker.changed.notify_all();
                        #[cfg(feature = "async-client")]
                        worker.progress.send_replace(());
                        return;
                    }
                    let selected = *worker.deadline.lock().unwrap_or_else(|p| p.into_inner());
                    match selected {
                        None => {
                            state = worker
                                .changed
                                .wait(state)
                                .unwrap_or_else(|p| p.into_inner())
                        }
                        Some(end) => {
                            let expired = Instant::now() >= end;
                            drop(state);
                            worker.changed.notify_all();
                            #[cfg(feature = "async-client")]
                            worker.progress.send_replace(());
                            if expired {
                                return;
                            }
                            state = worker.state.lock().unwrap_or_else(|p| p.into_inner());
                            let waited = worker
                                .changed
                                .wait_timeout(
                                    state,
                                    end.saturating_duration_since(Instant::now())
                                        .min(Duration::from_millis(2)),
                                )
                                .unwrap_or_else(|p| p.into_inner());
                            state = waited.0;
                        }
                    }
                }
            })?;
        Ok(Self {
            shared,
            notifier: notifier.thread().id(),
        })
    }
    pub(super) fn on_worker_thread(&self) -> bool {
        self.notifier == std::thread::current().id()
    }

    pub(super) fn select(&self, deadline: Instant) -> (Instant, bool) {
        let state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut selected = self
            .shared
            .deadline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(selected) = *selected {
            return (selected, false);
        }
        *selected = Some(deadline);
        drop(selected);
        drop(state);
        self.shared.changed.notify_all();
        (deadline, true)
    }
    /// End the selected deadline at `at` if it is later. Only `Drop` calls
    /// this: it holds the last reference, so no caller still waits on the
    /// longer deadline. Returns whether the deadline changed.
    pub(super) fn shorten_to(&self, at: Instant) -> bool {
        let state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut selected = self
            .shared
            .deadline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if selected.is_some_and(|end| end <= at) {
            return false;
        }
        *selected = Some(at);
        drop(selected);
        drop(state);
        self.shared.changed.notify_all();
        true
    }
    pub(super) fn complete(&self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .abandoned = true;
        self.shared.changed.notify_all();
    }

    pub(super) fn deadline_handle(&self) -> Arc<Mutex<Option<Instant>>> {
        self.shared.deadline.clone()
    }
    pub(super) fn deadline(&self) -> Option<Instant> {
        *self
            .shared
            .deadline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }
    pub(super) fn wait_blocking(&self, done: impl Fn() -> bool) {
        self.wait_blocking_until(self.deadline().unwrap(), done);
    }
    /// Wait until `done` or `bound`, whichever comes first, whatever deadline
    /// is selected.
    pub(super) fn wait_blocking_until(&self, bound: Instant, done: impl Fn() -> bool) {
        while !done() {
            let state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
            let remaining = bound.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            drop(
                self.shared
                    .changed
                    .wait_timeout(state, remaining.min(Duration::from_millis(2)))
                    .unwrap_or_else(|p| p.into_inner()),
            );
        }
    }
    #[cfg(feature = "async-client")]
    pub(super) async fn wait_async(&self, done: impl Fn() -> bool) {
        let mut progress = self.shared.progress.subscribe();
        while !done() && self.deadline().is_some_and(|end| Instant::now() < end) {
            if progress.changed().await.is_err() {
                return;
            }
        }
    }
}
impl Drop for Shutdown {
    fn drop(&mut self) {
        self.complete();
    }
}
