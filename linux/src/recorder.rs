//! Owns the capture and serialises every lifecycle transition: a CLI request
//! and the supervisor thread can never start and stop capture concurrently.
//!
//! The portal picker makes a start slow, so the lock is never held across it.
//! A start that finishes after a `stop` or a config change is thrown away.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::annotation::{self, Overlay};
use crate::capture::Capture;
use crate::config_store;
use crate::image_file::{self, GifWriter};
use crate::mp4;
use crate::paths;
use crate::preview::PreviewEngine;
use crate::protocol::{ApplicationInfo, Clip, Crop, DisplayInfo, ExportFormat, ExportOptions, RawImage, RecorderConfig, State};
use crate::ring::{self, Snapshot, TICKS_PER_SECOND};
use crate::transcode::Reencoder;

/// How long a pause request keeps capture from appending. The overlay
/// refreshes it every five seconds; a crashed overlay resumes within this.
const PAUSE_LEASE: Duration = Duration::from_secs(15);

struct Inner {
    config: RecorderConfig,
    capture: Option<Capture>,
    desired: bool,
    last_error: Option<String>,
}

pub struct Recorder {
    inner: Mutex<Inner>,
    preview: Mutex<PreviewEngine>,
    export_progress: Mutex<Option<f64>>,
    /// Until when the overlay has asked capture not to append, if it has.
    pause_until: Mutex<Option<Instant>>,
    /// One start at a time; the other callers return the current state.
    starting: AtomicBool,
}

/// Clears the reported progress however the export ends, errors included.
struct ProgressGuard<'a>(&'a Mutex<Option<f64>>);

impl ProgressGuard<'_> {
    fn new(progress: &Mutex<Option<f64>>) -> ProgressGuard<'_> {
        *progress.lock().unwrap() = Some(0.0);
        ProgressGuard(progress)
    }

    fn report(&self, done: usize, total: usize) {
        *self.0.lock().unwrap() = Some(done as f64 / total.max(1) as f64);
    }
}

impl Drop for ProgressGuard<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = None;
    }
}

/// What an export or a decode needs once the lock is released.
struct Frozen {
    snapshot: Snapshot,
    size: (u32, u32),
    fps: u32,
    bitrate: u32,
}

impl Recorder {
    pub fn new(config: RecorderConfig) -> Arc<Self> {
        Arc::new(Recorder {
            inner: Mutex::new(Inner { config, capture: None, desired: false, last_error: None }),
            preview: Mutex::new(PreviewEngine::default()),
            export_progress: Mutex::new(None),
            pause_until: Mutex::new(None),
            starting: AtomicBool::new(false),
        })
    }

    /// Starts capture without waiting for the portal picker: the answer can
    /// take minutes, and an IPC reply must not. `state` reports the truth.
    pub fn start(self: &Arc<Self>) -> State {
        self.inner.lock().unwrap().desired = true;
        let this = self.clone();
        std::thread::spawn(move || this.try_begin());
        self.state()
    }

    /// The overlay is on screen: keep capturing, but append nothing. The
    /// lease is refreshed by the overlay and lapses if it dies, so a crash
    /// cannot leave the recorder frozen forever.
    pub fn pause(&self) {
        *self.pause_until.lock().unwrap() = Some(Instant::now() + PAUSE_LEASE);
        if let Some(capture) = &self.inner.lock().unwrap().capture {
            capture.ring.set_paused(true);
        }
    }

    pub fn resume(&self) {
        *self.pause_until.lock().unwrap() = None;
        if let Some(capture) = &self.inner.lock().unwrap().capture {
            capture.ring.set_paused(false);
        }
    }

    fn pause_active(&self) -> bool {
        self.pause_until.lock().unwrap().is_some()
    }

    fn expire_pause(&self) {
        let expired = {
            let mut until = self.pause_until.lock().unwrap();
            match *until {
                Some(deadline) if deadline <= Instant::now() => {
                    *until = None;
                    true
                }
                _ => false,
            }
        };
        if expired {
            if let Some(capture) = &self.inner.lock().unwrap().capture {
                capture.ring.set_paused(false);
            }
        }
    }

