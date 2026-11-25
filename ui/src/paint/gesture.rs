//! The Ink stroke pipeline: raw pointer readings -> smoothed B-spline segments
//! with eased width -> swept tips handed to a sink that rasterises them.
//!
//! A port of the canvas `gesture.ts`, narrowed to the Ink brush.

use super::dynamics::{
    CURVE_FLATNESS, MAX_CURVE_DEPTH, MAX_STROKE_SMOOTHING_RADIUS, MINIMUM_POINT_DISTANCE,
    ORIENTATION_SAMPLE_DELTA, PRESSURE_SAMPLE_DELTA, density_for_point, eased_stroke_width,
    initial_velocity_is_ready, orientation_change, smoothed_point_at, tap_width,
    tip_width_for_point, velocity_width,
};
use super::math::{blend_reading, blend_turns, clamp, interpolate};
use super::stamps::{BrushTip, create_brush_mark};
use super::types::{Brush, BrushAction, BrushKind, Point};

const STRAIGHT_RUN_WIDTH_DRIFT: f64 = CURVE_FLATNESS * 2.0;
const STRAIGHT_RUN_WIDTHS: f64 = 2.0;

/// Where a gesture's marks go.
pub trait MarkSink {
    fn draw(&mut self, action: &BrushAction, tip: BrushTip);
}

#[derive(Debug, Clone, Copy)]
struct CurveSample {
    amount: f64,
}

#[derive(Debug, Clone, Copy)]
struct Cubic {
    start: Point,
    control_one: Point,
    control_two: Point,
    end: Point,
}

#[derive(Debug, Clone, Copy)]
struct SegmentSweep {
    start_width: f64,
    end_width: f64,
}

#[derive(Debug, Clone, Copy)]
enum Placement {
    Dot,
    Smoothed { index: usize },
    Run { from: usize, to: usize },
    Curve { from: CurveSample, to: CurveSample },
}

fn point_at(x: f64, y: f64) -> Point {
    Point::at(x, y)
}

