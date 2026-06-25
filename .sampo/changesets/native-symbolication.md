---
posthog-rs: minor
---

Native symbolication capture for error tracking. Captured exceptions (both manual `capture_exception` and automatic panic capture) now attach the data PostHog needs to symbolicate native (Rust/C/C++) stack frames server-side against debug symbols uploaded with `posthog-cli`.

Each stack frame carries its absolute `instruction_addr` (plus `symbol_addr`/`image_addr` when known), and the event gains a `$debug_images` property listing the loaded modules referenced by the trace — `debug_id`, `code_id`, image base address, size, virtual address, code file, and a normalized `arch`. Frames are tagged `platform: "native"`. In stripped builds, frames are emitted as address-only entries so the server can still resolve them.

Debug ids are computed to match the server and CLI (`symbolic`): GNU build ids on ELF, `LC_UUID` on Mach-O (dSYM-cased), and GUID+age on Windows PDB. Addresses and debug-image linkage are omitted whenever no uploadable debug image matches a frame, so events stay clean when symbols can't be uploaded.

Gated behind the default-on `error-tracking` feature; pulls in the `findshlibs` dependency for loaded-module enumeration.
