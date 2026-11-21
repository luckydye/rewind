//! Small numeric helpers.

pub fn clamp(value: f64, minimum: f64, maximum: f64) -> f64 {
    value.min(maximum).max(minimum)
}

pub fn interpolate(start: f64, end: f64, amount: f64) -> f64 {
    start + (end - start) * amount
}

pub fn round_half_up(value: f64) -> f64 {
    let rounded = (value + 0.5).floor();
    if rounded == 0.0 && value < 0.0 {
        -0.0
    } else {
        rounded
    }
}

pub fn blend_reading(from: Option<f64>, to: Option<f64>, amount: f64) -> Option<f64> {
    match (from, to) {
        (None, to) => to,
        (from, None) => from,
        (Some(from), Some(to)) => Some(from + (to - from) * amount),
    }
}

/// Interpolates two readings in turns (0..1) the short way round.
pub fn blend_turns(from: Option<f64>, to: Option<f64>, amount: f64) -> Option<f64> {
    match (from, to) {
        (None, to) => to,
        (from, None) => from,
        (Some(from), Some(to)) => {
            let difference = to - from;
            let shortest = difference - round_half_up(difference);
            let turns = from + shortest * amount;
            Some(turns - turns.floor())
        }
    }
}