fn midpoint(a: Point, b: Point) -> Point {
    point_at((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
}

#[derive(Debug, Clone)]
pub struct GestureOptions {
    /// A mouse stroke begins no wider than the brush: the hand has not got
    /// going yet, so the slow-means-wide rule would make a blob.
    pub soft_start: bool,
    pub kind: BrushKind,
    pub point: Point,
    pub brush: Brush,
    /// World units per device pixel: how large a pixel of hand movement is.
    pub gesture_scale: f64,
}

#[derive(Debug, Clone)]
pub struct Gesture {
    pub action: BrushAction,
}

impl Gesture {
    pub fn new(options: GestureOptions, sink: &mut impl MarkSink) -> Self {
        let mut first = options.point;
        first.strength = if options.kind == BrushKind::Eraser {
            Some(options.brush.width)
        } else {
            None
        };
        let action = BrushAction {
            kind: options.kind,
            points: vec![first],
            brush: options.brush,
            gesture_scale: if options.gesture_scale > 0.0 {
                options.gesture_scale
            } else {
                1.0
            },
            smoothing_radius: clamp(options.brush.smoothing, 0.0, 1.0)
                * MAX_STROKE_SMOOTHING_RADIUS,
            soft_start: options.soft_start,
            rasterized_segments: 0,
            velocity_from: 0,
            velocity: 0.0,
            arc_lengths: vec![0.0],
        };
        let mut gesture = Self { action };
        if options.kind == BrushKind::Eraser {
            gesture.paint_dot(options.brush.width, sink);
        }
        gesture
    }

    pub fn append(&mut self, points: &[Point], sink: &mut impl MarkSink) {
        for point in points {
            self.append_point(*point);
        }
        self.paint_settled_segments(false, sink);
    }

    pub fn commit(&mut self, sink: &mut impl MarkSink) {
        let single_point = self.action.points.len() == 1;
        if self.action.kind == BrushKind::Stroke && self.action.points[0].strength.is_none() {
            if single_point {
                let width = tip_width_for_point(
                    &self.action,
                    self.action.points[0],
                    tap_width(self.action.brush.width),
                );
                self.action.points[0].strength = Some(width);
                self.paint_dot(width, sink);
            } else {
                self.paint_buffered_stroke_start(sink);
            }
        }
        if self.action.kind == BrushKind::Stroke && self.action.points.len() > 1 {
            self.paint_ready_stroke_segments(true, sink);
        }
        if self.action.kind == BrushKind::Eraser {
            self.paint_settled_eraser_segments(true, sink);
        }
    }

    fn append_point(&mut self, reported: Point) {
        let action = &mut self.action;
        let previous = *action.points.last().unwrap();
        let movement =
            ((reported.x - previous.x).powi(2) + (reported.y - previous.y).powi(2)).sqrt();
        let pressure_change = match (reported.pressure, previous.pressure) {
            (Some(reported), Some(previous)) => (reported - previous).abs(),
            _ => 0.0,
        };
        if action.kind == BrushKind::Stroke
            && movement < MINIMUM_POINT_DISTANCE * action.gesture_scale
            && pressure_change < PRESSURE_SAMPLE_DELTA
            && orientation_change(previous, reported) < ORIENTATION_SAMPLE_DELTA
        {
            return;
        }
        action.points.push(reported);
        action
            .arc_lengths
            .push(action.arc_lengths[action.arc_lengths.len() - 1] + movement);
        if action.kind != BrushKind::Stroke || action.points[0].strength.is_none() {
            return;
        }
        let previous_strength = previous.strength.unwrap_or(action.brush.width);
        let from_velocity = velocity_width(action);
        let target = tip_width_for_point(action, reported, from_velocity);
        action.points.last_mut().unwrap().strength = Some(eased_stroke_width(
            action,
            previous_strength,
            target,
            movement,
            pressure_change,
        ));
    }

    fn paint_settled_segments(&mut self, flush_tail: bool, sink: &mut impl MarkSink) {
        if self.action.kind == BrushKind::Eraser {
            self.paint_settled_eraser_segments(flush_tail, sink);
        } else if self.action.points[0].strength.is_some() {
            self.paint_ready_stroke_segments(flush_tail, sink);
        }
        if self.action.points[0].strength.is_none() && initial_velocity_is_ready(&self.action) {
            self.paint_buffered_stroke_start(sink);
            self.paint_ready_stroke_segments(flush_tail, sink);
        }
    }

    fn paint_settled_eraser_segments(&mut self, flush_tail: bool, sink: &mut impl MarkSink) {
        let travelled = self.action.arc_lengths[self.action.arc_lengths.len() - 1];
        let settled_travel = self.action.smoothing_radius * self.action.gesture_scale;
        while self.action.rasterized_segments < self.action.points.len() - 1 {
            let index = self.action.rasterized_segments;
            if !flush_tail && travelled - self.action.arc_lengths[index + 1] < settled_travel {
                break;
            }
            self.paint_smoothed_segment(index, sink);
            self.action.rasterized_segments += 1;
        }
    }

    fn paint_buffered_stroke_start(&mut self, sink: &mut impl MarkSink) {
        let action = &mut self.action;
        // Too short to measure is a tap, not the slow end of the curve: use
        // the light width instead of letting a zero velocity read as widest.
        let initial = if initial_velocity_is_ready(action) {
            velocity_width(action)
        } else {
            tap_width(action.brush.width)
        };
        let widths: Vec<f64> = action
            .points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let width = tip_width_for_point(action, *point, initial);
                if !action.soft_start || point.pressure.is_some() {
                    return width;
                }
                // the first stretch of a mouse stroke grows in from about the
                // brush's own width, instead of starting at the widest
                let t = (action.arc_lengths[index]
                    / (action.brush.width * action.gesture_scale * 1.5))
                    .min(1.0);
                width * (0.55 + 0.45 * t * t * (3.0 - 2.0 * t))
            })
            .collect();
        for (point, width) in action.points.iter_mut().zip(widths) {
            point.strength = Some(width);
        }
        self.paint_ready_stroke_segments(false, sink);
    }

    fn paint_ready_stroke_segments(&mut self, flush_tail: bool, sink: &mut impl MarkSink) {
        let travelled = self.action.arc_lengths[self.action.arc_lengths.len() - 1];
        let settled_travel = self.action.smoothing_radius * self.action.gesture_scale;
        let readings = self.action.points.len();
        let Some(last_ready) = readings.checked_sub(if flush_tail { 2 } else { 3 }) else {
            return;
        };
        let arcs = self.action.arc_lengths.clone();
        let settled = |segment: usize| {
            flush_tail || travelled - arcs[(segment + 2).min(readings - 1)] >= settled_travel
        };
        while self.action.rasterized_segments <= last_ready {
            let first = self.action.rasterized_segments;
            if !settled(first) {
                break;
            }
            let mut last = first;
            while last < last_ready && settled(last + 1) {
                last += 1;
            }
            let straight_to = self.straight_run_through(first, last);
            if straight_to > first {
                self.paint_straight_run(first, straight_to, sink);
            } else {
                self.paint_curve_segment(first, sink);
            }
            self.action.rasterized_segments = straight_to + 1;
        }
    }

    fn paint_dot(&mut self, width: f64, sink: &mut impl MarkSink) {
        let density = density_for_point(&self.action, self.action.points[0]);
        self.paint_across(Placement::Dot, width, width, density, density, sink);
    }

    fn paint_smoothed_segment(&mut self, index: usize, sink: &mut impl MarkSink) {
        let action = &self.action;
        let start = smoothed_point_at(action, &action.points, index);
        let end = smoothed_point_at(action, &action.points, index + 1);
        self.paint_across(
            Placement::Smoothed { index },
            start.strength.unwrap_or(action.brush.width),
            end.strength.unwrap_or(action.brush.width),
            density_for_point(action, start),
            density_for_point(action, end),
            sink,
        );
    }

    fn paint_straight_run(&mut self, first: usize, last: usize, sink: &mut impl MarkSink) {
        let from = self.segment_sweep(first);
        let to = self.segment_sweep(last);
        self.paint_across(
            Placement::Run {
                from: first,
                to: last + 1,
            },
            from.start_width,
            to.end_width,
            self.action.brush.density,
            self.action.brush.density,
            sink,
        );
    }

    fn paint_curve_segment(&mut self, index: usize, sink: &mut impl MarkSink) {
        let sweep = self.segment_sweep(index);
        let reference = self.curve_through(index);
        let mut samples = vec![CurveSample { amount: 0.0 }];
        flatten_cubic(
            reference.start,
            reference.control_one,
            reference.control_two,
            reference.end,
            0.0,
            1.0,
            &mut samples,
            0,
        );
        for pair in samples.windows(2) {
            let previous = pair[0];
            let current = pair[1];
            self.paint_across(
                Placement::Curve {
                    from: previous,
                    to: current,
                },
                interpolate(sweep.start_width, sweep.end_width, previous.amount),
                interpolate(sweep.start_width, sweep.end_width, current.amount),
                self.action.brush.density,
                self.action.brush.density,
                sink,
            );
        }
    }

    fn place(&self, placement: Placement) -> (Point, Point) {
        let action = &self.action;
        match placement {
            Placement::Dot => (action.points[0], action.points[0]),
            Placement::Smoothed { index } => (
                smoothed_point_at(action, &action.points, index),
                smoothed_point_at(action, &action.points, index + 1),
            ),
            Placement::Run { from, to } => (
                self.curve_through(from).start,
                self.curve_through(to - 1).end,
            ),
            Placement::Curve { from, to } => {
                let curve = self.curve_through(action.rasterized_segments);
                (cubic_at(&curve, from.amount), cubic_at(&curve, to.amount))
            }
        }
    }

    fn paint_across(
        &mut self,
        placement: Placement,
        start_width: f64,
        end_width: f64,
        start_density: f64,
        end_density: f64,
        sink: &mut impl MarkSink,
    ) {
        let (start, end) = self.place(placement);
        let tip = create_brush_mark(super::stamps::BrushMarkRequest {
            start,
            end,
            start_width,
            end_width,
            density: start_density,
            end_density,
        });
        sink.draw(&self.action, tip);
    }

    fn segment_sweep(&self, index: usize) -> SegmentSweep {
        let action = &self.action;
        let points = &action.points;
        let width_at = |at: usize| points[at].strength.unwrap_or(action.brush.width);
        let start_width = width_at(index);
        let end_width = width_at(index + 1);
        let before_width = if index > 0 {
            width_at(index - 1)
        } else {
            start_width * 2.0 - end_width
        };
        let after_width = if index + 2 < points.len() {
            width_at(index + 2)
        } else {
            end_width * 2.0 - start_width
        };
        SegmentSweep {
            start_width: (before_width + start_width * 4.0 + end_width) / 6.0,
            end_width: (start_width + end_width * 4.0 + after_width) / 6.0,
        }
    }

    fn curve_through(&self, index: usize) -> Cubic {
        let action = &self.action;
        let points = &action.points;
        let start = smoothed_point_at(action, points, index);
        let end = smoothed_point_at(action, points, index + 1);
        let before = if index > 0 {
            smoothed_point_at(action, points, index - 1)
        } else {
            extrapolate(start, end)
        };
        let after = if index + 2 < points.len() {
            smoothed_point_at(action, points, index + 2)
        } else {
            extrapolate(end, start)
        };
        bspline_bezier(before, start, end, after)
    }

    fn straight_run_through(&self, first: usize, last: usize) -> usize {
        let action = &self.action;
        if last == first {
            return first;
        }
        let mut curves = vec![self.curve_through(first)];
        let mut sweeps = vec![self.segment_sweep(first)];
        let start = curves[0].start;
        let reach = sweeps[0].start_width.max(action.brush.width) * STRAIGHT_RUN_WIDTHS;
        let mut through = first;
        for segment in (first + 1)..=last {
            curves.push(self.curve_through(segment));
            sweeps.push(self.segment_sweep(segment));
            let end = curves[curves.len() - 1].end;
            if ((end.x - start.x).powi(2) + (end.y - start.y).powi(2)).sqrt() > reach {
                break;
            }
            if !run_holds_chord(&curves, &sweeps, start, end) {
                break;
            }
            through = segment;
        }
        through
    }
}

