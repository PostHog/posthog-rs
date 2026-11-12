//! Owned screenshot acquisition and end-to-end admission. No renderer access.
#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;
use chrono::{DateTime, Utc};
use std::sync::{Arc, Mutex, Weak};
use uuid::Uuid;

use super::producer::Producer;
use super::sessions::{Operation, SessionClock};
use super::writer::StateAccess;
use super::{Client, Config, Snapshot};
use crate::client::transport::{Completion, Control};

#[path = "replay_encoder.rs"]
pub(crate) mod encoder;
#[path = "replay_policy.rs"]
mod policy;

/// Screenshot encoding and resident frame limits. Recording still requires start.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ReplayConfig {
    /// JPEG quality, 1..=100. Default: 65.
    pub jpeg_quality: u8,
    /// Frames from acquisition through delivery/retries. Default: 2.
    pub max_frames_in_flight: usize,
}
impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            jpeg_quality: 65,
            max_frames_in_flight: 2,
        }
    }
}

/// Host-prepared, top-down pixel byte order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PixelFormat {
    /// Eight-bit B, G, R, A channels. Alpha is ignored; the image is composited.
    Bgra8888,
}

/// Already-masked and already-downscaled pixels; encoding does not resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameLayout {
    pub width: u32,
    pub height: u32,
    /// Includes row padding; at least width * 4.
    pub row_stride_bytes: usize,
    pub format: PixelFormat,
}
impl FrameLayout {
    fn byte_len(self) -> Option<usize> {
        if self.width == 0
            || self.height == 0
            || self.width > u16::MAX as u32
            || self.height > u16::MAX as u32
        {
            return None;
        }
        let pixel_bytes = match self.format {
            PixelFormat::Bgra8888 => 4,
        };
        let row = (self.width as usize).checked_mul(pixel_bytes)?;
        if self.row_stride_bytes < row {
            return None;
        }
        let bytes = self.row_stride_bytes.checked_mul(self.height as usize)?;
        (bytes <= isize::MAX as usize).then_some(bytes)
    }
}

/// Local submission result, not hook acceptance, encoding or ingestion success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubmitOutcome {
    Accepted,
    /// The frame belongs to another client's recorder.
    WrongRecorder,
    /// A before_send callback is already preparing an event for this client.
    Reentrant,
    /// Identity, consent or the active session changed during acquisition.
    Stale,
    /// The client or encoder has closed.
    Closed,
}

/// A cheap non-owning handle. Obtaining/dropping it does not start/stop recording.
#[derive(Clone)]
pub struct SessionReplay(Weak<Controller>);

/// One single-use acquisition, containing attribution and its owned pixel lease.
pub struct ReplayFrame {
    pub(super) layout: FrameLayout,
    pub(super) pixels: Vec<u8>,
    pub(super) snapshot: Arc<Snapshot>,
    pub(super) attribution: Attribution,
    permit: Option<Permit>,
    run: u64,
}
impl ReplayFrame {
    /// Fill every image pixel with already-masked, composited BGRA8888 data.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }
}
impl Drop for ReplayFrame {
    fn drop(&mut self) {
        if let Some(permit) = &self.permit {
            let mut pool = permit.pool.lock().unwrap_or_else(|p| p.into_inner());
            if pool.active && pool.run == self.run {
                pool.buffers.push(std::mem::take(&mut self.pixels));
            }
        }
    }
}

pub(crate) struct Permit {
    pool: Arc<Mutex<Pool>>,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.pool
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .in_flight -= 1;
    }
}
struct Pool {
    active: bool,
    run: u64,
    sequence: u64,
    in_flight: usize,
    max_frames: usize,
    buffers: Vec<Vec<u8>>,
}

#[derive(Clone)]
pub(crate) struct Attribution {
    pub(crate) distinct_id: String,
    pub(crate) session_id: Uuid,
    pub(crate) window_id: Uuid,
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) uuid: Uuid,
    pub(crate) sequence: u64,
    generation: u64,
    consent_generation: u64,
}

