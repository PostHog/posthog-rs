---
cargo/posthog: patch
---

`Client::shutdown` now also stops the local evaluation poller, so a shut-down client no longer fetches flag definitions in the background.
