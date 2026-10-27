//! Answers a call to the `get_more_tools` virtual tool: the agent gets an
//! acknowledgement, and PostHog gets the report as a
//! `$mcp_missing_capability` event rather than a tool call.

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::Value;

use super::finish::set_intent_and_model;
use super::handle_delivery::deliver_handle;
use super::intent::{Intent, FROM_CONTEXT_ARGUMENT};
use super::prepare::PreparedCall;
use crate::mcp::analytics::Core;
use crate::mcp::bounds::bounded_text;
use crate::mcp::capture::OutgoingEvent;
use crate::mcp::contract::arguments::CONTEXT;
use crate::mcp::contract::events;
use crate::mcp::contract::limits::MAX_NAME_LENGTH;
use crate::mcp::contract::properties as key;
use crate::mcp::privacy::TextPass;
use crate::mcp::tools::virtual_tools::MISSING_CAPABILITY_REPLY;

pub(crate) fn answer_virtual_call(core: &Core, mut call: PreparedCall) -> CallToolResult {
    call.transport.call_ended();
    let stated_need = call
        .sent_arguments
        .as_ref()
        .and_then(|arguments| arguments.get(CONTEXT))
        .and_then(Value::as_str);
    call.intent = stated_need.and_then(|text| Intent::new(text, FROM_CONTEXT_ARGUMENT));

    let reply = ContentBlock::text(MISSING_CAPABILITY_REPLY);
    let mut result = CallToolResult::success(vec![reply]);
    deliver_handle(&mut result, &call.conversation, false);
    core.emit(|| vec![missing_capability_event(&call)]);
    result
}

/// The report carries no `$mcp_parameters`: its argument is free text that
/// only the free-text redaction may capture.
fn missing_capability_event(call: &PreparedCall) -> OutgoingEvent {
    let mut event = OutgoingEvent::new(events::MISSING_CAPABILITY, &call.context);
    let tool_name = bounded_text(&call.tool_name, MAX_NAME_LENGTH, TextPass::Name);
    event.set(key::RESOURCE_NAME, tool_name);
    set_intent_and_model(&mut event, call);
    event.bounded(&[key::RESOURCE_NAME])
}