/// Serialized native envelope plus the reservation retained until terminal disposition.
pub(crate) struct EncodedFrame {
    pub(crate) bytes: Vec<u8>,
    pub(crate) attribution: Attribution,
    _permit: Permit,
}

pub(super) struct Controller {
    state: StateAccess,
    pool: Arc<Mutex<Pool>>,
    gate: Arc<encoder::Gate>,
    session_clock: Arc<dyn SessionClock>,
    default_opt_out: bool,
    window_id: Uuid,
    capture_key: usize,
}
impl Controller {
    pub(super) fn new(
        state: &super::writer::StateWriter,
        transport: &crate::client::transport::TransportHandle,
        producer: &Producer,
        config: &Config,
    ) -> Option<Arc<Self>> {
        let gate = transport
            .install_replay(
                producer.clone(),
                &config.session_replay,
                config.project_token.clone(),
                config.shutdown_timeout,
            )
            .ok()?;
        Some(Arc::new(Self {
            state: state.access(),
            pool: Arc::new(Mutex::new(Pool {
                active: false,
                run: 0,
                sequence: 0,
                in_flight: 0,
                max_frames: config.session_replay.max_frames_in_flight,
                buffers: Vec::new(),
            })),
            gate,
            session_clock: producer.session_clock.clone(),
            default_opt_out: config.opt_out,
            window_id: Uuid::now_v7(),
            capture_key: producer.capture_key(),
        }))
    }
    pub(super) fn close(&self) {
        self.stop();
    }
    fn stop(&self) {
        let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
        pool.active = false;
        pool.run = pool.run.wrapping_add(1);
        pool.buffers.clear();
    }
    fn allowed(&self, snapshot: &Snapshot) -> bool {
        !snapshot.opt_out.unwrap_or(self.default_opt_out)
            && policy::eligible(
                snapshot
                    .project_config
                    .as_ref()
                    .and_then(|c| c.session_recording.as_ref()),
            )
    }
}

