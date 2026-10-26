//! Names and limits shared with PostHog's other MCP analytics SDKs.

pub(crate) mod arguments;
pub(crate) mod events;
pub(crate) mod limits;
pub(crate) mod properties;

pub(crate) const ANALYTICS_SOURCE: &str = "posthog_mcp_analytics";
pub(crate) const EXCEPTION_SOURCE: &str = "mcp.tool_call";
pub(crate) const INTERFACE: &str = "mcp";
pub(crate) const LIB_NAME: &str = "posthog-rs-mcp";
pub(crate) const REDACTED: &str = "[redacted]";
