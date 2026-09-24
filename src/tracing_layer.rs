use std::cell::Cell;

thread_local! {
    static IS_CAPTURING: Cell<bool> = const { Cell::new(false) };
}

struct CapturingGuard;

impl CapturingGuard {
    #[inline]
    fn try_acquire() -> Option<Self> {
        IS_CAPTURING.with(|capturing| {
            if capturing.get() {
                None
            } else {
                capturing.set(true);
                Some(Self)
            }
        })
    }
}

impl Drop for CapturingGuard {
    #[inline]
    fn drop(&mut self) {
        IS_CAPTURING.with(|capturing| capturing.set(false));
    }
}
