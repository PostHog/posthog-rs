//! Configuration. Defaults follow the configuration table of PostHog's MCP
//! analytics spec.

use std::sync::Arc;

use derive_builder::Builder;

use super::identity::Identity;
use super::request_info::RequestInfo;
use crate::client::BeforeSendHook;
use crate::Event;

pub type IdentifyCallback = Arc<dyn Fn(&RequestInfo<'_>) -> Option<Identity> + Send + Sync>;
pub type IntentFallback = Arc<dyn Fn(&RequestInfo<'_>) -> Option<String> + Send + Sync>;

/// Configures MCP analytics. Build it with [`McpOptionsBuilder`].
///
/// ```
/// use posthog_rs::mcp::{Identity, McpOptionsBuilder};
///
/// let options = McpOptionsBuilder::default()
///     .identify(|request| {
///         let user = request.http?.extensions.get::<String>()?;
///         Some(Identity::new(user.clone()))
///     })
///     .missing_capability_tool("get_more_tools")
///     .build()
///     .unwrap();
/// # let _ = options;
/// ```
#[derive(Builder, Clone)]
#[builder(default)]
pub struct McpOptions {
    /// Advertise a required `context` argument on every tool, in which the
    /// agent says why it calls the tool, captured as `$mcp_intent` (default:
    /// `true`).
    pub(crate) context: bool,
    /// Replaces the default description of the `context` argument. Keep the
    /// default's instruction to leave out personal and identifying details.
    #[builder(setter(into, strip_option))]
    pub(crate) context_description: Option<String>,
    /// Capture the model that made each call, from the request's `_meta`, or
    /// else from an advertised `llm_model` argument (default: `true`).
    pub(crate) capture_model: bool,
    /// Replaces the default description of the `llm_model` argument.
    #[builder(setter(into, strip_option))]
    pub(crate) model_description: Option<String>,
    /// Supplies the intent of a call whose agent stated none, captured with
    /// `$mcp_intent_source: "inferred"`. It runs before the tool does, so
    /// keep it fast.
    #[builder(setter(custom))]
    pub(crate) intent_fallback: Option<IntentFallback>,
    /// Conversation anchoring: advertise an optional `conversation_id`
    /// argument, hand the agent a handle on its first call, and group the
    /// calls that echo it into one session (default: `true`). Without it,
    /// every request of a stateless HTTP client is its own session.
    pub(crate) conversation_id: bool,
    /// Resolves the person each request acts for.
    #[builder(setter(custom))]
    pub(crate) identify: Option<IdentifyCallback>,
    /// Send a `$exception` next to each failed call (default: `true`).
    pub(crate) exception_autocapture: bool,
    /// Send the call's arguments as `$mcp_parameters` (default: `true`).
    pub(crate) capture_parameters: bool,
    /// Send the call's result as `$mcp_response`, successful calls included
    /// (default: `true`).
    pub(crate) capture_responses: bool,
    /// Modifies or drops each MCP event after redaction and size bounds.
    /// Dropping a `$mcp_tool_call` also drops its `$exception`. A panic drops
    /// the event.
    #[builder(setter(custom))]
    pub(crate) before_send: Option<BeforeSendHook>,
    /// Advertise a virtual tool of this name (`get_more_tools` when blank)
    /// that agents call to report a capability the server lacks. It is
    /// answered by [`McpAnalytics::instrument`](super::McpAnalytics::instrument),
    /// never by the manual API, and is not advertised when the server has a
    /// tool of the same name. A server that implements neither `get_tool` nor
    /// a listing on the instrumented instance cannot tell analytics it has
    /// one, so give it a name the server's own tools do not use.
    #[builder(setter(into, strip_option))]
    pub(crate) missing_capability_tool: Option<String>,
}

impl Default for McpOptions {
    fn default() -> Self {
        Self {
            context: true,
            context_description: None,
            capture_model: true,
            model_description: None,
            intent_fallback: None,
            conversation_id: true,
            identify: None,
            exception_autocapture: true,
            capture_parameters: true,
            capture_responses: true,
            before_send: None,
            missing_capability_tool: None,
        }
    }
}

impl std::fmt::Debug for McpOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpOptions")
            .field("context", &self.context)
            .field("capture_model", &self.capture_model)
            .field("conversation_id", &self.conversation_id)
            .field("exception_autocapture", &self.exception_autocapture)
            .field("capture_parameters", &self.capture_parameters)
            .field("capture_responses", &self.capture_responses)
            .field("missing_capability_tool", &self.missing_capability_tool)
            .finish_non_exhaustive()
    }
}

impl McpOptionsBuilder {
    pub fn identify<F>(&mut self, resolve: F) -> &mut Self
    where
        F: Fn(&RequestInfo<'_>) -> Option<Identity> + Send + Sync + 'static,
    {
        self.identify = Some(Some(Arc::new(resolve)));
        self
    }

    pub fn intent_fallback<F>(&mut self, infer: F) -> &mut Self
    where
        F: Fn(&RequestInfo<'_>) -> Option<String> + Send + Sync + 'static,
    {
        self.intent_fallback = Some(Some(Arc::new(infer)));
        self
    }

    pub fn before_send<F>(&mut self, hook: F) -> &mut Self
    where
        F: FnMut(Event) -> Option<Event> + Send + 'static,
    {
        self.before_send = Some(Some(BeforeSendHook::new(hook)));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_spec() {
        let options = McpOptionsBuilder::default().build().expect("valid");
        assert!(options.context && options.capture_model && options.conversation_id);
        assert!(options.exception_autocapture && options.capture_parameters);
        assert!(options.capture_responses);
        assert!(options.missing_capability_tool.is_none());
    }
}
