//! Owns the pipeline and serialises every lifecycle transition, like the
//! `Recorder` actor on macOS: a tray click and an IPC request can never start
//! and stop capture concurrently.
//!
//! Two locks: `inner` for lifecycle and state (held briefly), `preview` for
//! the decoder (held for a decode). A status request never waits on a decode.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

use crate::annotation::{self, Overlay};
use crate::config_store;
use crate::transcode::Reencoder;
use crate::d3d;
use crate::mp4;
use crate::paths;
use crate::pipeline::Pipeline;
use crate::preview::PreviewEngine;
use crate::protocol::{ApplicationInfo, Clip, Crop, DisplayInfo, RawImage, RecorderConfig, State};
use crate::ring::{Snapshot, TICKS_PER_SECOND};

const RETRY_INTERVAL: Duration = Duration::from_secs(5);

struct Inner {
    config: RecorderConfig,
    pipeline: Option<Pipeline>,
    display: Option<DisplayInfo>,
    desired: bool,
    last_error: Option<String>,
    last_attempt: Option<Instant>,
}

pub struct Recorder {
    inner: Mutex<Inner>,
    preview: Mutex<PreviewEngine>,
}

/// What an export or a decode needs once the lock is released.
struct Frozen {
    snapshot: Snapshot,
    size: (u32, u32),
    fps: u32,
}

impl Recorder {
    pub fn new(config: RecorderConfig) -> Arc<Self> {
        Arc::new(Recorder {
            inner: Mutex::new(Inner {
                config,
                pipeline: None,
                display: None,
                desired: false,
                last_error: None,
                last_attempt: None,
            }),
            preview: Mutex::new(PreviewEngine::default()),
        })
    }

    pub fn start(&self) -> State {
        let mut inner = self.inner.lock().unwrap();
        inner.desired = true;
        if inner.pipeline.is_none() {
            Self::begin(&mut inner);
        }
        Self::state_of(&inner)
    }

    pub fn stop(&self) -> State {
        let mut inner = self.inner.lock().unwrap();
        inner.desired = false;
        inner.pipeline = None;
        inner.last_error = None;
        Self::state_of(&inner)
    }

    pub fn apply(&self, config: RecorderConfig) -> State {
        let mut inner = self.inner.lock().unwrap();
        if config == inner.config {
            return Self::state_of(&inner);
        }
        let resume = inner.desired || inner.pipeline.is_some();
        inner.pipeline = None;
        inner.config = config;
        config_store::save(&inner.config);
        if resume {
            inner.desired = true;
            Self::begin(&mut inner);
        }
        Self::state_of(&inner)
    }

    pub fn config(&self) -> RecorderConfig {
        self.inner.lock().unwrap().config.clone()
    }

    /// Called once a second: retries a failed start and rebuilds the
    /// pipeline after a display change, like the retry task on macOS.
    pub fn supervise(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.pipeline.as_ref().is_some_and(|p| p.needs_restart()) {
            inner.pipeline = None;
            inner.last_attempt = None;
        }
        let due = inner.last_attempt.is_none_or(|t| t.elapsed() >= RETRY_INTERVAL);
        if inner.desired && inner.pipeline.is_none() && due {
            Self::begin(&mut inner);
        }
    }

    fn begin(inner: &mut Inner) {
        inner.last_attempt = Some(Instant::now());
        let monitors = d3d::monitors();
        let index = inner.config.display_index.max(0) as usize;
        let Some(monitor) = monitors.get(index).or(monitors.first()) else {
            inner.last_error = Some("no display to capture".into());
            return;
        };
        match Pipeline::start(&inner.config, monitor) {
            Ok(pipeline) => {
                inner.display = displays().into_iter().find(|d| d.screen_x == monitor.rect.left as f64 && d.screen_y == monitor.rect.top as f64);
                inner.pipeline = Some(pipeline);
                inner.last_error = None;
            }
            Err(error) => {
                eprintln!("rewindd: {error}");
                inner.last_error = Some(error);
            }
        }
    }

    pub fn state(&self) -> State {
        Self::state_of(&self.inner.lock().unwrap())
    }

