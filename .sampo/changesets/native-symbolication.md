---
posthog-rs: minor
---

Native symbolication for error tracking: captured exceptions and panics now attach each frame's `instruction_addr` and an event-level `$debug_images` list, so PostHog can symbolicate native (Rust/C/C++) stack frames server-side against symbols uploaded with `posthog-cli`. Debug ids match the server/CLI convention (GNU build id on ELF, `LC_UUID` on Mach-O, GUID+age on Windows PDB). Behind the default-on `error-tracking` feature.
