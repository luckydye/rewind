//! Decodes frames from the ring on demand, for the overlay's preview, the
//! timeline thumbnails and Copy Frame. Same contract as `PreviewEngine.swift`:
//! decoder state is kept between requests, so scrubbing forward costs one
//! decode per frame and only a backward seek replays from the keyframe.
//!
//! Microsoft's H.264 decoder MFT, synchronous and in system memory: preview
//! cost is paid only while the overlay is open, and a CPU frame is what the
//! socket ships anyway.

use std::mem::ManuallyDrop;
use std::sync::Arc;

use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::Result as WinResult;

use crate::protocol::RawImage;
use crate::ring::{Packet, ParameterSets, Snapshot};

const START_CODE: [u8; 4] = [0, 0, 0, 1];

struct Decoded {
    packet: Arc<Packet>,
    /// NV12 as the decoder laid it out: `stride` bytes per row, chroma after
    /// `plane_rows` rows of luma.
    pixels: Vec<u8>,
    stride: usize,
    plane_rows: usize,
}

struct Decoder {
    transform: IMFTransform,
    sps: Vec<u8>,
    stride: usize,
    plane_rows: usize,
}

#[derive(Default)]
pub struct PreviewEngine {
    decoder: Option<Decoder>,
    decoded: Option<Decoded>,
}

// The MFT is created and used only behind the recorder's preview mutex.
unsafe impl Send for PreviewEngine {}

impl PreviewEngine {
    /// `width` 0 means native: never downscale below the capture size unless
    /// asked, text stops being readable.
    pub fn frame(&mut self, snapshot: &Snapshot, index: usize, width: u32, size: (u32, u32)) -> Result<RawImage, String> {
        if snapshot.packets.is_empty() {
            return Err("the rolling buffer is empty".into());
        }
        let index = index.min(snapshot.packets.len() - 1);
        self.decode(snapshot, index, size)?;
        let decoded = self.decoded.as_ref().ok_or("the decoder produced no frame")?;
        Ok(to_bgra(decoded, size, width))
    }

    /// Thumbnails come from keyframes, which decode without their
    /// neighbours, so they are independent: each distinct keyframe is decoded
    /// once (a short buffer maps several slots onto the same one), spread over
    /// a few decoders in parallel. One decoder in sequence took 1.1 s for 24
    /// at 1440p; the strip is fetched on every show.
    pub fn thumbnails(&mut self, snapshot: &Snapshot, indices: &[usize], width: u32, size: (u32, u32)) -> Result<Vec<RawImage>, String> {
        let mut unique: Vec<usize> = indices.to_vec();
        unique.sort_unstable();
        unique.dedup();

        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 6).min(unique.len().max(1));
        let per_worker = unique.len().div_ceil(workers).max(1);
        let decoded: Vec<Result<Vec<(usize, RawImage)>, String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = unique
                .chunks(per_worker)
                .map(|part| {
                    scope.spawn(move || {
                        let mut engine = PreviewEngine::default();
                        part.iter()
                            .map(|&index| {
                                engine.decoded = None;
                                engine.frame(snapshot, index, width, size).map(|image| (index, image))
                            })
                            .collect()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("thumbnail decoder panicked".into()))).collect()
        });

        let mut by_index = std::collections::HashMap::new();
        for part in decoded {
            for (index, image) in part? {
                by_index.insert(index, image);
            }
        }
        indices
            .iter()
            .map(|index| {
                let image = by_index.get(index).ok_or("missing thumbnail")?;
                Ok(RawImage { width: image.width, height: image.height, pixels: image.pixels.clone() })
            })
            .collect()
    }

    fn decode(&mut self, snapshot: &Snapshot, target: usize, size: (u32, u32)) -> Result<(), String> {
        let needs_decoder = self
            .decoder
            .as_ref()
            .is_none_or(|d| d.sps != snapshot.parameters.sps);
        if needs_decoder {
            self.decoder = None;
            self.decoded = None;
            self.decoder = Some(Decoder::new(size).map_err(|e| format!("cannot create the H.264 decoder: {}", e.message()))?);
        }

        let keyframe = snapshot.keyframe_index(target);
        let mut start = keyframe;
        let mut flush = true;
        if let Some(decoded) = &self.decoded {
            // Same frame again (a static screen between two scrub positions),
            // or a frame further along the same GOP: continue from there.
            if let Some(existing) = (keyframe..=target).find(|&i| Arc::ptr_eq(&snapshot.packets[i], &decoded.packet)) {
                if existing == target {
                    return Ok(());
                }
                start = existing + 1;
                flush = false;
            }
        }

        let decoder = self.decoder.as_mut().unwrap();
        if flush {
            self.decoded = None;
            decoder.flush();
        }
        for index in start..=target {
            let packet = &snapshot.packets[index];
            let output = decoder
                .decode(packet, &snapshot.parameters, index == target)
                .map_err(|e| format!("decode failed: {}", e.message()))?;
            if let Some((pixels, stride, plane_rows)) = output {
                self.decoded = Some(Decoded { packet: packet.clone(), pixels, stride, plane_rows });
            }
        }
        // A decoder that hands frames out late would leave an older frame
        // here; never present it as the requested one.
        match &self.decoded {
            Some(decoded) if Arc::ptr_eq(&decoded.packet, &snapshot.packets[target]) => Ok(()),
            _ => {
                self.decoded = None;
                Err("the decoder produced no frame for this position".into())
            }
        }
    }
}

