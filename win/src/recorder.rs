//! Owns the pipeline and serialises every lifecycle transition, like the
//! `Recorder` actor on macOS: a tray click and an IPC request can never start
//! and stop capture concurrently.
//!
//! Two locks: `inner` for lifecycle and state (held briefly), `preview` for
//! the decoder (held for a decode). A status request never waits on a decode.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

use crate::annotation::{self, Overlay};
use crate::config_store;
use crate::transcode::Reencoder;
use crate::d3d;
use crate::image_file::{self, GifWriter};
use crate::mp4;
use crate::paths;
use crate::pipeline::Pipeline;
use crate::preview::{Decoded, PreviewEngine};
use crate::protocol::{ApplicationInfo, Clip, Crop, DisplayInfo, ExportFormat, ExportOptions, RawImage, RecorderConfig, State};
use crate::ring::{self, Snapshot, TICKS_PER_SECOND};

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
    preview: Arc<Mutex<PreviewEngine>>,
    /// Playback cache: the frame last served, then the frames decoded after
    /// it. Its own lock, so a request for a frame that is already decoded
    /// never waits on the decoder.
    ahead: Arc<Mutex<VecDeque<Arc<Decoded>>>>,
    prefetch: Sender<Prefetch>,
    /// Requests waiting for the decoder; the prefetch worker yields to them.
    waiting: Arc<AtomicUsize>,
    /// How far the export in progress is, 0 to 1; reported with the state.
    /// Its own lock, so the render never holds `inner`.
    export_progress: Mutex<Option<f64>>,
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

/// Frames decoded ahead of playback. A keyframe takes the software decoder
/// roughly ten times as long as a P-frame (~50 ms at 1440p); decoding it a
/// few hundred milliseconds early keeps it out of the overlay's way.
const AHEAD: usize = 8;

struct Prefetch {
    snapshot: Arc<Snapshot>,
}

/// What an export or a decode needs once the lock is released.
struct Frozen {
    snapshot: Snapshot,
    size: (u32, u32),
    fps: u32,
}

