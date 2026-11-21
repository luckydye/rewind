//! Reusable Ink brush engine and annotation layer.
//!
//! The engine is deliberately independent of GPUI and of rewind's daemon: give
//! it normalised pointer readings and it produces a transparent RGBA layer at
//! whatever size you ask for. The overlay uses it at preview size, and the same
//! strokes are rasterised at native size for the copied image or exported clip.

mod color;
mod dynamics;
mod gesture;
mod math;
mod raster;
mod stamps;
mod types;

pub use color::from_srgb_hex;
pub use raster::{Paint, StrokeSession};
pub use types::{Brush, BrushKind, Point, Rgb};