    pub fn stop(&self) -> State {
        *self.pause_until.lock().unwrap() = None;
        let mut inner = self.inner.lock().unwrap();
        inner.desired = false;
        inner.capture = None;
        inner.last_error = None;
        Self::state_of(&inner)
    }

    pub fn apply(self: &Arc<Self>, config: RecorderConfig) -> State {
        *self.pause_until.lock().unwrap() = None;
        {
            let mut inner = self.inner.lock().unwrap();
            if config == inner.config {
                return Self::state_of(&inner);
            }
            let resume = inner.desired || inner.capture.is_some();
            inner.capture = None;
            inner.config = config;
            config_store::save(&inner.config);
            inner.desired = resume;
            if !resume {
                return Self::state_of(&inner);
            }
        }
        let this = self.clone();
        std::thread::spawn(move || this.try_begin());
        self.state()
    }

    pub fn config(&self) -> RecorderConfig {
        self.inner.lock().unwrap().config.clone()
    }

    /// Called once a second: notices a screen cast the compositor ended and
    /// closes the pipeline. It does not retry a failed start — on Wayland that
    /// would mean re-opening the portal picker every few seconds.
    pub fn supervise(&self) {
        self.expire_pause();
        let ended = {
            let inner = self.inner.lock().unwrap();
            inner.capture.as_ref().is_some_and(|capture| capture.poll())
        };
        if !ended {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        if let Some(capture) = inner.capture.take() {
            inner.last_error = capture.info.last_error();
        }
        inner.desired = false;
    }

    fn try_begin(&self) {
        loop {
            if self.starting.swap(true, Ordering::SeqCst) {
                return;
            }
            let config = {
                let inner = self.inner.lock().unwrap();
                if !inner.desired || inner.capture.is_some() {
                    self.starting.store(false, Ordering::SeqCst);
                    return;
                }
                inner.config.clone()
            };

            let built = Capture::start(&config);

            let mut inner = self.inner.lock().unwrap();
            let retry = inner.desired && inner.capture.is_none() && inner.config != config;
            match built {
                Ok(capture) => {
                    if inner.desired && inner.config == config && inner.capture.is_none() {
                        inner.last_error = None;
                        capture.ring.set_paused(self.pause_active());
                        inner.capture = Some(capture);
                        *self.preview.lock().unwrap() = PreviewEngine::default();
                    }
                    // Otherwise the user stopped or changed the config while
                    // the picker was open; dropping the capture closes the
                    // portal session.
                }
                Err(error) => {
                    if inner.desired {
                        inner.desired = false;
                        inner.last_error = Some(error);
                    }
                }
            }
            drop(inner);
            self.starting.store(false, Ordering::SeqCst);
            if !retry {
                return;
            }
        }
    }

    pub fn state(&self) -> State {
        let mut state = Self::state_of(&self.inner.lock().unwrap());
        state.export_progress = *self.export_progress.lock().unwrap();
        state
    }

    fn state_of(inner: &Inner) -> State {
        let o = Ordering::Relaxed;
        let capture = inner.capture.as_ref();
        let stats = capture.map(|capture| capture.ring.statistics());
        let (bytes, duration, span) = stats.as_ref().map(|s| (s.bytes, s.duration, s.span)).unwrap_or((0, 0.0, 0.0));
        let size = capture.map(|capture| capture.info.size()).unwrap_or((0, 0));
        State {
            recording: capture.is_some(),
            config: inner.config.clone(),
            display: capture.map(display_of),
            width: size.0 as i64,
            height: size.1 as i64,
            buffer_seconds: inner.config.buffer_seconds,
            available_seconds: duration,
            buffer_bytes: bytes as i64,
            bitrate: if span > 0.0 { bytes as f64 * 8.0 / span } else { 0.0 },
            frames_captured: capture.map(|c| c.counters.delivered.load(o) as i64).unwrap_or(0),
            frames_encoded: capture.map(|c| c.counters.encoded.load(o) as i64).unwrap_or(0),
            dropped_frames: capture.map(|c| c.counters.errors.load(o) as i64).unwrap_or(0),
            // Wayland reports no idle notices: frames simply stop arriving.
            idle_notices: 0,
            on_battery: on_battery(),
            last_error: inner.last_error.clone(),
            newest_timestamp: stats.and_then(|s| s.newest),
            export_progress: None,
        }
    }

    fn freeze(&self, from: f64, to: f64, anchor: Option<f64>) -> Result<Frozen, String> {
        let inner = self.inner.lock().unwrap();
        let capture = inner.capture.as_ref().ok_or("rewindd is not recording")?;
        // Every overlay request carries its anchor; keep what it can reach.
        if let Some(anchor) = anchor {
            capture.ring.pin(anchor);
        }
        Ok(Frozen {
            snapshot: capture.ring.snapshot(from, to, anchor)?,
            size: capture.info.size(),
            fps: capture.fps,
            bitrate: capture.bitrate,
        })
    }

    /// Without annotations or a crop the encoded frames are muxed as they are;
    /// either one needs every frame decoded, changed and re-encoded. A GIF is
    /// always decoded.
    pub fn export(
        &self,
        from: f64,
        to: f64,
        path: Option<PathBuf>,
        options: ExportOptions,
        anchor: Option<f64>,
        overlay_path: Option<&str>,
        crop: Option<&Crop>,
    ) -> Result<Clip, String> {
        let frozen = self.freeze(from, to, anchor)?;
        let path = path.unwrap_or_else(|| paths::clips_dir().join(paths::timestamped_clip_name(options.format.extension())));
        let overlay = overlay_path.map(Overlay::load).transpose()?;
        let progress = ProgressGuard::new(&self.export_progress);

        if options.format == ExportFormat::Gif {
            let overlay = overlay.map(|o| o.fitted(frozen.size.0, frozen.size.1));
            let size = write_gif(&frozen, overlay.as_ref(), crop, options, &path, &progress)?;
            return Ok(Clip {
                bytes: file_size(&path)?,
                path: path.display().to_string(),
                duration: frozen.snapshot.duration(),
                frames: frozen.snapshot.packets.len() as i64,
                width: size.0 as i64,
                height: size.1 as i64,
            });
        }

        let (snapshot, size) = if overlay.is_some() || crop.is_some() {
            let overlay = overlay.map(|o| o.fitted(frozen.size.0, frozen.size.1));
            let size = crop.map(|c| c.pixels(frozen.size.0, frozen.size.1)).map(|(_, _, w, h)| (w, h)).unwrap_or(frozen.size);
            (compose(&frozen, overlay.as_ref(), crop, size, &progress)?, size)
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
    pub fn composed_frame(
        &self,
        seconds_ago: f64,
        anchor: Option<f64>,
        overlay_path: Option<&str>,
        crop: Option<&Crop>,
    ) -> Result<RawImage, String> {
        let frame = self.preview(seconds_ago, 0, anchor)?;
        let overlay = overlay_path.map(Overlay::load).transpose()?.map(|o| o.fitted(frame.width, frame.height));
        Ok(annotation::apply(frame, overlay.as_ref(), crop))
    }

    /// The composed frame as a PNG next to the clips.
    pub fn export_frame(
        &self,
        seconds_ago: f64,
        path: Option<PathBuf>,
        anchor: Option<f64>,
        overlay_path: Option<&str>,
        crop: Option<&Crop>,
    ) -> Result<Clip, String> {
        let image = self.composed_frame(seconds_ago, anchor, overlay_path, crop)?;
        let path = path.unwrap_or_else(|| paths::clips_dir().join(paths::timestamped_clip_name("png")));
        image_file::write_png(&path, &image)?;
        Ok(Clip {
            bytes: file_size(&path)?,
            path: path.display().to_string(),
            duration: 0.0,
            frames: 1,
            width: image.width as i64,
            height: image.height as i64,
        })
    }

    /// Exact encoded size of a range: the exporter's cut, without muxing.
    pub fn estimate(&self, from: f64, to: f64, anchor: Option<f64>) -> Result<i64, String> {
        Ok(self.freeze(from, to, anchor)?.snapshot.bytes() as i64)
    }

    pub fn preview(&self, seconds_ago: f64, width: u32, anchor: Option<f64>) -> Result<RawImage, String> {
        let frozen = self.freeze(f64::INFINITY, 0.0, anchor)?;
        let now = frozen.snapshot.now(anchor);
        let index = frozen.snapshot.frame_index(now - seconds_ago);
        let mut engine = self.preview.lock().unwrap();
        engine.frame(&frozen.snapshot, index, width)
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
        self.preview.lock().unwrap().thumbnails(&frozen.snapshot, &indices, width)
    }

    /// First and last frame of a range, from keyframes like thumbnails.
    pub fn edge_frames(&self, from: f64, to: f64, width: u32, anchor: Option<f64>) -> Result<Vec<RawImage>, String> {
        let frozen = self.freeze(f64::INFINITY, 0.0, anchor)?;
        let now = frozen.snapshot.now(anchor);
        let indices: Vec<usize> = [from, to]
            .iter()
            .map(|ago| frozen.snapshot.keyframe_index(frozen.snapshot.frame_index(now - ago)))
            .collect();
        self.preview.lock().unwrap().thumbnails(&frozen.snapshot, &indices, width)
    }

    /// The one stream the portal is capturing, if it is capturing. Wayland
    /// exposes no list of outputs before the user picks one.
    pub fn displays(&self) -> Vec<DisplayInfo> {
        self.inner.lock().unwrap().capture.as_ref().map(display_of).into_iter().collect()
    }
}

fn display_of(capture: &Capture) -> DisplayInfo {
    let (width, height) = capture.info.size();
    let (x, y) = capture.info.position.unwrap_or((0, 0));
    DisplayInfo {
        index: 0,
        display_id: 0,
        width: width as i64,
        height: height as i64,
        is_main: true,
        safe_area_top: 0.0,
        screen_x: x as f64,
        screen_y: y as f64,
        screen_width: width as f64,
        screen_height: height as f64,
    }
}

/// Decodes the range in order, burns in the drawing, crops, and re-encodes. A
/// decoder of its own, so the overlay's preview state is left alone.
fn compose(
    frozen: &Frozen,
    overlay: Option<&Overlay>,
    crop: Option<&Crop>,
    size: (u32, u32),
    progress: &ProgressGuard,
) -> Result<Snapshot, String> {
    let mut decoder = PreviewEngine::default();
    let encoder = Reencoder::new(size, frozen.fps, frozen.bitrate)?;
    let total = frozen.snapshot.packets.len();
    for (index, packet) in frozen.snapshot.packets.iter().enumerate() {
        let frame = decoder.frame(&frozen.snapshot, index, 0).map_err(|error| format!("frame {index}: {error}"))?;
        let frame = annotation::apply(frame, overlay, crop);
        encoder.push(&frame, packet.pts)?;
        progress.report(index + 1, total);
    }
    encoder.finish()
}

/// Decodes the frames the GIF timeline keeps (the decoder steps through the
/// ones in between), burns in the drawing, crops, and quantises. Returns the
/// GIF's pixel size.
fn write_gif(
    frozen: &Frozen,
    overlay: Option<&Overlay>,
    crop: Option<&Crop>,
    options: ExportOptions,
    path: &Path,
    progress: &ProgressGuard,
) -> Result<(u32, u32), String> {
    let times: Vec<f64> = frozen.snapshot.packets.iter().map(|packet| ring::seconds(packet.pts)).collect();
    let mut decoder = PreviewEngine::default();
    let mut gif = GifWriter::create(path, options.gif_max_width)?;
    let timeline = image_file::gif_timeline(&times, options.gif_fps);
    let total = timeline.len();
    for (done, frame) in timeline.into_iter().enumerate() {
        let image = decoder.frame(&frozen.snapshot, frame.index, 0).map_err(|error| format!("frame {}: {error}", frame.index))?;
        gif.push(&annotation::apply(image, overlay, crop), frame.delay)?;
        progress.report(done + 1, total);
    }
    gif.finish()
}

fn file_size(path: &Path) -> Result<i64, String> {
    std::fs::metadata(path).map(|m| m.len() as i64).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

/// Wayland exposes no list of applications before the portal picker, and the
/// portal's ScreenCast interface does not enumerate windows either.
pub fn applications() -> Vec<ApplicationInfo> {
    Vec::new()
}

fn on_battery() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = std::fs::read_to_string(path.join("type")).unwrap_or_default();
        if kind.trim() == "Mains" || kind.trim() == "USB" {
            if let Ok(online) = std::fs::read_to_string(path.join("online")) {
                return online.trim() == "0";
            }
        }
    }
    false
}
