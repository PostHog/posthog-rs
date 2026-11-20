# posthog-rs-ffi

## 0.1.0 — 2026-11-20

### Minor changes

- [e424052](https://github.com/posthog/posthog-rs/commit/e42405262c7f55f306d0c5ebac391d3b6d113c08) Add C bindings for C and C++ applications. Each `posthog-rs-ffi` release attaches prebuilt archives for Linux, macOS and Windows to its GitHub Release, with the shared and static libraries, the `posthog.h` header and C and C++ examples. The bindings wrap the blocking client and report events with `$lib` set to `posthog-rs-ffi` and `$lib_version` set to the `posthog-rs-ffi` version. — Thanks @marandaneto!

### Patch changes

- Updated dependencies: posthog@1.3.0

