//! The stateful delivery inbox: where admitted events wait for the coordinator.
//!
//! Admission runs on the caller's thread, possibly under SDK state locks, so it
//! only takes this short lock: no I/O, hooks or logging. It assigns each event
//! its queue position (`seq`), and when more than the count or byte limit is
//! waiting it drops the oldest waiting events, always keeping the newest, as
//! the disk queue does. The coordinator reports those drops. Once the
//! coordinator takes the last waiting events at shutdown, the inbox is closed
//! and refuses every later admission.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::Admission;
use crate::capture_event::CaptureEvent;

pub(super) struct Waiting {
    pub(super) seq: u64,
    pub(super) event: CaptureEvent,
    pub(super) bytes: usize,
}

struct State {
    last_seq: u64,
    waiting: VecDeque<Waiting>,
    bytes: usize,
    /// Set by the final drain: nothing will take a later event.
    closed: bool,
}

pub(super) struct Inbox {
    state: Mutex<State>,
    max_events: usize,
    max_bytes: usize,
    max_event_bytes: usize,
    /// Events dropped to make room since the coordinator last looked.
    dropped: AtomicUsize,
    /// Set when a wake message is already on its way to the coordinator.
    wake: AtomicBool,
    /// Events admitted and not yet delivered, dropped or written to disk.
    pending: Arc<AtomicUsize>,
}

