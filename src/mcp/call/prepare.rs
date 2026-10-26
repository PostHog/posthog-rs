//! What a `tools/call` decides before the tool runs: the arguments the tool
//! receives, the call's session and conversation, and what the agent said
//! about the call.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use serde_json::{json, Map, Value};

use super::intent::{self, Intent, INFERRED};
use super::model::{self, Model};
use super::scope::RequestScope;
use crate::mcp::analytics::Core;
use crate::mcp::capture::EventContext;
use crate::mcp::contract::arguments::{CONTEXT, CONVERSATION_ID};
use crate::mcp::request_info::RequestInfo;
use crate::mcp::session::{Conversation, TransportSession};
use crate::mcp::tools::catalog::ToolFacts;
use crate::mcp::tools::ownership::{strip_owned_arguments, ArgumentFlags};

const TOOLS_CALL: &str = "tools/call";

/// Whether the server has the tool a call names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolExistence {
    Registered,
    NotRegistered,
    /// The server has no registry analytics can read.
    Unknown,
}

pub(crate) struct CallRequest<'a> {
    pub(crate) name: &'a str,
    pub(crate) arguments: Option<&'a Map<String, Value>>,
    /// The tool as the server registered it, before injection.
    pub(crate) registered: Option<ToolFacts>,
    pub(crate) existence: ToolExistence,
}

pub(crate) struct PreparedCall {
    pub(crate) tool_name: String,
    pub(crate) sent_arguments: Option<Map<String, Value>>,
    pub(crate) dispatch_arguments: Option<Map<String, Value>>,
    pub(crate) existence: ToolExistence,
    pub(crate) facts: Option<ToolFacts>,
    pub(crate) context: EventContext,
    pub(crate) conversation: Conversation,
    pub(crate) intent: Option<Intent>,
    pub(crate) model: Option<Model>,
    pub(crate) parameters: Option<Value>,
    pub(crate) started: Instant,
    pub(crate) transport: TransportSession,
}

/// Which analytics arguments a call may strip, and which it may read.
/// Stripping needs proof that analytics added the argument; reading only
/// needs no proof that the tool declares it.
struct ArgumentAccess {
    strip: ArgumentFlags,
    read: ArgumentFlags,
}

pub(crate) fn prepare_call(
    core: &Core,
    scope: &RequestScope<'_>,
    request: CallRequest<'_>,
) -> PreparedCall {
    let CallRequest {
        name,
        arguments: sent,
        registered,
        existence,
    } = request;
    let facts = merge_facts(registered, core.catalog.get(name));
    let access = argument_access(core, facts.as_ref());

    let conversation = resolve_conversation(access.read, sent, scope.transport);
    let dispatch_arguments = dispatched_arguments(sent, access.strip);
    let info = scope.request_info(Some(name), sent);
    let session = scope.transport.resolve_for_call();
    let context = core.event_context(scope, session, conversation.id.clone(), &info);
    let parameters = core
        .options
        .capture_parameters
        .then(|| captured_parameters(name, dispatch_arguments.as_ref(), facts.is_none()));

    PreparedCall {
        tool_name: name.to_string(),
        sent_arguments: sent.cloned(),
        intent: call_intent(core, scope, &info, access.read),
        model: call_model(core, scope, sent, access.read),
        dispatch_arguments,
        existence,
        facts,
        context,
        conversation,
        parameters,
        started: scope.started,
        transport: scope.transport.clone(),
    }
}

/// Ownership comes from what the agent was shown, when this server listed
/// the tool; descriptions come from the registry, the server's own word.
fn merge_facts(registered: Option<ToolFacts>, listed: Option<ToolFacts>) -> Option<ToolFacts> {
    match (registered, listed) {
        (Some(registered), Some(listed)) => Some(ToolFacts {
            description: registered.description.or(listed.description),
            category: registered.category.or(listed.category),
            input_schema: registered.input_schema.or(listed.input_schema),
            injected: listed.injected,
            mirrors_instructions: listed.mirrors_instructions,
        }),
        (registered, listed) => registered.or(listed),
    }
}

fn argument_access(core: &Core, facts: Option<&ToolFacts>) -> ArgumentAccess {
    let owned = facts.map(|facts| facts.injected);
    ArgumentAccess {
        strip: owned.unwrap_or_default(),
        read: owned.unwrap_or(core.injection.enabled),
    }
}

/// An echoed handle wins. Without one, a request with no transport session
/// gets a new handle, the only way its later calls can join it.
fn resolve_conversation(
    readable: ArgumentFlags,
    sent: Option<&Map<String, Value>>,
    transport: &TransportSession,
) -> Conversation {
    if !readable.conversation_id {
        return Conversation::default();
    }
    let echoed = sent
        .and_then(|arguments| arguments.get(CONVERSATION_ID))
        .and_then(Value::as_str);
    let conversation = Conversation::echoed(echoed);
    if conversation.is_present() || transport.carries_session() {
        conversation
    } else {
        Conversation::minted()
    }
}

fn dispatched_arguments(
    sent: Option<&Map<String, Value>>,
    strip: ArgumentFlags,
) -> Option<Map<String, Value>> {
    let mut arguments = sent.cloned()?;
    strip_owned_arguments(&mut arguments, strip);
    Some(arguments)
}

fn call_intent(
    core: &Core,
    scope: &RequestScope<'_>,
    info: &RequestInfo<'_>,
    readable: ArgumentFlags,
) -> Option<Intent> {
    let can_read_context = core.options.context && readable.context;
    intent::from_client_metadata(scope)
        .or_else(|| sent_intent(info.arguments, can_read_context))
        .or_else(|| fallback_intent(core, info))
}

fn sent_intent(sent: Option<&Map<String, Value>>, can_read_context: bool) -> Option<Intent> {
    can_read_context
        .then(|| intent::from_context_argument(sent))
        .flatten()
}

fn call_model(
    core: &Core,
    scope: &RequestScope<'_>,
    sent: Option<&Map<String, Value>>,
    readable: ArgumentFlags,
) -> Option<Model> {
    core.options
        .capture_model
        .then(|| model::resolve(scope, sent, readable.llm_model))
        .flatten()
}

fn fallback_intent(core: &Core, info: &RequestInfo<'_>) -> Option<Intent> {
    let infer = core.options.intent_fallback.as_ref()?;
    match catch_unwind(AssertUnwindSafe(|| infer(info))) {
        Ok(inferred) => Intent::new(&inferred?, INFERRED),
        Err(_) => {
            tracing::warn!("MCP analytics intent fallback panicked; the call has no intent");
            None
        }
    }
}

/// The JSON-RPC request with only `name` and `arguments`: a retry's answers
/// and state are never captured. When analytics cannot tell who owns
/// `context`, it is left out, because only the intent gets personal data
/// redacted.
fn captured_parameters(
    name: &str,
    dispatch_arguments: Option<&Map<String, Value>>,
    ownership_unknown: bool,
) -> Value {
    let mut arguments = dispatch_arguments.cloned().unwrap_or_default();
    if ownership_unknown {
        arguments.remove(CONTEXT);
    }
    json!({ "request": {
        "method": TOOLS_CALL,
        "params": { "name": name, "arguments": arguments },
    } })
}
