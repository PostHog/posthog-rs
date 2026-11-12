//! One bounded-by-admission encoder. Controls cross its FIFO before delivery.
use std::io;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine};
use jpeg_encoder::{rgb_to_ycbcr, ImageBuffer, JpegColorType};
use serde::Serialize;
use serde_json::json;

use super::super::producer::{CaptureGuard, Producer};
use super::super::{Capture, CaptureOutcome};
use super::{FrameLayout, ReplayConfig, ReplayFrame};
use crate::client::event_store::RecordBuffer;
use crate::client::transport::replay::MAX_BODY_BYTES;
use crate::client::transport::{Completion, Control};
use crate::constants::{
    COOKIELESS_MODE_OPT, COOKIELESS_MODE_PROP, DISABLE_SKEW_CORRECTION_OPT, IGNORE_SENT_AT_PROP,
};

// The native playback transformer reserves lower IDs for its synthetic document.
const SCREENSHOT_ID: u64 = 10_000_000;

pub(crate) trait ImageEncoder: Send + 'static {
    fn encode(
        &self,
        layout: FrameLayout,
        pixels: &[u8],
        quality: u8,
    ) -> Result<Vec<u8>, &'static str>;
}
struct Jpeg;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jpeg_ignores_alpha_and_padding_without_resizing() {
        let packed = FrameLayout {
            width: 32,
            height: 24,
            row_stride_bytes: 128,
            format: super::super::PixelFormat::Bgra8888,
        };
        let padded = FrameLayout {
            row_stride_bytes: 144,
            ..packed
        };
        let mut pixels = vec![0; 128 * 24];
        let mut rows = vec![255; 144 * 24];
        for y in 0..24 {
            for x in 0..32 {
                pixels[y * 128 + x * 4..][..4].copy_from_slice(&[255, 17, 3, 0]);
                rows[y * 144 + x * 4..][..4].copy_from_slice(&[255, 17, 3, 255]);
            }
        }
        assert_eq!(
            Jpeg.encode(packed, &pixels, 65).unwrap(),
            Jpeg.encode(padded, &rows, 65).unwrap()
        );
    }

    #[test]
    fn jpeg_decodes_dimensions_colors_rows_and_masked_pixels() {
        let layout = FrameLayout {
            width: 35,
            height: 27,
            row_stride_bytes: 35 * 4 + 12,
            format: super::super::PixelFormat::Bgra8888,
        };
        let mut pixels = vec![255; layout.row_stride_bytes * layout.height as usize];
        for y in 0..layout.height as usize {
            for x in 0..layout.width as usize {
                let color = match (x < 16, y < 16) {
                    (true, true) => [255, 0, 0, 7],
                    (false, true) => [0, 0, 255, 31],
                    (true, false) => [0, 255, 0, 77],
                    (false, false) => [0, 0, 0, 255],
                };
                let start = y * layout.row_stride_bytes + x * 4;
                pixels[start..start + 4].copy_from_slice(&color);
            }
        }
        let jpeg = Jpeg.encode(layout, &pixels, 65).unwrap();
        let mut decoder = jpeg_decoder::Decoder::new(jpeg.as_slice());
        let rgb = decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        assert_eq!((info.width, info.height), (35, 27));
        assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
        for (x, y, expected) in [
            (4, 4, [0u8, 0, 255]),
            (28, 4, [255, 0, 0]),
            (4, 22, [0, 255, 0]),
            (28, 22, [0, 0, 0]),
        ] {
            let start = (y * 35 + x) * 3;
            let actual = &rgb[start..start + 3];
            assert!(
                actual
                    .iter()
                    .zip(expected)
                    .all(|(a, b)| a.abs_diff(b) <= 25),
                "pixel ({}, {}): {:?}, expected {:?}",
                x,
                y,
                actual,
                expected
            );
        }
    }

    #[test]
    #[ignore = "local encoding throughput measurement"]
    fn replay_codec_benchmark() {
        for (width, height) in [(640, 360), (1280, 720), (1920, 1080)] {
            let layout = FrameLayout {
                width,
                height,
                row_stride_bytes: width as usize * 4,
                format: super::super::PixelFormat::Bgra8888,
            };
            let mut pixels = vec![0; layout.row_stride_bytes * height as usize];
            let mut seed = 47u32;
            for (i, pixel) in pixels.chunks_exact_mut(4).enumerate() {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                if i % (width as usize) < width as usize / 2 {
                    pixel.copy_from_slice(&[
                        seed as u8,
                        (seed >> 8) as u8,
                        (seed >> 16) as u8,
                        255,
                    ]);
                }
            }
            let start = Instant::now();
            let mut output_bytes = 0;
            for _ in 0..8 {
                output_bytes += Jpeg.encode(layout, &pixels, 65).unwrap().len();
            }
            println!("replay_codec width={} height={} quality=65 frames=8 elapsed_us={} raw_capacity_bytes={} jpeg_bytes_mean={}", width, height, start.elapsed().as_micros(), pixels.capacity(), output_bytes / 8);
        }
    }
}

