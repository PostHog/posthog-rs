# Rust SDK compliance profiles

CI uses harness **1.0.0** with server-wire assertions and parallel test isolation.
The adapter links the in-tree `posthog-rs` crate and forwards capture, timestamp,
flag evaluation, flush and shutdown through its public APIs. `test-harness`
enables the existing queue-depth observation and test-ID headers.

| Profile | SDK entry | Selection | Expected cases |
| --- | --- | --- | ---: |
| V0 gzip | Async client (default features) | Capture V0 + flags | 47 |
| V1 gzip | Async client + `capture-v1` | Capture V1 + flags | 112 |
| V1 deflate / br / zstd | Async client + `capture-v1` | Capture V1 + flags, per codec | 111 each |
| Blocking flags | Blocking client (`--no-default-features`) | `feature_flags` | 17 |

Both capture suites include non-UTC timestamp override coverage. The blocking
profile uses `Client::evaluate_flags` followed by the SDK snapshot's `get_flag`;
request construction, retries, result parsing and `$feature_flag_called` remain
SDK-owned. Its ordinary and side-effect captures use the shared background
transport, so it does not duplicate the full capture suite. Blocking operations
and client destruction run in Tokio's blocking context.

`COMPRESSION` selects a process codec; `/init` only enables that codec when
`enable_compression` is true. Use `gzip` for V0; the other codecs require V1.
The disabled-compression assertion runs within each V1 codec profile. Separate
Dockerfiles configure the V1 launches because the pinned reusable workflow does
not accept adapter environment or build arguments. Each profile has a distinct
artifact name and health name (used for the workflow's PR report comment).
Compliance remains advisory; ordinary SDK build/test gates are unchanged.

## Local runs

From the repository root:

```sh
cargo build --locked --manifest-path compliance/adapter/Cargo.toml
COMPRESSION=gzip PORT=18240 target/debug/sdk-adapter

# Build V1 instead:
cargo build --locked --manifest-path compliance/adapter/Cargo.toml --features capture-v1
# Launch with COMPRESSION=gzip, deflate, br or zstd.

# Build the blocking flags profile instead:
cargo build --locked --manifest-path compliance/adapter/Cargo.toml --no-default-features
```

Use the adapter manifest when selecting features: the workspace's Cargo resolver
applies root-level `--no-default-features` to the root package, not the adapter.

Run harness 1.0.0 against that listener with `--sdk-type server` and
`--concurrency 10`; add `--suite feature_flags` for the blocking build. Choose a
free mock port with `--mock-port` and `--mock-url`. `PORT` defaults to 8080.
The V0/V1 Compose files run capture and flags; the V1 file accepts
`COMPRESSION=deflate docker compose -f compliance/v1/docker-compose.yml up --build`
(and similarly `br` or `zstd`).

## Coverage limits

Non-gzip V1 codec cases check **Content-Encoding headers only**. Harness 1.0.0
does not decode those bodies or provide meaningful compressed per-event results;
these passes are not decoded-delivery or partial-response certification.
Dedicated AI and local flag evaluation are not covered.

Wire-suite success does not establish full adapter-interface compliance. The
existing capture response UUID is separate from the SDK-generated wire UUID;
health reports the adapter package version; state uses queue-depth estimates
rather than confirmed deliveries and retries. Flush forwards the SDK's single
attempt barrier, not a terminal retry-completion barrier. These interface fields
are not asserted by the selected wire definitions.