impl Inbox {
    /// `last_seq` is the highest key already on disk, so new events follow it.
    pub(super) fn new(
        last_seq: u64,
        max_events: usize,
        max_bytes: usize,
        max_event_bytes: usize,
        pending: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            state: Mutex::new(State {
                last_seq,
                waiting: VecDeque::new(),
                bytes: 0,
                closed: false,
            }),
            max_events: max_events.max(1),
            max_bytes,
            max_event_bytes: max_event_bytes.min(max_bytes),
            dropped: AtomicUsize::new(0),
            wake: AtomicBool::new(false),
            pending,
        }
    }

    /// Admit an event whose serialized record size the caller measured.
    pub(super) fn admit(&self, event: CaptureEvent, bytes: usize) -> Admission {
        if bytes > self.max_event_bytes {
            return Admission::Full;
        }
        let mut state = self.lock();
        if state.closed {
            return Admission::Closed;
        }
        let Some(seq) = state.last_seq.checked_add(1) else {
            return Admission::Full;
        };
        state.last_seq = seq;
        // Counted before the coordinator can see the event, which it may then
        // deliver and count out at once.
        self.pending.fetch_add(1, Ordering::AcqRel);
        state.waiting.push_back(Waiting { seq, event, bytes });
        state.bytes += bytes;
        let mut dropped = 0;
        while state.waiting.len() > 1
            && (state.waiting.len() > self.max_events || state.bytes > self.max_bytes)
        {
            let oldest = state.waiting.pop_front().expect("more than one waiting");
            state.bytes -= oldest.bytes;
            dropped += 1;
        }
        if dropped > 0 {
            super::dec_len(&self.pending, dropped);
            self.dropped.fetch_add(dropped, Ordering::AcqRel);
        }
        Admission::Accepted
    }

    /// The position of the newest admitted event. A flush requested now covers
    /// every event at or below it.
    pub(super) fn last_seq(&self) -> u64 {
        self.lock().last_seq
    }

    /// Take every waiting event, oldest first.
    pub(super) fn drain(&self) -> Vec<Waiting> {
        let mut state = self.lock();
        state.bytes = 0;
        state.waiting.drain(..).collect()
    }

    /// Close the inbox and take every waiting event, oldest first. Later
    /// admissions are refused, so no event is accepted after the final drain.
    pub(super) fn close_and_drain(&self) -> Vec<Waiting> {
        let mut state = self.lock();
        state.closed = true;
        state.bytes = 0;
        state.waiting.drain(..).collect()
    }

    pub(super) fn take_dropped(&self) -> usize {
        self.dropped.swap(0, Ordering::AcqRel)
    }

    /// Whether the caller should send a wake message: true for the first
    /// admission since the coordinator last cleared the flag.
    pub(super) fn arm(&self) -> bool {
        !self.wake.swap(true, Ordering::AcqRel)
    }

    pub(super) fn clear_wake(&self) {
        self.wake.store(false, Ordering::Release);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Event;

    fn wire(name: &str) -> CaptureEvent {
        CaptureEvent::from_event(&Event::new(name, "user"))
    }

    fn inbox(max_events: usize, max_bytes: usize, max_event_bytes: usize) -> Inbox {
        Inbox::new(
            41,
            max_events,
            max_bytes,
            max_event_bytes,
            Arc::new(AtomicUsize::new(0)),
        )
    }

    fn names(waiting: &[Waiting]) -> Vec<(u64, &str)> {
        waiting
            .iter()
            .map(|item| (item.seq, item.event.event.as_str()))
            .collect()
    }

    #[test]
    fn admission_follows_the_last_key_on_disk_and_drains_in_order() {
        let inbox = inbox(10, 1000, 100);
        assert_eq!(inbox.last_seq(), 41);
        assert_eq!(inbox.admit(wire("a"), 10), Admission::Accepted);
        assert_eq!(inbox.admit(wire("b"), 10), Admission::Accepted);
        assert_eq!(inbox.last_seq(), 43);
        assert_eq!(inbox.pending.load(Ordering::Acquire), 2);
        assert_eq!(names(&inbox.drain()), [(42, "a"), (43, "b")]);
        assert!(inbox.drain().is_empty());
        // Draining moves events to the coordinator; they are still pending.
        assert_eq!(inbox.pending.load(Ordering::Acquire), 2);
    }

    #[test]
    fn a_full_inbox_drops_its_oldest_events_and_keeps_the_newest() {
        let inbox = inbox(2, 25, 25);
        for name in ["a", "b", "c"] {
            assert_eq!(inbox.admit(wire(name), 10), Admission::Accepted);
        }
        assert_eq!(inbox.take_dropped(), 1);
        // Over the byte limit, the newest event is kept even when it alone fills it.
        assert_eq!(inbox.admit(wire("d"), 25), Admission::Accepted);
        assert_eq!(inbox.take_dropped(), 2);
        assert_eq!(inbox.take_dropped(), 0);
        assert_eq!(names(&inbox.drain()), [(45, "d")]);
        assert_eq!(inbox.pending.load(Ordering::Acquire), 1);
    }

    #[test]
    fn an_event_over_the_per_event_or_byte_limit_is_refused() {
        let inbox = inbox(10, 50, 100);
        assert_eq!(inbox.admit(wire("too-big"), 51), Admission::Full);
        assert_eq!(inbox.admit(wire("fits"), 50), Admission::Accepted);
        let inbox = self::inbox(10, 1000, 100);
        assert_eq!(inbox.admit(wire("too-big"), 101), Admission::Full);
        assert_eq!(inbox.last_seq(), 41, "a refused event takes no position");
        assert_eq!(inbox.pending.load(Ordering::Acquire), 0);
    }

    #[test]
    fn the_final_drain_closes_the_inbox_to_later_admissions() {
        let inbox = inbox(10, 1000, 100);
        assert_eq!(inbox.admit(wire("last"), 10), Admission::Accepted);
        assert_eq!(names(&inbox.close_and_drain()), [(42, "last")]);
        assert_eq!(inbox.admit(wire("late"), 10), Admission::Closed);
        assert_eq!(inbox.last_seq(), 42, "a refused event takes no position");
        assert_eq!(inbox.pending.load(Ordering::Acquire), 1);
        assert!(inbox.drain().is_empty());
    }

    #[test]
    fn only_the_first_admission_after_a_wake_asks_for_another() {
        let inbox = inbox(10, 1000, 100);
        assert!(inbox.arm());
        assert!(!inbox.arm());
        inbox.clear_wake();
        assert!(inbox.arm());
    }
}
