//! Rasterises Ink tips into a transparent RGBA layer.
//!
//! Strokes are kept as tips in normalised coordinates. A document can therefore
//! be rendered at preview size while drawing and again at native resolution for
//! the exported image or clip, without keeping a second copy of the frame.

use super::color::encode_transfer;
use super::gesture::{Gesture, GestureOptions, MarkSink};
use super::stamps::BrushTip;
use super::types::{Brush, BrushAction, BrushKind, Point, Rgb};

/// A finished stroke, stored as the tips the gesture produced.
#[derive(Debug, Clone)]
pub struct Stroke {
    pub color: Rgb,
    pub kind: BrushKind,
    pub tips: Vec<BrushTip>,
}

struct Recorder<'a> {
    tips: &'a mut Vec<BrushTip>,
}

impl MarkSink for Recorder<'_> {
    fn draw(&mut self, _action: &BrushAction, tip: BrushTip) {
        self.tips.push(tip);
    }
}

/// A stroke under the pointer. Readings accumulate here until the stroke ends.
pub struct StrokeSession {
    gesture: Gesture,
    color: Rgb,
    kind: BrushKind,
    tips: Vec<BrushTip>,
}

impl StrokeSession {
    pub fn new(
        kind: BrushKind,
        brush: Brush,
        color: Rgb,
        point: Point,
        gesture_scale: f64,
    ) -> Self {
        let mut tips = Vec::new();
        let gesture = {
            let mut recorder = Recorder { tips: &mut tips };
            Gesture::new(
                GestureOptions {
                    soft_start: true,
                    kind,
                    point,
                    brush,
                    gesture_scale,
                },
                &mut recorder,
            )
        };
        Self {
            gesture,
            color,
            kind,
            tips,
        }
    }

    pub fn push(&mut self, points: &[Point]) {
        let mut recorder = Recorder {
            tips: &mut self.tips,
        };
        self.gesture.append(points, &mut recorder);
    }

    pub fn finish(mut self) -> Stroke {
        let mut recorder = Recorder {
            tips: &mut self.tips,
        };
        self.gesture.commit(&mut recorder);
        Stroke {
            color: self.color,
            kind: self.kind,
            tips: self.tips,
        }
    }

    pub fn tips(&self) -> &[BrushTip] {
        &self.tips
    }

    pub fn color(&self) -> Rgb {
        self.color
    }

    pub fn kind(&self) -> BrushKind {
        self.kind
    }
}

/// A stack of annotations over a capture, in normalised coordinates.
#[derive(Debug, Default, Clone)]
pub struct Paint {
    strokes: Vec<Stroke>,
    undone: Vec<Stroke>,
}

impl Paint {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
    }

    pub fn can_undo(&self) -> bool {
        !self.strokes.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    pub fn push(&mut self, stroke: Stroke) {
        self.strokes.push(stroke);
        self.undone.clear();
    }

    pub fn undo(&mut self) -> bool {
        let Some(stroke) = self.strokes.pop() else {
            return false;
        };
        self.undone.push(stroke);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(stroke) = self.undone.pop() else {
            return false;
        };
        self.strokes.push(stroke);
        true
    }

    pub fn clear(&mut self) {
        self.strokes.clear();
        self.undone.clear();
    }

    /// Renders the whole stack, optionally including a stroke that is still
    /// under the pointer. Returns straight-alpha RGBA.
    pub fn render_rgba(&self, width: u32, height: u32, live: Option<&StrokeSession>) -> Vec<u8> {
        let mut layer = vec![0f32; width as usize * height as usize * 4];
        let mut mask = vec![0f32; width as usize * height as usize];
        for stroke in &self.strokes {
            composite(&mut layer, &mut mask, width, height, stroke);
        }
        if let Some(live) = live {
            let stroke = Stroke {
                color: live.color(),
                kind: live.kind(),
                tips: live.tips().to_vec(),
            };
            composite(&mut layer, &mut mask, width, height, &stroke);
        }
        to_rgba8(&layer)
    }
}

