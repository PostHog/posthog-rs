---
cargo/posthog: minor
---

Add C bindings for C and C++ applications. Each release now attaches prebuilt `posthog-ffi` archives for Linux, macOS and Windows to its GitHub Release, with the shared and static libraries, the `posthog.h` header and C and C++ examples. The bindings wrap the blocking client and report events with `$lib` set to `posthog-rs-ffi`.
