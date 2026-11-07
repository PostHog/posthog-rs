//! Cancellation-safe transfer of a client from its initializer to an async caller.
//! Only a notification crosses the async channel. The initializer remains
//! responsible for destroying a client until the caller actually claims it.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use super::runtime::Client;

#[derive(Default)]
struct State {
    client: Option<Client>,
    finished: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

pub(super) struct Owner(Arc<Shared>);
pub(super) struct Pending(Arc<Shared>);

pub(super) fn handoff() -> (Owner, Pending) {
    let shared = Arc::new(Shared::default());
    (Owner(shared.clone()), Pending(shared))
}

impl Owner {
    /// Run on the initializer thread. Keep ownership until the caller claims
    /// the client or abandons its future. Drop also handles an unwinding waker.
    pub(super) fn publish(self, client: Client, ready: tokio::sync::oneshot::Sender<()>) {
        self.0.lock().client = Some(client);
        // Waking can run application code; no lock is held here.
        let _ = ready.send(());
        let mut state = self.0.lock();
        while !state.finished {
            state = self
                .0
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let unclaimed = self.0.lock().client.take();
        // Joining workers belongs to this thread, outside the handoff lock.
        drop(unclaimed);
    }
}

impl Pending {
    /// Called only after notification, with no subsequent await before returning
    /// the client to the application. A suspended future never owns Client.
    pub(super) fn claim(&self) -> Option<Client> {
        let client = {
            let mut state = self.0.lock();
            state.finished = true;
            state.client.take()
        };
        self.0.changed.notify_all();
        client
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.0.lock().finished = true;
        self.0.changed.notify_all();
    }
}
