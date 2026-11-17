---
cargo/posthog: minor
---

Add `reload_feature_flags` to the async and blocking clients. It fetches local feature flag definitions now and returns once they are in use, or with the error that kept them from loading. Before, a client could only wait for its next poll.
