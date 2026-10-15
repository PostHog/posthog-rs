//! Host delivery controls; all state and operations live in the analytics transport.
use super::Client;
use crate::Event;

impl Client {
    /// Submit a fully prepared event to the existing bounded transport.
    pub fn capture_prepared(&self, event: Event) -> crate::embedding::EnqueueResult {
        self.transport
            .as_ref()
            .map_or(crate::embedding::EnqueueResult::Disabled, |t| {
                t.enqueue_checked(event)
            })
    }

    /// Stop admission immediately and schedule durable queue purge.
    pub fn pause_and_purge(&self) {
        if let Some(transport) = &self.transport {
            transport.pause_and_purge();
        }
    }

    /// Wait for purge and resume delivery. Call off a host UI/game thread.
    pub fn resume_delivery(&self, generation: u64) -> bool {
        self.transport
            .as_ref()
            .is_some_and(|t| t.resume(generation))
    }

    /// Capture the consent epoch before scheduling an asynchronous opt-in.
    pub fn delivery_generation(&self) -> u64 {
        self.transport.as_ref().map_or(0, |t| t.generation())
    }

    /// Stop admission and establish the shutdown deadline before waiting for
    /// other host calls. Finish with shutdown to join all workers.
    pub fn begin_shutdown(&self) {
        if let Some(transport) = &self.transport {
            transport.request_close();
        }
    }

    /// Wait for preceding worker commands, including durable writes.
    pub fn checkpoint_delivery(&self) -> crate::embedding::DeliveryReport {
        self.transport
            .as_ref()
            .map_or_else(Default::default, |t| t.checkpoint())
    }
}