fn run_holds_chord(curves: &[Cubic], sweeps: &[SegmentSweep], start: Point, end: Point) -> bool {
    let span_x = end.x - start.x;
    let span_y = end.y - start.y;
    let span_squared = span_x * span_x + span_y * span_y;
    if span_squared <= 0.0 {
        return false;
    }
    let from = sweeps[0];
    let to = sweeps[sweeps.len() - 1];
    for (segment, curve) in curves.iter().enumerate() {
        if distance_to_chord(curve.control_one, start, end) > CURVE_FLATNESS
            || distance_to_chord(curve.control_two, start, end) > CURVE_FLATNESS
            || distance_to_chord(curve.end, start, end) > CURVE_FLATNESS
        {
            return false;
        }
        if segment == curves.len() - 1 {
            break;
        }
        let joint = curve.end;
        let along = clamp(
            ((joint.x - start.x) * span_x + (joint.y - start.y) * span_y) / span_squared,
            0.0,
            1.0,
        );
        let held = sweeps[segment];
        if (held.end_width - interpolate(from.start_width, to.end_width, along)).abs()
            > STRAIGHT_RUN_WIDTH_DRIFT
        {
            return false;
        }
    }
    true
}

fn extrapolate(origin: Point, toward: Point) -> Point {
    point_at(origin.x * 2.0 - toward.x, origin.y * 2.0 - toward.y)
}

