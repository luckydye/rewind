//! Colour helpers. The engine works in linear RGB; swatches are sRGB hex.

pub fn decode_transfer(encoded: f64) -> f64 {
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

pub fn encode_transfer(linear: f64) -> f64 {
    let magnitude = linear.abs();
    let encoded = if magnitude <= 0.003_130_8 {
        magnitude * 12.92
    } else {
        1.055 * magnitude.powf(1.0 / 2.4) - 0.055
    };
    if linear < 0.0 { -encoded } else { encoded }
}

/// A colour swatch's sRGB hex to the linear RGB the engine paints with.
pub fn from_srgb_hex(hex: u32) -> [f64; 3] {
    let to_linear = |channel: u32| decode_transfer(channel as f64 / 255.0);
    [
        to_linear((hex >> 16) & 0xff),
        to_linear((hex >> 8) & 0xff),
        to_linear(hex & 0xff),
    ]
}
