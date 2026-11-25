use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use tracing::error;

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
                Some(event) => self.client.capture_finalized(event),
                None if index == 0 => return,
                None => {}
            }
        }
    }

    fn apply_hook(&self, event: Event) -> Option<Event> {
        let Some(hook) = &self.before_send else {
            return Some(event);
        };
        // The hook takes the event, so keep its name for the panic log line.
        let name = event.event_name().to_string();
        let outcome = catch_unwind(AssertUnwindSafe(|| hook.apply(event)));
        outcome.unwrap_or_else(|_| {
            // Never log the panic payload: it can hold the values the hook was redacting.
            error!("panic in PostHog before_send hook for event {name:?}; dropping event");
            None
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tests::with_logs;
    use crate::ClientOptionsBuilder;

    #[tokio::test]
    async fn a_panicking_hook_drops_the_event_and_logs_only_its_name() {
        let client = crate::client(ClientOptionsBuilder::default().build().unwrap()).await;
        let hook = BeforeSendHook::new(|_| panic!("secret-payload"));
        let delivery = Delivery::new(Arc::new(client), Some(hook));

        let (kept, logs) =
            with_logs(|| delivery.apply_hook(Event::new("$mcp_tool_call", "user-1")));

        assert!(kept.is_none());
        assert!(
            logs.contains(
                r#"ERROR posthog::mcp::capture::delivery: panic in PostHog before_send hook for event "$mcp_tool_call"; dropping event"#
            ),
            "{}",
            logs
        );
        assert!(!logs.contains("secret-payload"), "{}", logs);
    }

    #[tokio::test]
    async fn mcp_events_are_never_held_by_the_scope_they_are_sent_in() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let options = ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .host("http://127.0.0.1:9".to_string())
            .before_send(move |event: Event| {
                recorded
                    .lock()
                    .unwrap()
                    .push(event.event_name().to_string());
                Some(event)
            })
            .build()
            .unwrap();
        let client = Arc::new(crate::client(options).await);
        let delivery = Delivery::new(client.clone(), None);
        let holding = crate::RequestContext {
            held: Some(crate::request_context::HeldEvents::new()),
            ..crate::RequestContext::new()
        };

        let context = crate::mcp::capture::EventContext {
            timestamp: chrono::Utc::now(),
            session: crate::mcp::session::Session::generated(),
            conversation_id: None,
            identity: None,
            client: Default::default(),
            server: Default::default(),
            server_build: None,
            host_properties: Default::default(),
            options: Default::default(),
        };
        let event = OutgoingEvent::new("$mcp_tool_call", &context);
        holding.scope(async { delivery.send(vec![event]) }).await;
        client.flush().await;

        assert_eq!(*seen.lock().unwrap(), vec!["$mcp_tool_call".to_string()]);
    }
}
