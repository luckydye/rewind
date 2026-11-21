//! PNG and GIF files through WIC, the Windows twin of ImageIO in the macOS
//! daemon. WIC also scales and quantises the GIF frames, so the daemon has no
//! image code of its own.

use std::path::Path;

use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, InitPropVariantFromBuffer, PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VT_UI2;
use windows::core::{GUID, HSTRING, PCWSTR, w};

use crate::protocol::RawImage;

fn e(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |error| format!("{what}: {}", error.message())
}

/// One GIF frame: which captured frame it shows, and for how long.
pub struct GifFrame {
    pub index: usize,
    /// Centiseconds, which is all GIF stores.
    pub delay: u16,
}

/// Capture is variable-rate: an unchanged screen produces no frames at all.
/// So rather than resampling on a fixed clock, the first frame in each 1/fps
/// slot is kept, shown from the start of its slot, and stands until the next
/// kept frame. Snapping to slots keeps delays even. Delays come from rounded
/// timestamps, not rounded durations, so rounding does not add up over a long
/// clip. Mirrors `GifExporter.timeline` in the macOS daemon.
pub fn gif_timeline(times: &[f64], fps: f64) -> Vec<GifFrame> {
    let (Some(&first), Some(&last)) = (times.first(), times.last()) else {
        return Vec::new();
    };
    let mut kept: Vec<(usize, f64)> = Vec::new();
    let mut last_slot = -1.0;
    for (index, &time) in times.iter().enumerate() {
        let slot = ((time - first) * fps + 1e-6).floor();
        if slot <= last_slot {
            continue;
        }
        kept.push((index, slot / fps));
        last_slot = slot;
    }
    let end = last - first + 1.0 / fps;
    let centiseconds = |t: f64| (t * 100.0).round();
    (0..kept.len())
        .map(|position| {
            let (index, time) = kept[position];
            let until = kept.get(position + 1).map(|k| k.1).unwrap_or(end);
            // Browsers play delays under 2 cs at 10 cs; never write one.
            let delay = (centiseconds(until) - centiseconds(time)).clamp(2.0, u16::MAX as f64) as u16;
            GifFrame { index, delay }
        })
        .collect()
}

struct Encoder {
    factory: IWICImagingFactory,
    encoder: IWICBitmapEncoder,
    // The encoder writes through it until `Commit`.
    _stream: IWICStream,
}

impl Encoder {
    fn create(path: &Path, container: &GUID) -> Result<Encoder, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        unsafe {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(e("cannot create the WIC factory"))?;
            let stream = factory.CreateStream().map_err(e("cannot create a WIC stream"))?;
            stream
                .InitializeFromFilename(&HSTRING::from(path), GENERIC_WRITE.0)
                .map_err(e("cannot open the output file"))?;
            let encoder = factory.CreateEncoder(container, std::ptr::null()).map_err(e("no WIC encoder"))?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache).map_err(e("cannot start the WIC encoder"))?;
            Ok(Encoder { factory, encoder, _stream: stream })
        }
    }

    fn frame(&self) -> Result<IWICBitmapFrameEncode, String> {
        unsafe {
            let mut frame = None;
            let mut options: Option<IPropertyBag2> = None;
            self.encoder.CreateNewFrame(&mut frame, &mut options).map_err(e("cannot add a frame"))?;
            let frame = frame.ok_or("WIC returned no frame")?;
            frame.Initialize(options.as_ref()).map_err(e("cannot initialise a frame"))?;
            Ok(frame)
        }
    }

    fn bitmap(&self, image: &RawImage) -> Result<IWICBitmap, String> {
        unsafe {
            self.factory
                .CreateBitmapFromMemory(image.width, image.height, &GUID_WICPixelFormat32bppBGR, image.width * 4, &image.pixels)
                .map_err(e("cannot wrap the frame"))
        }
    }

    fn commit(self) -> Result<(), String> {
        unsafe { self.encoder.Commit().map_err(e("cannot finish the file")) }
    }
}

/// A frame as a lossless PNG.
pub fn write_png(path: &Path, image: &RawImage) -> Result<(), String> {
    let encoder = Encoder::create(path, &GUID_ContainerFormatPng)?;
    let frame = encoder.frame()?;
    let bitmap = encoder.bitmap(image)?;
    unsafe {
        frame.SetSize(image.width, image.height).map_err(e("cannot size the PNG"))?;
        let mut format = GUID_WICPixelFormat32bppBGR;
        frame.SetPixelFormat(&mut format).map_err(e("cannot set the PNG pixel format"))?;
        frame.WriteSource(&bitmap, std::ptr::null()).map_err(e("cannot write the PNG"))?;
        frame.Commit().map_err(e("cannot finish the PNG frame"))?;
    }
    encoder.commit()
}

