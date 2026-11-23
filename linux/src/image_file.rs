//! PNG and GIF files through the `image` crate, which is already the UI's
//! image dependency. The GIF writer keeps the capture's variable rate: the
//! first frame in each 1/fps slot is kept and held until the next one.

use std::fs::File;
use std::path::{Path, PathBuf};

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, RgbaImage};

use crate::preview::scale_bgra;
use crate::protocol::RawImage;

/// One GIF frame: which captured frame it shows, and for how long.
pub struct GifFrame {
    pub index: usize,
    /// Centiseconds, which is all GIF stores.
    pub delay: u16,
}

/// Capture is variable-rate: an unchanged screen produces no frames at all.
/// So rather than resampling on a fixed clock, the first frame in each 1/fps
/// slot is kept, shown from the start of its slot, and stands until the next
/// kept frame. `GifExporter.timeline` in the macOS daemon does the same.
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

fn bgra_to_rgba(pixels: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(pixels.len());
    for pixel in pixels.chunks_exact(4) {
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    rgba
}

pub fn png_bytes(image: &RawImage) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let rgba = bgra_to_rgba(&image.pixels);
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(&rgba, image.width, image.height, image::ExtendedColorType::Rgba8)
        .map_err(|error| format!("cannot encode PNG: {error}"))?;
    Ok(bytes)
}

pub fn write_png(path: &Path, image: &RawImage) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    std::fs::write(path, png_bytes(image)?).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

pub struct GifWriter {
    encoder: GifEncoder<std::io::BufWriter<File>>,
    max_width: u32,
    path: PathBuf,
    pub size: (u32, u32),
}

impl GifWriter {
    pub fn create(path: &Path, max_width: u32) -> Result<GifWriter, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        let file = File::create(path).map_err(|error| format!("cannot create {}: {error}", path.display()))?;
        let mut encoder = GifEncoder::new(std::io::BufWriter::new(file));
        encoder.set_repeat(Repeat::Infinite).map_err(|error| format!("cannot start the GIF: {error}"))?;
        Ok(GifWriter { encoder, max_width: max_width.max(1), path: path.to_path_buf(), size: (0, 0) })
    }

    pub fn push(&mut self, image: &RawImage, delay_cs: u16) -> Result<(), String> {
        let scaled = if image.width > self.max_width {
            scale_bgra(image.width, image.height, &image.pixels, self.max_width)
        } else {
            RawImage { width: image.width, height: image.height, pixels: image.pixels.clone() }
        };
        let buffer = RgbaImage::from_raw(scaled.width, scaled.height, bgra_to_rgba(&scaled.pixels))
            .ok_or("the GIF frame has the wrong size")?;
        let frame = Frame::from_parts(buffer, 0, 0, Delay::from_numer_denom_ms(delay_cs as u32 * 10, 1));
        self.encoder.encode_frame(frame).map_err(|error| format!("cannot write the GIF: {error}"))?;
        self.size = (scaled.width, scaled.height);
        Ok(())
    }

    /// Dropping the encoder writes the GIF trailer, so the file can only be
    /// checked after that. A zero-length file means the trailer never landed.
    pub fn finish(self) -> Result<(u32, u32), String> {
        let (size, path) = (self.size, self.path);
        drop(self.encoder);
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if bytes == 0 {
            return Err(format!("the GIF at {} is empty", path.display()));
        }
        Ok(size)
    }
}
