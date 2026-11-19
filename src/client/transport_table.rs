//! The stateful delivery queue: one ordered entry per queued event, owned by
//! the coordinator thread. It is the only record of what is queued, in every
//! persistence mode.
//!
//! An entry is either on disk (no payload in memory; read back when sent) or
//! memory-only (its payload stays here): memory-only persistence, or an event
//! whose disk write failed. Both kinds share one order, so a failed write never
//! reorders delivery.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use crate::capture_event::CaptureEvent;

pub(super) struct Entry {
    pub(super) seq: u64,
    pub(super) bytes: u64,
    /// `Some` for a memory-only entry, `None` once the event is on disk.
    pub(super) payload: Option<Arc<CaptureEvent>>,
    /// Set after a transient read failure: not selectable before this time.
    pub(super) read_after: Option<Instant>,
}

impl Entry {
    pub(super) fn on_disk(&self) -> bool {
        self.payload.is_none()
    }

    fn ready(&self, now: Instant) -> bool {
        self.read_after.map_or(true, |after| now >= after)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Limits {
    pub(super) events: usize,
    pub(super) bytes: u64,
}

#[derive(Default)]
pub(super) struct Table {
    entries: VecDeque<Entry>,
    memory: Usage,
    disk: Usage,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Usage {
    events: usize,
    bytes: u64,
}

impl Usage {
    fn add(&mut self, bytes: u64) {
        self.events += 1;
        self.bytes += bytes;
    }

    fn remove(&mut self, bytes: u64) {
        self.events -= 1;
        self.bytes -= bytes;
    }

    fn over(&self, limits: Limits) -> bool {
        self.events > limits.events || self.bytes > limits.bytes
    }
}

impl Table {
    /// Append an entry. Sequence numbers only grow.
    pub(super) fn push(&mut self, entry: Entry) {
        debug_assert!(self
            .entries
            .back()
            .map_or(true, |last| last.seq < entry.seq));
        self.usage_mut(&entry).add(entry.bytes);
        self.entries.push_back(entry);
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[cfg(test)]
    fn memory(&self) -> Usage {
        self.memory
    }

    #[cfg(test)]
    fn disk(&self) -> Usage {
        self.disk
    }

    pub(super) fn first_seq(&self) -> Option<u64> {
        self.entries.front().map(|entry| entry.seq)
    }

    pub(super) fn last_seq(&self) -> Option<u64> {
        self.entries.back().map(|entry| entry.seq)
    }

    pub(super) fn get(&self, seq: u64) -> Option<&Entry> {
        self.position(seq).map(|index| &self.entries[index])
    }

    pub(super) fn delay_read(&mut self, seq: u64, until: Instant) {
        if let Some(index) = self.position(seq) {
            self.entries[index].read_after = Some(until);
        }
    }

    /// Make an entry ready again once a read of it succeeds.
    pub(super) fn clear_read_delay(&mut self, seq: u64) {
        if let Some(index) = self.position(seq) {
            self.entries[index].read_after = None;
        }
    }

    pub(super) fn contains_all(&self, seqs: &[u64]) -> bool {
        seqs.iter().all(|seq| self.position(*seq).is_some())
    }

    /// Remove the given entries, returning the ones that were queued.
    pub(super) fn remove(&mut self, seqs: &[u64]) -> Vec<Entry> {
        let mut removed = Vec::new();
        for seq in seqs {
            if let Some(index) = self.position(*seq) {
                let entry = self.entries.remove(index).expect("position is in range");
                self.usage_mut(&entry).remove(entry.bytes);
                removed.push(entry);
            }
        }
        removed
    }

    /// Remove the oldest memory-only entries over `memory` and the oldest
    /// on-disk entries over `disk`. The newest entry of each kind is kept.
    pub(super) fn evict(&mut self, memory: Limits, disk: Limits) -> Vec<Entry> {
        let mut evicted = Vec::new();
        for on_disk in [false, true] {
            let limits = if on_disk { disk } else { memory };
            while {
                let usage = if on_disk { self.disk } else { self.memory };
                usage.events > 1 && usage.over(limits)
            } {
                let index = self
                    .entries
                    .iter()
                    .position(|entry| entry.on_disk() == on_disk)
                    .expect("usage counts a matching entry");
                let entry = self.entries.remove(index).expect("position is in range");
                self.usage_mut(&entry).remove(entry.bytes);
                evicted.push(entry);
            }
        }
        evicted
    }

    /// The oldest entries ready to send, in order: at most `max_events`, and
    /// within `target_bytes` except that the first entry is always included.
    pub(super) fn select(&self, max_events: usize, target_bytes: u64, now: Instant) -> Vec<u64> {
        let mut selected = Vec::new();
        let mut bytes = 0u64;
        for entry in self.entries.iter().filter(|entry| entry.ready(now)) {
            if selected.len() >= max_events
                || (!selected.is_empty() && bytes + entry.bytes > target_bytes)
            {
                break;
            }
            bytes += entry.bytes;
            selected.push(entry.seq);
        }
        selected
    }

    /// Whether any entry at or below `through` is ready to send.
    pub(super) fn ready_through(&self, through: u64, now: Instant) -> bool {
        self.entries
            .iter()
            .take_while(|entry| entry.seq <= through)
            .any(|entry| entry.ready(now))
    }

    /// The earliest time an entry waiting after a read failure becomes ready.
    pub(super) fn next_read(&self) -> Option<Instant> {
        self.entries
            .iter()
            .filter_map(|entry| entry.read_after)
            .min()
    }

    /// The earliest time later than `now` that an entry waiting after a read
    /// failure becomes ready.
    pub(super) fn next_read_after(&self, now: Instant) -> Option<Instant> {
        self.entries
            .iter()
            .filter_map(|entry| entry.read_after)
            .filter(|after| *after > now)
            .min()
    }

    /// Remove every entry, for shutdown.
    pub(super) fn take_all(&mut self) -> Vec<Entry> {
        self.memory = Usage::default();
        self.disk = Usage::default();
        self.entries.drain(..).collect()
    }

    fn position(&self, seq: u64) -> Option<usize> {
        self.entries
            .binary_search_by_key(&seq, |entry| entry.seq)
            .ok()
    }

    fn usage_mut(&mut self, entry: &Entry) -> &mut Usage {
        if entry.on_disk() {
            &mut self.disk
        } else {
            &mut self.memory
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Event;
    use std::time::Duration;

    fn memory(seq: u64, bytes: u64) -> Entry {
        Entry {
            seq,
            bytes,
            payload: Some(Arc::new(CaptureEvent::from_event(&Event::new(
                "event", "user",
            )))),
            read_after: None,
        }
    }

    fn disk(seq: u64, bytes: u64) -> Entry {
        Entry {
            seq,
            bytes,
            payload: None,
            read_after: None,
        }
    }

    fn seqs(entries: &[Entry]) -> Vec<u64> {
        entries.iter().map(|entry| entry.seq).collect()
    }

    fn limits(events: usize, bytes: u64) -> Limits {
        Limits { events, bytes }
    }

    #[test]
    fn memory_and_disk_entries_share_one_order_and_separate_usage() {
        let mut table = Table::default();
        table.push(disk(1, 10));
        table.push(memory(2, 20));
        table.push(disk(3, 30));
        assert_eq!(table.select(10, 1000, Instant::now()), [1, 2, 3]);
        assert_eq!(
            table.memory(),
            Usage {
                events: 1,
                bytes: 20
            }
        );
        assert_eq!(
            table.disk(),
            Usage {
                events: 2,
                bytes: 40
            }
        );
        assert!(!table.get(2).unwrap().on_disk());
        assert!(table.get(3).unwrap().on_disk());
    }

    #[test]
    fn selection_takes_the_oldest_ready_entries_within_count_and_byte_targets() {
        let mut table = Table::default();
        for seq in 1..=5 {
            table.push(disk(seq, 10));
        }
        let now = Instant::now();
        assert_eq!(table.select(2, 1000, now), [1, 2]);
        assert_eq!(table.select(10, 25, now), [1, 2]);
        // An entry larger than the target still goes, alone.
        assert_eq!(table.select(10, 5, now), [1]);
        table.delay_read(1, now + Duration::from_secs(1));
        assert_eq!(table.select(2, 1000, now), [2, 3]);
        assert_eq!(table.next_read(), Some(now + Duration::from_secs(1)));
        assert_eq!(table.select(2, 1000, now + Duration::from_secs(1)), [1, 2]);
    }

    #[test]
    fn ready_through_ignores_later_and_delayed_entries() {
        let mut table = Table::default();
        table.push(disk(1, 10));
        table.push(disk(5, 10));
        let now = Instant::now();
        assert!(table.ready_through(1, now));
        table.delay_read(1, now + Duration::from_secs(1));
        assert!(!table.ready_through(4, now));
        assert!(table.ready_through(5, now));
    }

    #[test]
    fn the_next_read_time_skips_past_and_cleared_delays() {
        let mut table = Table::default();
        for seq in 1..=3 {
            table.push(disk(seq, 10));
        }
        let now = Instant::now();
        table.delay_read(1, now + Duration::from_secs(1));
        table.delay_read(2, now + Duration::from_secs(3));
        let later = now + Duration::from_secs(2);
        assert_eq!(
            table.next_read_after(later),
            Some(now + Duration::from_secs(3))
        );
        table.clear_read_delay(2);
        assert_eq!(table.next_read_after(later), None);
    }

    #[test]
    fn eviction_drops_the_oldest_of_each_kind_and_keeps_the_newest() {
        let mut table = Table::default();
        table.push(disk(1, 10));
        table.push(memory(2, 10));
        table.push(disk(3, 10));
        table.push(memory(4, 10));
        table.push(disk(5, 10));
        let evicted = table.evict(limits(1, 1000), limits(2, 1000));
        assert_eq!(seqs(&evicted), [2, 1]);
        assert_eq!(table.select(10, 1000, Instant::now()), [3, 4, 5]);
        // A single entry over the byte limit stays.
        let evicted = table.evict(limits(10, 5), limits(10, 5));
        assert_eq!(seqs(&evicted), [3]);
        assert_eq!(table.select(10, 1000, Instant::now()), [4, 5]);
    }

    #[test]
    fn removal_reports_only_queued_entries_and_membership_follows_it() {
        let mut table = Table::default();
        table.push(disk(1, 10));
        table.push(memory(2, 10));
        assert!(table.contains_all(&[1, 2]));
        assert_eq!(seqs(&table.remove(&[2, 9])), [2]);
        assert!(!table.contains_all(&[1, 2]));
        assert_eq!(table.memory(), Usage::default());
        assert_eq!(seqs(&table.take_all()), [1]);
        assert!(table.is_empty());
        assert_eq!(table.disk(), Usage::default());
    }
}
