//! The drawing the overlay sends with a copy or an export, and the crop.
//! Mirrors `AnnotationOverlay.swift` and `Crop.swift`.
//!
//! The file is `u32 width`, `u32 height` (big-endian), then straight-alpha
//! RGBA: kept out of the JSON request because at native resolution it is tens
//! of megabytes.

use crate::protocol::{Crop, RawImage};

pub struct Overlay {
    width: u32,
    height: u32,
    /// Straight-alpha RGBA.
    pixels: Vec<u8>,
}

impl Overlay {
    pub fn load(path: &str) -> Result<Overlay, String> {
        let data = std::fs::read(path).map_err(|e| format!("cannot read annotations at {path}: {e}"))?;
        if data.len() < 8 {
            return Err("the annotation layer is malformed".into());
        }
        let width = u32::from_be_bytes(data[0..4].try_into().unwrap());
        let height = u32::from_be_bytes(data[4..8].try_into().unwrap());
        if width == 0 || height == 0 || data.len() != 8 + width as usize * height as usize * 4 {
            return Err("the annotation layer is malformed".into());
        }
        Ok(Overlay { width, height, pixels: data[8..].to_vec() })
    }

    /// Resampled to a frame size once, so a clip composites hundreds of frames
    /// without resampling each. The overlay is drawn at the frame's native
    /// size, so this is normally a no-op.
    pub fn fitted(self, width: u32, height: u32) -> Overlay {
        if self.width == width && self.height == height {
            return self;
        }
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        for y in 0..height as usize {
            let sy = y * self.height as usize / height as usize;
            for x in 0..width as usize {
                let sx = x * self.width as usize / width as usize;
                let from = (sy * self.width as usize + sx) * 4;
                let to = (y * width as usize + x) * 4;
                pixels[to..to + 4].copy_from_slice(&self.pixels[from..from + 4]);
            }
        }
        Overlay { width, height, pixels }
    }

    /// Source-over onto a BGRA frame. Transparent pixels, nearly all of them,
    /// are skipped.
    pub fn composite(&self, frame: &mut RawImage) {
        if frame.width != self.width || frame.height != self.height {
            return;
        }
        for (dst, src) in frame.pixels.chunks_exact_mut(4).zip(self.pixels.chunks_exact(4)) {
            let a = src[3] as u32;
            if a == 0 {
                continue;
            }
            let keep = 255 - a;
            let mix = |s: u8, d: u8| ((s as u32 * a + d as u32 * keep + 127) / 255) as u8;
            dst[0] = mix(src[2], dst[0]);
            dst[1] = mix(src[1], dst[1]);
            dst[2] = mix(src[0], dst[2]);
            dst[3] = 255;
        }
    }
}

/// The part of a BGRA frame a crop keeps.
pub fn crop(frame: &RawImage, crop: &Crop) -> RawImage {
    let (x, y, w, h) = crop.pixels(frame.width, frame.height);
    let mut pixels = Vec::with_capacity(w as usize * h as usize * 4);
    let stride = frame.width as usize * 4;
    for row in y as usize..(y + h) as usize {
        let start = row * stride + x as usize * 4;
        pixels.extend_from_slice(&frame.pixels[start..start + w as usize * 4]);
    }
    RawImage { width: w, height: h, pixels }
}

/// Annotations first, then the crop: the drawing is in full-frame coordinates.
pub fn apply(mut frame: RawImage, overlay: Option<&Overlay>, crop_rect: Option<&Crop>) -> RawImage {
    if let Some(overlay) = overlay {
        overlay.composite(&mut frame);
    }
    match crop_rect {
        Some(rect) => crop(&frame, rect),
        None => frame,
    }
}
