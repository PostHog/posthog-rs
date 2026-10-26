//! What a `tools/call` sends once it ends, and the result the agent receives.

use std::convert::TryFrom;

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::Value;

use super::handle_delivery::deliver_handle;
use super::prepare::{PreparedCall, ToolExistence};
use crate::mcp::analytics::Core;
use crate::mcp::bounds::{bounded_non_empty, bounded_payload, bounded_text, truncate};
use crate::mcp::capture::{captured_response, Failure, OutgoingEvent};
use crate::mcp::contract::limits::{MAX_INPUT_REQUEST_METHODS, MAX_NAME_LENGTH, MAX_TEXT_LENGTH};
use crate::mcp::contract::properties as key;
use crate::mcp::contract::{events, ANALYTICS_SOURCE, REDACTED};
use crate::mcp::privacy::TextPass;
use crate::mcp::session::new_session_id;

const VALIDATION_ERROR_TYPE: &str = "validation";
const RMCP_ARGUMENT_ERROR_PREFIX: &str = "failed to deserialize parameters:";

const TOOL_CALL_REQUIRED: [&str; 4] =
    [key::TOOL_NAME, key::IS_ERROR, key::DURATION_MS, key::SOURCE];

/// How a call ended, when it ended without a result to deliver.
pub(crate) enum CallEnd {
    /// A round asking the client for more input; the retry completes the call.
    InputRequired { methods: Vec<String> },
    /// A task handle, which is never a successful call.
    Task,
    /// An error the handler raised.
    Failed(Failure),
    /// The call named a tool the server does not have.
    UnknownTool,
}

/// Sends the call's events and returns the result to deliver, carrying the
/// conversation handle when it is new.
pub(crate) fn complete_call(
    core: &Core,
    mut call: PreparedCall,
    result: CallToolResult,
) -> CallToolResult {
    let duration_ms = elapsed_ms(&call);
    call.transport.call_ended();
    let response = core
        .options
        .capture_responses
        .then(|| serde_json::to_value(&result).ok())
        .flatten();
    let failure = (result.is_error == Some(true)).then(|| reported_failure(&result));
    let mut delivered = result;
    let mirror = call
        .facts
        .as_ref()
        .is_some_and(|facts| facts.mirrors_instructions);
    if !deliver_handle(&mut delivered, &call.conversation, mirror) {
        forget_undelivered_handle(&mut call);
    }
    if names_a_tool(&call) {
        core.emit(|| match &failure {
            Some(failure) => failure_events(core, &call, duration_ms, failure, response.as_ref()),
            None => vec![tool_call_event(&call, duration_ms, None, response.as_ref())],
        });
    }
    delivered
}

/// Sends the events of a call that ended without a result.
pub(crate) fn end_call(core: &Core, mut call: PreparedCall, end: CallEnd) {
    let duration_ms = elapsed_ms(&call);
    call.transport.call_ended();
    forget_undelivered_handle(&mut call);
    if !names_a_tool(&call) {
        return;
    }
    core.emit(|| match end {
        CallEnd::InputRequired { methods } => {
            vec![input_required_event(&call, duration_ms, &methods)]
        }
        CallEnd::Task => Vec::new(),
        CallEnd::Failed(failure) => failure_events(core, &call, duration_ms, &failure, None),
        CallEnd::UnknownTool => vec![unknown_tool_event(&call)],
    });
}

/// A call naming no tool is neither a tool call nor an unknown tool.
fn names_a_tool(call: &PreparedCall) -> bool {
    !call.tool_name.trim().is_empty()
}

/// A minted handle the agent never received names no conversation, so the
/// call keeps a session of its own.
fn forget_undelivered_handle(call: &mut PreparedCall) {
    if call.conversation.minted {
        call.conversation.undelivered();
        call.context.conversation_id = None;
        call.context.session_id = new_session_id();
    }
}

fn failure_events(
    core: &Core,
    call: &PreparedCall,
    duration_ms: u64,
    failure: &Failure,
    response: Option<&Value>,
) -> Vec<OutgoingEvent> {
    let mut events = vec![tool_call_event(call, duration_ms, Some(failure), response)];
    if core.options.exception_autocapture {
        events.push(exception_event(call, failure));
    }
    events
}

