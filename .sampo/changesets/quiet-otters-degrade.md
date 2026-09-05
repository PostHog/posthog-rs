---
cargo/posthog-rs: patch
---

Do not panic when the HTTP client cannot be built. A container without CA certificates makes the reqwest builder fail, which stopped the calling program. The client now logs a warning, disables itself, and lets the program continue.
