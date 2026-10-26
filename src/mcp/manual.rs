//! The manual API, for servers analytics cannot wrap: a custom dispatcher, a
//! gateway, or a framework without an integration. Given the same traffic, it
//! sends the same events as [`McpAnalytics::instrument`].
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use posthog_rs::mcp::{McpAnalytics, McpOptions, ToolCallError, ToolCallRequest, ToolRegistration};
//! # use rmcp::model::{CallToolResult, ContentBlock, Tool};
//! # fn run(analytics: McpAnalytics, tool: Tool, arguments: Option<rmcp::model::JsonObject>) {
//! let call = analytics.start_tool_call(
//!     ToolCallRequest::new("search_docs", arguments).tool(ToolRegistration::Registered(tool)),
//! );
//! let outcome: Result<CallToolResult, std::io::Error> =
//!     Ok(CallToolResult::success(vec![ContentBlock::text("Found 3 results")]));
//! match outcome {
//!     Ok(result) => {
//!         let delivered = call.complete(result);
//!         # let _ = delivered;
//!     }
//!     Err(error) => call.fail(ToolCallError::new(&error)),
//! }
//! # }
//! ```

use std::any::type_name;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use rmcp::model::{CallToolResult, Implementation, InputRequiredResult, JsonObject, Tool};

use super::analytics::{Core, McpAnalytics};
use super::call::{
    complete_call, end_call, prepare_call, CallEnd, CallRequest, PreparedCall, RequestScope,
    ToolExistence,
};
use super::capture::{Failure, ServerIdentity};
use super::session::TransportSession;
use super::tools::catalog::ToolFacts;

const TOOLS_CALL: &str = "tools/call";

/// Whether the server has the tool a call names.
#[derive(Clone, Debug)]
pub enum ToolRegistration {
    /// The tool as the server registered it, before analytics advertised its
    /// arguments.
    Registered(Tool),
    /// The server has no such tool, so the call is not a tool call:
    /// [`McpAnalytics::start_tool_call`] sends `$mcp_unknown_tool` instead.
    NotRegistered,
    /// The server has no registry to tell.
    NoRegistry,
}

/// A `tools/call` as the server received it.
#[derive(Clone, Debug)]
pub struct ToolCallRequest {
    name: String,
    arguments: Option<JsonObject>,
    meta: Option<JsonObject>,
    http: Option<http::request::Parts>,
    tool: ToolRegistration,
    transport_session: TransportSession,
    server: Option<Implementation>,
    started: Option<Instant>,
}

impl ToolCallRequest {
    /// `arguments` as the agent sent them, analytics arguments included.
    pub fn new(name: impl Into<String>, arguments: Option<JsonObject>) -> Self {
        Self {
            name: name.into(),
            arguments,
            meta: None,
            http: None,
            tool: ToolRegistration::NoRegistry,
            transport_session: TransportSession::None,
            server: None,
            started: None,
        }
    }

    /// The request's `_meta`.
    pub fn meta(mut self, meta: JsonObject) -> Self {
        self.meta = Some(meta);
        self
    }

    /// The HTTP request, on HTTP transports.
    pub fn http(mut self, parts: http::request::Parts) -> Self {
        self.http = Some(parts);
        self
    }

    pub fn tool(mut self, registration: ToolRegistration) -> Self {
        self.tool = registration;
        self
    }

    /// How the transport groups this client's requests. Without one, the call
    /// is a stateless request.
    pub fn transport_session(mut self, session: TransportSession) -> Self {
        self.transport_session = session;
        self
    }

    pub fn server(mut self, server: Implementation) -> Self {
        self.server = Some(server);
        self
    }

    /// When the request arrived, if before [`McpAnalytics::start_tool_call`].
    pub fn started_at(mut self, started: Instant) -> Self {
        self.started = Some(started);
        self
    }
}

/// A tool call analytics is following. Finish it with
/// [`complete`](Self::complete), [`fail`](Self::fail) or
/// [`input_required`](Self::input_required). Drop it for a task handle, which
/// is not a final outcome.
pub struct StartedToolCall {
    core: Arc<Core>,
    call: Option<PreparedCall>,
    arguments: Option<JsonObject>,
}

/// An error a tool raised.
#[derive(Clone, Debug)]
pub struct ToolCallError {
    failure: Failure,
}

impl ToolCallError {
    /// Named by the error's type, which error tracking groups by.
    pub fn new<E: std::error::Error + ?Sized>(error: &E) -> Self {
        Self {
            failure: Failure::thrown(type_name::<E>(), error.to_string()),
        }
    }

