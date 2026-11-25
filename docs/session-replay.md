# Manual screenshot replay

Enable `session-replay` on `posthog` (or the compatibility package `posthog-rs`).
It implies `stateful` and supports both client modes, with Rust 1.78.

The host owns when to try a capture, renderer lifetime, downscaling and privacy
masking. The SDK throttles frames to one a second by default.
Supply already-composited, already-masked BGRA8888 pixels. Rust encodes those
dimensions unchanged as JPEG. Alpha is ignored.

## Capture

```rust,no_run
use posthog::stateful::{Client, Config, FrameLayout, FrameUnavailable, PixelFormat, Persistence};

let mut config = Config::new("phc_project_token");
config.persistence = Persistence::Memory;
config.session_replay.jpeg_quality = 65;
config.session_replay.max_frames_in_flight = 2;
config.session_replay.throttle_delay = std::time::Duration::from_secs(1);
let client = Client::init_blocking(config);
let replay = client.session_replay();
replay.start();
if !replay.is_active() {
    // Skip this capture. A long-lived host can check again on a later frame.
    client.shutdown_blocking(None);
    return;
}

// Immediately before host acquisition/readback:
let layout = FrameLayout::new(640, 360, 640 * 4, PixelFormat::Bgra8888);
match replay.begin_frame(layout) {
    Ok(mut frame) => {
        // Fill every pixel from already-masked host data. This example uses black.
        frame.pixels_mut().fill(0);
        let outcome = replay.submit_frame(frame);
        // Accepted means queued work, not successful encoding, hooks or ingestion.
    }
    // Too soon after the last frame, or earlier frames are still uploading:
    // skip this frame. Calling `begin_frame` every render frame is fine.
    Err(FrameUnavailable::Throttled | FrameUnavailable::AtCapacity) => {}
    // Not recording, not permitted, opted out, no session, closed, …
    Err(_) => {}
}
replay.stop();
client.shutdown_blocking(None);
```

Carry the owned frame through asynchronous readback and submit it once. It can
move between threads; it cannot be cloned. Dropping it abandons acquisition.
At capacity, skip that capture instead of prefetching or accumulating handles.

`begin_frame` returns `FrameUnavailable::Throttled` when it is called sooner
than `throttle_delay` (default one second, as posthog-ios `throttleDelay` and
posthog-android `throttleDelayMs`) after the last frame it began, so a host can
call it on every render frame and record about one frame a second. A dropped
frame still counts. Set `throttle_delay` to zero to disable throttling and pace
captures yourself.

A throttled frame is skipped, not deferred. posthog-ios and posthog-android take
one more screenshot when the delay ends, so a change inside the window is still
recorded. A host that renders only when something changes should do the same:
after `Throttled`, call `begin_frame` again (for example, by requesting a redraw)
`throttle_delay` later. Otherwise the last change before the screen goes still
isn't recorded until the next redraw.

Layouts require nonzero dimensions fitting JPEG's 16-bit width/height, at least
four bytes per pixel per row, and checked allocation arithmetic; otherwise
`begin_frame` returns `FrameUnavailable::InvalidLayout`. Allocation failures
return `FrameUnavailable::OutOfMemory`. Tokens have no time-based expiry.

## Eligibility and attribution

Only `start()` requests recording. It resumes a valid session or creates one
through the existing session manager, without touching activity. A `start()`
made while the user is opted out is ignored, as in posthog-ios and
posthog-android; call it again after `opt_in()`. A `stop()` made while opted
out still takes effect. Recording also
requires current consent, a valid session and remotely enabled project settings.
Settings load asynchronously; delivery flush does not wait for them. Check
`is_active()` on a later capture opportunity if recording is not yet eligible.
Unavailable settings prevent recording, while a valid restored project cache
can permit it during a failed/pending refresh.

Remote enablement alone never requests recording. A manual stop remains stopped
until another explicit start. Remote disablement stops new acquisitions.

Supported remote settings are `sessionRecording: true` or an unrestricted object.
Absent/null controls, `sampleRate: "1"` (or equivalent string rate 1),
zero minimum duration, empty event triggers and a `triggerMatchType` of `all`
or `any` are neutral. Linked flags, other sample rates, nonempty event triggers,
positive minimum duration, other `triggerMatchType` values and malformed
controls prevent recording (`FrameUnavailable::NotPermitted`); this client does
not support them. Browser-only fields (URL triggers, URL blocklists, trigger
groups and the config version) are ignored, as in posthog-ios and
posthog-android. Screenshots post to the remote `endpoint` path, or `/s/` when
it names none. Unrelated extension fields are tolerated.

Acquisition fixes identity, session, timestamp, UUID and ordering. The replay
window ID is the session ID, so a new session also starts a new replay window. Submission
rechecks identity, consent transitions and the current session; changes during
readback reject the frame instead of reattributing it. Backgrounding adds no
special invalidation, but ordinary session expiry still applies. Screenshots
do not extend session activity. In automatic session mode in the foreground,
`begin_frame` starts a new session when the last one expired (30 minutes
without activity, or 24 hours) or was ended by `reset()` or `end_session()`,
as `get_session_id` does; posthog-android's frames rotate the same way. Call
`session_activity()` on user input to keep one session across an idle screen.
A host that keeps capturing an idle screen without reporting activity starts a
new recording every 30 minutes; pause capture while the app is idle to avoid it.
Once accepted, work retains its attribution through later identity, session
and consent changes.

