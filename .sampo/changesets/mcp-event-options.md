---
cargo/posthog: minor
---

MCP analytics can set capture options on its events. `McpOptionsBuilder::event_options` takes a callback that returns options for each request's events, and `ToolCallRequest::event_option` sets an option on one call's events in the manual API. The call's option wins over the callback's, both win over the request context, and an anonymous MCP event keeps `process_person_profile: false`. The `McpOptions` hook and the client's `before_send` see the options and can change them.
