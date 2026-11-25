---
cargo/posthog: patch
---

MCP analytics no longer starts a new stdio session while a tool call is still running: the 30-minute inactivity rotation now counts from the end of the last call only once no call is in flight, and a cancelled call counts as ended even when its tool keeps running.
