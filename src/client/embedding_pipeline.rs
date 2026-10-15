//! Durable dispositions layered onto the existing analytics retry pipeline.
use super::*;
use crate::capture_event::EventStatus;
use crate::client::retry::{backoff_duration, is_retryable_status, Step};

impl Pipeline {
    pub(super) fn purge(&mut self) -> bool {
        dec_len(
            &self.len,
            self.retries.iter().map(|batch| batch.pending.len()).sum(),
        );
        self.retries.clear();
        if let (Some(config), Some(gate)) = (&self.options.embedding, &self.gate) {
            let generation = gate.generation.load(Ordering::Acquire);
            match config.store.purge() {
                Ok(()) => {
                    let _admission = gate.admission.lock().unwrap_or_else(|e| e.into_inner());
                    if generation == gate.generation.load(Ordering::Acquire) {
                        gate.purge_required.store(false, Ordering::Release);
                        true
                    } else {
                        false
                    }
                }
                Err(_) => {
                    gate.persistence_error();
                    false
                }
            }
        } else {
            true
        }
    }

    /// The normal v1 pipeline consumes final-attempt retry verdicts. Durable
    /// delivery ends that attempt sequence without retiring those records:
    /// restore their payloads in the same retry batch and start the next sequence
    /// on a later flush or timer. They continue to occupy queue capacity.
    pub(super) fn durable_disposition(
        &self,
        batch: &mut RetryBatch,
        step: &mut Step,
        status: Option<u16>,
        retry_after: Option<Duration>,
        body_transport_error: bool,
        sent: Vec<CaptureEvent>,
    ) {
        let (Some(config), Some(gate)) = (&self.options.embedding, &self.gate) else {
            return;
        };
        let exhausted = batch.attempt >= self.options.max_capture_attempts;
        let mut restart_sequence = false;
        if exhausted {
            for event in &sent {
                if batch
                    .final_results
                    .get(&event.uuid)
                    .is_some_and(|r| r.result == EventStatus::Retry)
                {
                    batch.pending.push(event.clone());
                    batch.final_results.remove(&event.uuid);
                }
            }
            let retryable = matches!(step, Step::Fail(_))
                && (body_transport_error || status.map_or(true, is_retryable_status));
            if retryable || matches!(step, Step::Done) && !batch.pending.is_empty() {
                let delay = backoff_duration(&self.options, batch.attempt, retry_after)
                    .max(Duration::from_millis(self.options.flush_interval_ms.max(1)));
                *step = Step::Backoff(delay);
                restart_sequence = true;
            }
        }

        let remaining: std::collections::HashSet<_> =
            batch.pending.iter().map(|event| event.uuid).collect();
        let mut terminal = Vec::new();
        for event in sent {
            if !remaining.contains(&event.uuid) || matches!(step, Step::Fail(_)) {
                let delivered = batch.final_results.get(&event.uuid).is_some_and(|result| {
                    matches!(result.result, EventStatus::Ok | EventStatus::Warning)
                });
                if delivered {
                    gate.delivered.fetch_add(1, Ordering::Relaxed);
                } else {
                    gate.dropped.fetch_add(1, Ordering::Relaxed);
                }
                terminal.push(event.uuid);
            }
        }
        if !terminal.is_empty() && config.store.remove(&terminal).is_err() {
            gate.persistence_error();
        }
        if restart_sequence {
            // The existing Backoff arm increments this to the first attempt.
            batch.attempt = 0;
            batch.request_id = Uuid::now_v7();
            batch.final_results.clear();
        }
    }
}