fn frame_pair(a: u32, b: u32) -> u64 {
    ((a as u64) << 32) | b as u64
}

impl Decoder {
    fn new(size: (u32, u32)) -> WinResult<Self> {
        unsafe {
            let transform: IMFTransform = CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?;
            // One frame out for every frame in: without it the decoder holds
            // frames back for reordering we never use (there are no B-frames).
            if let Ok(attributes) = transform.GetAttributes() {
                let _ = attributes.SetUINT32(&CODECAPI_AVLowLatencyMode, 1);
            }

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, frame_pair(size.0, size.1))?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            transform.SetInputType(0, &input, 0)?;

            let mut decoder = Decoder { transform, sps: Vec::new(), stride: size.0 as usize, plane_rows: size.1 as usize };
            decoder.select_nv12()?;
            decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(decoder)
        }
    }

    /// Picks NV12 among the output types and remembers its layout. The
    /// decoder pads height to a multiple of 16 (1080 -> 1088), so the chroma
    /// plane starts after the padded rows, not after `height`.
    fn select_nv12(&mut self) -> WinResult<()> {
        unsafe {
            let mut index = 0;
            loop {
                let candidate = self.transform.GetOutputAvailableType(0, index)?;
                if candidate.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    self.transform.SetOutputType(0, &candidate, 0)?;
                    let packed = candidate.GetUINT64(&MF_MT_FRAME_SIZE)?;
                    let (width, height) = ((packed >> 32) as usize, (packed & 0xffff_ffff) as usize);
                    let stride = candidate.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(width as i32);
                    self.stride = stride.unsigned_abs() as usize;
                    self.plane_rows = height;
                    return Ok(());
                }
                index += 1;
            }
        }
    }

    fn flush(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
    }

    /// Feeds one access unit. Returns the frame that came out, if any.
    fn decode(&mut self, packet: &Packet, parameters: &ParameterSets, last: bool) -> WinResult<Option<(Vec<u8>, usize, usize)>> {
        let annex_b = to_annex_b(packet, parameters);
        if self.sps.is_empty() {
            self.sps = parameters.sps.clone();
        }

        unsafe {
            let buffer = MFCreateMemoryBuffer(annex_b.len() as u32)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(annex_b.as_ptr(), data, annex_b.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(annex_b.len() as u32)?;

            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(packet.pts)?;
            if packet.keyframe {
                sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
            }

            let mut output = None;
            if let Err(error) = self.transform.ProcessInput(0, &sample, 0) {
                if error.code() != MF_E_NOTACCEPTING {
                    return Err(error);
                }
                output = self.drain_output()?;
                self.transform.ProcessInput(0, &sample, 0)?;
            }
            if let Some(frame) = self.drain_output()? {
                output = Some(frame);
            }
            // The decoder may still hold the frame we need; a drain forces it
            // out and leaves the reference frames intact for the next step.
            if output.is_none() && last {
                self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
                output = self.drain_output()?;
            }
            Ok(output)
        }
    }

    fn drain_output(&mut self) -> WinResult<Option<(Vec<u8>, usize, usize)>> {
        let mut latest = None;
        unsafe {
            loop {
                let info = self.transform.GetOutputStreamInfo(0)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(1))?)?;

                let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(Some(sample)),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                let result = self.transform.ProcessOutput(0, &mut buffers, &mut status);
                let sample = ManuallyDrop::take(&mut buffers[0].pSample);
                ManuallyDrop::drop(&mut buffers[0].pEvents);

                match result {
                    Ok(()) => {}
                    Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(latest),
                    Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        self.select_nv12()?;
                        continue;
                    }
                    Err(error) => return Err(error),
                }

                let Some(sample) = sample else { continue };
                let buffer = sample.ConvertToContiguousBuffer()?;
                let mut data = std::ptr::null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut data, None, Some(&mut len))?;
                let pixels = std::slice::from_raw_parts(data, len as usize).to_vec();
                buffer.Unlock()?;
                latest = Some((pixels, self.stride, self.plane_rows));
            }
        }
    }
}

