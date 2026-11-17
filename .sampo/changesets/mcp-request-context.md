---
cargo/posthog: patch
---

MCP events sent over HTTP now take the `RequestContext` that `PostHogLayer` or the application placed in the request extensions, so their context properties and options are no longer lost when `rmcp` runs the handler in its own task. The MCP identity stays the distinct ID, and an anonymous MCP event keeps `process_person_profile: false`. The `McpOptions` `before_send` hook now sees the context values.
