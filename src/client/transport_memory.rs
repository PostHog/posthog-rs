//! Serialized-byte reservations bound resident staging and memory fallback.
//! Persisting or evicting a record releases its resident share; delivery owns one
//! separately count/byte-bounded upload copy of the same logical queue.

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
}

pub(super) struct ByteReservation {
    budget: Arc<ByteBudget>,
    bytes: usize,
}

impl ByteReservation {
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Drop for ByteReservation {
    fn drop(&mut self) {
        // Shared queue entries retain one reservation through an in-progress write.
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

use crate::capture_event::CaptureEvent;
use std::collections::VecDeque;
use std::sync::{atomic::AtomicBool, Mutex};

pub(super) struct Item {
    pub(super) id: u64,
    pub(super) wire: CaptureEvent,
    attempted: AtomicBool,
    _bytes: ByteReservation,
    count: Arc<AtomicUsize>,
}
impl Drop for Item {
    fn drop(&mut self) {
        super::dec_len(&self.count, 1);
    }
}
struct Resident {
    next: u64,
    disk_end: u64,
    items: VecDeque<Arc<Item>>,
}
/// Staging and memory fallback share one count/byte-bounded resident view.
pub(super) struct Queue {
    resident: Mutex<Resident>,
    budget: Arc<ByteBudget>,
    count: Arc<AtomicUsize>,
    capacity: usize,
    wake: AtomicBool,
}
impl Queue {
    pub(super) fn new(
        budget: Arc<ByteBudget>,
        count: Arc<AtomicUsize>,
        capacity: usize,
        disk_end: Option<u64>,
    ) -> Arc<Self> {
        Arc::new(Self {
            resident: Mutex::new(Resident {
                next: disk_end.unwrap_or(0),
                disk_end: disk_end.unwrap_or(u64::MAX),
                items: VecDeque::new(),
            }),
            budget,
            count,
            capacity,
            wake: AtomicBool::new(false),
        })
    }
    pub(super) fn admit(&self, wire: CaptureEvent) -> super::Admission {
        let bytes = super::super::event_store::record_size(&wire);
        if bytes > self.budget.limit || self.capacity == 0 {
            return super::Admission::Full;
        }
        let mut resident = self.resident.lock().unwrap_or_else(|p| p.into_inner());
        let reclaimable: Vec<_> = resident
            .items
            .iter()
            .filter(|item| Arc::strong_count(item) == 1)
            .collect();
        let free_count = self
            .capacity
            .saturating_sub(self.count.load(Ordering::Acquire));
        let free_bytes = self.budget.available();
        if free_count + reclaimable.len() == 0
            || free_bytes
                + reclaimable
                    .iter()
                    .map(|item| item._bytes.bytes())
                    .sum::<usize>()
                < bytes
        {
            return super::Admission::Full;
        }
        let Some(id) = resident.next.checked_add(1) else {
            return super::Admission::Full;
        };
        while self.count.load(Ordering::Acquire) >= self.capacity || self.budget.available() < bytes
        {
            resident.items.pop_front();
        }
        let reservation = self
            .budget
            .reserve(bytes)
            .expect("admission lock reserves bounded memory");
        self.count.fetch_add(1, Ordering::AcqRel);
        resident.next = id;
        resident.items.push_back(Arc::new(Item {
            id,
            wire,
            attempted: AtomicBool::new(false),
            _bytes: reservation,
            count: self.count.clone(),
        }));
        super::Admission::Accepted
    }
    pub(super) fn arm(&self) -> bool {
        !self.wake.swap(true, Ordering::AcqRel)
    }
    pub(super) fn clear_wake(&self) {
        self.wake.store(false, Ordering::Release);
    }
    pub(super) fn limit(&self) -> usize {
        self.budget.limit
    }
    pub(super) fn len(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
    pub(super) fn contains(&self, id: u64) -> bool {
        self.resident
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .items
            .iter()
            .any(|item| item.id == id)
    }
    pub(super) fn remove(&self, id: u64) -> bool {
        let mut resident = self.resident.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(index) = resident.items.iter().position(|item| item.id == id) {
            resident.items.remove(index);
            true
        } else {
            false
        }
    }
    /// Drop every resident event and return how many there were.
    pub(super) fn clear(&self) -> usize {
        let mut resident = self.resident.lock().unwrap_or_else(|p| p.into_inner());
        let dropped = resident.items.len();
        resident.items.clear();
        dropped
    }
    pub(super) fn persisted(&self, key: u64) {
        let mut resident = self.resident.lock().unwrap_or_else(|p| p.into_inner());
        resident.disk_end = resident.disk_end.max(key);
    }
    pub(super) fn checkpoint(&self) -> (u64, u64) {
        let resident = self.resident.lock().unwrap_or_else(|p| p.into_inner());
        (resident.disk_end.max(resident.next), resident.next)
    }
    pub(super) fn end(&self) -> u64 {
        self.resident.lock().unwrap_or_else(|p| p.into_inner()).next
    }
    pub(super) fn stage(&self, through: u64) -> Option<(Arc<Item>, bool)> {
        let resident = self.resident.lock().unwrap_or_else(|p| p.into_inner());
        let mut can_persist = true;
        for item in &resident.items {
            if item.id > through {
                return None;
            }
            if !item.attempted.swap(true, Ordering::AcqRel) {
                return Some((item.clone(), can_persist));
            }
            can_persist = false;
        }
        None
    }
    pub(super) fn view(
        &self,
        count: usize,
        bytes: usize,
        persistent: bool,
    ) -> Vec<(u64, CaptureEvent)> {
        let resident = self.resident.lock().unwrap_or_else(|p| p.into_inner());
        let mut used = 0;
        resident
            .items
            .iter()
            .take(count)
            // Only the coordinator calls stage and view. An unattempted
            // persistent entry can still acquire a disk identity on the next
            // turn and must not enter the upload view as memory first.
            .take_while(|item| !persistent || item.attempted.load(Ordering::Acquire))
            .take_while(|item| {
                let next = item._bytes.bytes();
                if used != 0 && next > bytes.saturating_sub(used) {
                    return false;
                }
                used = used.saturating_add(next);
                true
            })
            .map(|item| (item.id, item.wire.clone()))
            .collect()
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    use crate::client::transport::{storage_tests::wire, Admission};

    #[test]
    fn clearing_reports_how_many_resident_events_were_dropped() {
        let count = Arc::new(AtomicUsize::new(0));
        let queue = Queue::new(ByteBudget::new(4096), count.clone(), 4, None);
        assert_eq!(queue.admit(wire("first")), Admission::Accepted);
        assert_eq!(queue.admit(wire("second")), Admission::Accepted);
        assert_eq!(queue.clear(), 2);
        assert_eq!(count.load(Ordering::Acquire), 0);
        assert_eq!(queue.clear(), 0);
    }

    #[test]
    fn retained_memory_is_fifo_and_replacements_have_exact_admission_identity() {
        let queue = Queue::new(
            ByteBudget::new(4096),
            Arc::new(AtomicUsize::new(0)),
            2,
            None,
        );
        let first = wire("first");
        let mut replacement = wire("replacement");
        replacement.uuid = first.uuid;
        assert_eq!(queue.admit(first), Admission::Accepted);
        let old_id = queue.view(1, 4096, false)[0].0;
        assert_eq!(queue.admit(wire("second")), Admission::Accepted);
        assert_eq!(queue.admit(replacement), Admission::Accepted);
        assert!(!queue.remove(old_id));
        let view = queue.view(10, 4096, false);
        assert_eq!(
            view.iter()
                .map(|(_, event)| event.event.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "replacement"]
        );
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn failed_byte_admission_preserves_resident_records_and_io_reservations() {
        let first = wire("first");
        let bytes = super::super::super::event_store::record_size(&first);
        let queue = Queue::new(
            ByteBudget::new(bytes),
            Arc::new(AtomicUsize::new(0)),
            1,
            None,
        );
        assert_eq!(queue.admit(first), Admission::Accepted);
        let mut huge = wire("oversized");
        huge.properties = serde_json::json!({"data": "x".repeat(bytes)});
        assert_eq!(queue.admit(huge), Admission::Full);
        let (in_progress, _) = queue.stage(queue.end()).unwrap();
        assert_eq!(queue.admit(wire("next")), Admission::Full);
        assert!(queue.contains(in_progress.id));
        assert_eq!(queue.budget.available(), 0);
        let id = in_progress.id;
        drop(in_progress);
        assert_eq!(queue.admit(wire("next")), Admission::Accepted);
        assert!(!queue.contains(id));
        assert!(queue.len() <= 1);
    }
}
