//! Rolling buffer of encoded frames. Same policy as `FrameRing.swift`: evict by
//! age and by bytes, cut exports at the keyframe before the requested start.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// 100 ns units, the clock Windows.Graphics.Capture and Media Foundation use.
pub const TICKS_PER_SECOND: i64 = 10_000_000;

pub fn seconds(ticks: i64) -> f64 {
    ticks as f64 / TICKS_PER_SECOND as f64
}

/// One access unit. Shared, so a snapshot of the whole buffer for a preview
/// request is ~1800 reference bumps rather than a copy of 60 MB.
pub struct Packet {
    /// Presentation time in 100 ns units, relative to the first captured frame.
    pub pts: i64,
    pub keyframe: bool,
    /// Length-prefixed (AVCC) NAL units, ready for MP4.
    pub data: Vec<u8>,
}

#[derive(Clone)]
pub struct ParameterSets {
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

pub struct Snapshot {
    /// Starts at a keyframe, ordered by presentation time.
    pub packets: Vec<Arc<Packet>>,
    pub parameters: ParameterSets,
}

impl Snapshot {
    pub fn duration(&self) -> f64 {
        match (self.packets.first(), self.packets.last()) {
            (Some(first), Some(last)) => seconds(last.pts - first.pts),
            _ => 0.0,
        }
    }

    pub fn bytes(&self) -> usize {
        self.packets.iter().map(|p| p.data.len()).sum()
    }

    /// The newest frame at or before `time`: what was on screen then.
    pub fn frame_index(&self, time: f64) -> usize {
        self.packets.iter().rposition(|p| seconds(p.pts) <= time).unwrap_or(0)
    }

    /// Keyframes are at most a second apart, so this walks back a few dozen
    /// frames at most.
    pub fn keyframe_index(&self, at_or_before: usize) -> usize {
        (0..=at_or_before.min(self.packets.len().saturating_sub(1)))
            .rev()
            .find(|&i| self.packets[i].keyframe)
            .unwrap_or(0)
    }

    pub fn now(&self, anchor: Option<f64>) -> f64 {
        anchor.unwrap_or_else(|| self.packets.last().map(|p| seconds(p.pts)).unwrap_or(0.0))
    }
}

pub struct Statistics {
    pub bytes: usize,
    pub duration: f64,
    pub frames: usize,
    pub newest: Option<f64>,
}

struct Inner {
    packets: VecDeque<Arc<Packet>>,
    bytes: usize,
    parameters: Option<ParameterSets>,
}

pub struct Ring {
    inner: Mutex<Inner>,
    capacity: i64,
    byte_limit: usize,
}

impl Ring {
    pub fn new(capacity_seconds: f64, byte_limit: usize) -> Self {
        Ring {
            inner: Mutex::new(Inner { packets: VecDeque::new(), bytes: 0, parameters: None }),
            capacity: (capacity_seconds * TICKS_PER_SECOND as f64) as i64,
            byte_limit,
        }
    }

    pub fn set_parameters(&self, parameters: ParameterSets) {
        self.inner.lock().unwrap().parameters = Some(parameters);
    }

    pub fn has_parameters(&self) -> bool {
        self.inner.lock().unwrap().parameters.is_some()
    }

    pub fn append(&self, packet: Packet) {
        let mut inner = self.inner.lock().unwrap();
        inner.bytes += packet.data.len();
        inner.packets.push_back(Arc::new(packet));

        while inner.packets.len() > 1 {
            let span = inner.packets.back().unwrap().pts - inner.packets.front().unwrap().pts;
            if span <= self.capacity && inner.bytes <= self.byte_limit {
                break;
            }
            let evicted = inner.packets.pop_front().unwrap();
            inner.bytes -= evicted.data.len();
        }
    }

    /// Frames between `from` and `to` seconds before `anchor` (default: the
    /// newest frame), starting at the last keyframe at or before `from`.
    /// `from` may be infinite to mean the whole buffer.
    pub fn snapshot(&self, from: f64, to: f64, anchor: Option<f64>) -> Result<Snapshot, String> {
        let inner = self.inner.lock().unwrap();
        let packets = &inner.packets;
        let last = packets.back().ok_or("the rolling buffer is empty")?;
        let parameters = inner
            .parameters
            .clone()
            .ok_or("the encoder has not produced SPS/PPS yet")?;

        let now = anchor.unwrap_or_else(|| seconds(last.pts));
        let window_start = now - from;
        let window_end = now - to;

        let boundary = packets
            .iter()
            .position(|p| seconds(p.pts) >= window_start)
            .unwrap_or(packets.len() - 1);

        // Walk back to a keyframe; if eviction left the oldest frames mid-GOP,
        // cut forward to the first keyframe instead.
        let start = (0..=boundary)
            .rev()
            .find(|&i| packets[i].keyframe)
            .or_else(|| packets.iter().position(|p| p.keyframe))
            .ok_or("the rolling buffer holds no keyframe to cut from")?;

        let end = packets
            .iter()
            .rposition(|p| seconds(p.pts) <= window_end)
            .unwrap_or(start);
        if end < start {
            return Err("the requested range is empty".into());
        }

        Ok(Snapshot { packets: packets.range(start..=end).cloned().collect(), parameters })
    }

    pub fn statistics(&self) -> Statistics {
        let inner = self.inner.lock().unwrap();
        let (Some(first), Some(last)) = (inner.packets.front(), inner.packets.back()) else {
            return Statistics { bytes: 0, duration: 0.0, frames: 0, newest: None };
        };
        Statistics {
            bytes: inner.bytes,
            duration: seconds(last.pts - first.pts),
            frames: inner.packets.len(),
            newest: Some(seconds(last.pts)),
        }
    }
}
