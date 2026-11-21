//! Rasterises Ink tips into a transparent RGBA layer.
//!
//! A stroke is laid in the units the pointer moves in (screen points), because
//! the brush dynamics are tuned in those units: curve flatness, smoothing reach
//! and velocity all assume one unit is about one pixel of hand movement. Each
//! stroke remembers the size of the space it was drawn in, so the same document
//! renders at preview size and again at native resolution for export.

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
    /// Width and height of the surface the stroke was drawn on, in the same
    /// units as its tips.
    pub space: (f64, f64),
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
    space: (f64, f64),
}

impl StrokeSession {
    /// `point` and later readings are in the units of a surface `space` wide
    /// and tall; `brush.width` is in those units too.
    pub fn new(kind: BrushKind, brush: Brush, color: Rgb, point: Point, space: (f64, f64)) -> Self {
        assert!(space.0 > 0.0 && space.1 > 0.0, "a stroke needs a surface to draw on");
        let mut tips = Vec::new();
        let gesture = {
            let mut recorder = Recorder { tips: &mut tips };
            Gesture::new(
                GestureOptions {
                    soft_start: true,
                    kind,
                    point,
                    brush,
                    gesture_scale: 1.0,
                },
                &mut recorder,
            )
        };
        Self {
            gesture,
            color,
            kind,
            tips,
            space,
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
            space: self.space,
        }
    }

    pub fn kind(&self) -> BrushKind {
        self.kind
    }

    /// The stroke as laid so far.
    pub fn stroke(&self) -> Stroke {
        Stroke { color: self.color, kind: self.kind, tips: self.tips.clone(), space: self.space }
    }

    /// The stroke so far, rasterised for a surface `width` by `height` but
    /// only over the area it covers, so drawing costs the stroke rather than
    /// the whole surface. `None` until the gesture has laid a mark.
    pub fn render_region(&self, width: u32, height: u32) -> Option<Region> {
        let scale = (width as f64 / self.space.0, height as f64 / self.space.1);
        let mut bounds: Option<(f64, f64, f64, f64)> = None;
        for tip in &self.tips {
            let reach = tip.reach * scale.0 + 1.0;
            let (x0, x1) = (tip.start.x.min(tip.end.x) * scale.0, tip.start.x.max(tip.end.x) * scale.0);
            let (y0, y1) = (tip.start.y.min(tip.end.y) * scale.1, tip.start.y.max(tip.end.y) * scale.1);
            let next = (x0 - reach, y0 - reach, x1 + reach, y1 + reach);
            bounds = Some(match bounds {
                None => next,
                Some(b) => (b.0.min(next.0), b.1.min(next.1), b.2.max(next.2), b.3.max(next.3)),
            });
        }
        let (x0, y0, x1, y1) = bounds?;
        let x = (x0.floor().max(0.0) as u32).min(width);
        let y = (y0.floor().max(0.0) as u32).min(height);
        let region_width = (x1.ceil().min(width as f64) as u32).saturating_sub(x);
        let region_height = (y1.ceil().min(height as f64) as u32).saturating_sub(y);
        if region_width == 0 || region_height == 0 {
            return None;
        }
        let target = Target { width: region_width, height: region_height, origin: (x as f64, y as f64), scale };
        let mut layer = vec![0f32; region_width as usize * region_height as usize * 4];
        let mut mask = vec![0f32; region_width as usize * region_height as usize];
        composite(&mut layer, &mut mask, &target, &self.stroke());
        Some(Region { x, y, width: region_width, height: region_height, pixels: to_rgba8(&layer) })
    }
}

/// Part of a surface: where it sits, its size, and straight-alpha RGBA.
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The buffer being rasterised into: its size, where it sits on the surface,
/// and how stroke units map to surface pixels.
struct Target {
    width: u32,
    height: u32,
    origin: (f64, f64),
    scale: (f64, f64),
}

/// A stack of annotations over a capture.
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

    /// Renders the finished strokes, plus `live` on top when given, as
    /// straight-alpha RGBA. An eraser under the pointer needs this: it only
    /// shows against the strokes it removes.
    pub fn render_rgba(&self, width: u32, height: u32, live: Option<&StrokeSession>) -> Vec<u8> {
        let mut layer = vec![0f32; width as usize * height as usize * 4];
        let mut mask = vec![0f32; width as usize * height as usize];
        let live = live.map(StrokeSession::stroke);
        for stroke in self.strokes.iter().chain(live.as_ref()) {
            let target = Target {
                width,
                height,
                origin: (0.0, 0.0),
                scale: (width as f64 / stroke.space.0, height as f64 / stroke.space.1),
            };
            composite(&mut layer, &mut mask, &target, stroke);
        }
        to_rgba8(&layer)
    }
}

fn composite(layer: &mut [f32], mask: &mut [f32], target: &Target, stroke: &Stroke) {
    mask.fill(0.0);
    for tip in &stroke.tips {
        rasterize_tip(mask, target, tip);
    }
    let (width, height) = (target.width, target.height);

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
fn rasterize_tip(mask: &mut [f32], target: &Target, tip: &BrushTip) {
    let (width, height, scale) = (target.width, target.height, target.scale);
    let sx = tip.start.x * scale.0 - target.origin.0;
    let sy = tip.start.y * scale.1 - target.origin.1;
    let ex = tip.end.x * scale.0 - target.origin.0;
    let ey = tip.end.y * scale.1 - target.origin.1;
    // The surface keeps the capture's aspect, so either axis scales a radius.
    let start_radius = tip.start_radius * scale.0;
    let end_radius = tip.end_radius * scale.0;
    let reach = tip.reach * scale.0 + 1.0;

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
