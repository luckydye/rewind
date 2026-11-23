//! One running capture: portal node -> PipeWire -> NV12 -> H.264 -> ring.
//!
//! Built whole and torn down whole. The portal session and its PipeWire remote
//! are owned here because closing the session stops the stream, and the fd has
//! to outlive `pipewiresrc`.

use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSinkCallbacks};

use crate::codec::{self, Encoder, Settings};
use crate::config_store;
use crate::h264;
use crate::portal::{self, PortalSession};
use crate::protocol::RecorderConfig;
use crate::ring::{Packet, ParameterSets, Ring};

/// Upper bound on the ring regardless of `bufferSeconds`, like macOS.
const BYTE_LIMIT: usize = 512 * 1024 * 1024;

#[derive(Default)]
pub struct Counters {
    /// Buffers `pipewiresrc` delivered to the encoder.
    pub delivered: AtomicUsize,
    /// Access units appended to the ring.
    pub encoded: AtomicUsize,
    /// Malformed buffers and pipeline errors.
    pub errors: AtomicUsize,
}

/// What the recorder reports and the bus watcher fills in. Separate from the
/// pipeline so `State` can be built without touching the capture.
pub struct Info {
    /// The stream size the portal reported, before any scaling.
    pub source: (u32, u32),
    /// The encoded size; corrected from the encoder's negotiated caps when the
    /// first frame arrives, since the portal size is in compositor coordinates.
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub encoder: &'static str,
    pub position: Option<(i32, i32)>,
    pub error: Mutex<Option<String>>,
    pub ended: AtomicBool,
}

impl Info {
    pub fn size(&self) -> (u32, u32) {
        (self.width.load(Ordering::Relaxed), self.height.load(Ordering::Relaxed))
    }

    pub fn name(&self) -> String {
        let (width, height) = self.size();
        format!("Screen — {width}×{height}")
    }

    pub fn last_error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    fn fail(&self, message: String) {
        eprintln!("rewindd: {message}");
        let mut error = self.error.lock().unwrap();
        if error.is_none() {
            *error = Some(message);
        }
        self.ended.store(true, Ordering::Relaxed);
    }
}

pub struct Capture {
    pipeline: gst::Pipeline,
    bus: gst::Bus,
    pub ring: Arc<Ring>,
    pub info: Arc<Info>,
    pub counters: Arc<Counters>,
    pub fps: u32,
    pub bitrate: u32,
    session: Option<PortalSession>,
    runtime: tokio::runtime::Runtime,
}

fn even(n: u32) -> u32 {
    n & !1
}

fn caps_size(pad: &gst::Pad) -> Option<(u32, u32)> {
    let caps = pad.current_caps()?;
    let structure = caps.structure(0)?;
    let width = structure.get::<i32>("width").ok()?;
    let height = structure.get::<i32>("height").ok()?;
    Some((width.max(1) as u32, height.max(1) as u32))
}

/// `vah264enc`/`vaapih264enc` can take DMA-BUFs; everything else wants system
/// memory from `pipewiresrc`, which is where the portal's buffer is copied.
fn description(encoder: Encoder, settings: &Settings, session: &PortalSession) -> String {
    let format = if settings.height == 0 {
        "video/x-raw,format=NV12".to_string()
    } else {
        format!("video/x-raw,format=NV12,width={},height={}", settings.width, settings.height)
    };
    format!(
        "pipewiresrc path={node} fd={fd} do-timestamp=true always-copy={copy} ! \
         queue max-size-buffers=3 leaky=downstream ! \
         videorate drop-only=true ! video/x-raw,framerate={fps}/1 ! \
         {converter} ! {format} ! {encoder} ! \
         h264parse config-interval=-1 ! video/x-h264,stream-format=avc,alignment=au ! \
         appsink name=sink sync=false max-buffers=8 drop=true emit-signals=false",
        node = session.node_id,
        fd = session.remote.as_raw_fd(),
        copy = !encoder.uses_vaapi(),
        fps = settings.fps,
        converter = codec::converter_description(encoder),
        encoder = codec::encoder_description(encoder, settings),
    )
}

