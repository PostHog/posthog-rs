use std::sync::atomic::Ordering;

use super::{Client, Snapshot};

impl Client {
    /// Whether new collection is opted out. Reads memory only; disabled or closed
    /// clients return true. This does not describe previously admitted delivery work.
    pub fn is_opt_out(&self) -> bool {
        self.is_disabled()
            || self.closed.load(Ordering::Acquire)
            || self.snapshot_opted_out(&self.state.as_ref().unwrap().snapshot())
    }

    /// Allow new collection immediately, persisting the choice asynchronously.
    /// Does not enable a disabled/closed client or recreate previously dropped events.
    pub fn opt_in(&self) {
        self.set_opt_out(false);
    }

    /// Block new collection immediately, persisting the choice asynchronously.
    /// Captures permitted before this transition and previously queued requests
    /// may still complete, persist, retry and deliver. Reset clears this choice.
    pub fn opt_out(&self) {
        self.set_opt_out(true);
    }

    fn set_opt_out(&self, opt_out: bool) {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return;
        }
        let _ = self.state.as_ref().unwrap().mutate(|state| {
            state.opt_out = Some(opt_out);
        });
    }

    pub(super) fn snapshot_opted_out(&self, snapshot: &Snapshot) -> bool {
        snapshot
            .opt_out
            .unwrap_or(self.producer.as_ref().unwrap().default_opt_out)
    }
}
