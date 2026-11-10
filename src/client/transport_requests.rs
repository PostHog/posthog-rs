//! Stateful flag execution. This owner never holds event storage.

use super::{build_http, flags, ClientOptions, Completion, Control, StartupFailure};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle, ThreadId};

pub(super) struct Worker {
    tx: mpsc::Sender<Control>,
    worker: Mutex<Option<JoinHandle<()>>>,
    id: ThreadId,
}

impl Worker {
    pub(super) fn spawn(
        options: ClientOptions,
        slot: Arc<flags::Slot>,
        closed: Arc<AtomicBool>,
        deadline: Arc<Mutex<Option<std::time::Instant>>>,
    ) -> Result<Self, StartupFailure> {
        let (tx, rx) = mpsc::channel();
        let (ready, wait) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("posthog-requests".into())
            .spawn(move || {
                let Some(http) = build_http(&options) else {
                    let _ = ready.send(false);
                    slot.discard();
                    return;
                };
                let _ = ready.send(true);
                while let Ok(control) = rx.recv() {
                    if closed.load(Ordering::Acquire)
                        || deadline.lock().unwrap_or_else(|p| p.into_inner()).is_some()
                    {
                        break;
                    }
                    match control {
                        #[cfg(test)]
                        Control::RequestBarrier(tx) => {
                            let _ = tx.send(());
                        }
                        Control::Flags(slot) => slot.run(&http, &options),
                        _ => break,
                    }
                }
                // Pending completions may own application captures. Destroy them
                // here, never on the thread requesting bounded shutdown.
                slot.discard();
                drop(rx);
            })
            .map_err(|_| StartupFailure::Spawn)?;
        let id = worker.thread().id();
        match wait.recv() {
            Ok(true) => Ok(Self {
                tx,
                worker: Mutex::new(Some(worker)),
                id,
            }),
            result => {
                let _ = worker.join();
                Err(if result.is_ok() {
                    StartupFailure::Http
                } else {
                    StartupFailure::WorkerStopped
                })
            }
        }
    }

    pub(super) fn sender(&self) -> &mpsc::Sender<Control> {
        &self.tx
    }

    pub(super) fn on_worker_thread(&self) -> bool {
        self.id == thread::current().id()
    }

    pub(super) fn request_close(&self) {
        let (tx, _) = mpsc::channel();
        let _ = self.tx.send(Control::Shutdown {
            completion: Completion::Blocking(tx),
            deadline: None,
        });
    }

    pub(super) fn is_stopped(&self) -> bool {
        self.worker
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map_or(true, JoinHandle::is_finished)
    }

    pub(super) fn join(&self) {
        if !self.on_worker_thread() {
            let worker = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some(worker) = worker {
                let _ = worker.join();
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.request_close();
    }
}
