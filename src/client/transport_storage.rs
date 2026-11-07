//! Persistence policy composed into the shared transport worker. No separate
//! queue or retry loop: recovered and newly persisted events use the same
//! batching, response classification, and scheduled retries as memory events.

use std::collections::HashSet;
use std::io;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::{dec_len, reserve_slot, DeliveryEvent, Pipeline, QueuedEvent};
use crate::client::common::report_storage_error;
use crate::client::event_store::StoreKey;
use crate::client::StorageOperation;

impl Pipeline {
    pub(super) fn report_storage_error(&self, operation: StorageOperation, error: &io::Error) {
        report_storage_error(&self.options.on_error, operation, error);
    }

    pub(super) fn persist(&mut self, event: &mut DeliveryEvent) {
        if event.store_key.is_some() {
            return;
        }
        if let Some(store) = &mut self.storage.store {
            match store.put(&event.wire) {
                Ok(key) => event.store_key = Some(key),
                Err(error) => {
                    // Keep delivering this already accepted event from memory.
                    self.report_storage_error(StorageOperation::Write, &error);
                }
            }
        }
    }

    pub(super) fn stored_keys(&self, events: &[DeliveryEvent]) -> Vec<StoreKey> {
        if self.storage.store.is_none() {
            return Vec::new();
        }
        events.iter().filter_map(|event| event.store_key).collect()
    }

    pub(super) fn forget_keys(&mut self, keys: &[StoreKey]) {
        for &key in keys {
            if let Some(store) = &mut self.storage.store {
                if let Err(error) = store.remove(key) {
                    // Do not resend in this process solely because deletion
                    // failed. The on-disk record may be delivered after restart.
                    self.report_storage_error(StorageOperation::Remove, &error);
                }
            }
        }
    }

    pub(super) fn forget_finished(&mut self, before: &[StoreKey], pending: &[DeliveryEvent]) {
        if before.is_empty() {
            return;
        }
        let retained: HashSet<_> = pending.iter().filter_map(|event| event.store_key).collect();
        let terminal: Vec<_> = before
            .iter()
            .copied()
            .filter(|key| !retained.contains(key))
            .collect();
        self.forget_keys(&terminal);
    }

    pub(super) fn next_delivery_work(&self) -> Option<Instant> {
        let retry = self.earliest_retry();
        if self.storage.recovery_pending
            && self.len.load(Ordering::Acquire) < self.lane.max_queue_size(&self.options)
            && self
                .storage
                .byte_budget
                .as_ref()
                .is_some_and(|budget| budget.available() > 0)
        {
            let recovery = self
                .storage
                .next_recovery
                .unwrap_or_else(|| self.clock.now());
            Some(retry.map_or(recovery, |retry| retry.min(recovery)))
        } else {
            retry
        }
    }

    /// Restore bounded pages directly into the shared send/retry pipeline.
    /// Normal timer servicing reads one page; explicit flush/shutdown may drain
    /// more until recovery ends, memory capacity fills, or its deadline expires.
    pub(super) fn recover_due(&mut self, deadline: Option<Instant>, force: bool) {
        loop {
            if !self.storage.recovery_pending
                || deadline.is_some_and(|deadline| self.clock.now() >= deadline)
                || (!force
                    && self
                        .storage
                        .next_recovery
                        .is_some_and(|at| self.clock.now() < at))
            {
                return;
            }
            let max_queue = self.lane.max_queue_size(&self.options);
            let mut reserved = 0;
            // Reserve before I/O so producers cannot consume recovery's space
            // while the worker is blocked on the backend.
            while reserved < self.options.max_batch_size.max(1)
                && reserve_slot(&self.len, max_queue).is_some()
            {
                reserved += 1;
            }
            if reserved == 0 {
                return;
            }
            let Some(mut memory) = self
                .storage
                .byte_budget
                .as_ref()
                .and_then(|budget| budget.reserve_available())
            else {
                dec_len(&self.len, reserved);
                return;
            };
            let max_bytes = memory.bytes() as u64;
            let Some(store) = &mut self.storage.store else {
                dec_len(&self.len, reserved);
                self.storage.recovery_pending = false;
                return;
            };
            let recovered = store.recover(reserved, max_bytes);
            self.storage.next_recovery = Some(
                self.clock.now() + Duration::from_millis(self.options.flush_interval_ms.max(1)),
            );
            let recovered = match recovered {
                Ok(recovered) => recovered,
                Err(error) => {
                    dec_len(&self.len, reserved);
                    self.report_storage_error(StorageOperation::Read, &error);
                    return;
                }
            };
            self.storage.recovery_pending = recovered.more;
            for error in recovered.errors {
                self.report_storage_error(StorageOperation::Read, &error);
            }
            // Internal backends promise a bounded page. Reject a broken backend
            // response rather than corrupting the queue's reservation count.
            let recovered_bytes = recovered
                .events
                .iter()
                .try_fold(0u64, |bytes, event| bytes.checked_add(event.bytes));
            if recovered.events.len() > reserved
                || recovered_bytes.map_or(true, |bytes| bytes > max_bytes)
            {
                dec_len(&self.len, reserved);
                self.report_storage_error(
                    StorageOperation::Read,
                    &io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Event store exceeded recovery bounds",
                    ),
                );
                return;
            }
            dec_len(&self.len, reserved - recovered.events.len());
            if recovered.events.is_empty() {
                return;
            }
            let events = recovered
                .events
                .into_iter()
                .map(|record| {
                    let mut event = DeliveryEvent::from(record.event);
                    event.store_key = Some(record.key);
                    event.reservation = Some(
                        memory
                            .take(record.bytes as usize)
                            .expect("validated recovery size"),
                    );
                    QueuedEvent::Prepared(event)
                })
                .collect();
            drop(memory); // Return unused window capacity before sending.
            self.send_queued_batch(events, false, deadline);
            if !force {
                return;
            }
        }
    }
}
