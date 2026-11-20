//! The stateful delivery queue: one ordered entry per queued event, owned by
//! the coordinator thread. It is the only record of what is queued.
//!
//! With a store every entry is on disk and its payload is read back when it is
//! sent. Without one every entry holds its payload in memory.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::capture_event::CaptureEvent;

pub(super) struct Entry {
    pub(super) seq: u64,
    pub(super) bytes: u64,
    /// `Some` without a store, `None` for an event on disk.
    pub(super) payload: Option<Arc<CaptureEvent>>,
}

impl Entry {
    pub(super) fn on_disk(&self) -> bool {
        self.payload.is_none()
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
    bytes: u64,
}

impl Table {
    /// Append an entry. Sequence numbers only grow.
    pub(super) fn push(&mut self, entry: Entry) {
        debug_assert!(self
            .entries
            .back()
            .map_or(true, |last| last.seq < entry.seq));
        self.bytes += entry.bytes;
        self.entries.push_back(entry);
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn last_seq(&self) -> Option<u64> {
        self.entries.back().map(|entry| entry.seq)
    }

    pub(super) fn get(&self, seq: u64) -> Option<&Entry> {
        self.position(seq).map(|index| &self.entries[index])
    }

    pub(super) fn contains_all(&self, seqs: &[u64]) -> bool {
        seqs.iter().all(|seq| self.position(*seq).is_some())
    }

    /// Whether any entry at or below `through` is queued.
    pub(super) fn has_through(&self, through: u64) -> bool {
        self.entries
            .front()
            .is_some_and(|entry| entry.seq <= through)
    }

    /// Remove the given entries, returning the ones that were queued.
    pub(super) fn remove(&mut self, seqs: &[u64]) -> Vec<Entry> {
        let mut removed = Vec::new();
        for seq in seqs {
            if let Some(index) = self.position(*seq) {
                let entry = self.entries.remove(index).expect("position is in range");
                self.bytes -= entry.bytes;
                removed.push(entry);
            }
        }
        removed
    }

    /// Remove the oldest entries while over `limits`. The newest entry is kept
    /// even when it alone is over the byte limit.
    pub(super) fn evict(&mut self, limits: Limits) -> Vec<Entry> {
        let mut evicted = Vec::new();
        while self.entries.len() > 1
            && (self.entries.len() > limits.events || self.bytes > limits.bytes)
        {
            let entry = self.entries.pop_front().expect("more than one entry");
            self.bytes -= entry.bytes;
            evicted.push(entry);
        }
        evicted
    }

    /// The oldest entries, in order: at most `max_events`, and within
    /// `target_bytes` except that the first entry is always included.
    pub(super) fn select(&self, max_events: usize, target_bytes: u64) -> Vec<u64> {
        let mut selected = Vec::new();
        let mut bytes = 0u64;
        for entry in &self.entries {
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

    /// Remove every entry, for shutdown.
    pub(super) fn take_all(&mut self) -> Vec<Entry> {
        self.bytes = 0;
        self.entries.drain(..).collect()
    }

    fn position(&self, seq: u64) -> Option<usize> {
        self.entries
            .binary_search_by_key(&seq, |entry| entry.seq)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: u64, bytes: u64) -> Entry {
        Entry {
            seq,
            bytes,
            payload: None,
        }
    }

    fn seqs(entries: &[Entry]) -> Vec<u64> {
        entries.iter().map(|entry| entry.seq).collect()
    }

    fn limits(events: usize, bytes: u64) -> Limits {
        Limits { events, bytes }
    }

    #[test]
    fn selection_takes_the_oldest_entries_within_count_and_byte_targets() {
        let mut table = Table::default();
        for seq in 1..=5 {
            table.push(entry(seq, 10));
        }
        assert_eq!(table.select(2, 1000), [1, 2]);
        assert_eq!(table.select(10, 25), [1, 2]);
        // An entry larger than the target still goes, alone.
        assert_eq!(table.select(10, 5), [1]);
    }

    #[test]
    fn has_through_looks_only_at_entries_at_or_below_the_position() {
        let mut table = Table::default();
        table.push(entry(5, 10));
        assert!(!table.has_through(4));
        assert!(table.has_through(5));
    }

    #[test]
    fn eviction_drops_the_oldest_and_keeps_the_newest() {
        let mut table = Table::default();
        for seq in 1..=4 {
            table.push(entry(seq, 10));
        }
        assert_eq!(seqs(&table.evict(limits(2, 1000))), [1, 2]);
        assert_eq!(table.select(10, 1000), [3, 4]);
        // A single entry over the byte limit stays.
        assert_eq!(seqs(&table.evict(limits(10, 5))), [3]);
        assert_eq!(table.select(10, 1000), [4]);
    }

    #[test]
    fn removal_reports_only_queued_entries_and_membership_follows_it() {
        let mut table = Table::default();
        table.push(entry(1, 10));
        table.push(entry(2, 10));
        assert!(table.contains_all(&[1, 2]));
        assert_eq!(seqs(&table.remove(&[2, 9])), [2]);
        assert!(!table.contains_all(&[1, 2]));
        assert_eq!(seqs(&table.take_all()), [1]);
        assert!(table.is_empty());
    }
}
