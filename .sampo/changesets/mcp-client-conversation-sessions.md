---
cargo/posthog: minor
---

MCP analytics groups ChatGPT, Codex and VS Code tool calls by the conversation id each client sends in `_meta`, adds a `session_resolver` option that ranks ahead of every other session source, and sends `$mcp_session_source` on every MCP event. ChatGPT is no longer offered a `conversation_id` argument.