impl Capture {
    pub fn start(config: &RecorderConfig) -> Result<Capture, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("cannot start the portal runtime: {error}"))?;

        let session = runtime.block_on(async {
            // A portal backend that stops answering must not wedge every later
            // start behind a flag that never clears. Five minutes is longer
            // than any picker should sit unanswered.
            tokio::time::timeout(Duration::from_secs(300), portal::open(config.shows_cursor, config_store::load_restore_token()))
                .await
                .map_err(|_| "the screen-sharing portal did not answer within five minutes".to_string())?
        })?;
        if let Some(token) = &session.restore_token {
            config_store::save_restore_token(token);
        }

        let fps = config.fps.clamp(1, 240) as u32;
        let (source_width, source_height) = session.size;
        let target_height = config.height.max(0) as u32;
        let (width, height) = if target_height == 0 || target_height >= source_height {
            (even(source_width), even(source_height))
        } else {
            (even((source_width as u64 * target_height as u64 / source_height as u64) as u32), even(target_height))
        };
        let bitrate = if config.bitrate > 0 {
            config.bitrate as u32
        } else {
            (width as f64 * height as f64 * fps as f64 * 0.18) as u32
        };

        let encoder = codec::choose_encoder()?;
        let settings = Settings { bitrate, fps, width, height };
        let pipeline_element = gst::parse::launch(&description(encoder, &settings, &session))
            .map_err(|error| format!("cannot build the capture pipeline: {error}"))?;
        let pipeline: gst::Pipeline = pipeline_element
            .downcast()
            .map_err(|_| "the capture description did not produce a pipeline")?;

        let sink: AppSink = pipeline
            .by_name("sink")
            .ok_or("the capture pipeline has no appsink")?
            .downcast()
            .map_err(|_| "the capture appsink is not an appsink")?;
        let encoder_element = pipeline.by_name("enc").ok_or("the capture pipeline has no encoder")?;
        let bus = pipeline.bus().ok_or("the capture pipeline has no bus")?;

        let ring = Arc::new(Ring::new(config.buffer_seconds.max(1.0), BYTE_LIMIT));
        let counters = Arc::new(Counters::default());
        let info = Arc::new(Info {
            source: (source_width, source_height),
            width: AtomicU32::new(width),
            height: AtomicU32::new(height),
            encoder: encoder.element(),
            position: session.position,
            error: Mutex::new(None),
            ended: AtomicBool::new(false),
        });

        {
            let ring = ring.clone();
            let counters = counters.clone();
            let info = info.clone();
            sink.set_callbacks(
                AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                        let pts = buffer.pts().ok_or(gst::FlowError::Error)?.nseconds() as i64;
                        let data = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;

                        counters.delivered.fetch_add(1, Ordering::Relaxed);
                        let Some(unit) = h264::from_avcc(data.as_slice()) else {
                            if counters.errors.fetch_add(1, Ordering::Relaxed) == 0 {
                                eprintln!("capture: h264parse produced a buffer that is not AVCC");
                            }
                            return Ok(gst::FlowSuccess::Ok);
                        };
                        drop(data);

                        if let (Some(sps), Some(pps)) = (unit.sps, unit.pps) {
                            if !ring.has_parameters() {
                                ring.set_parameters(ParameterSets { sps, pps });
                            }
                        }

                        // The encoder's negotiated input caps carry the size the
                        // stream really has, which a HiDPI compositor may make
                        // larger than the portal's logical size.
                        if let Some((width, height)) = encoder_element.static_pad("sink").and_then(|pad| caps_size(&pad)) {
                            info.width.store(width, Ordering::Relaxed);
                            info.height.store(height, Ordering::Relaxed);
                        }

                        if ring.append(Packet { pts, keyframe: unit.keyframe, data: unit.avcc }) {
                            counters.encoded.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|error| format!("cannot start the capture pipeline: {error:?}"))?;
        let (changed, _, _) = pipeline.state(gst::ClockTime::from_seconds(15));
        changed.map_err(|error| format!("the capture pipeline did not start: {error:?}"))?;

        Ok(Capture {
            pipeline,
            bus,
            ring,
            info,
            counters,
            fps,
            bitrate,
            session: Some(session),
            runtime,
        })
    }

    /// Drains pipeline messages. Called once a second by the recorder; returns
    /// true once the stream is over.
    pub fn poll(&self) -> bool {
        if self.info.ended.load(Ordering::Relaxed) {
            return true;
        }
        while let Some(message) = self.bus.pop() {
            match message.view() {
                gst::MessageView::Error(error) => {
                    let debug = error.debug().map(|d| format!(" ({d})")).unwrap_or_default();
                    self.info.fail(format!("capture: {}{debug}", error.error()));
                    return true;
                }
                gst::MessageView::Eos(_) => {
                    self.info.fail("the screen cast ended".into());
                    return true;
                }
                _ => {}
            }
        }
        false
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        if let Some(session) = self.session.take() {
            // A wedged portal must not make `stop` or a config change hang.
            let _ = self.runtime.block_on(async {
                let _ = tokio::time::timeout(Duration::from_secs(5), session.close()).await;
            });
        }
    }
}