/// The ring stores AVCC without parameter sets; the decoder wants start
/// codes, with SPS/PPS in front of every keyframe so any GOP decodes alone.
fn to_annex_b(packet: &Packet, parameters: &ParameterSets) -> Vec<u8> {
    let mut out = Vec::with_capacity(packet.data.len() + 64);
    if packet.keyframe {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(&parameters.sps);
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(&parameters.pps);
    }
    let mut data = packet.data.as_slice();
    while data.len() >= 4 {
        let len = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
        let end = (4 + len).min(data.len());
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(&data[4..end]);
        data = &data[end..];
    }
    out
}

/// BT.709 limited range, which is what the encoder was told it gets.
#[inline]
fn yuv_to_bgra(y: i32, u: i32, v: i32, out: &mut [u8]) {
    let c = 298 * (y - 16);
    let d = u - 128;
    let e = v - 128;
    out[0] = ((c + 541 * d + 128) >> 8).clamp(0, 255) as u8;
    out[1] = ((c - 55 * d - 136 * e + 128) >> 8).clamp(0, 255) as u8;
    out[2] = ((c + 459 * e + 128) >> 8).clamp(0, 255) as u8;
    out[3] = 255;
}

/// NV12 to tightly packed BGRA, cropped to the encoded size. A smaller
/// `width` box-filters while converting, so a thumbnail never materialises
/// a full-size frame first. Rows are split across cores.
fn to_bgra(frame: &Decoded, size: (u32, u32), width: u32) -> RawImage {
    let (src_w, src_h) = (size.0 as usize, size.1 as usize);
    let (out_w, out_h) = if width == 0 || width as usize >= src_w {
        (src_w, src_h)
    } else {
        let w = width.max(1) as usize;
        (w, ((src_h * w + src_w / 2) / src_w).max(1))
    };

    let mut pixels = vec![0u8; out_w * out_h * 4];
    let luma = &frame.pixels[..];
    let chroma_offset = frame.stride * frame.plane_rows;
    let stride = frame.stride;
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let rows_per = out_h.div_ceil(threads);

    std::thread::scope(|scope| {
        for (chunk_index, chunk) in pixels.chunks_mut(rows_per * out_w * 4).enumerate() {
            scope.spawn(move || {
                let first_row = chunk_index * rows_per;
                for (row_in_chunk, row) in chunk.chunks_mut(out_w * 4).enumerate() {
                    let dy = first_row + row_in_chunk;
                    if out_w == src_w {
                        let y_row = &luma[dy * stride..];
                        let uv_row = &luma[chroma_offset + (dy / 2) * stride..];
                        for dx in 0..out_w {
                            let uv = (dx & !1) as usize;
                            yuv_to_bgra(y_row[dx] as i32, uv_row[uv] as i32, uv_row[uv + 1] as i32, &mut row[dx * 4..dx * 4 + 4]);
                        }
                    } else {
                        let y0 = dy * src_h / out_h;
                        let y1 = ((dy + 1) * src_h / out_h).max(y0 + 1).min(src_h);
                        for dx in 0..out_w {
                            let x0 = dx * src_w / out_w;
                            let x1 = ((dx + 1) * src_w / out_w).max(x0 + 1).min(src_w);
                            let (mut ys, mut us, mut vs, mut n, mut cn) = (0u32, 0u32, 0u32, 0u32, 0u32);
                            for sy in y0..y1 {
                                let y_row = &luma[sy * stride..];
                                for sx in x0..x1 {
                                    ys += y_row[sx] as u32;
                                    n += 1;
                                }
                            }
                            for cy in (y0 / 2)..y1.div_ceil(2) {
                                let uv_row = &luma[chroma_offset + cy * stride..];
                                for cx in (x0 / 2)..x1.div_ceil(2) {
                                    us += uv_row[cx * 2] as u32;
                                    vs += uv_row[cx * 2 + 1] as u32;
                                    cn += 1;
                                }
                            }
                            yuv_to_bgra((ys / n) as i32, (us / cn) as i32, (vs / cn) as i32, &mut row[dx * 4..dx * 4 + 4]);
                        }
                    }
                }
            });
        }
    });

    RawImage { width: out_w as u32, height: out_h as u32, pixels }
}
