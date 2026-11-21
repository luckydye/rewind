//! One running capture: display -> NV12 -> H.264 -> ring. Built whole and torn
//! down whole; a config change or a display change builds a new one.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use windows::Win32::Graphics::Direct3D11::ID3D11Device;

use crate::capture::{self, Capture};
use crate::convert::Converter;
use crate::d3d::{self, Monitor};
use crate::encoder::{self, Message};
use crate::protocol::RecorderConfig;
use crate::ring::Ring;

/// NV12 textures between the converter and the encoder.
const POOL: usize = 8;
/// Upper bound on the ring regardless of `bufferSeconds`, like the macOS ring.
const BYTE_LIMIT: usize = 512 * 1024 * 1024;

pub struct Pipeline {
    capture: Option<Capture>,
    sender: Sender<Message>,
    encoder_thread: Option<JoinHandle<()>>,
    _device: ID3D11Device,
    pub ring: Arc<Ring>,
    pub capture_counters: Arc<capture::Counters>,
    pub encoder_counters: Arc<encoder::Counters>,
    pub monitor_name: String,
    pub encoder_name: String,
    pub source: (u32, u32),
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub border: bool,
    pub rate_limited_by_os: bool,
}

// The WinRT capture objects are agile and the D3D device is multithread
// protected; the pipeline is only ever touched behind the recorder's mutex.
unsafe impl Send for Pipeline {}

fn even(n: u32) -> u32 {
    n & !1
}

impl Pipeline {
    pub fn start(config: &RecorderConfig, monitor: &Monitor) -> Result<Pipeline, String> {
        fn e(what: &'static str) -> impl Fn(windows::core::Error) -> String {
            move |error| format!("{what}: {}", error.message())
        }
        let fps = config.fps.clamp(1, 240) as u32;

        let device = d3d::create_device().map_err(e("cannot create a D3D11 device"))?;
        let winrt_device = d3d::winrt_device(&device).map_err(e("cannot wrap the D3D11 device"))?;
        let item = capture::item_for_monitor(monitor.handle).map_err(e("cannot capture this display"))?;
        let size = item.Size().map_err(e("cannot read the display size"))?;
        let source = (size.Width as u32, size.Height as u32);

        // NV12 needs even dimensions; scaling keeps the aspect ratio.
        let target_height = config.height.max(0) as u32;
        let (width, height) = if target_height == 0 || target_height >= source.1 {
            (even(source.0), even(source.1))
        } else {
            (even((source.0 as u64 * target_height as u64 / source.1 as u64) as u32), even(target_height))
        };
        let bitrate = if config.bitrate > 0 {
            config.bitrate as u32
        } else {
            (width as f64 * height as f64 * fps as f64 * 0.18) as u32
        };

        let ring = Arc::new(Ring::new(config.buffer_seconds.max(1.0), BYTE_LIMIT));
        let capture_counters = Arc::new(capture::Counters::default());
        let encoder_counters = Arc::new(encoder::Counters::default());
        let (sender, receiver) = std::sync::mpsc::channel();

        let (info, encoder_thread) = encoder::start(
            &device,
            encoder::Settings { width, height, fps, bitrate },
            ring.clone(),
            encoder_counters.clone(),
            sender.clone(),
            receiver,
        )
        .map_err(e("cannot start the hardware H.264 encoder"))?;

        let mut pipeline = Pipeline {
            capture: None,
            sender: sender.clone(),
            encoder_thread: Some(encoder_thread),
            _device: device.clone(),
            ring,
            capture_counters: capture_counters.clone(),
            encoder_counters: encoder_counters.clone(),
            monitor_name: monitor.name.clone(),
            encoder_name: info.name,
            source,
            width,
            height,
            fps,
            bitrate,
            border: true,
            rate_limited_by_os: false,
        };

        // From here on, a failure drops `pipeline`, which stops the encoder.
        let converter = Converter::new(&device, source, (width, height), fps, POOL)
            .map_err(e("cannot create the colour converter"))?;
        let capture = Capture::start(
            &winrt_device,
            &item,
            converter,
            capture::Options { fps, cursor: config.shows_cursor, pool: POOL },
            sender,
            capture_counters,
            encoder_counters,
        )
        .map_err(e("cannot start capture"))?;

        pipeline.border = capture.border;
        pipeline.rate_limited_by_os = capture.rate_limited_by_os;
        pipeline.capture = Some(capture);
        Ok(pipeline)
    }

    pub fn needs_restart(&self) -> bool {
        self.capture_counters.restart.load(Ordering::Relaxed)
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        if let Some(capture) = self.capture.take() {
            capture.stop();
        }
        let _ = self.sender.send(Message::Stop);
        if let Some(thread) = self.encoder_thread.take() {
            let _ = thread.join();
        }
    }
}
