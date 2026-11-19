# posthog-rs

## 1.2.0 — 2026-11-19

### Minor changes

- [2f87ed4](https://github.com/posthog/posthog-rs/commit/2f87ed43b0615f2ae45a2df54ec3c8d008c659e7) `Client::get_remote_config_payload(flag_key)` reads the payload of a remote config feature flag, with no distinct ID. It works with the async and the blocking client, and needs the `secret_key` option. A failed request or a non-2xx status returns an `Error`, and a flag with no payload returns `Ok(None)`. — Thanks @ioannisj!

## 1.1.3 — 2026-11-18

### Patch changes

- [a66aefb](https://github.com/posthog/posthog-rs/commit/a66aefbd8b476146aba16b5f61d8b00411fd513d) Fix local flag evaluation for projects with an `is_set` or `is_not_set` filter that has no `value`.
  
  PostHog accepts these two property filters without a `value`, but `Property` required one, so a single such filter made the whole `LocalEvaluationResponse` fail to deserialize and no flags loaded at all. They now deserialize without a `value`, on flags and on cohort leaves. Other operators still require one. — Thanks @shaun-wispr!
- [a66aefb](https://github.com/posthog/posthog-rs/commit/a66aefbd8b476146aba16b5f61d8b00411fd513d) Stop one unreadable flag definition from disabling local evaluation for the whole project.
  
  The flag pollers rejected a definitions response when any single flag definition failed to deserialize, so no flag loaded. A response that deserializes as a whole is handled as before. When it does not, each flag definition is now tried on its own: the ones that fail are skipped with a warning naming the flag, and the rest load. A skipped flag is absent from the local cache, so it falls back to `/flags` like any other flag local evaluation cannot resolve. A response that is unreadable for any other reason is still rejected. — Thanks @shaun-wispr!
- [1b623af](https://github.com/posthog/posthog-rs/commit/1b623afa3dde285143ac5cc142a32692c8860fca) Local flag evaluation no longer stops at the first property filter in a condition it can't check. A later filter that doesn't match now rules the condition out, like the other SDKs, so a flag that can't match is `false` instead of inconclusive and doesn't need a call to `/flags`. — Thanks @luantaraschi!

## 1.1.2 — 2026-11-18

### Patch changes

- [d83cb97](https://github.com/posthog/posthog-rs/commit/d83cb97ccd847f4542ba7d45428e36f8a062fcc6) Send transactional email through PostHog's key-scoped `/api/messaging/` routes, which take the project from the secret key, instead of `/api/projects/@current/messaging/`. Needs PostHog Cloud, or a self-hosted PostHog from 2026-11-18 or later. Older self-hosted versions answer these calls with a 404. — Thanks @rafaeelaudibert!

## 1.1.1 — 2026-11-18

### Patch changes

- [f022a30](https://github.com/posthog/posthog-rs/commit/f022a30c859e248007a464665c0701efed636c21) Local flag evaluation now checks condition groups in the order they are defined, like the other SDKs, instead of moving groups with a variant override to the front. For a flag with a variant override on a group other than the first, some users can now get a different value: the variant from the first group they match, or `false` from an `early_exit` group. With `early_exit`, a rollout exclusion now also stops evaluation after an earlier inconclusive group, so the flag stays inconclusive (and falls back to the server) instead of matching a later group. — Thanks @luantaraschi!

## 1.1.0 — 2026-11-18

### Minor changes

- [3188466](https://github.com/posthog/posthog-rs/commit/3188466a5f56fa6f9c15a67f40656c9138f4e1a5) Add `reload_feature_flags` to the async and blocking clients. It fetches local feature flag definitions now and returns once they are in use, or with the error that kept them from loading. Before, a client could only wait for its next poll. — Thanks @eli-r-ph!

## 1.0.0 — 2026-11-17

### Major changes

- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Mark EvaluateFlagsOptions as non-exhaustive so new options can be added without breaking future minor releases. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) The SDK sends `posthog-rs/<version>` in the `PostHog-Sdk-Info` and `User-Agent` headers on every capture request, and adds `$lib` and `$lib_version` to each event's properties before `before_send` runs. Both are sent as they are at send time, and PostHog stores them. PostHog uses the header only to fill in what an event leaves unset: both properties when `$lib` isn't set, and the version when `$lib` is `posthog-rs` and `$lib_version` isn't set.
  
  A `$lib` counts as set only when it's a non-empty string. If `$lib` isn't set, the SDK sets `$lib` to `posthog-rs` and `$lib_version` to the crate version, replacing any `$lib_version` set on its own. If `$lib` is `posthog-rs` and `$lib_version` isn't set, the SDK adds the version. Any other `$lib` is sent with whatever `$lib_version` the event has, so set both when you name another library. Unlike the default 0.x path, the SDK doesn't add its `$lib_version` or version parts to another `$lib` you set, and it replaces a `null` or empty `$lib`. It adds `$lib_version__major`, `$lib_version__minor` and `$lib_version__patch` as numbers only when it adds its own `$lib_version`. MCP events send `$lib` as `posthog-rs-mcp` with the crate version as `$lib_version`, and no longer carry the version parts. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Assign an event's uuid when the client takes the event, so a cloned or reused `Event` no longer sends one uuid twice. `Event::set_timestamp` no longer rejects future timestamps. `Event::new` with an empty or whitespace-only distinct ID creates an anonymous event. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Set per-event capture options with `Event::insert_option`, or `CaptureExceptionOptions::option` for exceptions. The SDK sends them in an `options` map next to `properties`, unchanged, and PostHog validates them. `Event::alias` and `Event::group_identify` are now public, so you can set options on the events that `alias` and `group_identify` send. They return the new `Error::InvalidArgument` for a blank ID, group type or group key.
  
  Breaking changes:
  
  - Move the `$process_person_profile`, `$cookieless_mode`, `$ignore_sent_at` and `$product_tour_id` properties to the `process_person_profile`, `cookieless_mode`, `disable_skew_correction` and `product_tour_id` options. Legacy option properties still work, but an option overrides its legacy property. Use options.
  - The SDK no longer converts option values. PostHog reads common forms such as `"false"`, `"no"` or `0`, and drops an event whose option value it cannot read. That event reaches `on_error` as a per-event `drop` with the detail `invalid_options`.
  - Adding a group no longer turns on person processing. An anonymous event with groups stays personless; set the `process_person_profile` option to `true` if you need it in group analytics.
  - `Event::new_anon` sets the `process_person_profile` option to `false`. Set the option to `true` to process person profiles for an anonymous event.
  - `$os`, `$os_version`, `$is_server` and `$geoip_disable` are added before `before_send`, only for keys the event leaves unset. The hook sees them and can remove them, and they are not added again after the hook.
  - `Event::with_flags` keeps flag properties the event already has, and `Event::add_group` groups merge into a `$groups` property key by key. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) TLS is now a Cargo feature. `default-features = false` no longer enables TLS: add `tls` for reqwest's default Rustls provider, or `tls-no-provider` when the application installs a process-level Rustls `CryptoProvider`. With `tls-no-provider`, constructing a client before a provider is installed panics. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Mark more public types `#[non_exhaustive]`: `FeatureFlagsResponse`, `FlagDetail`, `FlagMetadata`, `FlagReason`, `LocalEvaluationResponse`, `CaptureResponse` and `EventResult`. A `match` on `FeatureFlagsResponse` needs a wildcard arm, and the structs no longer accept struct literals. Build a `LocalEvaluationResponse` with the new `LocalEvaluationResponse::new`. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Remove deprecated feature flag methods and the `personal_api_key` builder alias, and rename `LocalEvaluationConfig::personal_api_key` to `secret_key`. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Publish the SDK implementation as `posthog` starting with 1.0. Keep `posthog-rs` as a compatibility crate that forwards features and re-exports `posthog`, sharing its types and global state. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Remove the legacy V0 capture pipeline. Capture V1 (`POST /i/v1/analytics/events`) is now the only capture implementation and the `capture-v1` Cargo feature has been removed.
  
  Breaking changes:
  
  - `V1ErrorResponse` is renamed `CaptureErrorResponse`, which also changes the return type of `CaptureFailure::error_response()`.
  - `Endpoint::Batch` is removed, and `Endpoint` is now `#[non_exhaustive]` so future endpoints can be added without breaking callers.
  - `Endpoint::Capture` now resolves to `/i/v1/analytics/events` instead of `/i/v0/e/`. Code calling `Endpoint::Capture.path()` compiles unchanged but observes a different URL.
  - The `capture-v1` feature no longer exists; remove it from your feature list. `brotli` and `zstd` are now unconditional dependencies (`zstd-sys` builds C via `cc`).
  
  Behavior changes:
  
  - All four content encodings the capture endpoint accepts — `gzip`, `deflate`, `br`, `zstd` — now work. Previously, selecting `CaptureCompression::{Deflate, Br, Zstd}` on default features silently sent the body **uncompressed**; it is now genuinely compressed. Check any proxy or WAF in front of PostHog that may not handle `Content-Encoding: zstd` or `br`.
  - `CaptureSummary::not_persisted()` and `all_persisted()` are now data-driven from the backend's per-event verdicts. On V0 they were hard-wired to `0` and `true`, so callers gating durable state on `all_persisted()` should re-check that logic. — Thanks @marandaneto!

### Minor changes

- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) MCP analytics can set capture options on its events. `McpOptionsBuilder::event_options` takes a callback that returns options for each request's events, and `ToolCallRequest::event_option` sets an option on one call's events in the manual API. The call's option wins over the callback's, both win over the request context, and an anonymous MCP event keeps `process_person_profile: false`. The `McpOptions` hook and the client's `before_send` see the options and can change them. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Log a warning when the SDK drops a `$session_id` or `$window_id` value that is not a string. The warning names the key and the value's type, never the value. A `null` value still drops without a warning. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) `before_send` now sees groups from `Event::add_group` in the `$groups` property and can change or remove them. For minimized `$feature_flag_called` events, the SDK trims properties to the allowlist before `before_send`, so properties the hook adds are sent. The capture and `/flags` HTTP clients the SDK builds follow only 307 and 308 redirects to the same scheme, host and port, at most 5 times; any other redirect fails the request. Shutdown and queue-full drop warnings now name the capture endpoint. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Split each background analytics batch into requests of at most 5 MiB of serialized events, so a batch of large events no longer exceeds capture's 10 MiB request limit. A larger single event is sent on its own. `before_send` timing and `capture_batch_immediate` requests are unchanged. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Retry capture requests that get HTTP 404 or 429, like 408 and 5xx, with the same backoff and `Retry-After` as the minimum wait. 0.x retried a 429 only when it had a `Retry-After` header and never retried a 404. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Capture now makes 4 attempts by default instead of 3, and the first retry waits 100 ms instead of 200 ms. Remote `/flags` retries no longer use the capture retry options. They wait 300 ms before the first retry and double up to 30 s. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Add `CaptureFailure::verdict_summary()`, which lists every dropped and retried event reason with its count on one line, for logging from an `on_error` hook. Without a hook, the per-batch warning now shows totals only, for example `3 event(s) not persisted by <endpoint>: 2 dropped, 1 out of retries`. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Add `capture_ai`, `capture_ai_batch`, `capture_ai_immediate`, `capture_ai_batch_immediate` and the global `posthog::capture_ai` for LLM analytics events. They post to `/i/v1/ai/events` on their own background lane, with the `capture_ai_compression` and `capture_ai_max_queue_size` options. Both the background lane and the immediate variants drop an event over 8 MiB locally; the immediate variants report it in the returned `CaptureSummary` as an `ai_event_too_big` drop. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) `RequestContext::option` sets a capture option on every event in a request context, including exceptions. An option the event sets, other than `null`, wins. A context `process_person_profile` replaces the `false` that `Event::new_anon` sets for a personless event. A context `process_person_profile` does not set `process_person_profile` on identify-style events (`$identify`, `$create_alias`, `$merge_dangerously`, `$groupidentify`, `$set`, `$set_once`) whose distinct ID the SDK did not generate; those default to `true` unless the event sets it. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) A `$set`, `$set_once`, `$groups` or `$group_set` from the request context fills in only the keys the event's own value lacks, as in the other v1 server SDKs. In 0.x, the context value was used only when the event had none of that property.
  
  `$feature_flag_called` events now take a request context value over the client's `$is_server` and `$geoip_disable` defaults, as other events already did. A per-call `disable_geoip` still wins. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Report event copies that capture dropped as duplicates. When a request contains one uuid more than once, capture keeps the first event and drops the later copies. The SDK no longer resends those copies, and counts each one as lost in `on_error` (`drop/duplicate_event_uuid` in `CaptureFailure::verdict_summary`) and in `CaptureSummary::not_persisted`. The new `EventResult::duplicates_dropped` field gives the number of copies dropped for that uuid. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) A `$trace_id` or `$span_id` set with `CaptureExceptionOptions::property` now wins over the trace context provider, and setting either one stops the SDK adding both provider IDs. `CaptureExceptionOptions::trace_context` still wins over both. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) A personal API key (`phx_...`) passed as the project `api_key` disables the client and logs one error, as an empty key does. The empty-key message is now logged at the error level, and a client built with `disabled(true)` logs nothing about its key. — Thanks @marandaneto!

### Patch changes

- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) The batch `created_at` and the `PostHog-Request-Timestamp` header now use the same format as event timestamps: UTC with a `Z` and 3, 6 or 9 fractional-second digits. Before, they used a `+00:00` offset and left out the fraction when the clock was on a whole second. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) `capture_ai_immediate` and `capture_ai_batch_immediate` now apply the current request context, like `capture_ai`, `capture_immediate` and `capture_batch_immediate`. Before, context properties (such as `$session_id`) and context options were missing from events sent this way. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) MCP events sent over HTTP now take the `RequestContext` that `PostHogLayer` or the application placed in the request extensions, so their context properties and options are no longer lost when `rmcp` runs the handler in its own task. The MCP identity stays the distinct ID, and an anonymous MCP event keeps `process_person_profile: false`. The `McpOptions` `before_send` hook now sees the context values. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) `group_identify` now sends the request context's distinct id when the context has one, and `$<group_type>_<group_key>` otherwise. It no longer adds the context session id to `$groupidentify` events. This matches the other v1 server SDKs. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) A per-call `disable_geoip: Some(false)` in `EvaluateFlagsOptions` now sends `$geoip_disable: false` on `$feature_flag_called` events, so it beats a request context `$geoip_disable`. Before, it sent nothing and the context value applied. `Some(true)` and `None` are unchanged. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) An event from `Event::new_anon` whose `process_person_profile` option is `null` or removed gets `false` again before `before_send`, so it stays personless unless the option is set to `true`. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) When a `before_send` hook panics, the error log now names the event it dropped: `panic in PostHog before_send hook for event "<name>"; dropping event`. It still never logs the panic message, which can hold the values the hook was redacting. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Event timestamps keep microsecond and nanosecond precision instead of being cut to milliseconds. Whole-millisecond timestamps are sent as before. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) Capture retry waits now vary at random by up to 25% either way, so many clients do not retry at the same moment. — Thanks @marandaneto!
- [07702b3](https://github.com/posthog/posthog-rs/commit/07702b3bcc871e277ecef3f3b4ccff8343219598) When an MCP analytics `before_send` hook panics, the SDK now logs an error that names the event it dropped: `panic in PostHog before_send hook for event "<name>"; dropping event`. Before, it logged a warning without the event name. It never logs the panic message. — Thanks @marandaneto!

## 0.33.1 — 2026-11-13

### Patch changes

- [2b664da](https://github.com/posthog/posthog-rs/commit/2b664dada02437d7a0896a6858854f3b7cce2abd) Ignore failed detailed feature flag records and their payloads in direct reads, bulk results, and snapshot enrichment. — Thanks @marandaneto!

## 0.33.0 — 2026-11-13

### Minor changes

- [90cb06a](https://github.com/posthog/posthog-rs/commit/90cb06a594835f455dc39ce6ee77547d10ca33bf) A `CodeEmail` takes copies and files: `cc` and `bcc` each add one address, and `attachment` adds an `EmailAttachment` made from the file's bytes. An email carries at most 10 files and 40 MB after Base64 encoding, counting the bodies (the HTML twice without text, since PostHog derives the text from it) and the subject, about 29 MB of files; the client checks both before it sends anything. The client hashes each file, uploads the files PostHog doesn't have yet straight to PostHog's file store over https, and then sends the email naming each file by its SHA-256. A file store URL that isn't https fails the send before any upload, unless `app_host` is plain http itself, as a PostHog on your own machine is. A refused upload fails the send with `attachment_upload_failed` and sends no email. Add `content_type` and `content_id`, and reference a `content_id` as `cid:<id>` in the HTML to show the file inline. PostHog can't fetch a file from a path or URL, and an attachment's `Debug` output never shows its content. A `TemplateEmail` takes none of these, because the template sets its own recipients and content. — Thanks @Silthus!

## 0.32.0 — 2026-11-12

### Minor changes

- [8b0fb8b](https://github.com/posthog/posthog-rs/commit/8b0fb8be20fd6d20aaf6b3656d0a66beab5340d0) A `CodeEmail` now takes `.sensitive()` and `.tracking()`.
  
  `.sensitive()` marks an email that carries a login link, a reset token or a code. PostHog keeps a sensitive email's links unchanged, leaves its subject off engagement events and keeps its content out of logs. Without it, a code send takes the setting of the saved template whose key matches its `template_key`, or is sensitive when no template has that key, as before.
  
  `.tracking()` says what PostHog tracks for the email: `true` or `EmailTracking::On`, `false` or `EmailTracking::Off`, or `EmailTrackingOptions::new()` with `.opens()`, `.clicks()` and `.engagement()` to set each kind on its own. Without it, PostHog counts opens, captures engagement events, and counts clicks only when the email isn't sensitive. Turn clicks on only when the email's links are safe to send through a redirect. Click tracking needs open tracking.
  
  `.capture_engagement()` is deprecated. Use `.tracking(EmailTrackingOptions::new().engagement(...))` instead. It still works, but PostHog refuses a send that sets both. The SDK now leaves `capture_engagement` out of the request unless you set it, which PostHog reads the same way.
  
  `.sensitive()` and `.tracking()` need PostHog's API support, which is rolling out. Until it reaches your PostHog region or self-hosted version, PostHog refuses a send that sets either. — Thanks @Silthus!

## 0.31.3 — 2026-11-11

### Patch changes

- [2e77fbb](https://github.com/posthog/posthog-rs/commit/2e77fbb60e805c7e25ca1ca59b4308222b1c6d9e) Relative date filters in local flag evaluation now follow the server: months and years are calendar steps instead of 30 and 365 days, `7d` without a leading `-` is accepted, and a number of 10,000 or more is inconclusive instead of panicking. — Thanks @luantaraschi for your first contribution 🎉!

## 0.31.2 — 2026-11-06

### Patch changes

- [98d2bf1](https://github.com/posthog/posthog-rs/commit/98d2bf1b6186e9461efa70977f1a28f5dca34899) Aligned the `mcp` module with the sdk-specs MCP spec. The `get_more_tools` tool now advertises `readOnlyHint`, `destructiveHint`, `idempotentHint` and `openWorldHint` as `false`, because a call records an event. The injected `context` argument has a shorter description, and the injected `llm_model` argument is no longer required. This changes your published tool definitions; if your server is listed in the ChatGPT or Claude directories, those platforms may re-review the updated tools. — Thanks @lucasheriques!

## 0.31.1 — 2026-11-05

### Patch changes

- [7d78b25](https://github.com/posthog/posthog-rs/commit/7d78b2554219bb146557e7bc2ad22fc1cb40feaf) Reworded the descriptions of the injected `conversation_id` and `llm_model` arguments and the `get_more_tools` virtual tool so they describe the field instead of instructing the model. This changes your published tool definitions; if your server is listed in the ChatGPT or Claude directories, those platforms may re-review the updated tools. — Thanks @lucasheriques!

## 0.31.0 — 2026-11-05

### Minor changes

- [1b43466](https://github.com/posthog/posthog-rs/commit/1b4346656c1cbcd6a43f785596d2b387361fea92) The new `client.emails()` sends transactional email from your Rust code and reads each email's delivery status.
  
  ```rust
  use posthog_rs::{ClientOptionsBuilder, CodeEmail};
  
  let options = ClientOptionsBuilder::default()
      .api_key("phc_...".to_string())
      .secret_key("phs_...")
      .build()?;
  let posthog = posthog_rs::client(options).await;
  
  let sent = posthog
      .emails()
      .send(
          CodeEmail::new("user@example.com", "password-reset", "Reset your password")
              .html(rendered_html)
              .distinct_id("user_123"),
      )
      .await?;
  
  let status = posthog.emails().get(&sent.id).await?;
  println!("{}", status.status.as_str()); // "delivered", "bounced", ...
  ```
  
  You can also send a template made in PostHog with `TemplateEmail::new` and its `template_id`.
  
  You need a project secret API key, the kind that starts with `phs_`. Create one under **Project secret API keys** in your project settings. Set its **Transactional email** scope to Write to send emails, or to Read for a key that only reads statuses. Set it with `secret_key` on `ClientOptionsBuilder`. Set `app_host` to your PostHog app URL when you self-host PostHog or send through a reverse proxy.
  
  PostHog records each email's delivery, opens and clicks on the person with that `distinct_id`. It groups every email with the same `template_key`, so you see their metrics and logs in one place. The SDK retries a request that hits a network error, a rate limit or an outage twice by default. Every send carries an idempotency key, so a retry never sends the email twice. Pass your own `idempotency_key`, such as `welcome:user_123`, and your job runner's retries are safe too.
  
  A `CodeEmail` also takes `.from()`, `.reply_to()` and `.capture_engagement(false)`. Set `migrated_from` on `ClientOptionsBuilder` to tell PostHog which email provider you moved from. A failed call returns an `EmailError` with PostHog's `code()` and the HTTP `status()`.
  
  Docs: [Send transactional emails with Rust](https://posthog.com/docs/workflows/transactional-emails/rust) and the [transactional email overview](https://posthog.com/docs/workflows/transactional-emails). — Thanks @Silthus for your first contribution 🎉!

## 0.30.0 — 2026-10-28

### Minor changes

- [6694c32](https://github.com/posthog/posthog-rs/commit/6694c32f756b1f565e7b4687c91f019cf9f5d1c0) Add an opt-in asynchronous application HTTP client that sends current request context to exact allowed hosts and returns redirects without following them. — Thanks @pauldambra!

## 0.29.0 — 2026-10-26

### Minor changes

- [8408705](https://github.com/posthog/posthog-rs/commit/84087054667e89e6aa8a8232622ed040cd31213d) Add experimental MCP analytics for servers built on `rmcp`, behind the new `mcp` feature (Rust 1.88+). `posthog_rs::mcp::instrument(server, client)` sends each tool call, tool listing and handshake to PostHog as `$mcp_*` events, with `$lib` `posthog-rs-mcp`, following PostHog's cross-SDK MCP analytics spec. A manual API covers servers that cannot be wrapped. Unsupported for now. — Thanks @wthrajat!

## 0.28.0 — 2026-10-26

### Minor changes

- [3341880](https://github.com/posthog/posthog-rs/commit/3341880e8a47772ca646f8cd6a0463c251fbb63d) Add isolated request context for captures, exceptions, and feature flags, with optional Tower and Axum middleware. — Thanks @pauldambra for your first contribution 🎉!

## 0.27.1 — 2026-10-23

### Patch changes

- [7e6527f](https://github.com/posthog/posthog-rs/commit/7e6527f99c0f4e9c1d30225b3691ab57bd14dc33) Fall back to remote feature flag evaluation for unsupported or incompatible regex syntax instead of returning incorrect local results. Invalid local patterns no longer match `not_regex`. — Thanks @marandaneto!

## 0.27.0 — 2026-09-24

### Minor changes

- [fedcb87](https://github.com/posthog/posthog-rs/commit/fedcb872ce5448716deec0a08c56c4785a67e0dd) Add optional async and blocking reqwest clients for sharing application HTTP configuration and connection pools across capture, feature flags, and local evaluation. SDK request and feature flag timeout options also apply to supplied clients. — Thanks @marandaneto!

## 0.26.0 — 2026-09-21

### Minor changes

- [6bc074f](https://github.com/posthog/posthog-rs/commit/6bc074fa707efc3e2c0bf9a27f95f89ad534014c) Honor definition snapshots' `property_matching_version` during local flag evaluation, including group conditions, recursive cohorts, and flag dependencies. Version 2 uses explicit case-insensitive equality instead of legacy boolean coercion; missing metadata and other versions retain legacy matching. Empty filters retain recursive truthiness in both versions. Keep matching semantics and definitions together across cache refreshes and version-only updates.
  
  `LocalEvaluationResponse` and `EvaluationContext` struct literals now require a `property_matching_version` field (use `1` for legacy behavior); older serialized definitions still load without it. The context-free `match_feature_flag` helper remains legacy. — Thanks @marandaneto!

## 0.25.5 — 2026-09-15

### Patch changes

- [af85090](https://github.com/posthog/posthog-rs/commit/af85090a57ca6e357f15125ff4248e6b9e6b5ee6) Do not panic when the HTTP client cannot be built. A container without CA certificates makes the reqwest builder fail, which stopped the calling program. The client now logs a warning, disables itself, and lets the program continue. The feature flag pollers degrade the same way: they log a warning and stay stopped instead of panicking. — Thanks @posthog[bot]!

## 0.25.4 — 2026-08-31

### Patch changes

- [20cf390](https://github.com/posthog/posthog-rs/commit/20cf3903269cc03b20f650cb2dddedbda203ae15) Match local feature flag properties with the same case folding, string coercion, and boolean-like filter precedence as the flags service. Substring and prefix/suffix operators now fold ASCII only, while `exact` and `is_not` use the service's truthiness handling before Unicode-lowercase comparison. — Thanks @marandaneto!

## 0.25.3 — 2026-08-26

### Patch changes

- [6d1a444](https://github.com/posthog/posthog-rs/commit/6d1a444cda522b0c3dd69bb15903e617e3237ba6) Treat omitted local evaluation properties as inconclusive for presence operators. — Thanks @marandaneto!

## 0.25.2 — 2026-08-25

### Patch changes

- [e4ac827](https://github.com/posthog/posthog-rs/commit/e4ac82711d44e2715eac8fc94579086df9059a98) Return an empty feature flag snapshot without evaluation when feature flag keys are explicitly empty. — Thanks @marandaneto!

## 0.25.1 — 2026-08-24

### Patch changes

- [253a7c0](https://github.com/posthog/posthog-rs/commit/253a7c09a6776dda0d753ed847a9f2d4fbbe7d0d) Prevent transport worker deadlocks during client shutdown. — Thanks @marandaneto!

## 0.25.0 — 2026-08-24

### Minor changes

- [710ea93](https://github.com/posthog/posthog-rs/commit/710ea931e9f580f3b364509f03c152112c5b244e) Add `group_identify` helper to `Client` (async) and `Client` (blocking) for creating or updating group properties via `$groupidentify`. — Thanks @vjymisal0 for your first contribution 🎉!

## 0.24.0 — 2026-08-19

### Minor changes

- [7e2bede](https://github.com/posthog/posthog-rs/commit/7e2bede1a34b2f9edb599c4e2566684736d8cccf) Add a `Client::alias` helper that merges two distinct IDs onto the same person, so callers no longer need to hand-build the `$create_alias` event and know that `distinct_id` is written both at the top level and inside `properties`. Available on the async and blocking clients with the same fire-and-forget signature as `capture`. A blank ID on either side cannot describe a merge, so the event is dropped with a warning rather than sent. — Thanks @HughScott2002 for your first contribution 🎉!

## 0.23.3 — 2026-08-14

### Patch changes

- [f4e940d](https://github.com/posthog/posthog-rs/commit/f4e940d8940c45a513401ed5490f389d68134b72) Normalize SDK timestamps to UTC — Thanks @marandaneto!

## 0.23.2 — 2026-08-10

### Patch changes

- [ea92c93](https://github.com/posthog/posthog-rs/commit/ea92c93251a97255208332c40df7a50854c57aa8) Surface feature flag payloads from local evaluation.
  
  The definitions manifest carries each flag's payloads, but locally evaluated flags always reported `payload: None`, so `FeatureFlagEvaluations::get_flag_payload` returned nothing for them. When `flag_keys` was fully covered locally there was also no `/flags` round trip left to recover the payload from. Local evaluation now resolves the payload for the matched value (`"true"` for a boolean match, the variant key for a multivariate one) and decodes it exactly like a `/flags` payload, so a flag returns the same payload whichever path evaluated it. As a result, `$feature_flag_called` events for locally evaluated flags now carry `$feature_flag_payload`, matching remote evaluation. — Thanks @slshults!
- [962282c](https://github.com/posthog/posthog-rs/commit/962282cf3b0be3353d61ce088fbfa3514f1eebf4) Stop malformed cohort definitions from aborting the process during local evaluation.
  
  Cohort resolution recursed through nested cohort references with nothing bounding the recursion, so a cohort that referenced itself, directly or through another cohort, recursed until the thread stack was exhausted. A Rust stack overflow aborts the process rather than panicking catchably, so a single bad definitions response could take down the calling server, and would again on restart as soon as the poller refetched. A long chain of distinct cohorts reached the same abort without any cycle at all, and because each cohort is a separate shallow entry in the manifest, such a chain also cleared `serde_json`'s own nesting limit on the way in.
  
  Cohort resolution now tracks the cohorts on the active resolution path and the combined depth of cohort references and nested property groups. A repeated cohort is reported as a cycle, and total resolution depth is capped at 100. Either is reported as inconclusive, so the flag falls back to server-side evaluation rather than resolving to a wrong value. Referencing the same cohort twice down sibling branches still resolves normally, since IDs are released as the recursion unwinds. — Thanks @slshults!

## 0.23.1 — 2026-08-10

### Patch changes

- [cf8952a](https://github.com/posthog/posthog-rs/commit/cf8952a3ea46b1765e551b2afac374e7253df493) Fix local flag evaluation for projects that use cohorts.
  
  The `/flags/definitions/?send_cohorts` payload maps each cohort ID directly to its property group (`{"type": "AND"|"OR", "values": [...]}`), but `Cohort` required non-existent `id`/`name`/`properties` fields, so the whole `LocalEvaluationResponse` failed to deserialize and no flags loaded at all. `Cohort` now deserializes the real payload, and cohort matching recurses through nested AND/OR groups and cohort references instead of flattening them. — Thanks @posthog[bot] for your first contribution 🎉!

## 0.23.0 — 2026-08-07

### Minor changes

- [015e5a3](https://github.com/posthog/posthog-rs/commit/015e5a3fa0b1a915d356a081ebc186479ba7c5ce) Add an atomic `trace_context` capture option and a client-level trace-context provider so exception events can be linked to distributed traces without adding telemetry dependencies to the SDK. — Thanks @hpouillot for your first contribution 🎉!

## 0.22.0 — 2026-08-05

### Minor changes

- [860af1c](https://github.com/posthog/posthog-rs/commit/860af1cea0fe5849d5c560064901ef48d723a804) Support the `starts_with`, `not_starts_with`, `ends_with`, and `not_ends_with` property filter operators in feature flag local evaluation. Matching is case-insensitive and mirrors `icontains`, so flags using these operators no longer fall back to remote evaluation. — Thanks @haacked!

## 0.21.0 — 2026-07-20

### Minor changes

- [7dab8cd](https://github.com/posthog/posthog-rs/commit/7dab8cd3209f666851b563a853593064524ae851) `$feature_flag_called` events now carry a tri-state `$feature_flag_has_experiment` property and are minimized for non-experiment flags when the server enables it. The experiment signal is read from `has_experiment` on each flag — from the `/flags?v=2` `metadata` object (remote evaluation) or the `/flags/definitions` flag definitions (local evaluation) — and is sent as `true`/`false` when reported, omitted entirely when the server does not report it (never fabricated as `false`).
  
  When the server-controlled gate is on — `minimalFlagCalledEvents: true` on the `/flags?v=2` response (remote) or `minimal_flag_called_events: true` on the local-evaluation definitions payload (local) — and the evaluated flag has `has_experiment: false`, the event is rebuilt from a strict allowlist (`$feature_flag`, `$feature_flag_response`, `$feature_flag_has_experiment`, `$feature_flag_id`, `$feature_flag_version`, `$feature_flag_reason`, `$feature_flag_request_id`, `$feature_flag_evaluated_at`, `$feature_flag_error`, `locally_evaluated`, `$groups`, `$process_person_profile`, `$geoip_disable`, `$session_id`, `$window_id`, `$device_id`, `$lib`, `$lib_version`, `$is_server`, `$os`, `$os_version`); everything else — `$feature/<key>`, `$feature_flag_payload`, and any user-supplied properties — is stripped. Any missing signal (gate absent, `has_experiment` unknown, experiment-linked flag, legacy response shapes) keeps today's full event shape. The gate is captured per evaluation source and pinned onto each flag's snapshot record, so a concurrent poller refresh cannot reshape an event after its value was determined. The gate is server-controlled per project; no SDK configuration is added.
  
  Local-evaluation support for the `minimal_flag_called_events` field depends on a PostHog server release that emits it; until then the client defaults to the full event shape. — Thanks @haacked!

## 0.20.0 — 2026-07-15

### Minor changes

- [8704f78](https://github.com/posthog/posthog-rs/commit/8704f78f67e91c27005f95838099b7945c866754) Resolvable error tracking frames with local debug info now send their full client-side inline expansion as a marked group (the physical frame leads; `inline: true` members share its `instruction_addr`; all carry `client_resolved: true`): PostHog symbolicates the group's address once and replaces the whole group with its own expansion, or keeps the client frames verbatim when no debug symbols were uploaded, so inlined calls survive without symbol uploads and don't duplicate with them. Stripped builds still send bare addressed frames. Grouped replacement requires a PostHog version with client-expanded inline group support (live on PostHog Cloud; self-hosted needs a release that includes cymbal's marker-grouped resolution). Older self-hosted servers keep working, but re-expand group addresses on symbol upload, duplicating inline frames until upgraded. — Thanks @cat-ph!

## 0.19.2 — 2026-07-14

### Patch changes

- [fd59b20](https://github.com/posthog/posthog-rs/commit/fd59b20d60954f8857bda678ba23f567a1177902) Default in-app frame classification now covers three gaps that left dependency and runtime frames marked as app code:
  
  - Cargo dependency sources are recognized by cargo's own on-disk layouts — `registry/src/index.crates.io-<hash>/` and `git/checkouts/<repo>-<hash>/<rev>/` — under any `CARGO_HOME`, not just `~/.cargo` (e.g. `/usr/local/cargo` in the official Rust Docker images).
  - Crate-denylist matching strips trailing generic arguments first, so DWARF-derived names like `poll_future<tokio::runtime::...>` no longer feed a garbage segment (`poll_future<tokio`) into the crate check. Such names classify by their file path (fix above); when there is no file, they deliberately stay in-app — generic arguments are instantiation types, not the defining crate, and guessing from them would mislabel app functions generic over vendor types.
  - Fileless thread/process bootstrap symbols (`__clone`, `start_thread`, `_start`, the C `main` shim, and friends) are classified as not in-app. Other bare symbols — `#[no_mangle]` exports, C code linked into the binary — keep their previous in-app classification. — Thanks @cat-ph!

## 0.19.1 — 2026-07-10

### Patch changes

- [31213ee](https://github.com/posthog/posthog-rs/commit/31213ee5d61e36e4228b4e01de13a119a9e012d2) fix: strip SDK capture frames under newer demangler renderings (`<Type>::method::<T>`), which previously survived at the crash-site end of the stack — Thanks @cat-ph!

## 0.19.0 — 2026-07-10

### Minor changes

- [7e1bac5](https://github.com/posthog/posthog-rs/commit/7e1bac50e6a65a74abc307829b76fd6362658725) Error tracking stack frames now ship in canonical wire order — `$exception_list[].stacktrace.frames` is bottom-up, so `frames[0]` is the outermost frame (the entry point, e.g. `main`) and the last frame is the crash/capture site. This matches the other PostHog SDKs and the server-side native symbolication contract, which assumes bottom-up input. The whole flattened stack is globally bottom-up, including client-side inline expansion: within a single physical frame the outermost logical layer comes first and the inlined leaf comes last. Frame trimming now drops the outermost frames from the front and keeps the ones nearest the crash site. This is a breaking change to the wire order; `$exception_list` ordering itself is unchanged (`[0]` is still the outermost error with the `exception_id`/`parent_id` links). — Thanks @cat-ph!

## 0.18.0 — 2026-07-09

### Minor changes

- [fb22559](https://github.com/posthog/posthog-rs/commit/fb22559b2d51038b228883e991e188d5c9467987) Immediate (inline) capture for callers that must know a batch persisted before advancing their own durable state (e.g. a server-side importer committing an upstream offset). `Client::capture_immediate` and `Client::capture_batch_immediate` send inline — bypassing the background transport worker — and return `Result<CaptureSummary, Error>` once the request reaches a terminal outcome, retrying transient failures per the client's existing retry configuration. They are available on both the async and blocking clients and on both the v0 and `capture-v1` pipelines, reusing the same sans-IO request-build/retry/response-classification core as the fire-and-forget path.
  
  The new public `CaptureSummary` (`#[non_exhaustive]`, read through accessors) reports the outcome: `submitted()` (events sent after `before_send` filtering), `not_persisted()`, and `all_persisted()`. On the v0 pipeline a `2xx` persists the whole batch, so `all_persisted()` is always true; on `capture-v1` the backend returns per-event verdicts, so a `2xx` can still leave events unpersisted — `not_persisted()` counts submitted events without an `ok`/`warning` verdict, and `event_results()` (under `capture-v1`) exposes the full per-event map.
  
  Fire-and-forget `capture`/`capture_batch` remain the primary, recommended API and are unchanged. The immediate variants do NOT fire `on_error` hooks: the returned `Result` is the delivery signal, so there is no double-reporting. Disabled clients and an empty (or fully `before_send`-filtered) batch return a default `CaptureSummary` without sending a request. New public export: `CaptureSummary`. — Thanks @eli-r-ph!

## 0.17.3 — 2026-07-05

### Patch changes

- [9d94264](https://github.com/posthog/posthog-rs/commit/9d9426412bf171468c5ed5736599674ac1858127) Clamp a server `Retry-After` to `retry_max_backoff_ms` (default 30s) instead of an internal 1-day cap, so a single retry wait never exceeds the configured max backoff. `Retry-After` still acts as a minimum and the configured backoff itself is never truncated. This unifies the default retry-wait ceiling (30s) with posthog-go and posthog-python. — Thanks @eli-r-ph!

## 0.17.2 — 2026-07-02

### Patch changes

- [7ee049c](https://github.com/posthog/posthog-rs/commit/7ee049c5020cca2ae9cfb943d78fd9fe047c902e) Stop duplicating distinct_id inside /flags person properties — Thanks @marandaneto!

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