fn held_at(x: f64, y: f64, reading: Point) -> Point {
    if reading.tilt.is_none() {
        return point_at(x, y);
    }
    Point {
        x,
        y,
        tilt: reading.tilt,
        azimuth: reading.azimuth,
        twist: reading.twist,
        ..Point::at(0.0, 0.0)
    }
}

fn bspline_bezier(before: Point, start: Point, end: Point, after: Point) -> Cubic {
    Cubic {
        start: held_at(
            (before.x + start.x * 4.0 + end.x) / 6.0,
            (before.y + start.y * 4.0 + end.y) / 6.0,
            start,
        ),
        control_one: point_at((start.x * 2.0 + end.x) / 3.0, (start.y * 2.0 + end.y) / 3.0),
        control_two: point_at((start.x + end.x * 2.0) / 3.0, (start.y + end.y * 2.0) / 3.0),
        end: held_at(
            (start.x + end.x * 4.0 + after.x) / 6.0,
            (start.y + end.y * 4.0 + after.y) / 6.0,
            end,
        ),
    }
}

fn cubic_at(curve: &Cubic, amount: f64) -> Point {
    let rest = 1.0 - amount;
    let sw = rest * rest * rest;
    let ow = 3.0 * rest * rest * amount;
    let tw = 3.0 * rest * amount * amount;
    let ew = amount * amount * amount;
    let x =
        curve.start.x * sw + curve.control_one.x * ow + curve.control_two.x * tw + curve.end.x * ew;
    let y =
        curve.start.y * sw + curve.control_one.y * ow + curve.control_two.y * tw + curve.end.y * ew;
    if curve.start.tilt.is_none() && curve.end.tilt.is_none() {
        return point_at(x, y);
    }
    Point {
        x,
        y,
        tilt: blend_reading(curve.start.tilt, curve.end.tilt, amount),
        azimuth: blend_turns(curve.start.azimuth, curve.end.azimuth, amount),
        twist: blend_turns(curve.start.twist, curve.end.twist, amount),
        time: None,
        pressure: None,
        strength: None,
    }
}

