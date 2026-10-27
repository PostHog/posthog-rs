//! Events for requests that are not tool calls: the handshake and the tool
//! listing.

use serde_json::Value;

use super::scope::RequestScope;
use crate::mcp::analytics::Core;
use crate::mcp::bounds::{bounded_payload, bounded_text};
use crate::mcp::capture::{EventContext, OutgoingEvent};
use crate::mcp::contract::events;
use crate::mcp::contract::limits::MAX_NAME_LENGTH;
use crate::mcp::contract::properties as key;
use crate::mcp::privacy::TextPass;

/// `$mcp_tools_list` describes the server's catalogue: a listing carries no
/// conversation handle, so it groups only by the transport's session.
pub(crate) fn emit_tools_listed(
    core: &Core,
    scope: &RequestScope<'_>,
    names: Vec<String>,
    envelope: Option<Value>,
) {
    let context = request_context(core, scope);
    core.emit(|| {
        let mut event = OutgoingEvent::new(events::TOOLS_LIST, &context);
        let names: Vec<String> = names
            .iter()
            .map(|name| bounded_text(name, MAX_NAME_LENGTH, TextPass::Name))
            .collect();
        event.set(key::LISTED_TOOL_NAMES, names);
        let envelope = envelope.filter(|envelope| {
            envelope
                .as_object()
                .is_some_and(|fields| !fields.is_empty())
        });
        event.set_optional(
            key::RESPONSE,
            envelope.map(|envelope| bounded_payload(&envelope, TextPass::Captured)),
        );
        vec![event.bounded(&[key::LISTED_TOOL_NAMES])]
    });
}

pub(crate) fn emit_initialized(core: &Core, scope: &RequestScope<'_>) {
    let context = request_context(core, scope);
    core.emit(|| vec![OutgoingEvent::new(events::INITIALIZE, &context).bounded(&[])]);
}

fn request_context(core: &Core, scope: &RequestScope<'_>) -> EventContext {
    let info = scope.request_info(None, None);
    core.event_context(scope, scope.transport.resolve(), None, &info)
}
