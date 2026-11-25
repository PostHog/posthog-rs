---
cargo/posthog: minor
---

Events an MCP tool captures through `Client::capture` while its call runs carry that call's `$session_id`, `$mcp_session_source` and, when `identify` resolves one, its distinct ID and `$groups`, even when other calls run on the same server meanwhile.