fn distance_to_chord(point: Point, start: Point, end: Point) -> f64 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_squared = dx * dx + dy * dy;
    if length_squared == 0.0 {
        return ((point.x - start.x).powi(2) + (point.y - start.y).powi(2)).sqrt();
    }
    let amount = clamp(
        ((point.x - start.x) * dx + (point.y - start.y) * dy) / length_squared,
        0.0,
        1.0,
    );
    ((point.x - (start.x + dx * amount)).powi(2) + (point.y - (start.y + dy * amount)).powi(2))
        .sqrt()
}

#[allow(clippy::too_many_arguments)]
fn flatten_cubic(
    start: Point,
    control_one: Point,
    control_two: Point,
    end: Point,
    start_amount: f64,
    end_amount: f64,
    samples: &mut Vec<CurveSample>,
    depth: usize,
) {
    let flatness =
        distance_to_chord(control_one, start, end).max(distance_to_chord(control_two, start, end));
    if flatness <= CURVE_FLATNESS || depth >= MAX_CURVE_DEPTH {
        samples.push(CurveSample { amount: end_amount });
        return;
    }
    let start_to_one = midpoint(start, control_one);
    let one_to_two = midpoint(control_one, control_two);
    let two_to_end = midpoint(control_two, end);
    let left_control_two = midpoint(start_to_one, one_to_two);
    let right_control_one = midpoint(one_to_two, two_to_end);
    let split = midpoint(left_control_two, right_control_one);
    let middle = (start_amount + end_amount) / 2.0;
    flatten_cubic(
        start,
        start_to_one,
        left_control_two,
        split,
        start_amount,
        middle,
        samples,
        depth + 1,
    );
    flatten_cubic(
        split,
        right_control_one,
        two_to_end,
        end,
        middle,
        end_amount,
        samples,
        depth + 1,
    );
}
