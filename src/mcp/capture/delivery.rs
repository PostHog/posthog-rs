use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use super::outgoing::OutgoingEvent;
use crate::client::BeforeSendHook;
use crate::{Client, Event};

/// Sends MCP events through the host's PostHog client, which batches and
/// delivers them in the background.
pub(crate) struct Delivery {
    client: Arc<Client>,
    before_send: Option<BeforeSendHook>,
}

impl Delivery {
    pub(crate) fn new(client: Arc<Client>, before_send: Option<BeforeSendHook>) -> Self {
        Self {
            client,
            before_send,
        }
    }

    /// The first event leads: when the hook drops it, the events describing
    /// it, such as its `$exception`, are dropped too.
    pub(crate) fn send(&self, events: Vec<OutgoingEvent>) {
        for (index, event) in events.into_iter().enumerate() {
            match self.apply_hook(event.into_posthog_event()) {
                Some(event) => self.client.capture(event),
                None if index == 0 => return,
                None => {}
            }
        }
    }

    fn apply_hook(&self, event: Event) -> Option<Event> {
        let Some(hook) = &self.before_send else {
            return Some(event);
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| hook.apply(event)));
        outcome.unwrap_or_else(|_| {
            tracing::warn!("MCP analytics before_send hook panicked; dropping the event");
            None
        })
    }
}
