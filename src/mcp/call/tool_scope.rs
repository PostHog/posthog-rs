//! The request context a tool's own events take while its call runs: the
//! call's session and identity, over the host's context.

use serde_json::Value;

use crate::mcp::capture::EventContext;
use crate::mcp::contract::properties as key;
use crate::mcp::session::Session;
use crate::request_context::HeldEvents;
use crate::RequestContext;

pub(crate) struct ToolScope {
    pub(crate) context: RequestContext,
}

impl ToolScope {
    /// While the call's session waits on a new handle reaching the agent,
    /// its tool's events wait too, and take the session only once it is known.
    pub(crate) fn new(host: Option<RequestContext>, call: &EventContext, pending: bool) -> Self {
        let mut context = host.unwrap_or_default();
        if let Some(identity) = &call.identity {
            context.distinct_id = Some(identity.distinct_id.clone());
            if !identity.groups.is_empty() {
                let groups = serde_json::to_value(&identity.groups).unwrap_or_default();
                context.properties.insert(key::GROUPS.to_string(), groups);
            }
        }
        if pending {
            context.held = Some(HeldEvents::new());
            context.properties.remove(key::SESSION_ID);
        } else {
            set_session(&mut context, &call.session);
        }
        Self { context }
    }

    /// Sends the events that waited, in the call's final session.
    pub(crate) fn settle(&self, session: &Session) {
        if let Some(held) = &self.context.held {
            let mut context = self.context.clone();
            set_session(&mut context, session);
            held.release(&context);
        }
    }
}

/// A call dropped before it ended never delivered its handle.
impl Drop for ToolScope {
    fn drop(&mut self) {
        if self.context.held.is_some() {
            self.settle(&Session::generated());
        }
    }
}

fn set_session(context: &mut RequestContext, session: &Session) {
    let properties = &mut context.properties;
    properties.insert(
        key::SESSION_ID.to_string(),
        Value::from(session.id.as_str()),
    );
    properties.insert(
        key::SESSION_SOURCE.to_string(),
        Value::from(session.source.as_str()),
    );
}