struct Bgra<'a> {
    layout: FrameLayout,
    pixels: &'a [u8],
}
impl ImageBuffer for Bgra<'_> {
    fn get_jpeg_color_type(&self) -> JpegColorType {
        JpegColorType::Ycbcr
    }
    fn width(&self) -> u16 {
        self.layout.width as u16
    }
    fn height(&self) -> u16 {
        self.layout.height as u16
    }
    fn fill_buffers(&self, y: u16, buffers: &mut [Vec<u8>; 4]) {
        let start = y as usize * self.layout.row_stride_bytes;
        for pixel in self.pixels[start..start + self.layout.width as usize * 4].chunks_exact(4) {
            let (y, cb, cr) = rgb_to_ycbcr(pixel[2], pixel[1], pixel[0]);
            buffers[0].push(y);
            buffers[1].push(cb);
            buffers[2].push(cr);
        }
    }
}
impl ImageEncoder for Jpeg {
    fn encode(
        &self,
        layout: FrameLayout,
        pixels: &[u8],
        quality: u8,
    ) -> Result<Vec<u8>, &'static str> {
        let mut output = RecordBuffer {
            bytes: Vec::new(),
            limit: (MAX_BODY_BYTES / 4 * 3) as u64,
        };
        jpeg_encoder::Encoder::new(&mut output, quality)
            .encode_image(Bgra { layout, pixels })
            .map_err(|_| "Screenshot JPEG encoding failed or exceeded its byte limit")?;
        Ok(output.bytes)
    }
}

enum Message {
    Frame(Box<ReplayFrame>, bool),
    Control(Control),
}
struct GateState {
    tx: mpsc::Sender<Message>,
    closed: bool,
}
pub(crate) struct Gate {
    state: Mutex<GateState>,
    deadline: Arc<Mutex<Option<Instant>>>,
    shutdown_timeout: Duration,
}
impl Gate {
    pub(crate) fn close_deadline(&self, deadline: Option<Instant>) -> Instant {
        deadline.unwrap_or_else(|| Instant::now() + self.shutdown_timeout)
    }
    pub(crate) fn is_closed(&self) -> bool {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).closed
    }
    pub(crate) fn frame(&self, frame: ReplayFrame, flush: bool) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed {
            return false;
        }
        if state
            .tx
            .send(Message::Frame(Box::new(frame), flush))
            .is_err()
        {
            state.closed = true;
            return false;
        }
        true
    }
    pub(crate) fn control(&self, control: Control) -> Result<(), Control> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed && matches!(control, Control::Shutdown { .. }) {
            return Err(control);
        }
        if let Control::Shutdown { deadline, .. } = &control {
            state.closed = true;
            *self.deadline.lock().unwrap_or_else(|p| p.into_inner()) =
                Some(deadline.unwrap_or_else(|| Instant::now() + self.shutdown_timeout));
        }
        state.tx.send(Message::Control(control)).map_err(|e| {
            state.closed = true;
            match e.0 {
                Message::Control(control) => control,
                Message::Frame(..) => unreachable!(),
            }
        })
    }
}