/// An endlessly looping GIF, written a frame at a time so a long clip is
/// never held in memory decoded.
pub struct GifWriter {
    encoder: Encoder,
    max_width: u32,
    pub size: (u32, u32),
}

impl GifWriter {
    pub fn create(path: &Path, max_width: u32) -> Result<GifWriter, String> {
        let encoder = Encoder::create(path, &GUID_ContainerFormatGif)?;
        unsafe {
            let metadata = encoder.encoder.GetMetadataQueryWriter().map_err(e("no GIF metadata writer"))?;
            // The NETSCAPE2.0 application extension: sub-block 1 is the loop
            // count, 0 meaning forever.
            set_bytes(&metadata, w!("/appext/application"), b"NETSCAPE2.0")?;
            set_bytes(&metadata, w!("/appext/data"), &[3, 1, 0, 0, 0])?;
        }
        Ok(GifWriter { encoder, max_width, size: (0, 0) })
    }

    /// Scaled to the maximum width with WIC's Fant filter, which averages
    /// rather than skips pixels, then quantised to a palette of its own.
    /// No dithering: on flat UI colours it only adds crawling noise.
    pub fn push(&mut self, image: &RawImage, delay: u16) -> Result<(), String> {
        let (width, height) = if image.width > self.max_width {
            (self.max_width, ((image.height as u64 * self.max_width as u64) / image.width as u64).max(1) as u32)
        } else {
            (image.width, image.height)
        };
        let frame = self.encoder.frame()?;
        let bitmap = self.encoder.bitmap(image)?;
        unsafe {
            let factory = &self.encoder.factory;
            let scaler = factory.CreateBitmapScaler().map_err(e("no WIC scaler"))?;
            scaler.Initialize(&bitmap, width, height, WICBitmapInterpolationModeFant).map_err(e("cannot scale the frame"))?;
            let palette = factory.CreatePalette().map_err(e("no WIC palette"))?;
            palette.InitializeFromBitmap(&scaler, 256, false).map_err(e("cannot build a palette"))?;
            let indexed = factory.CreateFormatConverter().map_err(e("no WIC converter"))?;
            indexed
                .Initialize(&scaler, &GUID_WICPixelFormat8bppIndexed, WICBitmapDitherTypeNone, &palette, 0.0, WICBitmapPaletteTypeCustom)
                .map_err(e("cannot quantise the frame"))?;

            frame.SetSize(width, height).map_err(e("cannot size the GIF frame"))?;
            let mut format = GUID_WICPixelFormat8bppIndexed;
            frame.SetPixelFormat(&mut format).map_err(e("cannot set the GIF pixel format"))?;
            frame.SetPalette(&palette).map_err(e("cannot set the GIF palette"))?;
            let metadata = frame.GetMetadataQueryWriter().map_err(e("no GIF frame metadata writer"))?;
            let mut value = PROPVARIANT::default();
            (*value.Anonymous.Anonymous).vt = VT_UI2;
            (*value.Anonymous.Anonymous).Anonymous.uiVal = delay;
            metadata.SetMetadataByName(w!("/grctlext/Delay"), &value).map_err(e("cannot set the frame delay"))?;
            frame.WriteSource(&indexed, std::ptr::null()).map_err(e("cannot write the GIF frame"))?;
            frame.Commit().map_err(e("cannot finish the GIF frame"))?;
        }
        self.size = (width, height);
        Ok(())
    }

    pub fn finish(self) -> Result<(), String> {
        self.encoder.commit()
    }
}

unsafe fn set_bytes(metadata: &IWICMetadataQueryWriter, name: PCWSTR, bytes: &[u8]) -> Result<(), String> {
    unsafe {
        let mut value = InitPropVariantFromBuffer(bytes.as_ptr() as *const _, bytes.len() as u32).map_err(e("cannot build GIF metadata"))?;
        let result = metadata.SetMetadataByName(name, &value).map_err(e("cannot write GIF metadata"));
        let _ = PropVariantClear(&mut value);
        result
    }
}
