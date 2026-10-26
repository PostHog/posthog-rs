---
cargo/posthog-rs: minor
---

MCP analytics (experimental): add `McpOptionsBuilder::server_build`, which sends `$mcp_server_build` on every event, and the opt-in `send_feedback` virtual tool (`McpOptionsBuilder::feedback_tool`), which sends `$mcp_feedback`. MCP events now include the host's request context before their size bound applies.