fn tool_call_event(
    call: &PreparedCall,
    duration_ms: u64,
    failure: Option<&Failure>,
    response: Option<&Value>,
) -> OutgoingEvent {
    let mut event = OutgoingEvent::new(events::TOOL_CALL, &call.context);
    let tool_name = tool_name(call);
    event.set(key::SOURCE, ANALYTICS_SOURCE);
    event.set(key::TOOL_NAME, tool_name.as_str());
    event.set(key::RESOURCE_NAME, tool_name);
    event.set(key::IS_ERROR, failure.is_some());
    event.set(key::DURATION_MS, duration_ms);
    if let Some(facts) = &call.facts {
        let limited =
            |text: &Option<String>| text.as_deref().map(|text| truncate(text, MAX_TEXT_LENGTH));
        event.set_optional(key::TOOL_DESCRIPTION, limited(&facts.description));
        event.set_optional(key::TOOL_CATEGORY, limited(&facts.category));
    }
    set_intent_and_model(&mut event, call);
    event.set_optional(
        key::PARAMETERS,
        call.parameters.as_ref().map(captured_parameters),
    );
    event.set_optional(key::RESPONSE, response.map(captured_response));
    if let Some(failure) = failure {
        event.extend(failure.call_properties());
    }
    event.bounded(&TOOL_CALL_REQUIRED)
}

fn exception_event(call: &PreparedCall, failure: &Failure) -> OutgoingEvent {
    let mut event = OutgoingEvent::new(events::EXCEPTION, &call.context);
    let tool_name = tool_name(call);
    event.set(key::TOOL_NAME, tool_name.as_str());
    event.set(key::RESOURCE_NAME, tool_name);
    event.extend(failure.exception_properties());
    event.bounded(&[key::TOOL_NAME])
}

fn input_required_event(
    call: &PreparedCall,
    duration_ms: u64,
    methods: &[String],
) -> OutgoingEvent {
    let mut event = OutgoingEvent::new(events::INPUT_REQUIRED, &call.context);
    let methods: Vec<String> = methods
        .iter()
        .take(MAX_INPUT_REQUEST_METHODS)
        .map(|method| bounded_text(method, MAX_NAME_LENGTH, TextPass::Name))
        .collect();
    event.set(key::TOOL_NAME, tool_name(call));
    event.set(key::DURATION_MS, duration_ms);
    event.set(key::INPUT_REQUEST_METHODS, methods);
    event.bounded(&[key::TOOL_NAME, key::DURATION_MS, key::INPUT_REQUEST_METHODS])
}

/// Its name is whatever the agent sent, so it is redacted as free text.
fn unknown_tool_event(call: &PreparedCall) -> OutgoingEvent {
    let mut event = OutgoingEvent::new(events::UNKNOWN_TOOL, &call.context);
    event.set(
        key::TOOL_NAME,
        bounded_name(&call.tool_name, TextPass::FreeText),
    );
    event.bounded(&[key::TOOL_NAME])
}

pub(crate) fn set_intent_and_model(event: &mut OutgoingEvent, call: &PreparedCall) {
    if let Some(intent) = &call.intent {
        event.set(key::INTENT, intent.text.as_str());
        event.set(key::INTENT_SOURCE, intent.source);
    }
    if let Some(model) = &call.model {
        event.set(key::LLM_MODEL, model.name.as_str());
        event.set(key::LLM_MODEL_SOURCE, model.source);
    }
}

/// A registered tool's name is the server's own; any other name came from
/// the agent and is redacted as free text.
fn tool_name(call: &PreparedCall) -> String {
    let pass = match (call.existence, &call.facts) {
        (ToolExistence::Registered, _) | (_, Some(_)) => TextPass::Name,
        _ => TextPass::FreeText,
    };
    bounded_name(&call.tool_name, pass)
}

/// A name redaction removed entirely still has to name something.
fn bounded_name(name: &str, pass: TextPass) -> String {
    bounded_non_empty(name, MAX_NAME_LENGTH, pass).unwrap_or_else(|| REDACTED.to_string())
}

fn captured_parameters(parameters: &Value) -> Value {
    bounded_payload(parameters, TextPass::Captured)
}

/// The text of the tool's own content, which never holds the handle: that is
/// appended to the delivered copy only.
fn reported_failure(result: &CallToolResult) -> Failure {
    let texts: Vec<&str> = result.content.iter().filter_map(text_of).collect();
    let message = texts.join(" ");
    let validation = message
        .trim_start()
        .starts_with(RMCP_ARGUMENT_ERROR_PREFIX)
        .then(|| VALIDATION_ERROR_TYPE.to_string());
    Failure::reported(message).with_explicit_type(validation)
}

fn text_of(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::Text(text) => Some(text.text.as_str()),
        _ => None,
    }
}

fn elapsed_ms(call: &PreparedCall) -> u64 {
    let elapsed = call.started.elapsed().as_millis();
    u64::try_from(elapsed).unwrap_or(u64::MAX)
}