    fn state_of(inner: &Inner) -> State {
        let o = Ordering::Relaxed;
        let pipeline = inner.pipeline.as_ref();
        let stats = pipeline.map(|p| p.ring.statistics());
        let (bytes, duration) = stats.as_ref().map(|s| (s.bytes, s.duration)).unwrap_or((0, 0.0));
        State {
            recording: pipeline.is_some(),
            config: inner.config.clone(),
            display: inner.display.clone(),
            width: pipeline.map(|p| p.width as i64).unwrap_or(0),
            height: pipeline.map(|p| p.height as i64).unwrap_or(0),
            buffer_seconds: inner.config.buffer_seconds,
            available_seconds: duration,
            buffer_bytes: bytes as i64,
            bitrate: if duration > 0.0 { bytes as f64 * 8.0 / duration } else { 0.0 },
            frames_captured: pipeline.map(|p| p.capture_counters.captured.load(o) as i64).unwrap_or(0),
            frames_encoded: pipeline.map(|p| p.encoder_counters.encoded.load(o) as i64).unwrap_or(0),
            dropped_frames: pipeline.map(|p| p.capture_counters.dropped.load(o) as i64).unwrap_or(0),
            idle_notices: pipeline.map(|p| p.capture_counters.skipped.load(o) as i64).unwrap_or(0),
            on_battery: on_battery(),
            last_error: inner.last_error.clone(),
            newest_timestamp: stats.and_then(|s| s.newest),
        }
    }

    fn freeze(&self, from: f64, to: f64, anchor: Option<f64>) -> Result<Frozen, String> {
        let inner = self.inner.lock().unwrap();
        let pipeline = inner.pipeline.as_ref().ok_or("rewindd is not recording")?;
        Ok(Frozen {
            snapshot: pipeline.ring.snapshot(from, to, anchor)?,
            size: (pipeline.width, pipeline.height),
            fps: pipeline.fps,
        })
    }

    /// Without annotations or a crop the encoded frames are muxed as they
    /// are; either one needs every frame decoded, changed and re-encoded.
    pub fn export(
        &self,
        from: f64,
        to: f64,
        path: Option<PathBuf>,
        anchor: Option<f64>,
        overlay_path: Option<&str>,
        crop: Option<&Crop>,
    ) -> Result<Clip, String> {
        let frozen = self.freeze(from, to, anchor)?;
        let path = path.unwrap_or_else(|| paths::clips_dir().join(paths::timestamped_clip_name()));
        let overlay = overlay_path.map(Overlay::load).transpose()?;

        let (snapshot, size) = if overlay.is_some() || crop.is_some() {
            let overlay = overlay.map(|o| o.fitted(frozen.size.0, frozen.size.1));
            let size = crop.map(|c| c.pixels(frozen.size.0, frozen.size.1)).map(|(_, _, w, h)| (w, h)).unwrap_or(frozen.size);
            (compose(&frozen, overlay.as_ref(), crop, size)?, size)
        } else {
            (frozen.snapshot, frozen.size)
        };

        let bytes = mp4::write(
            &path,
            &mp4::Track {
                width: size.0,
                height: size.1,
                parameters: &snapshot.parameters,
                samples: &snapshot.packets,
                frame_ticks: TICKS_PER_SECOND / frozen.fps as i64,
            },
        )
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        Ok(Clip {
            path: path.display().to_string(),
            duration: snapshot.duration(),
            bytes: bytes as i64,
            frames: snapshot.packets.len() as i64,
            width: size.0 as i64,
            height: size.1 as i64,
        })
    }

    /// A native frame with the annotations burnt in and the crop applied.
    pub fn composed_frame(&self, seconds_ago: f64, anchor: Option<f64>, overlay_path: Option<&str>, crop: Option<&Crop>) -> Result<RawImage, String> {
        let frame = self.preview(seconds_ago, 0, anchor)?;
        let overlay = overlay_path.map(Overlay::load).transpose()?.map(|o| o.fitted(frame.width, frame.height));
        Ok(annotation::apply(frame, overlay.as_ref(), crop))
    }