    /// The label `$mcp_error_type` counts the call under, such as
    /// `validation`, `permission`, `timeout`, `rate_limited`, `api_4xx`,
    /// `api_5xx` or `internal`. The `$exception` keeps the error's own type.
    pub fn error_type(mut self, label: impl Into<String>) -> Self {
        self.failure = self.failure.with_explicit_type(Some(label.into()));
        self
    }
}

impl McpAnalytics {
    /// The tools to advertise: the server's own, with the enabled analytics
    /// arguments added.
    pub fn list_tools(&self, tools: Vec<Tool>) -> Vec<Tool> {
        let core = &self.core;
        tools
            .into_iter()
            .map(|tool| {
                let Ok(descriptor) = serde_json::to_value(&tool) else {
                    return tool;
                };
                let facts = ToolFacts::from_descriptor(&descriptor, core.injection.enabled);
                core.catalog.remember(&tool.name, facts);
                serde_json::from_value(core.injection.inject(&descriptor)).unwrap_or(tool)
            })
            .collect()
    }

    /// Starts following a tool call. Hand the tool
    /// [`StartedToolCall::arguments`], which no longer hold the analytics
    /// arguments.
    pub fn start_tool_call(&self, request: ToolCallRequest) -> StartedToolCall {
        let core = self.core.clone();
        let (registered, existence) = registration(&core, &request.tool);
        let mut call = {
            let scope = scope(&request);
            let call_request = CallRequest {
                name: &request.name,
                arguments: request.arguments.as_ref(),
                registered,
                existence,
            };
            prepare_call(&core, &scope, call_request)
        };
        let arguments = call.dispatch_arguments.take();
        if existence == ToolExistence::NotRegistered {
            end_call(&core, call, CallEnd::UnknownTool);
            return StartedToolCall {
                core,
                call: None,
                arguments,
            };
        }
        StartedToolCall {
            core,
            call: Some(call),
            arguments,
        }
    }
}

impl StartedToolCall {
    /// The arguments to hand the tool.
    pub fn arguments(&self) -> Option<&JsonObject> {
        self.arguments.as_ref()
    }

    /// Sends the call's events and returns the result to deliver, which
    /// carries the conversation handle when it is new.
    pub fn complete(self, result: CallToolResult) -> CallToolResult {
        match self.call {
            Some(call) => complete_call(&self.core, call, result),
            None => result,
        }
    }

    pub fn fail(self, error: ToolCallError) {
        if let Some(call) = self.call {
            end_call(&self.core, call, CallEnd::Failed(error.failure));
        }
    }

    /// The client received a round asking for more input. The retry that
    /// completes the call is a call of its own.
    pub fn input_required(self, round: &InputRequiredResult) {
        let Some(call) = self.call else {
            return;
        };
        let methods = round
            .input_requests
            .iter()
            .flatten()
            .map(|(_, request)| {
                let wire = serde_json::to_value(request).unwrap_or_default();
                wire.get("method")
                    .and_then(|method| method.as_str())
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        end_call(&self.core, call, CallEnd::InputRequired { methods });
    }
}

fn registration(core: &Core, tool: &ToolRegistration) -> (Option<ToolFacts>, ToolExistence) {
    match tool {
        ToolRegistration::Registered(tool) => {
            let facts = serde_json::to_value(tool)
                .ok()
                .map(|descriptor| ToolFacts::from_descriptor(&descriptor, core.injection.enabled));
            (facts, ToolExistence::Registered)
        }
        ToolRegistration::NotRegistered => (None, ToolExistence::NotRegistered),
        ToolRegistration::NoRegistry => (None, ToolExistence::Unknown),
    }
}

fn scope(request: &ToolCallRequest) -> RequestScope<'_> {
    let started = request.started.unwrap_or_else(Instant::now);
    let elapsed =
        chrono::Duration::from_std(started.elapsed()).unwrap_or_else(|_| chrono::Duration::zero());
    let server = request.server.as_ref().map(|server| ServerIdentity {
        name: Some(server.name.clone()),
        version: Some(server.version.clone()),
    });
    RequestScope {
        method: TOOLS_CALL,
        meta: request.meta.as_ref(),
        http: request.http.as_ref(),
        transport: &request.transport_session,
        handshake: None,
        server: server.unwrap_or_default(),
        timestamp: Utc::now() - elapsed,
        started,
    }
}