pub(crate) struct Worker {
    pub(crate) gate: Arc<Gate>,
    id: ThreadId,
    thread: Mutex<Option<JoinHandle<()>>>,
}
impl Worker {
    pub(crate) fn spawn(
        producer: Producer,
        config: &ReplayConfig,
        token: String,
        delivery: mpsc::Sender<Control>,
        shutdown_timeout: Duration,
    ) -> io::Result<Self> {
        Self::spawn_with_encoder(
            producer,
            config,
            token,
            delivery,
            shutdown_timeout,
            Box::new(Jpeg),
        )
    }
    pub(super) fn spawn_with_encoder(
        producer: Producer,
        config: &ReplayConfig,
        token: String,
        delivery: mpsc::Sender<Control>,
        shutdown_timeout: Duration,
        codec: Box<dyn ImageEncoder>,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let deadline = Arc::new(Mutex::new(None));
        let gate = Arc::new(Gate {
            state: Mutex::new(GateState { tx, closed: false }),
            deadline: deadline.clone(),
            shutdown_timeout,
        });
        let quality = config.jpeg_quality;
        let thread = thread::Builder::new()
            .name("posthog-replay-encoder".into())
            .spawn(move || {
                while let Ok(message) = rx.recv() {
                    match message {
                        Message::Control(control) => {
                            let stop = matches!(control, Control::Shutdown { .. });
                            if delivery.send(control).is_err() || stop {
                                break;
                            }
                        }
                        Message::Frame(frame, flush) => {
                            if deadline
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .is_some_and(|end| Instant::now() >= end)
                            {
                                let _ = delivery.send(Control::ReplayShutdownAbandoned);
                                continue;
                            }
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    encode(*frame, &producer, &token, quality, codec.as_ref())
                                }));
                            let expired = deadline
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .is_some_and(|end| Instant::now() >= end);
                            let control = match result {
                                Ok(Ok(Some(_))) if expired => {
                                    Some(Control::ReplayShutdownAbandoned)
                                }
                                Ok(Ok(Some(encoded))) => {
                                    Some(Control::ReplayFrame(Box::new(encoded)))
                                }
                                Ok(Ok(None)) => None,
                                Ok(Err(message)) => Some(Control::ReplayFailure(message)),
                                Err(_) => Some(Control::ReplayFailure(
                                    "Screenshot encoder panicked; frame discarded",
                                )),
                            };
                            if let Some(control) = control {
                                if delivery.send(control).is_err() {
                                    break;
                                }
                            }
                            if flush && !expired {
                                let (tx, _) = mpsc::channel();
                                if delivery
                                    .send(Control::Flush(Completion::Blocking(tx)))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            id: thread.thread().id(),
            gate,
            thread: Mutex::new(Some(thread)),
        })
    }
    pub(crate) fn on_worker_thread(&self) -> bool {
        self.id == thread::current().id()
    }
    pub(crate) fn join(&self) {
        if self.on_worker_thread() {
            return;
        }
        if let Some(worker) = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = worker.join();
        }
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    api_key: &'a str,
    event: &'a str,
    distinct_id: &'a str,
    uuid: uuid::Uuid,
    timestamp: String,
    properties: &'a std::collections::HashMap<String, serde_json::Value>,
}
fn encode(
    frame: ReplayFrame,
    producer: &Producer,
    token: &str,
    quality: u8,
    codec: &dyn ImageEncoder,
) -> Result<Option<super::EncodedFrame>, &'static str> {
    let jpeg = codec.encode(frame.layout, &frame.pixels, quality)?;
    let timestamp = frame.attribution.timestamp;
    let session = frame.attribution.session_id.to_string();
    let window = frame.attribution.window_id.to_string();
    let mut input = Capture::new("$snapshot");
    input.uuid = Some(frame.attribution.uuid);
    input.timestamp = Some(timestamp);
    input
        .options
        .insert("process_person_profile".into(), false.into());
    input.properties.extend([
        ("$snapshot_source".into(), json!("mobile")),
        ("$session_id".into(), json!(session)),
        ("$window_id".into(), json!(window)),
        ("$process_person_profile".into(), json!(false)),
        ("$snapshot_data".into(), json!([
            {"type":4,"timestamp":timestamp.timestamp_millis(),"data":{"width":frame.layout.width,"height":frame.layout.height}},
            {"type":2,"timestamp":timestamp.timestamp_millis(),"data":{"initialOffset":{"top":0,"left":0},"wireframes":[{
                "id":SCREENSHOT_ID,"x":0,"y":0,"width":frame.layout.width,"height":frame.layout.height,"type":"screenshot",
                "base64":format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg))
            }]}}
        ])),
    ]);
    let _guard = CaptureGuard::enter_key(producer.capture_key())
        .map_err(|_| "Screenshot hook recursion guard unavailable")?;
    let mut event = match producer.assemble(
        input,
        &frame.snapshot,
        timestamp,
        Some(frame.attribution.session_id),
    ) {
        Ok(event) => event,
        Err(CaptureOutcome::Dropped) => return Ok(None),
        Err(_) => return Err("Screenshot hook produced an invalid event"),
    };
    if event.event_name() != "$snapshot"
        || event.distinct_id() != frame.attribution.distinct_id
        || event.uuid() != Some(frame.attribution.uuid)
        || event.timestamp() != Some(timestamp.naive_utc())
        || event
            .properties()
            .get("$session_id")
            .and_then(|v| v.as_str())
            != Some(&session)
        || event
            .properties()
            .get("$window_id")
            .and_then(|v| v.as_str())
            != Some(&window)
        || !event
            .properties()
            .get("$snapshot_data")
            .is_some_and(|v| v.as_array().is_some_and(|v| !v.is_empty()))
    {
        return Err("Screenshot hook changed required attribution or snapshot fields");
    }
    for (property, option) in [
        (COOKIELESS_MODE_PROP, COOKIELESS_MODE_OPT),
        (IGNORE_SENT_AT_PROP, DISABLE_SKEW_CORRECTION_OPT),
    ] {
        if let Some(value) = event
            .options()
            .get(option)
            .filter(|value| !value.is_null())
            .cloned()
        {
            event.set_prop(property.to_owned(), value);
        }
    }
    let mut body = RecordBuffer {
        bytes: Vec::new(),
        limit: (MAX_BODY_BYTES - 2) as u64,
    };
    serde_json::to_writer(
        &mut body,
        &Envelope {
            api_key: token,
            event: event.event_name(),
            distinct_id: event.distinct_id(),
            uuid: frame.attribution.uuid,
            timestamp: timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            properties: event.properties(),
        },
    )
    .map_err(|_| "Screenshot envelope exceeds its byte or serialization limit")?;
    Ok(Some(frame.finish(body.bytes)))
}
