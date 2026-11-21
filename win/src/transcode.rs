//! Re-encodes a clip whose frames were changed on the CPU (annotations burnt
//! in, cropped). The macOS daemon muxes the untouched clip and runs it through
//! an AVVideoComposition; here the decoded frames are uploaded to a texture
//! and go through the same converter and hardware encoder as live capture, so
//! the result has the same codec settings, keyframe spacing and colour.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use crate::convert::Converter;
use crate::d3d::{self, Shared};
use crate::encoder::{self, Message};
use crate::protocol::RawImage;
use crate::ring::{Ring, Snapshot};

const POOL: usize = 8;

pub struct Reencoder {
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    upload: ID3D11Texture2D,
    converter: Converter,
    ring: Arc<Ring>,
    counters: Arc<encoder::Counters>,
    sender: std::sync::mpsc::Sender<Message>,
    thread: Option<std::thread::JoinHandle<()>>,
    size: (u32, u32),
}

impl Reencoder {
    pub fn new(size: (u32, u32), fps: u32) -> Result<Self, String> {
        let e = |what: &'static str| move |error: windows::core::Error| format!("{what}: {}", error.message());
        let device = d3d::create_device().map_err(e("cannot create a D3D11 device"))?;
        let context = unsafe { device.GetImmediateContext().map_err(e("no device context"))? };

        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.0,
            Height: size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut upload = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut upload)).map_err(e("cannot create the upload texture"))? };

        // Unbounded: an export holds every frame it produces until it is muxed.
        let ring = Arc::new(Ring::new(1e9, usize::MAX));
        let counters = Arc::new(encoder::Counters::default());
        let (sender, receiver) = std::sync::mpsc::channel();
        let bitrate = (size.0 as f64 * size.1 as f64 * fps as f64 * 0.18) as u32;
        let (_, thread) = encoder::start(
            &device,
            encoder::Settings { width: size.0, height: size.1, fps, bitrate },
            ring.clone(),
            counters.clone(),
            sender.clone(),
            receiver,
        )
        .map_err(e("cannot start the hardware H.264 encoder"))?;
        let converter = Converter::new(&device, size, size, fps, POOL).map_err(e("cannot create the colour converter"))?;

        Ok(Reencoder {
            _device: device,
            context,
            upload: upload.unwrap(),
            converter,
            ring,
            counters,
            sender,
            thread: Some(thread),
            size,
        })
    }

    pub fn push(&mut self, frame: &RawImage, pts: i64) -> Result<(), String> {
        if (frame.width, frame.height) != self.size {
            return Err(format!("frame is {}×{}, encoder is {}×{}", frame.width, frame.height, self.size.0, self.size.1));
        }
        // The converter's NV12 pool must not wrap onto a texture the encoder
        // has not consumed; live capture drops a frame here, an export waits.
        let started = Instant::now();
        while self.counters.in_flight.load(Ordering::Relaxed) + 2 >= POOL {
            if started.elapsed() > Duration::from_secs(5) {
                return Err("the encoder stopped taking frames".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        unsafe {
            self.context.UpdateSubresource(&self.upload, 0, None, frame.pixels.as_ptr() as *const _, frame.width * 4, 0);
        }
        let nv12 = self.converter.convert(&self.upload).map_err(|e| format!("colour conversion failed: {}", e.message()))?;
        self.counters.in_flight.fetch_add(1, Ordering::Relaxed);
        self.sender
            .send(Message::Frame { texture: Shared(nv12), pts })
            .map_err(|_| "the encoder thread has stopped".to_string())
    }

    /// Waits for every frame to come out, then returns them, starting at a
    /// keyframe (the first frame is always forced to one).
    pub fn finish(mut self) -> Result<Snapshot, String> {
        let started = Instant::now();
        while self.counters.in_flight.load(Ordering::Relaxed) > 0 && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(2));
        }
        self.stop();
        self.ring.snapshot(f64::INFINITY, 0.0, None)
    }

    fn stop(&mut self) {
        let _ = self.sender.send(Message::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Reencoder {
    fn drop(&mut self) {
        self.stop();
    }
}
