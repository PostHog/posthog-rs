---
cargo/posthog-rs: minor
---

Add experimental MCP analytics for servers built on `rmcp`, behind the new `mcp` feature (Rust 1.88+). `posthog_rs::mcp::instrument(server, client)` sends each tool call, tool listing and handshake to PostHog as `$mcp_*` events, with `$lib` `posthog-rs-mcp`, following PostHog's cross-SDK MCP analytics spec. A manual API covers servers that cannot be wrapped. Unsupported for now.