## Replay status on events

With `session-replay` enabled, every captured event except `$snapshot` carries
the replay status PostHog uses to explain missing recordings:

- `$recording_status`: `active` while recording is started, eligible and has a
  session; otherwise `disabled`. Frames are never held, so it is never `buffering`.
- `$sdk_debug_replay_event_trigger_status` and
  `$sdk_debug_replay_linked_flag_trigger_status`: `trigger_pending` when the
  project configures that trigger (which prevents recording), otherwise
  `trigger_disabled`.
- `$sdk_debug_replay_internal_buffer_length`: frames acquired and not yet
  delivered.

If the screenshot encoder could not start, events carry only
`$recording_status: disabled`. A caller-supplied or registered property of the
same name wins, and `before_send` hooks see these properties.

## Hooks and encoding

One encoder worker performs JPEG encoding and then runs the existing
`before_send` chain on the assembled native `$snapshot` event. Hooks can redact
properties or drop it; a panic fails closed. Rewriting required attribution,
event name, UUID, timestamp or session/window fields rejects the screenshot.
Ordinary analytics hooks keep their synchronous capture behavior.

A submitted frame may therefore be accepted and later dropped by its hook.
Hook callbacks must not wait for another capture. Same-client recursive capture
is rejected; flush/shutdown from an encoder callback requests work and returns
Deferred rather than waiting on itself. Codecs are internal, not a public plugin
API.

## Capacity and memory

Defaults are JPEG quality 65, two frames in flight and a one-second throttle.
Each reservation spans
acquisition, encoding, queued upload and retries. Returning raw pixels to the pool
does not release it. With the default of two, frames waiting to retry pause new
acquisition until they upload or reach the attempt limit.

Raw buffers allocate lazily, reuse capacity and grow only while available.
Their high-water capacities remain until stop. Stop frees idle buffers; leased
allocations are freed safely when returned. A host-held frame cannot be forcibly
reclaimed by stop or flush.

Frame count is not a process-memory cap. Raw capacities, JPEG working memory,
encoded envelopes, JSON/base64 and gzip buffers all contribute. Requests are
bounded to 0.9 MB before compression, under the backend's limit of about 1 MB
decompressed, as in posthog-js. Oversized pairs split, and a single frame over
the limit (a JPEG over about 675 KB) is dropped with a sanitized diagnostic.
Hosts should downscale before acquisition to control memory, encoding cost and
frame size.

## Delivery and lifecycle

Native screenshot events go to the remote `endpoint` path (default `/s/`), not
analytics ingestion, on a dedicated replay thread with its own HTTP client built
from the client's settings. Event delivery never waits on screenshot uploads.
Requests use gzip whenever `capture_compression` is set (the default); setting
`capture_compression = None` sends plain JSON without a compression header.
Every request contains one session/distinct-ID pair. Batches are at most two
frames and become eligible at two encoded frames or one second after the first
is ready. The replay thread and HTTP can delay attempts.

Network failures and retryable statuses (404, 408, 429, 500, 502, 503 and 504)
are retried in memory with bounded jittered backoff, up to four attempts per
batch including the first. After the fourth, the frames are released and the
failure is reported through `on_error`. A 2xx response is delivered even when
its body cannot be read. Valid Retry-After is a floor subject to the backoff cap.
Flush does not bypass cooldown. Multi-frame HTTP 413 responses split; singleton
413 and other permanent/quota rejections release affected frames and report
through `on_error`. Retry scheduling is logged at debug level, and shutdown
reports abandoned screenshots once per drain. Diagnostics omit pixels, payloads, tokens and attribution.

Stop requests flush and allows valid outstanding host-held frames to submit.
Client flush includes previously submitted encoding and an eligible replay
delivery attempt, not host-held frames or guaranteed ingestion. Because flush
waits for `before_send` hooks on the encoder thread, don't flush while holding a
lock a hook takes. Existing `DrainReport` semantics remain local progress.

Shutdown closes acquisition/submission. Stateful SDK-controlled waiting uses the
first selected deadline, shared with state persistence and delivery; later calls
and Drop cannot extend it. `DeliveryOutcome::Stopped` means delivery workers
actually finished, while `TimedOut` means unfinished work remained at expiry.
Neither guarantees ingestion. Already-started encoding, HTTP or callbacks own
their resources until completion; work that never returns can retain them
indefinitely. Arbitrary destructors of application objects captured by callbacks
may block outside the SDK-controlled waiting bound.

After expiry no new encoding or upload starts. Already-admitted encoding is
observed and disposed after event storage is released, preserving report and
permit ownership without waiting for unsubmitted host-held frames. Queued replay
is memory-only: process exit, a crash or shutdown can lose it. Replay has no
persistence queue.

## Validation boundaries

Run `cargo run --features session-replay --example stateful_replay` with
`POSTHOG_PROJECT_TOKEN` and `POSTHOG_HOST` pointing at a mock project. The example
uses a synthetic blue image with an already-black masked half. Set
`REPLAY_WIDTH`, `REPLAY_HEIGHT` and `REPLAY_FRAMES` for local measurements.

Synthetic JPEGs and mock traffic establish Rust ownership, dimensions, masking
preservation and wire behavior. They do not establish Unreal renderer safety,
mask discovery, real screenshot privacy, live ingestion or backend playback.
