//! The manual API, for servers analytics cannot wrap: a custom dispatcher, a
//! gateway, or a framework without an integration. Given the same traffic, it
//! sends the same events as [`McpAnalytics::instrument`].
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use posthog::mcp::{McpAnalytics, McpOptions, ToolCallError, ToolCallRequest, ToolRegistration};
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
use serde::Serialize;

use super::analytics::{Core, McpAnalytics};
use super::call::{
    complete_call, end_call, prepare_call, CallEnd, CallRequest, PreparedCall, RequestScope,
    ToolExistence,
};
use super::capture::{ClientIdentity, Failure, ServerIdentity};
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
    client_info: Option<Implementation>,
    protocol_version: Option<String>,
    started: Option<Instant>,
    event_options: JsonObject,
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
            client_info: None,
            protocol_version: None,
            started: None,
            event_options: JsonObject::new(),
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

    /// The `clientInfo` of the `initialize` handshake bound to this request's
    /// transport session, which names the client when the request does not.
    /// Never one from a handshake with another client, such as the last one
    /// a server shared by many clients received.
    pub fn client_info(mut self, client: Implementation) -> Self {
        self.client_info = Some(client);
        self
    }

    /// The protocol revision that bound handshake negotiated, sent as
    /// `$mcp_protocol_version` when the request names none itself.
    pub fn protocol_version(mut self, version: impl Into<String>) -> Self {
        self.protocol_version = Some(version.into());
        self
    }

    pub fn server(mut self, server: Implementation) -> Self {
        self.server = Some(server);
        self
    }

    /// Sets a capture option, such as `disable_skew_correction`, on every
    /// event of this call, its `$exception` included. It wins over
    /// [`McpOptionsBuilder::event_options`](super::McpOptionsBuilder::event_options)
    /// and the request context. An anonymous call keeps
    /// `process_person_profile: false`. A `null` value sets nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Serialization`](crate::Error::Serialization) if
    /// `value` cannot be serialized.
    pub fn event_option(
        mut self,
        key: impl Into<String>,
        value: impl Serialize,
    ) -> Result<Self, crate::Error> {
        let value = serde_json::to_value(value)
            .map_err(|error| crate::Error::Serialization(error.to_string()))?;
        self.event_options.insert(key.into(), value);
        Ok(self)
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
    /// arguments added. Prefer [`list_tools_for_request`](Self::list_tools_for_request)
    /// when the listing came over HTTP.
    pub fn list_tools(&self, tools: Vec<Tool>) -> Vec<Tool> {
        self.advertise(tools, false)
    }

    /// Like [`list_tools`](Self::list_tools), for the listing request that
    /// carried `headers`. A client that names every conversation itself, such
    /// as ChatGPT, is offered no `conversation_id`.
    pub fn list_tools_for_request(&self, tools: Vec<Tool>, headers: &http::HeaderMap) -> Vec<Tool> {
        let client = ClientIdentity {
            user_agent: headers
                .get(http::header::USER_AGENT)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            ..ClientIdentity::default()
        };
        let handle_free = client.recognized_row().is_some_and(|row| row.handle_free);
        self.advertise(tools, handle_free)
    }

    fn advertise(&self, tools: Vec<Tool>, handle_free: bool) -> Vec<Tool> {
        let core = &self.core;
        let injection = core.injection.for_client(handle_free);
        tools
            .into_iter()
            .map(|tool| {
                let Ok(descriptor) = serde_json::to_value(&tool) else {
                    return tool;
                };
                let facts = ToolFacts::from_descriptor(&descriptor, core.injection.enabled);
                core.catalog.remember(&tool.name, facts);
                serde_json::from_value(injection.inject(&descriptor)).unwrap_or(tool)
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
    /// The context to run the tool in, with
    /// [`RequestContext::scope`](crate::RequestContext::scope), so events it
    /// captures carry this call's session and identity. `None` for a call
    /// to a tool the server does not have.
    pub fn request_context(&self) -> Option<crate::RequestContext> {
        let call = self.call.as_ref()?;
        Some(call.tool_scope.context.clone())
    }

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

fn handshake(request: &ToolCallRequest) -> Option<ClientIdentity> {
    let client = request.client_info.as_ref();
    let protocol_version = request
        .protocol_version
        .clone()
        .filter(|version| !version.is_empty());
    if client.is_none() && protocol_version.is_none() {
        return None;
    }
    Some(ClientIdentity {
        name: client
            .map(|client| client.name.clone())
            .filter(|name| !name.is_empty()),
        version: client
            .map(|client| client.version.clone())
            .filter(|version| !version.is_empty()),
        protocol_version,
        ..ClientIdentity::default()
    })
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
        handshake: handshake(request),
        server: server.unwrap_or_default(),
        timestamp: Utc::now() - elapsed,
        started,
        event_options: Some(&request.event_options),
    }
}
