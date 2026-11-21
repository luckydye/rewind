//! Brush, stroke and point types for the Ink brush.
//!
//! This is the reusable part: it knows nothing about the overlay, GPUI or the
//! daemon. Coordinates are normalised (0..1 across the capture), so one set of
//! strokes renders at preview size and again at native size for export.

/// One pointer reading. Optional fields are absent when the device does not
/// report them; a mouse has no pressure and draws by speed instead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    /// Milliseconds; only differences matter.
    pub time: Option<f64>,
    /// 0..1, pens only.
    pub pressure: Option<f64>,
    /// 0 = upright, 1 = lying flat.
    pub tilt: Option<f64>,
    /// Turns, 0..1.
    pub azimuth: Option<f64>,
    /// Turns, 0..1.
    pub twist: Option<f64>,
    /// Working width carried along the stroke (set by the gesture).
    pub strength: Option<f64>,
}

impl Point {
    pub fn at(x: f64, y: f64) -> Self {
        Self {
            x,
            y,
            time: None,
            pressure: None,
            tilt: None,
            azimuth: None,
            twist: None,
            strength: None,
        }
    }
}

/// The Ink brush: crisp, pressure- and speed-responsive, smoothed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Brush {
    pub width: f64,
    pub density: f64,
    pub dynamics: f64,
    pub smoothing: f64,
}

impl Brush {
    pub const SMOOTHING: f64 = 0.15;
    pub const DENSITY: f64 = 1.0;
    pub const DYNAMICS: f64 = 1.0;

    pub fn ink(width: f64) -> Self {
        Self {
            width,
            density: Self::DENSITY,
            dynamics: Self::DYNAMICS,
            smoothing: Self::SMOOTHING,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushKind {
    Stroke,
    Eraser,
}

/// A stroke in progress. Mirrors the reference engine's BrushAction.
#[derive(Debug, Clone)]
pub struct BrushAction {
    pub kind: BrushKind,
    pub points: Vec<Point>,
    pub brush: Brush,
    pub gesture_scale: f64,
    pub smoothing_radius: f64,
    pub soft_start: bool,
    pub rasterized_segments: usize,
    pub velocity_from: usize,
    pub velocity: f64,
    pub arc_lengths: Vec<f64>,
}

pub type Rgb = [f64; 3];