impl Recorder {
    pub fn new(config: RecorderConfig) -> Arc<Self> {
        let preview = Arc::new(Mutex::new(PreviewEngine::default()));
        let ahead = Arc::new(Mutex::new(VecDeque::new()));
        let (prefetch, jobs) = channel();
        let waiting = Arc::new(AtomicUsize::new(0));
        {
            let (preview, ahead, waiting) = (preview.clone(), ahead.clone(), waiting.clone());
            std::thread::Builder::new()
                .name("prefetch".into())
                .spawn(move || prefetch_worker(preview, ahead, waiting, jobs))
                .expect("spawn the prefetch thread");
        }
        Arc::new(Recorder {
            inner: Mutex::new(Inner {
                config,
                pipeline: None,
                display: None,
                desired: false,
                last_error: None,
                last_attempt: None,
            }),
            preview,
            ahead,
            prefetch,
            waiting,
            export_progress: Mutex::new(None),
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
        let mut state = Self::state_of(&self.inner.lock().unwrap());
        state.export_progress = *self.export_progress.lock().unwrap();
        state
    }

    fn state_of(inner: &Inner) -> State {
        let o = Ordering::Relaxed;
        let pipeline = inner.pipeline.as_ref();
        let stats = pipeline.map(|p| p.ring.statistics());
        let (bytes, duration, span) = stats.as_ref().map(|s| (s.bytes, s.duration, s.span)).unwrap_or((0, 0.0, 0.0));
        State {
            recording: pipeline.is_some(),
            config: inner.config.clone(),
            display: inner.display.clone(),
            width: pipeline.map(|p| p.width as i64).unwrap_or(0),
            height: pipeline.map(|p| p.height as i64).unwrap_or(0),
            buffer_seconds: inner.config.buffer_seconds,
            available_seconds: duration,
            buffer_bytes: bytes as i64,
            bitrate: if span > 0.0 { bytes as f64 * 8.0 / span } else { 0.0 },
            frames_captured: pipeline.map(|p| p.capture_counters.captured.load(o) as i64).unwrap_or(0),
            frames_encoded: pipeline.map(|p| p.encoder_counters.encoded.load(o) as i64).unwrap_or(0),
            dropped_frames: pipeline.map(|p| p.capture_counters.dropped.load(o) as i64).unwrap_or(0),
            idle_notices: pipeline.map(|p| p.capture_counters.skipped.load(o) as i64).unwrap_or(0),
            on_battery: on_battery(),
            last_error: inner.last_error.clone(),
            newest_timestamp: stats.and_then(|s| s.newest),
            export_progress: None,
        }
    }

    fn freeze(&self, from: f64, to: f64, anchor: Option<f64>) -> Result<Frozen, String> {
        let inner = self.inner.lock().unwrap();
        let pipeline = inner.pipeline.as_ref().ok_or("rewindd is not recording")?;
        // Every overlay request carries its anchor; keep what it can reach.
        if let Some(anchor) = anchor {
            pipeline.ring.pin(anchor);
        }
        Ok(Frozen {
            snapshot: pipeline.ring.snapshot(from, to, anchor)?,
            size: (pipeline.width, pipeline.height),
            fps: pipeline.fps,
        })
    }

    /// Without annotations or a crop the encoded frames are muxed as they
    /// are; either one needs every frame decoded, changed and re-encoded.
    /// A GIF is always decoded.
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
    pub fn composed_frame(&self, seconds_ago: f64, anchor: Option<f64>, overlay_path: Option<&str>, crop: Option<&Crop>) -> Result<RawImage, String> {
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
        let target = frozen.snapshot.packets.get(index).cloned().ok_or("the rolling buffer is empty")?;

        // Already decoded ahead: drop what playback has passed, keep the rest.
        let cached = {
            let mut ahead = self.ahead.lock().unwrap();
            if tracing() {
                let pts: Vec<i64> = ahead.iter().map(|d| d.packet().pts / 10_000).collect();
                eprintln!("preview {index} (pts {} ms): cache {pts:?}", target.pts / 10_000);
            }
            ahead.iter().position(|d| Arc::ptr_eq(d.packet(), &target)).map(|at| {
                ahead.drain(..at);
                ahead[0].clone()
            })
        };

        let trace = tracing();
        let started = Instant::now();
        let (decoded, forward) = match cached {
            Some(decoded) => {
                if trace {
                    eprintln!("preview {index}: hit");
                }
                (decoded, true)
            }
            None => {
                // The worker steps aside while a request waits; std's mutex
                // is not fair, and it would otherwise keep re-taking the lock.
                self.waiting.fetch_add(1, Ordering::SeqCst);
                let mut engine = self.preview.lock().unwrap();
                self.waiting.fetch_sub(1, Ordering::SeqCst);
                let waited = started.elapsed();

                // The worker may have decoded this very frame while we waited
                // for the lock. Decoding it again would seek the decoder back
                // and replay the whole GOP.
                let decoded_meanwhile = {
                    let mut ahead = self.ahead.lock().unwrap();
                    ahead.iter().position(|d| Arc::ptr_eq(d.packet(), &target)).map(|at| {
                        ahead.drain(..at);
                        ahead[0].clone()
                    })
                };
                if let Some(decoded) = decoded_meanwhile {
                    drop(engine);
                    if trace {
                        eprintln!("preview {index}: hit after waiting {:.0} ms", waited.as_secs_f64() * 1e3);
                    }
                    let image = decoded.render(frozen.size, width);
                    let _ = self.prefetch.send(Prefetch { snapshot: Arc::new(frozen.snapshot) });
                    return Ok(image);
                }

                let before = engine
                    .last()
                    .and_then(|last| frozen.snapshot.packets.iter().rposition(|p| Arc::ptr_eq(p, last.packet())));
                let decoded = engine.decode(&frozen.snapshot, index, frozen.size)?;
                // Under the decoder lock, so the prefetch worker cannot slip a
                // frame from the old position in between.
                let mut ahead = self.ahead.lock().unwrap();
                ahead.clear();
                ahead.push_back(decoded.clone());
                // Playing, or stepping forward: a few frames on from the last.
                let forward = before.is_some_and(|b| b <= index && index - b <= 4);
                if trace {
                    eprintln!(
                        "preview {index}: miss, decoder at {before:?}, lock {:.0} ms, total {:.0} ms",
                        waited.as_secs_f64() * 1e3,
                        started.elapsed().as_secs_f64() * 1e3
                    );
                }
                (decoded, forward)
            }
        };

        let image = decoded.render(frozen.size, width);
        if forward {
            let _ = self.prefetch.send(Prefetch { snapshot: Arc::new(frozen.snapshot) });
        }
        Ok(image)
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

/// `REWIND_TRACE=1` logs every preview request: cache hit or miss, where the
/// decoder was, and how long the request waited for it.
fn tracing() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("REWIND_TRACE").is_some())
}

/// Keeps the playback cache `AHEAD` frames past the one last served, one
/// decode per lock so a request that misses the cache waits for at most one
/// frame. Stops as soon as a newer request arrives, and empties the cache when
/// the overlay has been quiet for a while, so a closed overlay holds no frames.
fn prefetch_worker(
    preview: Arc<Mutex<PreviewEngine>>,
    ahead: Arc<Mutex<VecDeque<Arc<Decoded>>>>,
    waiting: Arc<AtomicUsize>,
    jobs: Receiver<Prefetch>,
) {
    loop {
        let mut job = match jobs.recv_timeout(Duration::from_secs(10)) {
            Ok(job) => job,
            Err(RecvTimeoutError::Timeout) => {
                ahead.lock().unwrap().clear();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        loop {
            while let Ok(newer) = jobs.try_recv() {
                job = newer;
            }
            if ahead.lock().unwrap().len() > AHEAD {
                break;
            }
            if waiting.load(Ordering::SeqCst) > 0 {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            let mut engine = preview.lock().unwrap();
            // The cache must end where the decoder is; if a request seeked in
            // the meantime, this job is stale.
            let in_step = {
                let ahead = ahead.lock().unwrap();
                matches!((ahead.back(), engine.last()), (Some(a), Some(b)) if Arc::ptr_eq(a, b))
            };
            if !in_step {
                break;
            }
            match engine.decode_next(&job.snapshot) {
                Some(decoded) => ahead.lock().unwrap().push_back(decoded),
                None => break,
            }
        }
    }
}

/// Decodes the range in order (one decode per frame), burns in the drawing,
/// crops, and re-encodes. A decoder of its own, so the overlay's preview
/// state is left alone.
fn compose(frozen: &Frozen, overlay: Option<&Overlay>, crop: Option<&Crop>, size: (u32, u32), progress: &ProgressGuard) -> Result<Snapshot, String> {
    let mut decoder = PreviewEngine::default();
    let mut encoder = Reencoder::new(size, frozen.fps)?;
    let total = frozen.snapshot.packets.len();
    for (index, packet) in frozen.snapshot.packets.iter().enumerate() {
        let frame = decoder.frame(&frozen.snapshot, index, 0, frozen.size)?;
        let frame = annotation::apply(frame, overlay, crop);
        encoder.push(&frame, packet.pts)?;
        progress.report(index + 1, total);
    }
    let output = encoder.finish()?;
    // One frame out per frame in is the low-latency encoder's contract.
    if output.packets.len() != frozen.snapshot.packets.len() {
        eprintln!("rewindd: re-encode produced {} of {} frames", output.packets.len(), frozen.snapshot.packets.len());
    }
    Ok(output)
}

/// Decodes the frames the GIF timeline keeps (the decoder steps through the
/// ones in between), burns in the drawing, crops, and hands each to WIC.
/// Returns the GIF's pixel size.
fn write_gif(
    frozen: &Frozen,
    overlay: Option<&Overlay>,
    crop: Option<&Crop>,
    options: ExportOptions,
    path: &Path,
    progress: &ProgressGuard,
) -> Result<(u32, u32), String> {
    let times: Vec<f64> = frozen.snapshot.packets.iter().map(|p| ring::seconds(p.pts)).collect();
    let mut decoder = PreviewEngine::default();
    let mut gif = GifWriter::create(path, options.gif_max_width)?;
    let timeline = image_file::gif_timeline(&times, options.gif_fps);
    let total = timeline.len();
    for (done, frame) in timeline.into_iter().enumerate() {
        let image = decoder.frame(&frozen.snapshot, frame.index, 0, frozen.size)?;
        gif.push(&annotation::apply(image, overlay, crop), frame.delay)?;
        progress.report(done + 1, total);
    }
    let size = gif.size;
    gif.finish()?;
    Ok(size)
}

fn file_size(path: &Path) -> Result<i64, String> {
    std::fs::metadata(path)
        .map(|m| m.len() as i64)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
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