    /// Exact encoded size of a range: the exporter's cut, without muxing.
    pub fn estimate(&self, from: f64, to: f64, anchor: Option<f64>) -> Result<i64, String> {
        Ok(self.freeze(from, to, anchor)?.snapshot.bytes() as i64)
    }

    pub fn preview(&self, seconds_ago: f64, width: u32, anchor: Option<f64>) -> Result<RawImage, String> {
        let frozen = self.freeze(f64::INFINITY, 0.0, anchor)?;
        let now = frozen.snapshot.now(anchor);
        let index = frozen.snapshot.frame_index(now - seconds_ago);
        self.preview.lock().unwrap().frame(&frozen.snapshot, index, width, frozen.size)
    }

    pub fn thumbnails(&self, count: usize, width: u32, anchor: Option<f64>) -> Result<Vec<RawImage>, String> {
        let frozen = self.freeze(f64::INFINITY, 0.0, anchor)?;
        let now = frozen.snapshot.now(anchor);
        let span = frozen.snapshot.duration().max(0.5);
        let count = count.clamp(1, 64);
        let indices: Vec<usize> = (0..count)
            .map(|slot| {
                let ago = span * (1.0 - (slot as f64 + 0.5) / count as f64);
                frozen.snapshot.keyframe_index(frozen.snapshot.frame_index(now - ago))
            })
            .collect();
        self.preview.lock().unwrap().thumbnails(&frozen.snapshot, &indices, width, frozen.size)
    }

    /// First and last frame of a range, from keyframes like thumbnails.
    pub fn edge_frames(&self, from: f64, to: f64, width: u32, anchor: Option<f64>) -> Result<Vec<RawImage>, String> {
        let frozen = self.freeze(f64::INFINITY, 0.0, anchor)?;
        let now = frozen.snapshot.now(anchor);
        let indices: Vec<usize> = [from, to]
            .iter()
            .map(|ago| frozen.snapshot.keyframe_index(frozen.snapshot.frame_index(now - ago)))
            .collect();
        self.preview.lock().unwrap().thumbnails(&frozen.snapshot, &indices, width, frozen.size)
    }
}

/// Decodes the range in order (one decode per frame), burns in the drawing,
/// crops, and re-encodes. A decoder of its own, so the overlay's preview
/// state is left alone.
fn compose(frozen: &Frozen, overlay: Option<&Overlay>, crop: Option<&Crop>, size: (u32, u32)) -> Result<Snapshot, String> {
    let mut decoder = PreviewEngine::default();
    let mut encoder = Reencoder::new(size, frozen.fps)?;
    for (index, packet) in frozen.snapshot.packets.iter().enumerate() {
        let frame = decoder.frame(&frozen.snapshot, index, 0, frozen.size)?;
        let frame = annotation::apply(frame, overlay, crop);
        encoder.push(&frame, packet.pts)?;
    }
    let output = encoder.finish()?;
    // One frame out per frame in is the low-latency encoder's contract.
    if output.packets.len() != frozen.snapshot.packets.len() {
        eprintln!("rewindd: re-encode produced {} of {} frames", output.packets.len(), frozen.snapshot.packets.len());
    }
    Ok(output)
}

pub fn displays() -> Vec<DisplayInfo> {
    d3d::monitors()
        .iter()
        .enumerate()
        .map(|(index, m)| {
            let (width, height) = (m.rect.right - m.rect.left, m.rect.bottom - m.rect.top);
            DisplayInfo {
                index: index as i64,
                display_id: m.name.trim_start_matches(r"\\.\DISPLAY").parse().unwrap_or(index as u32),
                width: width as i64,
                height: height as i64,
                is_main: m.primary,
                safe_area_top: 0.0,
                screen_x: m.rect.left as f64,
                screen_y: m.rect.top as f64,
                screen_width: width as f64,
                screen_height: height as f64,
            }
        })
        .collect()
}

/// Single-application capture is not implemented on Windows yet.
pub fn applications() -> Vec<ApplicationInfo> {
    Vec::new()
}

fn on_battery() -> bool {
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status).is_ok() && status.ACLineStatus == 0 }
}
