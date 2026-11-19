# posthog-ffi

C bindings for the [PostHog Rust SDK](../../README.md), for C and C++ applications.

The bindings wrap the SDK's blocking client:

- **Capture** queues events and returns immediately. A background thread batches, retries and sends them.
- **Feature flag evaluation** blocks the calling thread while it fetches flags, or evaluates them locally when local evaluation is enabled.

Events report `$lib` as `posthog-rs-ffi`. `$lib_version` is the version of the Rust SDK the bindings are built from.

## Download

Each [posthog-rs release](https://github.com/PostHog/posthog-rs/releases) attaches prebuilt archives for Linux (x86_64 and arm64, glibc 2.35 or newer), macOS (Intel and Apple silicon) and Windows (x86_64, MSVC). The archives are named `posthog-ffi-<version>-<target>`. Each contains the shared and static libraries, `include/posthog.h`, the examples, and `lib/native-static-libs.txt`, which lists the system libraries to link with the static library.

The bindings share the `posthog` crate's version, so `posthog-ffi-1.2.0` wraps `posthog` 1.2.0.

## Build

To build from source, or for another platform, you need Rust 1.78 or newer.

```bash
cd crates/posthog-ffi
cargo build --release
```

This builds:

- `target/release/libposthog_ffi.{so,dylib}` or `posthog_ffi.dll`: the shared library
- `target/release/libposthog_ffi.a` or `posthog_ffi.lib`: the static library
- `include/posthog.h`: the header (checked in)

### Linking

To link the shared library:

```bash
cc app.c -Ipath/to/include -Lpath/to/target/release -lposthog_ffi
```

At run time, the loader must find the library. Install it in a system library directory, link with an rpath, or set `LD_LIBRARY_PATH` on Linux or `DYLD_LIBRARY_PATH` on macOS.

The static library also needs the system libraries the SDK uses. Print them for your target with:

```bash
cargo rustc --release --lib --crate-type staticlib -- --print native-static-libs
```

On macOS, for example, that is `-framework CoreFoundation -framework Foundation -framework Security -lobjc -liconv`.

To build for another platform, pass `--target`, such as `cargo build --release --target x86_64-pc-windows-msvc`.

## Usage

```c
#include "posthog.h"

PostHogOptions *options = posthog_options_new("phc_project_api_key");
posthog_options_set_host(options, "https://eu.i.posthog.com");
posthog_options_set_is_server(options, false); // desktop and CLI apps
PostHogClient *client = posthog_client_new(options);
posthog_options_free(options);

PostHogEvent *event = posthog_event_new("file_exported", "user-123");
posthog_event_set_property_string(event, "format", "pdf");
posthog_event_set_property_int(event, "pages", 12);
posthog_event_set_property_json(event, "$set", "{\"plan\":\"pro\"}");
posthog_capture(client, event);
posthog_event_free(event);

PostHogFlags *flags = NULL;
if (posthog_evaluate_flags(client, "user-123", NULL, &flags) == POSTHOG_STATUS_OK) {
    bool enabled = false;
    posthog_flags_is_enabled(flags, "new-export-dialog", &enabled);
    posthog_flags_free(flags);
}

// Delivers queued events, then frees the client.
posthog_client_free(client);
```

See [`examples/capture.c`](examples/capture.c) for a complete C program. [`examples/capture.cpp`](examples/capture.cpp) is the same program in C++17. It wraps the handles in `std::unique_ptr` and turns error statuses into exceptions, a pattern you can copy into your app.

### Errors

Functions return a `PostHogStatus`, and constructors return `NULL` on error. `posthog_last_error()` describes the last error on the calling thread. The SDK never unwinds into your code. If it panics, the call returns `POSTHOG_STATUS_PANIC`, and it may have partly run.

### Memory and threads

- **Handles:** free every handle from a `*_new` function with the matching `*_free` function.
- **Strings:** free every `char *` the SDK writes to an `_out` argument with `posthog_string_free`.
- **Inputs:** input strings are borrowed for the duration of the call and must be NUL-terminated UTF-8.
- **Sharing:** you can share a `PostHogClient` or `PostHogFlags` between threads. Use each `PostHogOptions` and `PostHogEvent` from one thread at a time.

### Feature flag options

`posthog_evaluate_flags` accepts an optional JSON object:

```json
{
  "groups": { "company": "acme" },
  "person_properties": { "plan": "pro" },
  "group_properties": { "company": { "size": 50 } },
  "only_evaluate_locally": false,
  "disable_geoip": true,
  "flag_keys": ["new-export-dialog"]
}
```

Local evaluation is for servers you run. Set a project secret key (`phs_...`) with `posthog_options_set_secret_key` and call `posthog_options_set_enable_local_evaluation(options, true)`.

Never put a secret key in an application you distribute, such as a desktop app or CLI. Anyone with the binary or its configuration can extract the key and read your flag definitions. Distributed apps should use only the project API key (`phc_...`), which evaluates flags remotely.

### Not covered

- **Error tracking:** the bindings don't capture C or C++ crashes. Send your own `$exception` events with `posthog_capture`, or use a crash reporter.
- **Not in the C API yet:** the SDK's `before_send` and `on_error` hooks, AI events, and transactional email.

## Development

After changing a public function in `src/lib.rs`, regenerate the header:

```bash
cargo install cbindgen --version 0.29.4 --locked
cbindgen --config cbindgen.toml --output include/posthog.h
```

CI fails if `include/posthog.h` doesn't match the source.

`posthog-ffi` is its own Cargo workspace. In the root workspace, Cargo would enable the SDK's `async-client` feature for every member, and that feature replaces the blocking client the bindings wrap.
