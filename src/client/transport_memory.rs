//! Serialized-byte reservations follow prepared events through channel admission,
//! batching, retry filtering, and teardown. Dropping an event releases its share.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub(super) struct ByteBudget {
    limit: usize,
    used: AtomicUsize,
}

impl ByteBudget {
    pub(super) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
        })
    }

    pub(super) fn available(&self) -> usize {
        self.limit.saturating_sub(self.used.load(Ordering::Acquire))
    }

    pub(super) fn reserve(self: &Arc<Self>, bytes: usize) -> Option<ByteReservation> {
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            if bytes > self.limit.saturating_sub(used) {
                return None;
            }
            match self.used.compare_exchange_weak(
                used,
                used + bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(ByteReservation {
                        budget: Arc::clone(self),
                        bytes,
                    })
                }
                Err(current) => used = current,
            }
        }
    }

    /// Reserve a bounded recovery window before calling a storage backend.
    pub(super) fn reserve_available(self: &Arc<Self>) -> Option<ByteReservation> {
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            let bytes = self.limit.saturating_sub(used);
            if bytes == 0 {
                return None;
            }
            match self.used.compare_exchange_weak(
                used,
                self.limit,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(ByteReservation {
                        budget: Arc::clone(self),
                        bytes,
                    })
                }
                Err(current) => used = current,
            }
        }
    }
}

pub(super) struct ByteReservation {
    budget: Arc<ByteBudget>,
    bytes: usize,
}

impl ByteReservation {
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Transfer part of a recovery window into one event's lifetime.
    pub(super) fn take(&mut self, bytes: usize) -> Option<Self> {
        self.bytes = self.bytes.checked_sub(bytes)?;
        Some(Self {
            budget: Arc::clone(&self.budget),
            bytes,
        })
    }
}

impl Drop for ByteReservation {
    fn drop(&mut self) {
        // This reservation is never cloned; transfers subtract from the source.
        super::dec_len(&self.budget.used, self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Barrier};

    #[test]
    fn reservations_bound_bytes_and_release_on_drop() {
        let budget = ByteBudget::new(100);
        let first = budget.reserve(40).unwrap();
        let second = budget.reserve(60).unwrap();
        assert!(budget.reserve(1).is_none());
        assert!(budget.reserve(101).is_none());
        assert_eq!(budget.available(), 0);
        drop(first);
        assert_eq!(budget.available(), 40);
        drop(second);
        assert_eq!(budget.available(), 100);
    }

    #[test]
    fn recovery_transfers_preserve_reservations_without_double_charging() {
        let budget = ByteBudget::new(100);
        let mut page = budget.reserve_available().unwrap();
        let first = page.take(30).unwrap();
        let second = page.take(50).unwrap();
        assert!(page.take(21).is_none());
        assert_eq!(page.bytes(), 20);
        assert_eq!(budget.available(), 0);
        drop(page);
        assert_eq!(budget.available(), 20);
        drop(first);
        assert_eq!(budget.available(), 50);
        drop(second);
        assert_eq!(budget.available(), 100);
    }

    #[test]
    fn concurrent_reservations_never_exceed_capacity() {
        let budget = ByteBudget::new(256);
        let release = Arc::new(Barrier::new(17));
        let (tx, rx) = mpsc::channel();
        let mut threads = Vec::new();
        for _ in 0..16 {
            let budget = Arc::clone(&budget);
            let release = Arc::clone(&release);
            let tx = tx.clone();
            threads.push(std::thread::spawn(move || {
                let reservation = budget.reserve(64);
                tx.send(reservation.is_some()).unwrap();
                release.wait();
                drop(reservation);
            }));
        }
        drop(tx);
        assert_eq!(rx.iter().take(16).filter(|accepted| *accepted).count(), 4);
        assert_eq!(budget.available(), 0);
        release.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(budget.available(), 256);
    }
}
