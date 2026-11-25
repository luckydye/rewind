//! How an Ink stroke's width answers the hand. Port of the canvas
//! `dynamics.ts`, narrowed to the solid tip that Ink lays.

use super::math::{clamp, interpolate};
use super::types::{BrushAction, BrushKind, Point};

const SLOW_WIDTH_FACTOR: f64 = 1.8;
const FAST_WIDTH_FACTOR: f64 = 0.35;
const MAX_WIDTH_VELOCITY: f64 = 0.9;
const LIGHT_PRESSURE_WIDTH_FACTOR: f64 = 0.2;
const HEAVY_PRESSURE_WIDTH_FACTOR: f64 = 1.8;
const INITIAL_VELOCITY_DISTANCE: f64 = 10.0;
const VELOCITY_WINDOW_DISTANCE: f64 = 8.0;
pub const MINIMUM_POINT_DISTANCE: f64 = 0.35;
const WIDTH_GROWTH_WIDTHS: f64 = 2.0;
const WIDTH_SHRINK_WIDTHS: f64 = 3.5;
const WIDTH_RESPONSE_MINIMUM: f64 = 16.0;
pub const MAX_STROKE_SMOOTHING_RADIUS: f64 = 20.0;
pub const CURVE_FLATNESS: f64 = 0.2;
pub const MAX_CURVE_DEPTH: usize = 8;
pub const PRESSURE_SAMPLE_DELTA: f64 = 0.015;
pub const ORIENTATION_SAMPLE_DELTA: f64 = 0.015;
const PRESSURE_RESPONSE_DELTA: f64 = 0.5;

fn span(from: Option<f64>, to: Option<f64>) -> f64 {
    match (from, to) {
        (Some(from), Some(to)) => (to - from).abs(),
        _ => 0.0,
    }
}

fn turns_apart(from: Option<f64>, to: Option<f64>) -> f64 {
    match (from, to) {
        (Some(from), Some(to)) => {
            let difference = (to - from).abs() % 1.0;
            difference.min(1.0 - difference)
        }
        _ => 0.0,
    }
}

/// How much the pen's orientation changed between two readings (0..1).
pub fn orientation_change(from: Point, to: Point) -> f64 {
    let lean = from.tilt.unwrap_or(0.0).min(to.tilt.unwrap_or(0.0));
    span(from.tilt, to.tilt)
        .max(turns_apart(from.azimuth, to.azimuth) * lean)
        .max(turns_apart(from.twist, to.twist))
}

/// A reading averaged with its neighbours within the smoothing reach.
pub fn smoothed_point_at(action: &BrushAction, points: &[Point], index: usize) -> Point {
    let arc = &action.arc_lengths;
    let centre = points[index];
    let reach = (action.smoothing_radius * action.gesture_scale)
        .min(arc[index])
        .min(arc[arc.len() - 1] - arc[index]);
    if reach <= 0.0 {
        return centre;
    }
    let mut x = 0.0;
    let mut y = 0.0;
    let mut total = 0.0;
    for other in (0..=index).rev() {
        let offset = arc[index] - arc[other];
        if offset > reach {
            break;
        }
        let weight = 1.0 - offset / reach;
        x += points[other].x * weight;
        y += points[other].y * weight;
        total += weight;
    }
    for other in (index + 1)..points.len() {
        let offset = arc[other] - arc[index];
        if offset > reach {
            break;
        }
        let weight = 1.0 - offset / reach;
        x += points[other].x * weight;
        y += points[other].y * weight;
        total += weight;
    }
    if total <= 0.0 {
        return centre;
    }
    Point {
        x: x / total,
        y: y / total,
        ..centre
    }
}

pub fn tip_width_for_point(action: &BrushAction, point: Point, from_velocity: f64) -> f64 {
    let Some(pressure) = point.pressure else {
        return interpolate(action.brush.width, from_velocity, action.brush.dynamics);
    };
    let factor = interpolate(
        LIGHT_PRESSURE_WIDTH_FACTOR,
        HEAVY_PRESSURE_WIDTH_FACTOR,
        clamp(pressure, 0.0, 1.0),
    );
    interpolate(
        action.brush.width,
        action.brush.width * factor,
        action.brush.dynamics,
    )
}

pub fn density_for_point(action: &BrushAction, _point: Point) -> f64 {
    if action.kind == BrushKind::Eraser {
        return action.brush.density;
    }
    action.brush.density
}

pub fn initial_velocity_is_ready(action: &BrushAction) -> bool {
    action.arc_lengths[action.arc_lengths.len() - 1]
        >= INITIAL_VELOCITY_DISTANCE * action.gesture_scale
}

fn gesture_velocity(action: &mut BrushAction) -> Option<f64> {
    let last = action.points.len().checked_sub(1)?;
    if last < 1 {
        return None;
    }
    let end_time = action.points[last].time?;
    let arc = &action.arc_lengths;
    let opening = arc[last] - VELOCITY_WINDOW_DISTANCE * action.gesture_scale;
    let mut index = action.velocity_from;
    while index < last && arc[index + 1] <= opening {
        index += 1;
    }
    action.velocity_from = index;
    let start_time = action.points[index].time?;
    if start_time >= end_time {
        return None;
    }
    let next_time = action.points.get(index + 1).and_then(|point| point.time);
    let gap = arc[index + 1] - arc[index];
    if arc[index] < opening
        && opening < arc[index + 1]
        && gap > 0.0
        && next_time.is_some_and(|next| next < end_time)
    {
        let opened_at = interpolate(start_time, next_time.unwrap(), (opening - arc[index]) / gap);
        return Some(VELOCITY_WINDOW_DISTANCE / (end_time - opened_at));
    }
    Some((arc[last] - arc[index]) / action.gesture_scale / (end_time - start_time))
}

pub fn width_from_velocity(velocity: f64, base_width: f64) -> f64 {
    let factor = (velocity / MAX_WIDTH_VELOCITY).min(1.0);
    base_width * (SLOW_WIDTH_FACTOR + factor * (FAST_WIDTH_FACTOR - SLOW_WIDTH_FACTOR))
}

/// The width of a mark that never moved. Zero velocity there means "no
/// movement to measure", not "the slowest possible sweep", so a tap is the
/// light dip at the fast end of the curve rather than the widest stroke.
pub fn tap_width(base_width: f64) -> f64 {
    base_width * FAST_WIDTH_FACTOR
}

pub fn velocity_width(action: &mut BrushAction) -> f64 {
    if let Some(measured) = gesture_velocity(action) {
        action.velocity = measured;
    }
    width_from_velocity(action.velocity, action.brush.width)
}

pub fn eased_stroke_width(
    action: &BrushAction,
    previous_width: f64,
    target_width: f64,
    movement: f64,
    pressure_change: f64,
) -> f64 {
    let widths = if target_width > previous_width {
        WIDTH_GROWTH_WIDTHS
    } else {
        WIDTH_SHRINK_WIDTHS
    };
    let response_distance =
        (action.brush.width * widths).max(WIDTH_RESPONSE_MINIMUM * action.gesture_scale);
    let response =
        1.0 - (-movement / response_distance - pressure_change / PRESSURE_RESPONSE_DELTA).exp();
    previous_width + (target_width - previous_width) * response
}
