//! Turns a stretch of ink into one swept capsule. Ink never scatters, so the
//! whole mark is a single tip: coverage is computed when rasterising.

use super::types::Point;

#[derive(Debug, Clone, Copy)]
pub struct BrushTip {
    pub start: Point,
    pub end: Point,
    pub start_radius: f64,
    pub end_radius: f64,
    pub start_density: f64,
    pub end_density: f64,
    /// How far past the segment the tip reaches.
    pub reach: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct BrushMarkRequest {
    pub start: Point,
    pub end: Point,
    pub start_width: f64,
    pub end_width: f64,
    pub density: f64,
    pub end_density: f64,
}

pub fn create_brush_mark(request: BrushMarkRequest) -> BrushTip {
    // Widths are normalised to the capture, so the floor is a tiny fraction of
    // it rather than the half-world-unit floor used in canvas world space.
    let start_radius = (request.start_width / 2.0).max(1e-6);
    let end_radius = (request.end_width / 2.0).max(1e-6);
    BrushTip {
        start: request.start,
        end: request.end,
        start_radius,
        end_radius,
        start_density: request.density.clamp(0.0, 1.0),
        end_density: request.end_density.clamp(0.0, 1.0),
        reach: start_radius.max(end_radius),
    }
}
