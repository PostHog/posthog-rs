//! Whether this server advertises the `get_more_tools` virtual tool: it does
//! when enabled and no tool of the server's own takes its name.

use rmcp::model::Tool;
use rmcp::ServerHandler;
use serde_json::Value;

use super::Instrumented;
use crate::mcp::tools::catalog::ToolFacts;
use crate::mcp::tools::virtual_tools::missing_capability_descriptor;

impl<H: ServerHandler> Instrumented<H> {
    pub(super) fn is_virtual_tool(&self, name: &str) -> bool {
        let enabled = self.analytics.core.missing_capability_tool() == Some(name);
        enabled && !self.server_has_tool(name)
    }

    /// The virtual tool to append to a listing's last page, as advertised.
    pub(super) fn virtual_tool_for_listing(&self, page: &[Tool]) -> Option<Tool> {
        let name = self.analytics.core.missing_capability_tool()?;
        let on_page = page.iter().any(|tool| tool.name == name);
        if on_page || self.server_has_tool(name) {
            return None;
        }
        self.advertised_virtual_tool()
    }

    pub(super) fn advertised_virtual_tool(&self) -> Option<Tool> {
        let injection = self.analytics.core.injection.for_virtual_tool();
        serde_json::from_value(injection.inject(&self.virtual_descriptor())).ok()
    }

    pub(super) fn virtual_facts(&self) -> ToolFacts {
        let enabled = self.analytics.core.injection.for_virtual_tool().enabled;
        ToolFacts::from_descriptor(&self.virtual_descriptor(), enabled)
    }

    fn virtual_descriptor(&self) -> Value {
        let name = self.analytics.core.missing_capability_tool();
        missing_capability_descriptor(name.unwrap_or_default())
    }

    fn server_has_tool(&self, name: &str) -> bool {
        self.inner.get_tool(name).is_some() || self.analytics.core.catalog.get(name).is_some()
    }
}
