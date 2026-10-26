use rmcp::model::{ListToolsResult, PaginatedRequestParams, Tool};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde_json::Value;

use super::Instrumented;
use crate::mcp::call::emit_tools_listed;
use crate::mcp::tools::catalog::ToolFacts;

const TOOLS_LIST: &str = "tools/list";
const NOT_ENVELOPE: [&str; 2] = ["tools", "resultType"];

impl<H: ServerHandler> Instrumented<H> {
    pub(super) async fn traced_list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let facts = self.request_facts(&context);
        let mut page = self.inner.list_tools(request, context).await?;
        self.remember(&page.tools);

        let mut advertised: Vec<Tool> = page
            .tools
            .iter()
            .map(|tool| self.injected_tool(tool))
            .collect();
        if page.next_cursor.is_none() {
            advertised.extend(self.virtual_tool_for_listing(&page.tools));
        }
        page.tools = advertised;

        let names = page
            .tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        emit_tools_listed(
            &self.analytics.core,
            &facts.scope(TOOLS_LIST),
            names,
            envelope(&page),
        );
        Ok(page)
    }

    /// The tool as advertised, or as registered when injection fails.
    pub(super) fn injected_tool(&self, tool: &Tool) -> Tool {
        let Ok(descriptor) = serde_json::to_value(tool) else {
            return tool.clone();
        };
        let injected = self.analytics.core.injection.inject(&descriptor);
        serde_json::from_value(injected).unwrap_or_else(|error| {
            tracing::warn!("PostHog MCP analytics could not advertise its arguments: {error}");
            tool.clone()
        })
    }

    fn remember(&self, tools: &[Tool]) {
        let core = &self.analytics.core;
        for tool in tools {
            if let Ok(descriptor) = serde_json::to_value(tool) {
                let facts = ToolFacts::from_descriptor(&descriptor, core.injection.enabled);
                core.catalog.remember(&tool.name, facts);
            }
        }
    }
}

/// Only the result envelope, such as `nextCursor`, never the tools or the
/// result type discriminator.
fn envelope(page: &ListToolsResult) -> Option<Value> {
    let mut envelope = serde_json::to_value(page).ok()?;
    let fields = envelope.as_object_mut()?;
    for omitted in NOT_ENVELOPE {
        fields.remove(omitted);
    }
    Some(envelope)
}
