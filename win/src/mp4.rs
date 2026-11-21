//! Minimal MP4 writer for one H.264 track. The samples are muxed as they came
//! out of the encoder; nothing is re-encoded.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

use std::sync::Arc;

use crate::ring::{Packet, ParameterSets, TICKS_PER_SECOND};

const TIMESCALE: u32 = 90_000;
const MOVIE_TIMESCALE: u32 = 1_000;
const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

struct Atom(Vec<u8>);

impl Atom {
    fn new(kind: &[u8; 4]) -> Self {
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(kind);
        Atom(bytes)
    }

    fn full(kind: &[u8; 4], version: u8, flags: u32) -> Self {
        let mut atom = Atom::new(kind);
        atom.u32(((version as u32) << 24) | flags);
        atom
    }

    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }
    fn zeros(&mut self, n: usize) -> &mut Self {
        self.0.resize(self.0.len() + n, 0);
        self
    }
    fn child(&mut self, atom: Atom) -> &mut Self {
        self.bytes(&atom.finish())
    }
    fn finish(mut self) -> Vec<u8> {
        let size = self.0.len() as u32;
        self.0[..4].copy_from_slice(&size.to_be_bytes());
        self.0
    }
}

pub struct Track<'a> {
    pub width: u32,
    pub height: u32,
    pub parameters: &'a ParameterSets,
    /// Ordered by presentation time; the first must be a keyframe.
    pub samples: &'a [Arc<Packet>],
    /// Duration given to the last sample, which has no successor to measure.
    pub frame_ticks: i64,
}

pub fn write(path: &Path, track: &Track) -> io::Result<u64> {
    let samples = track.samples;
    if samples.is_empty() || !samples[0].keyframe {
        return Err(io::Error::other("a clip must start at a keyframe"));
    }

    let to_timescale = |ticks: i64| (ticks as i128 * TIMESCALE as i128 / TICKS_PER_SECOND as i128) as i64;
    let base = samples[0].pts;
    let mut durations: Vec<u32> = samples
        .windows(2)
        .map(|pair| (to_timescale(pair[1].pts - base) - to_timescale(pair[0].pts - base)).max(1) as u32)
        .collect();
    durations.push(to_timescale(track.frame_ticks).max(1) as u32);
    let media_duration: u64 = durations.iter().map(|&d| d as u64).sum();
    let movie_duration = media_duration * MOVIE_TIMESCALE as u64 / TIMESCALE as u64;

    let mut ftyp = Atom::new(b"ftyp");
    ftyp.bytes(b"isom").u32(0x200).bytes(b"isomiso2avc1mp41");
    let ftyp = ftyp.finish();

    let payload: u64 = samples.iter().map(|s| s.data.len() as u64).sum();
    let mdat_header = 8u64;
    if payload + mdat_header > u32::MAX as u64 {
        return Err(io::Error::other("clip larger than 4 GB"));
    }
    let chunk_offset = ftyp.len() as u64 + mdat_header;

    // stts: run-length encoded sample durations (variable frame rate).
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &d in &durations {
        match runs.last_mut() {
            Some((count, delta)) if *delta == d => *count += 1,
            _ => runs.push((1, d)),
        }
    }
    let mut stts = Atom::full(b"stts", 0, 0);
    stts.u32(runs.len() as u32);
    for (count, delta) in runs {
        stts.u32(count).u32(delta);
    }

    let keyframes: Vec<u32> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.keyframe)
        .map(|(i, _)| i as u32 + 1)
        .collect();
    let mut stss = Atom::full(b"stss", 0, 0);
    stss.u32(keyframes.len() as u32);
    for k in keyframes {
        stss.u32(k);
    }

    let mut stsz = Atom::full(b"stsz", 0, 0);
    stsz.u32(0).u32(samples.len() as u32);
    for s in samples {
        stsz.u32(s.data.len() as u32);
    }

    let mut stsc = Atom::full(b"stsc", 0, 0);
    stsc.u32(1).u32(1).u32(samples.len() as u32).u32(1);

    let mut stco = Atom::full(b"stco", 0, 0);
    stco.u32(1).u32(chunk_offset as u32);

    let ParameterSets { sps, pps } = track.parameters;
    let mut avcc = Atom::new(b"avcC");
    avcc.u8(1)
        .u8(sps[1])
        .u8(sps[2])
        .u8(sps[3])
        .u8(0xff) // four-byte NAL lengths
        .u8(0xe1) // one SPS
        .u16(sps.len() as u16)
        .bytes(sps)
        .u8(1)
        .u16(pps.len() as u16)
        .bytes(pps);

    let mut avc1 = Atom::new(b"avc1");
    avc1.zeros(6)
        .u16(1) // data reference index
        .zeros(16)
        .u16(track.width as u16)
        .u16(track.height as u16)
        .u32(0x0048_0000)
        .u32(0x0048_0000)
        .u32(0)
        .u16(1) // frame count
        .zeros(32) // compressor name
        .u16(0x0018)
        .u16(0xffff)
        .child(avcc);

    let mut stsd = Atom::full(b"stsd", 0, 0);
    stsd.u32(1).child(avc1);

    let mut stbl = Atom::new(b"stbl");
    stbl.child(stsd).child(stts).child(stss).child(stsc).child(stsz).child(stco);

    let mut dref = Atom::full(b"dref", 0, 0);
    dref.u32(1).child(Atom::full(b"url ", 0, 1));
    let mut dinf = Atom::new(b"dinf");
    dinf.child(dref);

    let mut vmhd = Atom::full(b"vmhd", 0, 1);
    vmhd.zeros(8);

    let mut minf = Atom::new(b"minf");
    minf.child(vmhd).child(dinf).child(stbl);

    let mut mdhd = Atom::full(b"mdhd", 0, 0);
    mdhd.u32(0).u32(0).u32(TIMESCALE).u32(media_duration as u32).u16(0x55c4).u16(0);

    let mut hdlr = Atom::full(b"hdlr", 0, 0);
    hdlr.u32(0).bytes(b"vide").zeros(12).bytes(b"VideoHandler\0");

    let mut mdia = Atom::new(b"mdia");
    mdia.child(mdhd).child(hdlr).child(minf);

    let mut tkhd = Atom::full(b"tkhd", 0, 3);
    tkhd.u32(0).u32(0).u32(1).u32(0).u32(movie_duration as u32).zeros(8);
    tkhd.u16(0).u16(0).u16(0).u16(0);
    for m in MATRIX {
        tkhd.u32(m);
    }
    tkhd.u32(track.width << 16).u32(track.height << 16);

    let mut trak = Atom::new(b"trak");
    trak.child(tkhd).child(mdia);

    let mut mvhd = Atom::full(b"mvhd", 0, 0);
    mvhd.u32(0).u32(0).u32(MOVIE_TIMESCALE).u32(movie_duration as u32);
    mvhd.u32(0x0001_0000).u16(0x0100).zeros(10);
    for m in MATRIX {
        mvhd.u32(m);
    }
    mvhd.zeros(24).u32(2);

    let mut moov = Atom::new(b"moov");
    moov.child(mvhd).child(trak);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = io::BufWriter::new(File::create(path)?);
    file.write_all(&ftyp)?;
    file.write_all(&((payload + mdat_header) as u32).to_be_bytes())?;
    file.write_all(b"mdat")?;
    for s in samples {
        file.write_all(&s.data)?;
    }
    let moov = moov.finish();
    file.write_all(&moov)?;
    file.flush()?;

    Ok(ftyp.len() as u64 + mdat_header + payload + moov.len() as u64)
}
