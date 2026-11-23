//! AVCC access units, the shape `h264parse` emits with
//! `stream-format=avc,alignment=au` and what the MP4 writer stores: NAL units
//! each preceded by a four-byte big-endian length. SPS and PPS ride in-band in
//! front of every IDR (`config-interval=-1`), so the ring gets them from the
//! stream rather than from an encoder API.

pub const NAL_IDR: u8 = 5;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;

pub struct AccessUnit {
    /// The buffer unchanged, ready for the ring.
    pub avcc: Vec<u8>,
    pub keyframe: bool,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

/// AVCC to Annex B: four-byte lengths become start codes. `h264parse` refuses
/// AVC caps without `codec_data`, and rebuilding that from the ring is more
/// work than converting the access unit; every H.264 decoder reads Annex B.
pub fn to_annex_b(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() + 32);
    let mut offset = 0;
    while offset < data.len() {
        let header = data.get(offset..offset + 4)?;
        let length = u32::from_be_bytes(header.try_into().ok()?) as usize;
        let start = offset + 4;
        let end = start.checked_add(length)?;
        let unit = data.get(start..end)?;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(unit);
        offset = end;
    }
    Some(out)
}

/// `None` means the buffer is not AVCC. That is a real fault — the caps filter
/// after `h264parse` guarantees the format — so callers report it instead of
/// guessing at Annex B.
pub fn from_avcc(data: &[u8]) -> Option<AccessUnit> {
    let mut sps = None;
    let mut pps = None;
    let mut keyframe = false;
    let mut offset = 0;

    while offset < data.len() {
        let header = data.get(offset..offset + 4)?;
        let length = u32::from_be_bytes(header.try_into().ok()?) as usize;
        let start = offset + 4;
        let end = start.checked_add(length)?;
        let unit = data.get(start..end)?;
        match unit.first()? & 0x1f {
            NAL_IDR => keyframe = true,
            NAL_SPS => sps = Some(unit.to_vec()),
            NAL_PPS => pps = Some(unit.to_vec()),
            _ => {}
        }
        offset = end;
    }

    Some(AccessUnit { avcc: data.to_vec(), keyframe, sps, pps })
}
