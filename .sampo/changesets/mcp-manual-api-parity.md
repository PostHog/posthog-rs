---
cargo/posthog: minor
---

The MCP manual API gains what the automatic path already uses: `McpAnalytics::list_tools_for_request` reads the listing's headers so ChatGPT is offered no `conversation_id`, `ToolCallRequest::client_info` and `ToolCallRequest::protocol_version` pass the bound handshake's client and negotiated revision so a client is recognized by name, and `StartedToolCall::request_context` gives the tool a context whose custom events carry the call's session and identity.
