use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use serde_json::{Map, Value};

use super::call::RequestScope;
use super::capture::{self, Delivery, EventContext, OutgoingEvent};
use super::contract::arguments::{CONTEXT_DESCRIPTION, LLM_MODEL_DESCRIPTION};
use super::identity::Identity;
use super::options::McpOptions;
use super::request_info::RequestInfo;
use super::session::{derive_session_id, new_session_id, ResolvedSession};
use super::tools::catalog::ToolCatalog;
use super::tools::injection::ArgumentInjection;
use super::tools::ownership::ArgumentFlags;
use super::tools::virtual_tools::VirtualTool;
use crate::{Client, RequestContext};

/// MCP analytics for one server: its options, what it learned about the
/// server's tools, and the PostHog client its events go through. Clones share
/// all of it.
///
/// Instrument an `rmcp` server with [`McpAnalytics::instrument`], or record
/// calls from a custom dispatcher with [`McpAnalytics::list_tools`] and
/// [`McpAnalytics::start_tool_call`].
#[derive(Clone)]
pub struct McpAnalytics {
    pub(crate) core: Arc<Core>,
}

pub(crate) struct Core {
    pub(crate) options: McpOptions,
    pub(crate) injection: ArgumentInjection,
    pub(crate) catalog: ToolCatalog,
    delivery: Delivery,
}

impl McpAnalytics {
    pub fn new(client: Arc<Client>, options: McpOptions) -> Self {
        let injection = argument_injection(&options);
        let delivery = Delivery::new(client, options.before_send.clone());
        let core = Core {
            options,
            injection,
            catalog: ToolCatalog::default(),
            delivery,
        };
        Self {
            core: Arc::new(core),
        }
    }
}

impl std::fmt::Debug for McpAnalytics {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpAnalytics")
            .field("options", &self.core.options)
            .finish_non_exhaustive()
    }
}

fn argument_injection(options: &McpOptions) -> ArgumentInjection {
    let description = |custom: &Option<String>, default: &str| {
        custom
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or(default)
            .to_string()
    };
    ArgumentInjection {
        enabled: ArgumentFlags {
            context: options.context,
            llm_model: options.capture_model,
            conversation_id: options.conversation_id,
        },
        context_description: description(&options.context_description, CONTEXT_DESCRIPTION),
        llm_model_description: description(&options.model_description, LLM_MODEL_DESCRIPTION),
    }
}

impl Core {
    /// Analytics must never break a request, so a failure while building
    /// events drops them.
    pub(crate) fn emit(&self, build: impl FnOnce() -> Vec<OutgoingEvent>) {
        let outcome = catch_unwind(AssertUnwindSafe(|| self.delivery.send(build())));
        if outcome.is_err() {
            tracing::warn!("PostHog MCP analytics failed to build an event; dropped it");
        }
    }

    pub(crate) fn event_context(
        &self,
        scope: &RequestScope<'_>,
        session: ResolvedSession,
        conversation_id: Option<String>,
        request: &RequestInfo<'_>,
    ) -> EventContext {
        let session_id = match &conversation_id {
            Some(handle) => derive_session_id(handle),
            None => session.session_id.unwrap_or_else(new_session_id),
        };
        let (host_properties, host_options) = host_context(scope);
        let mut options = self.event_options(scope, request);
        for (name, value) in host_options {
            options.entry(name).or_insert(value);
        }
        EventContext {
            timestamp: scope.timestamp,
            session_id,
            conversation_id,
            identity: self.identify(request),
            options,
            client: scope.client_identity(session.token_client.as_ref()),
            server: scope.server.clone(),
            server_build: self.options.server_build.clone(),
            host_properties,
        }
    }

    /// The name a virtual tool is advertised under, when it is enabled.
    pub(crate) fn virtual_tool_name(&self, tool: VirtualTool) -> Option<&str> {
        tool.name(self.options.virtual_tool_name(tool))
    }

    /// The enabled virtual tool advertised under `name`.
    pub(crate) fn virtual_tool(&self, name: &str) -> Option<VirtualTool> {
        IntoIterator::into_iter(VirtualTool::ALL)
            .find(|tool| self.virtual_tool_name(*tool) == Some(name))
    }

    /// Options set on the call win over the callback's, and a `null` sets nothing.
    fn event_options(
        &self,
        scope: &RequestScope<'_>,
        request: &RequestInfo<'_>,
    ) -> Map<String, Value> {
        let from_callback = self.options.event_options.as_ref().and_then(|callback| {
            catch_unwind(AssertUnwindSafe(|| callback(request))).unwrap_or_else(|_| {
                tracing::warn!(
                    "MCP analytics event_options callback panicked; the request sets no options"
                );
                None
            })
        });
        let mut options: Map<String, Value> = from_callback
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, value)| !value.is_null())
            .collect();
        for (key, value) in scope.event_options.into_iter().flatten() {
            if !value.is_null() {
                options.insert(key.clone(), value.clone());
            }
        }
        options
    }

    fn identify(&self, request: &RequestInfo<'_>) -> Option<Identity> {
        let identify = self.options.identify.as_ref()?;
        let outcome = catch_unwind(AssertUnwindSafe(|| identify(request)));
        match outcome {
            Ok(identity) => identity.filter(Identity::is_valid),
            Err(_) => {
                tracing::warn!(
                    "MCP analytics identify callback panicked; the request is anonymous"
                );
                None
            }
        }
    }
}

/// What the host's request context adds to the call's events: its
/// properties, and its capture options, which the call's own options override.
/// The context in scope wins, for a manual API call made inside the host's
/// handler; else the one its HTTP layer attached to the request, since `rmcp`
/// serves each request on a task of its own.
fn host_context(scope: &RequestScope<'_>) -> (Map<String, Value>, Map<String, Value>) {
    let read =
        |host: &RequestContext| (capture::host_properties(host), capture::host_options(host));
    RequestContext::with_current(read)
        .or_else(|| scope.request_context().as_ref().map(read))
        .unwrap_or_default()
}
