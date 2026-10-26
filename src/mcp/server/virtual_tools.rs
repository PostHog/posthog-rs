//! Which virtual tools this server advertises: each enabled one whose name no
//! tool of the server's own takes.

use rmcp::model::Tool;
use rmcp::ServerHandler;

use super::Instrumented;
use crate::mcp::tools::catalog::ToolFacts;
use crate::mcp::tools::virtual_tools::VirtualTool;

impl<H: ServerHandler> Instrumented<H> {
    pub(super) fn virtual_tool(&self, name: &str) -> Option<VirtualTool> {
        let tool = self.analytics.core.virtual_tool(name)?;
        (!self.server_has_tool(name)).then_some(tool)
    }

    /// The virtual tools to append to a listing's first page, the one every
    /// client reads, as advertised.
    pub(super) fn virtual_tools_for_listing(&self, page: &[Tool]) -> Vec<Tool> {
        IntoIterator::into_iter(VirtualTool::ALL)
            .filter(|tool| {
                let Some(name) = self.analytics.core.virtual_tool_name(*tool) else {
                    return false;
                };
                let on_page = page.iter().any(|listed| listed.name == name);
                !on_page && !self.server_has_tool(name)
            })
            .filter_map(|tool| self.advertised_virtual_tool(tool))
            .collect()
    }

    pub(super) fn advertised_virtual_tool(&self, tool: VirtualTool) -> Option<Tool> {
        let injection = self.analytics.core.injection.for_virtual_tool();
        serde_json::from_value(injection.inject(&self.virtual_descriptor(tool))).ok()
    }

    pub(super) fn virtual_facts(&self, tool: VirtualTool) -> ToolFacts {
        let enabled = self.analytics.core.injection.for_virtual_tool().enabled;
        ToolFacts::from_descriptor(&self.virtual_descriptor(tool), enabled)
    }

    fn virtual_descriptor(&self, tool: VirtualTool) -> serde_json::Value {
        let name = self.analytics.core.virtual_tool_name(tool);
        tool.descriptor(name.unwrap_or_default())
    }

    fn server_has_tool(&self, name: &str) -> bool {
        self.inner.get_tool(name).is_some() || self.analytics.core.catalog.get(name).is_some()
    }
}
