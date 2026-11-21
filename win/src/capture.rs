//! Windows.Graphics.Capture monitor capture. Like `SCStream`, it delivers GPU
//! textures and produces no frame at all while the screen is unchanged.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureAccess, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{IInspectable, Interface, Result};

use crate::convert::Converter;
use crate::d3d::Shared;
use crate::encoder::{Counters as EncoderCounters, Message};
use crate::ring::TICKS_PER_SECOND;

#[derive(Default)]
pub struct Counters {
    /// Frames the compositor delivered.
    pub delivered: AtomicUsize,
    /// Frames converted and handed to the encoder.
    pub captured: AtomicUsize,
    /// Delivered faster than `--fps`; not encoded.
    pub skipped: AtomicUsize,
    /// The encoder fell behind and the NV12 pool was full.
    pub dropped: AtomicUsize,
    pub errors: AtomicUsize,
    /// The display changed size or went away; the frame pool, converter and
    /// encoder are all sized for the old one, so the pipeline is rebuilt.
    pub restart: AtomicBool,
}

pub fn item_for_monitor(monitor: HMONITOR) -> Result<GraphicsCaptureItem> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    unsafe { interop.CreateForMonitor(monitor) }
}

pub struct Capture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    pub border: bool,
    pub rate_limited_by_os: bool,
}

pub struct Options {
    pub fps: u32,
    pub cursor: bool,
    /// NV12 textures in the converter's pool.
    pub pool: usize,
}

impl Capture {
    pub fn start(
        device: &IDirect3DDevice,
        item: &GraphicsCaptureItem,
        converter: Converter,
        options: Options,
        sender: Sender<Message>,
        counters: Arc<Counters>,
        encoder: Arc<EncoderCounters>,
    ) -> Result<Self> {
        let size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let session = pool.CreateCaptureSession(item)?;
        session.SetIsCursorCaptureEnabled(options.cursor)?;

        // Windows 11 lets an app drop the yellow capture border once it has
        // asked; older builds keep it.
        let borderless = GraphicsCaptureAccess::RequestAccessAsync(
            windows::Graphics::Capture::GraphicsCaptureAccessKind::Borderless,
        )
        .and_then(|op| op.join())
        .map(|status| status == AppCapabilityAccessStatus::Allowed)
        .unwrap_or(false);
        let border = !(borderless && session.SetIsBorderRequired(false).is_ok());

        let interval = TICKS_PER_SECOND / options.fps as i64;
        // Windows 11 24H2+: the compositor itself stops calling back faster
        // than this, which is cheaper than receiving and discarding.
        let rate_limited_by_os = session.SetMinUpdateInterval(TimeSpan { Duration: interval }).is_ok();

        let converter = Mutex::new(Shared(converter));
        let origin = AtomicI64::new(i64::MIN);
        let last = AtomicI64::new(i64::MIN);
        let pool_size = options.pool;

        let closed_counters = counters.clone();
        item.Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
            closed_counters.restart.store(true, Ordering::Relaxed);
            Ok(())
        }))?;

        let handler = TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |pool, _| {
            let Ok(pool) = pool.ok() else { return Ok(()) };
            let frame = pool.TryGetNextFrame()?;
            counters.delivered.fetch_add(1, Ordering::Relaxed);

            let content = frame.ContentSize()?;
            if content.Width != size.Width || content.Height != size.Height {
                counters.restart.store(true, Ordering::Relaxed);
                return Ok(());
            }

            let time = frame.SystemRelativeTime()?.Duration;
            if origin.load(Ordering::Relaxed) == i64::MIN {
                origin.store(time, Ordering::Relaxed);
            }
            let pts = time - origin.load(Ordering::Relaxed);

            // Accept with 10% slack so a 60 Hz display yields 30 fps, not 20.
            let previous = last.load(Ordering::Relaxed);
            if previous != i64::MIN && pts - previous < interval - interval / 10 {
                counters.skipped.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
            if encoder.in_flight.load(Ordering::Relaxed) + 2 >= pool_size {
                counters.dropped.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
            last.store(pts, Ordering::Relaxed);

            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
            let converted = converter.lock().unwrap().0.convert(&texture);
            drop(texture);
            frame.Close()?;

            match converted {
                Ok(nv12) => {
                    encoder.in_flight.fetch_add(1, Ordering::Relaxed);
                    counters.captured.fetch_add(1, Ordering::Relaxed);
                    let _ = sender.send(Message::Frame { texture: Shared(nv12), pts });
                }
                Err(error) => {
                    if counters.errors.fetch_add(1, Ordering::Relaxed) == 0 {
                        eprintln!("capture: conversion failed: {error}");
                    }
                }
            }
            Ok(())
        });
        pool.FrameArrived(&handler)?;
        session.StartCapture()?;

        Ok(Capture { pool, session, border, rate_limited_by_os })
    }

    pub fn stop(self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
