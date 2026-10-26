//! Answers a call to a virtual tool: the agent gets an acknowledgement, and
//! PostHog gets the report as a `$mcp_missing_capability` or `$mcp_feedback`
//! event rather than a tool call.

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Map, Value};

use super::finish::set_intent_and_model;
use super::handle_delivery::deliver_handle;
use super::intent::{Intent, FROM_CONTEXT_ARGUMENT};
use super::prepare::PreparedCall;
use crate::mcp::analytics::Core;
use crate::mcp::bounds::{bounded_non_empty, bounded_text};
use crate::mcp::capture::OutgoingEvent;
use crate::mcp::contract::arguments::CONTEXT;
use crate::mcp::contract::events;
use crate::mcp::contract::limits::{MAX_NAME_LENGTH, MAX_TEXT_LENGTH};
use crate::mcp::contract::properties as key;
use crate::mcp::privacy::TextPass;
use crate::mcp::tools::virtual_tools::{
    VirtualTool, FEEDBACK_REPLY, FEEDBACK_TYPES, MISSING_CAPABILITY_REPLY, SENTIMENTS,
};

const OTHER_FEEDBACK: &str = "other";

/// Free-text report fields and the property each becomes.
const FEEDBACK_TEXT: [(&str, &str); 4] = [
    ("summary", key::FEEDBACK_SUMMARY),
    ("details", key::FEEDBACK_DETAILS),
    ("friction_points", key::FEEDBACK_FRICTION_POINTS),
    ("suggested_improvement", key::FEEDBACK_SUGGESTED_IMPROVEMENT),
];

pub(crate) fn answer_virtual_call(
    core: &Core,
    tool: VirtualTool,
    mut call: PreparedCall,
) -> CallToolResult {
    call.transport.call_ended();
    let arguments = call.sent_arguments.clone().unwrap_or_default();
    let stated = match tool {
        VirtualTool::MissingCapability => text_argument(&arguments, CONTEXT).map(str::to_string),
        VirtualTool::Feedback => feedback_intent(&arguments),
    };
    call.intent = stated.and_then(|text| Intent::new(&text, FROM_CONTEXT_ARGUMENT));

    let reply = match tool {
        VirtualTool::MissingCapability => MISSING_CAPABILITY_REPLY,
        VirtualTool::Feedback => FEEDBACK_REPLY,
    };
    let mut result = CallToolResult::success(vec![ContentBlock::text(reply)]);
    deliver_handle(&mut result, &call.conversation, false);
    core.emit(|| {
        let event = match tool {
            VirtualTool::MissingCapability => missing_capability_event(&call),
            VirtualTool::Feedback => feedback_event(&call, &arguments),
        };
        vec![event]
    });
    result
}

/// A report carries no `$mcp_parameters`: its arguments are free text that
/// only the free-text redaction may capture.
fn report_event(name: &'static str, call: &PreparedCall) -> OutgoingEvent {
    let mut event = OutgoingEvent::new(name, &call.context);
    let tool_name = bounded_text(&call.tool_name, MAX_NAME_LENGTH, TextPass::Name);
    event.set(key::RESOURCE_NAME, tool_name);
    set_intent_and_model(&mut event, call);
    event
}

fn missing_capability_event(call: &PreparedCall) -> OutgoingEvent {
    report_event(events::MISSING_CAPABILITY, call).bounded(&[key::RESOURCE_NAME])
}

/// An unknown type is `other`; a sentiment or completion of the wrong shape
/// is left out rather than guessed.
fn feedback_event(call: &PreparedCall, arguments: &Map<String, Value>) -> OutgoingEvent {
    let mut event = report_event(events::FEEDBACK, call);
    let feedback_type = text_argument(arguments, "feedback_type")
        .filter(|kind| FEEDBACK_TYPES.contains(kind))
        .unwrap_or(OTHER_FEEDBACK);
    event.set(key::FEEDBACK_TYPE, feedback_type);
    for (argument, property) in FEEDBACK_TEXT {
        let text = text_argument(arguments, argument)
            .and_then(|text| bounded_non_empty(text, MAX_TEXT_LENGTH, TextPass::FreeText));
        event.set_optional(property, text);
    }
    let tool = text_argument(arguments, "tool_name")
        .and_then(|text| bounded_non_empty(text, MAX_NAME_LENGTH, TextPass::FreeText));
    event.set_optional(key::FEEDBACK_TOOL, tool);
    let sentiment =
        text_argument(arguments, "sentiment").filter(|sentiment| SENTIMENTS.contains(sentiment));
    event.set_optional(key::FEEDBACK_SENTIMENT, sentiment);
    let completed = arguments.get("task_completed").and_then(Value::as_bool);
    event.set_optional(key::FEEDBACK_TASK_COMPLETED, completed);
    event.bounded(&[key::RESOURCE_NAME, key::FEEDBACK_TYPE])
}

/// The non-empty of `summary` and `details`, joined by a blank line.
fn feedback_intent(arguments: &Map<String, Value>) -> Option<String> {
    let parts: Vec<&str> = ["summary", "details"]
        .iter()
        .filter_map(|argument| text_argument(arguments, argument))
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// A string argument that is not blank.
fn text_argument<'a>(arguments: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    let text = arguments.get(name)?.as_str()?;
    (!text.trim().is_empty()).then_some(text)
}