fn composite(layer: &mut [f32], mask: &mut [f32], width: u32, height: u32, stroke: &Stroke) {
    mask.fill(0.0);
    for tip in &stroke.tips {
        rasterize_tip(mask, width, height, tip);
    }

    let erase = stroke.kind == BrushKind::Eraser;
    let color = stroke.color;
    let count = width as usize * height as usize;
    for pixel in 0..count {
        let coverage = mask[pixel];
        if coverage <= 0.0 {
            continue;
        }
        let index = pixel * 4;
        let keep = 1.0 - coverage;
        if erase {
            layer[index] *= keep;
            layer[index + 1] *= keep;
            layer[index + 2] *= keep;
            layer[index + 3] *= keep;
        } else {
            layer[index] = color[0] as f32 * coverage as f32 + layer[index] * keep as f32;
            layer[index + 1] = color[1] as f32 * coverage as f32 + layer[index + 1] * keep as f32;
            layer[index + 2] = color[2] as f32 * coverage as f32 + layer[index + 2] * keep as f32;
            layer[index + 3] = coverage as f32 + layer[index + 3] * keep as f32;
        }
    }
}

/// One swept capsule with a one-pixel antialiased edge, matching the canvas
/// solid-tip coverage.
fn rasterize_tip(mask: &mut [f32], width: u32, height: u32, tip: &BrushTip) {
    let sx = tip.start.x * width as f64;
    let sy = tip.start.y * height as f64;
    let ex = tip.end.x * width as f64;
    let ey = tip.end.y * height as f64;
    let start_radius = tip.start_radius * width as f64;
    let end_radius = tip.end_radius * width as f64;
    let reach = tip.reach * width as f64 + 1.0;

    let min_x = ((sx.min(ex) - reach).floor().max(0.0)) as u32;
    let min_y = ((sy.min(ey) - reach).floor().max(0.0)) as u32;
    let max_x = ((sx.max(ex) + reach).ceil().min(width as f64)) as u32;
    let max_y = ((sy.max(ey) + reach).ceil().min(height as f64)) as u32;
    if min_x >= max_x || min_y >= max_y {
        return;
    }

    let span_x = ex - sx;
    let span_y = ey - sy;
    let length_squared = span_x * span_x + span_y * span_y;

    for y in min_y..max_y {
        let py = y as f64 + 0.5;
        for x in min_x..max_x {
            let px = x as f64 + 0.5;
            let amount = if length_squared == 0.0 {
                0.0
            } else {
                (((px - sx) * span_x + (py - sy) * span_y) / length_squared).clamp(0.0, 1.0)
            };
            let centre_x = sx + span_x * amount;
            let centre_y = sy + span_y * amount;
            let distance = ((px - centre_x).powi(2) + (py - centre_y).powi(2)).sqrt();
            let radius = start_radius + (end_radius - start_radius) * amount;
            let coverage = ((radius + 0.5 - distance) / 1.0).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let density = if length_squared == 0.0 {
                tip.start_density.max(tip.end_density)
            } else {
                tip.start_density + (tip.end_density - tip.start_density) * amount
            };
            let value = (coverage * density) as f32;
            let index = y as usize * width as usize + x as usize;
            if value > mask[index] {
                mask[index] = value;
            }
        }
    }
}

fn to_rgba8(layer: &[f32]) -> Vec<u8> {
    let mut pixels = vec![0u8; layer.len()];
    for (index, pixel) in layer.chunks_exact(4).enumerate() {
        let alpha = pixel[3].clamp(0.0, 1.0);
        let out = index * 4;
        if alpha <= 0.0 {
            continue;
        }
        pixels[out] =
            (encode_transfer((pixel[0] / alpha).clamp(0.0, 1.0) as f64) * 255.0).round() as u8;
        pixels[out + 1] =
            (encode_transfer((pixel[1] / alpha).clamp(0.0, 1.0) as f64) * 255.0).round() as u8;
        pixels[out + 2] =
            (encode_transfer((pixel[2] / alpha).clamp(0.0, 1.0) as f64) * 255.0).round() as u8;
        pixels[out + 3] = (alpha * 255.0).round() as u8;
    }
    pixels
}
