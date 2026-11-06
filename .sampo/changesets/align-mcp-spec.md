---
cargo/posthog-rs: patch
---

Aligned the `mcp` module with the sdk-specs MCP spec. The `get_more_tools` tool now advertises `readOnlyHint`, `destructiveHint`, `idempotentHint` and `openWorldHint` as `false`, because a call records an event. The injected `context` argument has a shorter description, and the injected `llm_model` argument is no longer required. This changes your published tool definitions; if your server is listed in the ChatGPT or Claude directories, those platforms may re-review the updated tools.
