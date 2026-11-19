# posthog-rs-ffi

C bindings for the [PostHog Rust SDK](../../README.md), for C and C++ applications.

The bindings wrap the SDK's blocking client:

- **Capture** queues events and returns immediately. A background thread batches, retries and sends them.
- **Feature flag evaluation** blocks the calling thread while it fetches flags, or evaluates them locally when local evaluation is enabled.

Events report `$lib` as `posthog-rs-ffi` and `$lib_version` as the `posthog-rs-ffi` version.

## Download

Prebuilt archives are attached to each [`posthog-rs-ffi` release](https://github.com/PostHog/posthog-rs/releases?q=posthog-rs-ffi), tagged `posthog-rs-ffi-v<version>`. They cover Linux (x86_64 and arm64, glibc 2.35 or newer), macOS (Intel and Apple silicon) and Windows (x86_64, MSVC). Each archive is named `posthog-rs-ffi-<version>-<target>` and has a `.sha256` file next to it.

```
include/posthog.h
lib/libposthog_ffi.so, .dylib or posthog_ffi.dll (+ posthog_ffi.dll.lib)   shared library
lib/libposthog_ffi.a or posthog_ffi.lib                                    static library
lib/native-static-libs.txt          system libraries the static library needs
lib/cmake/posthog-rs-ffi/           CMake package
lib/pkgconfig/                      pkg-config files (Linux and macOS)
examples/capture.c, capture.cpp
```

Each archive has signed build provenance linking it to the release workflow and commit that built it. Check it before use with the [GitHub CLI](https://cli.github.com/):

```bash
gh attestation verify posthog-rs-ffi-0.1.0-x86_64-unknown-linux-gnu.tar.gz --repo PostHog/posthog-rs
```

`posthog-rs-ffi` has its own version, independent of the `posthog` crate. Events report it as `$lib_version`.

## Linking

### CMake

Unpack the archive, point `CMAKE_PREFIX_PATH` at it, and link the imported target:

```cmake
find_package(posthog-rs-ffi 0.1 CONFIG REQUIRED)
target_link_libraries(app PRIVATE posthog-rs-ffi::posthog-rs-ffi)
```

```bash
cmake -S . -B build -DCMAKE_PREFIX_PATH=/path/to/posthog-rs-ffi-0.1.0-x86_64-unknown-linux-gnu
```

`posthog-rs-ffi::posthog-rs-ffi` is the shared library. Set `POSTHOG_RS_FFI_STATIC=ON` before `find_package` to link the static library, including the system libraries it needs, instead. You can also link `posthog-rs-ffi::shared` or `posthog-rs-ffi::static` directly.

Before 1.0, a minor version can change the API, so `find_package(posthog-rs-ffi 0.1)` accepts 0.1.x and rejects 0.2.

### pkg-config

On Linux and macOS, add the archive's `lib/pkgconfig` to `PKG_CONFIG_PATH`:

```bash
export PKG_CONFIG_PATH=/path/to/posthog-rs-ffi-0.1.0-x86_64-unknown-linux-gnu/lib/pkgconfig
cc app.c $(pkg-config --cflags --libs posthog-rs-ffi)          # shared
cc app.c $(pkg-config --cflags --libs posthog-rs-ffi-static)   # static
```

### By hand

To link the shared library on Linux or macOS:

```bash
cc app.c -Ipath/to/include -Lpath/to/lib -lposthog_ffi
```

On Windows, link the DLL's import library `posthog_ffi.dll.lib`, not `posthog_ffi.lib`, which is the static library:

```bat
cl app.c /Ipath\to\include path\to\lib\posthog_ffi.dll.lib
```

To link the static library, add the flags in `lib/native-static-libs.txt`, for example `-lgcc_s -lutil -lrt -lpthread -lm -ldl -lc` on Linux.

### Shipping the shared library

The shared library has to be found at run time. Ship it with your app and point the loader at it:

- **Linux:** link with an rpath such as `-Wl,-rpath,'$ORIGIN'`, or install it in a system library directory.
- **macOS:** the library's install name is `@rpath/libposthog_ffi.dylib`, so link with an rpath such as `-Wl,-rpath,@executable_path`.
- **Windows:** put `posthog_ffi.dll` next to your executable or in a directory on `PATH`.

The static library avoids this, at the cost of a larger binary.

## Build from source

To build for another platform or toolchain, you need Rust 1.78 or newer. From the repository root:

```bash
cargo build --release -p posthog-rs-ffi
```

The libraries are written to the repository's `target/release` directory, and the header is checked in at `crates/posthog-rs-ffi/include/posthog.h`. To list the system libraries the static library needs:

```bash
cargo rustc --release -p posthog-rs-ffi --lib --crate-type staticlib -- --print native-static-libs
```

To build for another platform, pass `--target`, such as `cargo build --release -p posthog-rs-ffi --target x86_64-pc-windows-msvc`. `packaging/package.sh` builds the same archive the release ships, from a release build.

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
- **Delivery failures:** `posthog_capture` returns `POSTHOG_STATUS_OK` once the event is queued. If PostHog later rejects the batch, or sending runs out of retries, the event is dropped and the C API doesn't report it. Flag requests run while you wait, so their failures come back as `POSTHOG_STATUS_REQUEST`.
- **Not in the C API yet:** the SDK's `before_send` and `on_error` hooks, AI events, and transactional email.

## Development

After changing a public function in `src/lib.rs`, regenerate the header:

```bash
cargo install cbindgen --version 0.29.4 --locked
cbindgen --config cbindgen.toml --output include/posthog.h
```

CI fails if `include/posthog.h` doesn't match the source.

`posthog-rs-ffi` is a member of the repository's Cargo workspace. Build and test it with `-p posthog-rs-ffi` (or from `crates/posthog-rs-ffi`), not with `--workspace`: a workspace-wide build enables the SDK's `async-client` feature, and that replaces the blocking client the bindings wrap.
