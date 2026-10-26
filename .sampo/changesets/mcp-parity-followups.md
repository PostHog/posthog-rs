---
cargo/posthog-rs: minor
---

MCP analytics (experimental): add `McpOptionsBuilder::server_build`, which sends `$mcp_server_build` on every event, and the opt-in `send_feedback` virtual tool (`McpOptionsBuilder::feedback_tool`), which sends `$mcp_feedback`. Hosts can replace its description, declare extra report fields (`feedback_field`, `required_feedback_field`), and handle each report with an async `on_feedback` that may replace the reply. MCP events now include the host's request context before their size bound applies.
