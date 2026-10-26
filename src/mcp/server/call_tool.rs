use std::any::Any;
use std::panic::{resume_unwind, AssertUnwindSafe};

use futures_util::FutureExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode, InputRequiredResult,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};

use super::request::RequestFacts;
use super::Instrumented;
use crate::mcp::call::{
    answer_virtual_call, complete_call, end_call, prepare_call, CallEnd, CallRequest, PreparedCall,
    ToolExistence,
};
use crate::mcp::capture::Failure;
use crate::mcp::tools::catalog::ToolFacts;

const TOOLS_CALL: &str = "tools/call";
const PROTOCOL_ERROR_TYPE: &str = "McpError";
const ROUTER_UNKNOWN_TOOL: &str = "tool not found";
const VALIDATION_ERROR_TYPE: &str = "validation";
const INPUT_REQUIRED_ERROR_TYPE: &str = "input_required";
const PANIC_ERROR_TYPE: &str = "Panic";
const LEGACY_INPUT_REQUIRED: &str =
    "InputRequiredResult requires negotiated protocol version 2026-07-28 or newer";

impl<H: ServerHandler> Instrumented<H> {
    pub(super) async fn traced_call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let facts = self.request_facts(&context);
        if self.is_virtual_tool(&request.name) {
            return Ok(self.answer_virtual_tool(&facts, &request).into());
        }

        let registered = self.registered_facts(&request.name);
        let existence = match registered {
            Some(_) => ToolExistence::Registered,
            None => ToolExistence::Unknown,
        };
        let call = self.prepare(&facts, &request, registered, existence);
        let mut downstream = request;
        downstream.arguments = call.dispatch_arguments.clone();

        let handled = AssertUnwindSafe(async { self.inner.call_tool(downstream, context).await })
            .catch_unwind()
            .await;
        match handled {
            Ok(outcome) => self.record_outcome(call, facts.modern_protocol, outcome),
            Err(panic) => {
                let failure = Failure::panicked(PANIC_ERROR_TYPE, panic_message(panic.as_ref()));
                end_call(&self.analytics.core, call, CallEnd::Failed(failure));
                resume_unwind(panic)
            }
        }
    }

    fn answer_virtual_tool(
        &self,
        facts: &RequestFacts,
        request: &CallToolRequestParams,
    ) -> CallToolResult {
        let registered = Some(self.virtual_facts());
        let call = self.prepare(facts, request, registered, ToolExistence::Registered);
        answer_virtual_call(&self.analytics.core, call)
    }

    fn record_outcome(
        &self,
        call: PreparedCall,
        modern_protocol: bool,
        outcome: Result<CallToolResponse, McpError>,
    ) -> Result<CallToolResponse, McpError> {
        let core = &self.analytics.core;
        match outcome {
            Ok(CallToolResponse::Complete(result)) => Ok(CallToolResponse::Complete(
                complete_call(core, call, result),
            )),
            Ok(CallToolResponse::InputRequired(round)) => {
                end_call(core, call, input_required_end(&round, modern_protocol));
                Ok(CallToolResponse::InputRequired(round))
            }
            Ok(other) => {
                end_call(core, call, CallEnd::Task);
                Ok(other)
            }
            Err(error) => {
                let end = self.error_end(&call, &error);
                end_call(core, call, end);
                Err(error)
            }
        }
    }

    fn prepare(
        &self,
        facts: &RequestFacts,
        request: &CallToolRequestParams,
        registered: Option<ToolFacts>,
        existence: ToolExistence,
    ) -> PreparedCall {
        let call = CallRequest {
            name: &request.name,
            arguments: request.arguments.as_ref(),
            registered,
            existence,
        };
        prepare_call(&self.analytics.core, &facts.scope(TOOLS_CALL), call)
    }

    fn registered_facts(&self, name: &str) -> Option<ToolFacts> {
        let tool = self.inner.get_tool(name)?;
        let descriptor = serde_json::to_value(tool).ok()?;
        let enabled = self.analytics.core.injection.enabled;
        Some(ToolFacts::from_descriptor(&descriptor, enabled))
    }

    /// A call naming a tool the server neither registers nor listed, which
    /// the server rejected as unroutable, is not a tool call.
    fn error_end(&self, call: &PreparedCall, error: &McpError) -> CallEnd {
        let unroutable = error.code == ErrorCode::METHOD_NOT_FOUND
            || (error.code == ErrorCode::INVALID_PARAMS && error.message == ROUTER_UNKNOWN_TOOL);
        let unknown_tool = call.facts.is_none() && unroutable;
        if unknown_tool && !call.tool_name.trim().is_empty() {
            return CallEnd::UnknownTool;
        }
        let validation =
            (error.code == ErrorCode::INVALID_PARAMS).then(|| VALIDATION_ERROR_TYPE.to_string());
        let failure = Failure::thrown(PROTOCOL_ERROR_TYPE, error.message.to_string());
        CallEnd::Failed(failure.with_explicit_type(validation))
    }
}

/// `rmcp` answers a legacy client's round with an error, which ends the call
/// without a final outcome.
fn input_required_end(round: &InputRequiredResult, modern_protocol: bool) -> CallEnd {
    if !modern_protocol {
        let failure = Failure::reported(LEGACY_INPUT_REQUIRED.to_string());
        return CallEnd::Failed(
            failure.with_explicit_type(Some(INPUT_REQUIRED_ERROR_TYPE.to_string())),
        );
    }
    let methods = round
        .input_requests
        .iter()
        .flatten()
        .map(|(_, request)| request_method(request))
        .collect();
    CallEnd::InputRequired { methods }
}

/// `panic!` carries a `&str` or a `String`; anything else has no message.
fn panic_message(panic: &(dyn Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|message| message.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

fn request_method(request: &rmcp::model::InputRequest) -> String {
    let wire = serde_json::to_value(request).unwrap_or_default();
    wire.get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}
