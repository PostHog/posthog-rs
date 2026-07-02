# posthog-rs

## 0.17.1 — 2026-07-02

### Patch changes

- [c7e2533](https://github.com/posthog/posthog-rs/commit/c7e2533cf7ccda39a05da5d6bd138b7cf51da4e2) Retry remote feature flag requests after transient 502 and 504 responses. — Thanks @marandaneto!

## 0.17.0 — 2026-07-02

### Minor changes

- [f00e91b](https://github.com/posthog/posthog-rs/commit/f00e91b578972002fa8e995d1353a0177470220e) General `on_error` observability hook. Register one or more hooks with `ClientOptionsBuilder::on_error` to observe terminal failures across every SDK network surface without giving up the non-blocking API: a capture batch the SDK gave up delivering (permanent reject, exhausted retries, serialization failure, or — on the `capture-v1` pipeline — a `2xx` whose per-event verdicts left events unpersisted), a failed remote `/flags` request, and a failed local-evaluation definitions poll. Hooks fire in registration order and may be invoked from whichever thread reaches the failure (the background transport worker, a flags request, or the poller).
  
  Each hook receives a `&PostHogError<'_>`, a `#[non_exhaustive]` enum with one variant per surface (`Capture`, `FeatureFlags`, `LocalEvaluation`), each carrying an enriched, `#[non_exhaustive]` failure struct read through accessors: `CaptureFailure` (cause, HTTP status, attempt, lost-event count, historical-migration flag, and — under `capture-v1` — request id, per-event `event_results`, and the parsed `V1ErrorResponse`), `FlagsFailure` (cause, endpoint, distinct id, status, body), and `LocalEvaluationFailure` (cause, status; the personal API key is never surfaced). The failure is passed by reference and borrows from the caller, so the common (hookless) path stays allocation-free. New public exports: `PostHogError`, `CaptureFailure`, `FlagsFailure`, `LocalEvaluationFailure`, and (under `capture-v1`) `V1ErrorResponse`.
  
  The hook is observability-only and MUST NOT call back into the SDK (no `capture`/`capture_batch`/`capture_exception`, `flush`, or `shutdown`): emitting an event while handling a capture failure forms an amplification loop. The hook is `Fn + Send + Sync` and invoked without holding any SDK lock, so it may run concurrently on multiple threads and must be internally thread-safe. Hook panics are caught and ignored. Registering at least one hook silences the default WARN logged for terminal capture batch rejects/exhaustion, since the caller now owns that signal; other drop warnings and the existing `/flags` and poller logs are unaffected.
  
  Also makes the blocking local-evaluation poller's background thread shut down promptly: it now wakes to check the stop signal in short steps instead of sleeping the full poll interval, so `stop`/`Drop` no longer blocks for up to a whole interval (previously up to the configured `poll_interval_seconds`). — Thanks @eli-r-ph!

## 0.16.0 — 2026-07-01

### Minor changes

- [c2766cd](https://github.com/posthog/posthog-rs/commit/c2766cd72f861d4b7e9f5eaef831bebfb72ceb04) Native symbolication for error tracking: captured exceptions and panics now attach each frame's `instruction_addr` and an event-level `$debug_images` list, so PostHog can symbolicate native (Rust/C/C++) stack frames server-side against symbols uploaded with `posthog-cli`. Debug ids match the server/CLI convention (GNU build id on ELF, `LC_UUID` on Mach-O, GUID+age on Windows PDB). Behind the default-on `error-tracking` feature. — Thanks @cat-ph!

## 0.15.1 — 2026-06-30

### Patch changes

- [91dfe67](https://github.com/posthog/posthog-rs/commit/91dfe677b9cbb582d4ecd7f36bb693ab340561ed) Include canonical feature flag request fields for remote evaluation, including person/group properties, GeoIP disable, and flag key filtering. — Thanks @marandaneto!

## 0.15.0 — 2026-06-29

### Minor changes

- [4294c51](https://github.com/posthog/posthog-rs/commit/4294c51e3a8f099cbe81b337da0280077f7248c2) Panic autocapture (opt-in). Set `ErrorTrackingOptions::capture_panics` to `true` and initialize the global client with `init_global`, and the SDK installs a process-wide `std::panic` hook that captures panics as personless `$exception` events through the global client, then calls the previously installed hook. Each event carries the panic payload, the panic-site location (`$exception_panic_file`/`_line`/`_column`), and a call-site stack trace honoring `capture_stacktrace`. Capture routes through the background worker thread, so it needs no async runtime and a panicking `before_send` hook can't abort the process. Gated by the default-on `error-tracking` feature.
  
  Panic autocapture is **global-only**: a panic hook is process-global (`std::panic::set_hook`), so it pairs with the process-global client. There is intentionally no per-`Client` panic API for now.
  
  New `ErrorTrackingOptions`: `capture_panics` (default `false`). The flush the hook performs on the panicking thread is bounded by a fixed, short timeout (2s) so a slow or unreachable PostHog can't freeze the crashing process or delay its panic message.
  
  Delivery is best-effort: the just-captured `$exception` is flushed ahead of queued retries, but under sustained backpressure (a slow/unreachable endpoint or a large capture backlog) it may not be sent within the bound before the process exits. — Thanks @cat-ph!

## 0.14.3 — 2026-06-29

### Patch changes

- [447ed07](https://github.com/posthog/posthog-rs/commit/447ed07512b76a471bc4598337cb9f6453b992f0) Respect Retry-After as minimum retry delay — Thanks @marandaneto!
- [650746c](https://github.com/posthog/posthog-rs/commit/650746cfc7b5169f7955edcffff520a28c484e26) Route feature flag called events through the normal capture transport. — Thanks @marandaneto!

## 0.14.2 — 2026-06-27

### Patch changes

- [10810b1](https://github.com/posthog/posthog-rs/commit/10810b182d68787b3169d586600594d515c8e02f) Type-coerce `capture-v1` options before placing them on the wire. A caller value whose type doesn't match the backend's strict `Options` schema is now coerced when possible, or dropped (backend applies its default) rather than rejecting the whole batch. No effect on default (v0) capture behavior. — Thanks @eli-r-ph!

## 0.14.1 — 2026-06-24

### Patch changes

- [e447b36](https://github.com/posthog/posthog-rs/commit/e447b3693fd84942d9f27f22c9711a55fa6b0f4b) Harden the transport's in-flight event counter against underflow. The counter is decremented from several paths (before_send drops, partial v1 batch results, terminal outcomes, shutdown drops, channel drain); a decrement bug on any of them would previously underflow the `AtomicUsize` and wrap to a huge value, making the bounded queue look permanently full and silently dropping every subsequent event. Decrements now saturate at 0 (with a `debug_assert` to surface the bug in tests), so a release build degrades gracefully instead of wedging the queue. — Thanks @eli-r-ph!

## 0.14.0 — 2026-06-24

### Minor changes

- [155d00a](https://github.com/posthog/posthog-rs/commit/155d00aa05019dd4a56412f7a432f55475e0b2f6) Runtime-independent background event transport. `capture` and `capture_batch` are now non-blocking enqueues onto a background worker — a plain `std::thread` with a blocking HTTP client, independent of any async runtime — that batches events, retries transient failures with backoff (honoring `Retry-After`), and sends them. They no longer block on the network or return delivery errors.
  
  New public API: `flush()` (awaited on the async client, blocking on the blocking client), `shutdown()` (flush + stop the worker + join; idempotent; drops further captures), and flush-on-`Drop`. New `ClientOptions`: `flush_at`, `max_batch_size`, `flush_interval_ms`, `max_queue_size` (a bounded queue that drops with a single warning when full), and `shutdown_timeout_ms` (bounds the shutdown/`Drop` drain). `before_send` hooks now run on the worker thread, so they apply to every queued event.
  
  Breaking change (0.x): `capture` and `capture_batch` no longer return a `Result` — and are no longer `async` on the async client. They enqueue the event and return immediately (infallibly) instead of awaiting delivery; transient HTTP failures surface as logged warnings rather than `Err`. Call `flush()` or `shutdown()` before process exit to ensure queued events are delivered. — Thanks @cat-ph!

## 0.13.3 — 2026-06-23

### Patch changes

- [911ceae](https://github.com/posthog/posthog-rs/commit/911ceae82b421ac1c0b1a96f73999638a1240f6b) Internal changes to the `capture-v1` feature path. No effect on default (v0) capture behavior. — Thanks @eli-r-ph!

## 0.13.2 — 2026-06-18

### Patch changes

- [a4d9724](https://github.com/posthog/posthog-rs/commit/a4d9724bfcdf5ab8167f48541fb4f6205bd2727d) Remove duplicated per-event API keys from v0 batch payloads. — Thanks @marandaneto!

## 0.13.1 — 2026-06-15

### Patch changes

- [8a291b6](https://github.com/posthog/posthog-rs/commit/8a291b67a9088342f5f2d2c8414fe60815e15532) Send the default SDK User-Agent on V0 capture requests. — Thanks @marandaneto!

## 0.13.0 — 2026-06-15

### Minor changes

- [84d06a1](https://github.com/posthog/posthog-rs/commit/84d06a1d68c894f3752e1268b4fc39f1350b966f) Requests now use a client-specific user-agent — Thanks @marandaneto!

## 0.12.0 — 2026-06-12

### Minor changes

- [d35ffa7](https://github.com/posthog/posthog-rs/commit/d35ffa7302f158a9aab7fe38507fabcd504d0daf) Add manual Rust error tracking capture APIs, enabled by default via the `error-tracking` feature. — Thanks @cat-ph for your first contribution 🎉!

## 0.11.0 — 2026-06-11

### Minor changes

- [b84ec43](https://github.com/posthog/posthog-rs/commit/b84ec433bb1f2049673442a6e58e41c4cb92bc08) Add OS runtime context properties to captured events. — Thanks @marandaneto!
- [dba50e1](https://github.com/posthog/posthog-rs/commit/dba50e13c275edd3326b1762afab22796acfca27) Add before_send hooks for mutating or dropping events before capture. — Thanks @marandaneto!

## 0.10.2 — 2026-06-10

### Patch changes

- [7131402](https://github.com/posthog/posthog-rs/commit/713140296c579472025ef7ee42a6ca504c2fffcb) `capture_batch` with an empty event list is now a no-op on both clients and
  both capture paths — no HTTP request is sent (the v1 backend rejects empty
  batches). Also derives `Debug` for the internal retry `Step` and pins that a
  body-less 2xx response is a terminal serialization error, not a retry. — Thanks @eli-r-ph!

## 0.10.1 — 2026-06-10

### Patch changes

- [4a6c67c](https://github.com/posthog/posthog-rs/commit/4a6c67c3d937f185993f256ddda2c087f5020980) Three fixes to the unstable `capture-v1` pipeline (off by default):
  
  - Accept any 2xx HTTP status as success on V1 capture responses instead of exactly 200, so a future 201/202/207 from capture is not misclassified as a connection error. Malformed bodies on 2xx still surface as `Error::Serialization`.
  - Send the canonical SDK identity `posthog-rs/<version>` (previously `posthog-rust/<version>`) in the `posthog-sdk-info` and `user-agent` headers. The name segment now matches the `$lib` value the v0 path sends, so capture-side `$lib`/`$lib_version` materialization attributes V1 traffic correctly in SDK Health, usage reports, and Library columns.
  - Route `$feature_flag_called` events through the V1 analytics endpoint when `capture-v1` is enabled (previously they always took the legacy v0 path, splitting the pipeline). Shipping stays fire-and-forget with a single attempt and no retry loop, matching the v0 flag-event semantics on both the async and blocking clients. — Thanks @eli-r-ph!

## 0.10.0 — 2026-06-05

### Minor changes

- [b4a60a7](https://github.com/posthog/posthog-rs/commit/b4a60a7d6a62c1ccd9ae2dc59983b81db99c2329) feat(feature-flags): support `early_exit` in local evaluation
  
  Adds support for the `early_exit` option on a feature flag's `filters` during local evaluation. When `early_exit` is `true` and a condition group's property filters match (or it has no property filters) but the rollout percentage excludes the user, evaluation now stops and returns a definitive disabled result instead of falling through to later condition groups. A property-filter mismatch always falls through, regardless of `early_exit`. The flag defaults to `false` (absent), preserving the existing fall-through behavior. This mirrors the PostHog Rust evaluation engine and the posthog-node/posthog-python implementations. — Thanks @gustavohstrassburger!

## 0.9.1 — 2026-06-05

### Patch changes

- [f757768](https://github.com/posthog/posthog-rs/commit/f7577685d0285d9db4dddd3a2debfcfb203e469b) Serialize the v0 capture `distinct_id` at the event root (canonical field) instead of the legacy `$distinct_id` alias, matching v1 and the v0 ingestion contract. Add retries to the v0 capture paths: transport errors and 408/500/502/503/504 are retried with exponential backoff honoring `Retry-After`, and a 429 is retried only when it carries a `Retry-After` (a bare 429 stays a terminal rate-limit). Retried requests resend the same bytes, preserving the event UUID and timestamp that dedup relies on. Add opt-in gzip compression for v0 capture via `capture_compression`: the body is gzipped with a `Content-Encoding: gzip` header and a `compression=gzip` query param (capture reads the query param on v0).
  
  Fix the retry backoff timing on both v0 and v1: the first retry now waits exactly `retry_initial_backoff_ms` instead of double it (the call sites previously passed `attempt + 1` into the backoff calculation, skipping the configured initial delay). — Thanks @eli-r-ph!

## 0.9.0 — 2026-06-04

### Minor changes

- [79673c3](https://github.com/posthog/posthog-rs/commit/79673c3f37286a4958c16eb4ca3d4bc7d7ea13f2) Add V1 capture pipeline (`/i/v1/analytics/events/`) behind the unstable `capture-v1` Cargo feature (off by default). Includes gzip/deflate/br/zstd compression, automatic partial-batch retry with exponential backoff, per-event options (cookieless mode, skew correction, person profile, product tour), and historical migration support. A separate `test-harness` feature enables injecting extra request headers for compliance test isolation. — Thanks @eli-r-ph for your first contribution 🎉!

## 0.8.0 — 2026-06-03

### Minor changes

- [edd3797](https://github.com/posthog/posthog-rs/commit/edd3797f5f0b3f4db707a9078163d56858e2d1d8) Add a configurable `$is_server` event property (default `true`) so PostHog can identify server-side events. Set `is_server` to `false` when using posthog-rs as a client/CLI so the device OS is attributed normally. — Thanks @turnipdabeets for your first contribution 🎉!

## 0.7.3 — 2026-05-28

### Patch changes

- [af5b4da](https://github.com/posthog/posthog-rs/commit/af5b4daa591d1501d5394d5fdceb5f6383e95b43) Include group context in the `$feature_flag_called` dedupe key so group-scoped flags fire a separate event for each group a user is evaluated under, instead of being dedup-ed against the first group context the same `(distinct_id, flag, response)` was seen under. — Thanks @gustavohstrassburger for your first contribution 🎉!

## 0.7.2 — 2026-05-28

### Patch changes

- [deb361b](https://github.com/posthog/posthog-rs/commit/deb361b68fe3d8c3196b3171ad553b061df2477c) Disable the client instead of sending requests when the API key is missing or blank. — Thanks @marandaneto!

## 0.7.1 — 2026-05-21

### Patch changes

- [7cd4efe](https://github.com/posthog/posthog-rs/commit/7cd4efe41a0960d6b30d61d5b26828d1160cc220) fix(flags): reject semver values with leading zeros in local evaluation
  
  Per semver 2.0.0 §2, numeric identifiers must not include leading zeros. Values like `1.07.3` are not valid semver and no longer match targeting conditions. Both override values and flag values are validated; invalid inputs surface an `InconclusiveMatchError` so the condition does not match. — Thanks @dmarticus!

## 0.7.0 — 2026-05-05

### Minor changes

- [db601db](https://github.com/posthog/posthog-rs/commit/db601db2a0561d55954993daf1d0cfe22853c1a5) feat(flags): support group-targeted and mixed-targeting feature flags in local evaluation
  
  Adds local evaluation support for pure group flags (where `aggregation_group_type_index` is set at the flag level) and mixed-targeting flags (where individual conditions can override the aggregation per condition). `LocalEvaluator::evaluate_flag`, `evaluate_flag_simple`, and `evaluate_all_flags` now take `groups` and `group_properties` parameters; `match_feature_flag` and `match_feature_flag_with_context` have updated signatures. Backwards-incompatible at the public-API level — see PR description for migration notes. — Thanks @patricio-posthog!

## 0.6.0 — 2026-05-01

### Minor changes

- [7950daf](https://github.com/posthog/posthog-rs/commit/7950dafc9060d7b258b3d12997edb5176134a547) Add `evaluate_flags(distinct_id, options)` for single-call snapshot-based feature flag evaluation. Returns a `FeatureFlagEvaluations` whose `is_enabled` / `get_flag` / `get_flag_payload` methods read from the cached evaluation. `is_enabled` and `get_flag` fire deduplicated `$feature_flag_called` events with full metadata (`$feature_flag_id`, `$feature_flag_version`, `$feature_flag_reason`, `$feature_flag_request_id`). Pass the snapshot to `Event::with_flags(&snapshot)` to attach `$feature/<key>` and `$active_feature_flags` to a captured event without an extra `/flags` call.
  
  When `flag_keys` is provided and local evaluation already covers every requested key, `evaluate_flags` skips the `/flags` round-trip entirely. If the remote `/flags` call fails after some flags were resolved locally, the snapshot is still returned with the local results (degraded), with `errors_while_computing_flags` propagated to subsequent `$feature_flag_called` events. String-encoded `metadata.payload` values from `/flags?v=2` are normalized to parsed JSON.
  
  Deprecates the legacy single-flag methods in favor of `evaluate_flags()`:
  
  - `Client::get_feature_flag`
  - `Client::is_feature_enabled`
  - `Client::get_feature_flag_payload`
  
  The methods continue to work but emit a `#[deprecated]` compile warning pointing at `evaluate_flags()`. They will be removed in a future major version. — Thanks @dmarticus!

## 0.5.3 — 2026-04-21

### Patch changes

- [c544e14](https://github.com/posthog/posthog-rs/commit/c544e145039ab7b6c9a4843983291d5cf44f7f70) feat(flags): switch local evaluation polling from `/api/feature_flag/local_evaluation` to `/flags/definitions` — Thanks @patricio-posthog!

## 0.5.2 — 2026-04-21

### Patch changes

- [db48ade](https://github.com/posthog/posthog-rs/commit/db48adecb5b6fdaa9544fe15bc002dd3dc06909a) Trim surrounding whitespace from API keys and host config before using them. — Thanks @marandaneto!

## 0.5.1 — 2026-04-14

### Patch changes

- [543ca47](https://github.com/posthog/posthog-rs/commit/543ca47c0d1c1365ca13a1882ea7089851bef19a) Update reqwest from 0.11.3 to 0.13.2 to replace the unmaintained feature "rustls-tls" with "rustls" (RUSTSEC-2025-0134) — Thanks @marandaneto!

## 0.5.0 — 2026-03-27

### Minor changes

- [842bb73](https://github.com/posthog/posthog-rs/commit/842bb73c17c9fce63df04d01d93a0c78b91e1a63) Add `local_evaluation_only` option to prevent remote API fallback when local evaluation is inconclusive — Thanks @dmarticus!

## 0.4.7 — 2026-03-20

Note: 0.4.4 - 0.4.6 were never released due to a bug in our release process. Commit signing is hard!

### Patch changes

- [6a04431](https://github.com/posthog/posthog-rs/commit/6a04431ecd66d85360500e15dbc28a19bb349d21) Sign commits during release process — Thanks @marandaneto!

## 0.4.3 — 2026-03-05

### Patch changes

- [b1b109d](https://github.com/posthog/posthog-rs/commit/b1b109dcdc52d9a0fd72268a44b3e367e593d8b1) Add semver comparison operators for local feature flag evaluation — Thanks @dmarticus!

## 0.4.2 — 2026-02-23

### Patch changes

- [d94ecbb](https://github.com/posthog/posthog-rs/commit/d94ecbb4e6960e775f18f7b81664c18fa35ddc12) Historical batch capture support — Thanks @z0br0wn!
- [6af1786](https://github.com/posthog/posthog-rs/commit/6af178641092740ac1cca24f08d1a1fc760f2cb1) Add Capture API response handling — Thanks @z0br0wn!
- [3fdab70](https://github.com/posthog/posthog-rs/commit/3fdab70a3a6b2310ed1f7772cf742b184104bedf) Generate (and allow overrides of) event UUID, allow for properties pass through. — Thanks @z0br0wn!

## 0.4.1 — 2026-02-18

### Patch changes

- [c15b195](https://github.com/posthog/posthog-rs/commit/c15b195728be26de67a66d64d04ada7e3b729351) Migrate release process to Sampo for automated versioning, changelogs, and publishing. — Thanks @rafaeelaudibert!

## 0.4.0 - 2026-02-04

- Add feature flags with local evaluation support (#36)
- Add group support and anonymous event support (#22)
- Add global disable function (#20)
- Add global client functions (#19)
- Add timestamp setter (#30)

## 0.2.0

- Add generic properties (#1)
- Derive Debug, PartialEq, and Eq on Event
