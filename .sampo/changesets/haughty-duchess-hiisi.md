---
cargo/posthog: patch
---

Stop one unreadable flag definition from disabling local evaluation for the whole project.

The flag pollers rejected a definitions response when any single flag definition failed to deserialize, so no flag loaded. A response that deserializes as a whole is handled as before. When it does not, each flag definition is now tried on its own: the ones that fail are skipped with a warning naming the flag, and the rest load. A skipped flag is absent from the local cache, so it falls back to `/flags` like any other flag local evaluation cannot resolve. A response that is unreadable for any other reason is still rejected.
