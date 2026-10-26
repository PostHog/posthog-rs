//! Answers a call to a virtual tool: the agent gets an acknowledgement, and
//! PostHog gets the report as a `$mcp_missing_capability` or `$mcp_feedback`
//! event rather than a tool call.

use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::Value;

use super::finish::set_intent_and_model;
use super::handle_delivery::deliver_handle;
use super::intent::{Intent, FROM_CONTEXT_ARGUMENT};
use super::prepare::PreparedCall;
use crate::mcp::analytics::Core;
use crate::mcp::bounds::{bounded_non_empty, bounded_payload, bounded_text, truncate};
use crate::mcp::capture::OutgoingEvent;
use crate::mcp::contract::arguments::CONTEXT;
use crate::mcp::contract::events;
use crate::mcp::contract::limits::{MAX_NAME_LENGTH, MAX_TEXT_LENGTH};
use crate::mcp::contract::properties as key;
use crate::mcp::contract::REDACTED;
use crate::mcp::feedback::FeedbackReport;
use crate::mcp::privacy::{is_credential_key, TextPass};
use crate::mcp::tools::virtual_tools::{VirtualTool, FEEDBACK_REPLY, MISSING_CAPABILITY_REPLY};

const FEEDBACK_PROPERTY_PREFIX: &str = "$mcp_feedback_";

pub(crate) async fn answer_virtual_call(
    core: &Core,
    tool: VirtualTool,
    mut call: PreparedCall,
) -> CallToolResult {
    call.transport.call_ended();
    let arguments = call.sent_arguments.take().unwrap_or_default();
    let reply = match tool {
        VirtualTool::MissingCapability => {
            let stated = arguments.get(CONTEXT).and_then(Value::as_str);
            call.intent = stated.and_then(|text| Intent::new(text, FROM_CONTEXT_ARGUMENT));
            core.emit(|| vec![missing_capability_event(&call)]);
            MISSING_CAPABILITY_REPLY.to_string()
        }
        VirtualTool::Feedback => {
            let report = FeedbackReport::parse(&arguments, &core.options.feedback_fields);
            let stated = report.intent();
            call.intent = stated.and_then(|text| Intent::new(&text, FROM_CONTEXT_ARGUMENT));
            core.emit(|| vec![feedback_event(&call, &report)]);
            feedback_reply(core, report).await
        }
    };
    let mut result = CallToolResult::success(vec![ContentBlock::text(reply)]);
    deliver_handle(&mut result, &call.conversation, false);
    result
}

/// The handler's reply, or the default when there is none, it is blank, or
/// the handler panicked. The report's text never reaches a log.
async fn feedback_reply(core: &Core, report: FeedbackReport) -> String {
    let Some(handle) = &core.options.on_feedback else {
        return FEEDBACK_REPLY.to_string();
    };
    let reply = AssertUnwindSafe(async { handle(report).await })
        .catch_unwind()
        .await;
    match reply {
        Ok(Some(reply)) if !reply.trim().is_empty() => reply,
        Ok(_) => FEEDBACK_REPLY.to_string(),
        Err(_) => {
            tracing::warn!("MCP analytics on_feedback handler panicked; sent the default reply");
            FEEDBACK_REPLY.to_string()
        }
    }
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

fn feedback_event(call: &PreparedCall, report: &FeedbackReport) -> OutgoingEvent {
    let mut event = report_event(events::FEEDBACK, call);
    event.set(key::FEEDBACK_TYPE, report.feedback_type.as_str());
    let texts = [
        (key::FEEDBACK_SUMMARY, Some(report.summary.as_str())),
        (key::FEEDBACK_DETAILS, report.details.as_deref()),
        (
            key::FEEDBACK_FRICTION_POINTS,
            report.friction_points.as_deref(),
        ),
        (
            key::FEEDBACK_SUGGESTED_IMPROVEMENT,
            report.suggested_improvement.as_deref(),
        ),
    ];
    for (property, text) in texts {
        let text = text.and_then(|text| free_text(text, MAX_TEXT_LENGTH));
        event.set_optional(property, text);
    }
    let tool = report
        .tool_name
        .as_deref()
        .and_then(|text| free_text(text, MAX_NAME_LENGTH));
    event.set_optional(key::FEEDBACK_TOOL, tool);
    event.set_optional(key::FEEDBACK_SENTIMENT, report.sentiment.as_deref());
    event.set_optional(key::FEEDBACK_TASK_COMPLETED, report.task_completed);
    for (name, value) in &report.extras {
        let property = format!("{FEEDBACK_PROPERTY_PREFIX}{name}");
        event.set_optional(&property, captured_extra(name, value));
    }
    event.bounded(&[key::RESOURCE_NAME, key::FEEDBACK_TYPE])
}

fn free_text(text: &str, limit: usize) -> Option<String> {
    bounded_non_empty(text, limit, TextPass::FreeText)
}

/// A string as free text, a number or boolean as is, and any other JSON as
/// text within the same limit, redacted leaf by leaf before it is written. A
/// field named like a credential is redacted whole, as any such key is.
fn captured_extra(name: &str, value: &Value) -> Option<Value> {
    if is_credential_key(name) {
        return Some(Value::from(REDACTED));
    }
    match value {
        Value::String(text) => free_text(text, MAX_TEXT_LENGTH).map(Value::from),
        Value::Number(_) | Value::Bool(_) => Some(value.clone()),
        Value::Null | Value::Array(_) | Value::Object(_) => {
            let redacted = bounded_payload(value, TextPass::FreeText).to_string();
            Some(Value::from(truncate(&redacted, MAX_TEXT_LENGTH)))
        }
    }
}