impl Client {
    /// Manual screenshot recording; requires remote permission and current consent.
    pub fn session_replay(&self) -> SessionReplay {
        SessionReplay(self.replay.as_ref().map_or_else(Weak::new, Arc::downgrade))
    }
}
impl SessionReplay {
    /// Request recording and resume/create a session without extending activity.
    /// Remote permission is still required. Remote settings never undo manual stop.
    pub fn start(&self) {
        let Some(controller) = self.0.upgrade() else {
            return;
        };
        if controller.gate.is_closed() {
            return;
        }
        let now = controller.session_clock.now();
        if controller
            .state
            .mutate(|snapshot| {
                let opted_out = snapshot.opt_out.unwrap_or(controller.default_opt_out);
                if !opted_out && snapshot.sessions.active_id(now.elapsed).is_none() {
                    snapshot.session_operation(Operation::End, now, false);
                    snapshot.session_operation(Operation::Start, now, false);
                }
            })
            .is_ok()
        {
            controller
                .pool
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .active = true;
        }
    }
    /// Stop acquisition, free idle buffers, and request flush. Valid host-held
    /// frames can still submit; only explicit start requests recording again.
    pub fn stop(&self) {
        let Some(controller) = self.0.upgrade() else {
            return;
        };
        controller.stop();
        let (tx, _) = std::sync::mpsc::channel();
        let _ = controller
            .gate
            .control(Control::Flush(Completion::Blocking(tx)));
    }
    /// Passive memory-only eligibility check; does not create or touch sessions.
    pub fn is_active(&self) -> bool {
        let Some(c) = self.0.upgrade() else {
            return false;
        };
        !c.gate.is_closed()
            && c.pool.lock().unwrap_or_else(|p| p.into_inner()).active
            && c.state.snapshot().is_some_and(|s| {
                c.allowed(&s)
                    && s.sessions
                        .active_id(c.session_clock.now().elapsed)
                        .is_some()
            })
    }
    /// Reserve immediately before acquisition. None means inactive, invalid
    /// layout, allocation failure, or end-to-end capacity exhausted.
    pub fn begin_frame(&self, layout: FrameLayout) -> Option<ReplayFrame> {
        let bytes = layout.byte_len()?;
        let c = self.0.upgrade()?;
        if super::producer::CaptureGuard::active(c.capture_key) {
            return None;
        }
        if c.gate.is_closed() {
            return None;
        }
        let snapshot = c.state.snapshot()?;
        if !c.allowed(&snapshot) {
            return None;
        }
        let session_id = snapshot.sessions.active_id(c.session_clock.now().elapsed)?;
        let mut pool = c.pool.lock().unwrap_or_else(|p| p.into_inner());
        if !pool.active || pool.in_flight >= pool.max_frames {
            return None;
        }
        let sequence = pool.sequence.checked_add(1)?;
        let mut pixels = pool.buffers.pop().unwrap_or_default();
        if pixels
            .try_reserve_exact(bytes.saturating_sub(pixels.len()))
            .is_err()
        {
            pool.buffers.push(pixels);
            return None;
        }
        pixels.resize(bytes, 0);
        pool.sequence = sequence;
        pool.in_flight += 1;
        let attribution = Attribution {
            distinct_id: snapshot.distinct_id.clone(),
            session_id,
            window_id: c.window_id,
            timestamp: Utc::now(),
            uuid: Uuid::now_v7(),
            sequence,
            generation: snapshot.generation,
            consent_generation: snapshot.replay_consent_generation,
        };
        Some(ReplayFrame {
            layout,
            pixels,
            snapshot,
            attribution,
            permit: Some(Permit {
                pool: c.pool.clone(),
            }),
            run: pool.run,
        })
    }
    /// Consume the frame. Accepted work can later be filtered by before_send or
    /// fail encoding/delivery. Manual stop does not invalidate host-held frames.
    pub fn submit_frame(&self, frame: ReplayFrame) -> SubmitOutcome {
        let Some(c) = self.0.upgrade() else {
            return SubmitOutcome::Closed;
        };
        if c.gate.is_closed() {
            return SubmitOutcome::Closed;
        }
        if super::producer::CaptureGuard::active(c.capture_key) {
            return SubmitOutcome::Reentrant;
        }
        if !Arc::ptr_eq(&c.pool, &frame.permit.as_ref().unwrap().pool) {
            return SubmitOutcome::WrongRecorder;
        }
        let Some(snapshot) = c.state.snapshot() else {
            return SubmitOutcome::Closed;
        };
        if snapshot.opt_out.unwrap_or(c.default_opt_out)
            || snapshot.generation != frame.attribution.generation
            || snapshot.distinct_id != frame.attribution.distinct_id
            || snapshot.replay_consent_generation != frame.attribution.consent_generation
            || snapshot.sessions.active_id(c.session_clock.now().elapsed)
                != Some(frame.attribution.session_id)
        {
            return SubmitOutcome::Stale;
        }
        let flush = !c.pool.lock().unwrap_or_else(|p| p.into_inner()).active;
        if c.gate.frame(frame, flush) {
            SubmitOutcome::Accepted
        } else {
            SubmitOutcome::Closed
        }
    }
}

impl ReplayFrame {
    pub(super) fn finish(mut self, bytes: Vec<u8>) -> EncodedFrame {
        // Return raw capacity without releasing the end-to-end reservation.
        let permit = self.permit.take().unwrap();
        {
            let mut pool = permit.pool.lock().unwrap_or_else(|p| p.into_inner());
            if pool.active && pool.run == self.run {
                pool.buffers.push(std::mem::take(&mut self.pixels));
            }
        }
        EncodedFrame {
            bytes,
            attribution: self.attribution.clone(),
            _permit: permit,
        }
    }
}
