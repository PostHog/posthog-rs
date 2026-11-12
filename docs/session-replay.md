# Manual screenshot replay

Enable `session-replay` on `posthog` (or the compatibility package `posthog-rs`).
It implies `stateful` and supports both client modes, with Rust 1.78.

The host owns capture cadence, renderer lifetime, downscaling and privacy masking.
Supply already-composited, already-masked BGRA8888 pixels. Rust encodes those
dimensions unchanged as JPEG. Alpha is ignored.

## Capture

```rust,no_run
use posthog::stateful::{Client, Config, FrameLayout, PixelFormat, Persistence};

let mut config = Config::new("phc_project_token");
config.persistence = Persistence::Memory;
config.session_replay.jpeg_quality = 65;
config.session_replay.max_frames_in_flight = 2;
let client = Client::init_blocking(config);
// Wait for the initial remote-settings attempt before this one-shot capture.
client.flush_blocking();
let replay = client.session_replay();
replay.start();
if !replay.is_active() {
    // Remote settings, consent or the session can still prevent recording.
    client.shutdown_blocking(None);
    return;
}

// Immediately before host acquisition/readback:
let layout = FrameLayout {
    width: 640,
    height: 360,
    row_stride_bytes: 640 * 4,
    format: PixelFormat::Bgra8888,
};
if let Some(mut frame) = replay.begin_frame(layout) {
    // Fill every pixel from already-masked host data. This example uses black.
    frame.pixels_mut().fill(0);
    let outcome = replay.submit_frame(frame);
    // Accepted means queued work, not successful encoding, hooks or ingestion.
}
replay.stop();
client.shutdown_blocking(None);
```

Carry the owned frame through asynchronous readback and submit it once. It can
move between threads; it cannot be cloned. Dropping it abandons acquisition.
At capacity, skip that capture instead of prefetching or accumulating handles.

Layouts require nonzero dimensions fitting JPEG's 16-bit width/height, at least
four bytes per pixel per row, and checked allocation arithmetic. Allocation
reservation failures return None. Tokens have no time-based expiry.

## Eligibility and attribution

Only `start()` requests recording. It resumes a valid session or creates one
through the existing session manager, without touching activity. Recording also
requires current consent, a valid session and remotely enabled project settings.
Settings load asynchronously; unavailable settings prevent recording, while a
valid restored project cache can permit it during a failed/pending refresh.

Remote enablement alone never requests recording. A manual stop remains stopped
until another explicit start. Remote disablement stops new acquisitions.

Supported remote settings are `sessionRecording: true` or an unrestricted object.
Absent/null controls, `sampleRate: "1"` (or equivalent string rate 1),
zero minimum duration and empty trigger/block lists are neutral. Linked flags,
nontrivial sampling, nonempty event/URL triggers or URL blocklists, positive
minimum duration, malformed controls and V2 trigger groups prevent recording.
An endpoint override other than `/s/` is unsupported. Unrelated extension fields
are tolerated. This is not full replay ingestion-control parity.

Acquisition fixes identity, session, timestamp, UUID and ordering. The replay
window ID is the session ID, so a new session also starts a new replay window. Submission
rechecks identity, consent transitions and the current session; changes during
readback reject the frame instead of reattributing it. Backgrounding adds no
special invalidation, but ordinary session expiry still applies. Screenshots
do not extend session activity. Once accepted, work retains its attribution
through later identity, session and consent changes.

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

Defaults are JPEG quality 65 and two frames in flight. Each reservation spans
acquisition, encoding, queued upload and retries. Returning raw pixels to the pool
does not release it. Two retained transient failures pause new acquisition.

Raw buffers allocate lazily, reuse capacity and grow only while available.
Their high-water capacities remain until stop. Stop frees idle buffers; leased
allocations are freed safely when returned. A host-held frame cannot be forcibly
reclaimed by stop or flush.

Frame count is not a process-memory cap. Raw capacities, JPEG working memory,
encoded envelopes, JSON/base64 and gzip buffers all contribute. Serialized and
compressed requests are bounded to 25 MiB; oversized pairs split, and an
oversized singleton is dropped with a sanitized diagnostic. Hosts should
downscale before acquisition to control memory and encoding cost.

## Delivery and lifecycle

Native screenshot events go to `/s/` on the existing delivery worker, not analytics
v1 ingestion. Requests use gzip by default; setting `capture_compression = None`
sends plain JSON without a compression header. Every request contains one session/distinct-ID pair.
Batches are at most two frames and become eligible at two encoded frames or one
second after the first is ready. The shared worker and HTTP can delay attempts.

Retryable failures remain in memory without an attempt-count cutoff, with bounded
jittered backoff. Valid Retry-After is a floor subject to the backoff cap.
Flush does not bypass cooldown. Multi-frame HTTP 413 responses split; singleton
413 and other permanent/quota rejections release affected frames and report
through `on_error`. Retry scheduling is logged at debug level, and shutdown
reports abandoned screenshots once per drain. Diagnostics omit pixels, payloads, tokens and attribution.

Stop requests flush and allows valid outstanding host-held frames to submit.
Client flush includes previously submitted encoding and an eligible replay
delivery attempt, not host-held frames or guaranteed ingestion. Existing
`DrainReport` semantics remain local progress.

Shutdown closes acquisition/submission and uses the existing shared best-effort
budget. In-flight work, callbacks and joins can exceed its elapsed duration.
Queued replay is memory-only: process exit, a crash or shutdown can lose it.
Replay does not acquire a second network client or persistence queue.

## Validation boundaries

Run `cargo run --features session-replay --example stateful_replay` with
`POSTHOG_PROJECT_TOKEN` and `POSTHOG_HOST` pointing at a mock project. The example
uses a synthetic blue image with an already-black masked half. Set
`REPLAY_WIDTH`, `REPLAY_HEIGHT` and `REPLAY_FRAMES` for local measurements.

Synthetic JPEGs and mock traffic establish Rust ownership, dimensions, masking
preservation and wire behavior. They do not establish Unreal renderer safety,
mask discovery, real screenshot privacy, live ingestion or backend playback.
