//! Synthetic, already-masked BGRA screenshots; no renderer or live credentials required.
use posthog::stateful::{Client, Config, FrameLayout, Persistence, PixelFormat, SubmitOutcome};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

fn number(name: &str, default: u32) -> Result<u32, Box<dyn std::error::Error>> {
    Ok(std::env::var(name)
        .ok()
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(default))
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::new(std::env::var("POSTHOG_PROJECT_TOKEN")?);
    config.host = std::env::var("POSTHOG_HOST")?;
    config.persistence = Persistence::Memory;
    config.preload_feature_flags = false;
    config.shutdown_timeout = Duration::from_secs(2);
    let drop_frames = std::env::var("REPLAY_DROP").as_deref() == Ok("true");
    config.before_send(move |mut event| {
        if event.event_name() == "$snapshot" {
            if drop_frames {
                return None;
            }
            event.remove_prop("private_label");
            event.insert_prop("synthetic_hook", true).ok()?;
        }
        Some(event)
    });
    let errors = Arc::new(AtomicUsize::new(0));
    let seen = errors.clone();
    config.on_error(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    let client = Client::init_blocking(config);
    client.flush_blocking();
    let replay = client.session_replay();
    replay.start();
    let active = replay.is_active();
    let width = number("REPLAY_WIDTH", 1280)?;
    let height = number("REPLAY_HEIGHT", 720)?;
    let count = number("REPLAY_FRAMES", 2)?;
    let stride = (width as usize).checked_mul(4).ok_or("invalid row size")?;
    let layout = FrameLayout {
        width,
        height,
        row_stride_bytes: stride,
        format: PixelFormat::Bgra8888,
    };
    let mut accepted = 0;
    let mut acquisition_us = 0;
    let mut fill_us = 0;
    let mut submission_us = 0;
    let mut analytics_capture_us = 0;
    let mut captured_analytics = false;
    let start = Instant::now();
    for index in 0..count {
        let before = Instant::now();
        let Some(mut frame) = replay.begin_frame(layout) else {
            break;
        };
        acquisition_us += before.elapsed().as_micros();
        let before = Instant::now();
        for row in frame.pixels_mut().chunks_exact_mut(stride) {
            for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                // The right half is masked black before submission.
                pixel.copy_from_slice(if x < width as usize / 2 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 0, 255]
                });
            }
        }
        fill_us += before.elapsed().as_micros();
        let before = Instant::now();
        if replay.submit_frame(frame) == SubmitOutcome::Accepted {
            accepted += 1;
        }
        submission_us += before.elapsed().as_micros();
        if !captured_analytics {
            let before = Instant::now();
            client.capture("synthetic replay example");
            analytics_capture_us = before.elapsed().as_micros();
            captured_analytics = true;
        }
        if index % 2 == 1 {
            client.flush_blocking();
        }
    }
    if !captured_analytics {
        let before = Instant::now();
        client.capture("synthetic replay example");
        analytics_capture_us = before.elapsed().as_micros();
    }
    client.flush_blocking();
    replay.stop();
    client.shutdown_blocking(None);
    println!("{{\"active\":{},\"accepted\":{},\"width\":{},\"height\":{},\"jpeg_quality\":65,\"acquisition_us\":{},\"fill_us\":{},\"submission_us\":{},\"analytics_capture_us\":{},\"elapsed_ms\":{},\"errors\":{}}}",
        active, accepted, width, height, acquisition_us, fill_us, submission_us, analytics_capture_us, start.elapsed().as_millis(), errors.load(Ordering::SeqCst));
    Ok(())
}
