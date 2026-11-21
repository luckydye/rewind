//! Annex B (start codes, what Media Foundation emits) to AVCC (length
//! prefixes, what MP4 stores), plus SPS/PPS extraction for the `avcC` box.

pub const NAL_IDR: u8 = 5;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
pub const NAL_AUD: u8 = 9;

/// Splits an Annex B byte stream into NAL units, without start codes.
pub fn nal_units(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }

    let mut units = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map(|&next| next - 3).unwrap_or(stream.len());
        // A four-byte start code leaves a zero at the end of the previous unit.
        while end > start && stream[end - 1] == 0 {
            end -= 1;
        }
        if end > start {
            units.push(&stream[start..end]);
        }
    }
    units
}

pub fn nal_type(unit: &[u8]) -> u8 {
    unit[0] & 0x1f
}

pub struct AccessUnit {
    pub avcc: Vec<u8>,
    pub keyframe: bool,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

/// Converts one encoded frame. Parameter sets and delimiters are lifted out:
/// they live once in `avcC`, not in every sample.
pub fn to_avcc(stream: &[u8]) -> AccessUnit {
    let mut unit = AccessUnit {
        avcc: Vec::with_capacity(stream.len()),
        keyframe: false,
        sps: None,
        pps: None,
    };
    for nal in nal_units(stream) {
        match nal_type(nal) {
            NAL_SPS => unit.sps = Some(nal.to_vec()),
            NAL_PPS => unit.pps = Some(nal.to_vec()),
            NAL_AUD => {}
            kind => {
                if kind == NAL_IDR {
                    unit.keyframe = true;
                }
                unit.avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                unit.avcc.extend_from_slice(nal);
            }
        }
    }
    unit
}
