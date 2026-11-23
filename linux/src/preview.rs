//! Decodes frames from the ring on demand, for the overlay's preview, the
//! timeline thumbnails, Copy Frame and annotated exports.
//!
//! Each request builds a decoder, pushes the access units from the previous
//! keyframe, and flushes with end-of-stream. See `PreviewEngine` for why it is
//! stateless.

use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc, AppStreamType};

use crate::codec;
use crate::h264;
use crate::protocol::RawImage;
use crate::ring::Snapshot;

/// One decoded frame, native BGRA.
struct Decoded {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Decoded {
    /// BGRA at `width` (0 = native). Never upscale: text stops being readable.
    fn render(self, width: u32) -> RawImage {
        if width == 0 || width >= self.width {
            return RawImage { width: self.width, height: self.height, pixels: self.pixels };
        }
        scale_bgra(self.width, self.height, &self.pixels, width)
    }
}

struct Decoder {
    pipeline: gst::Pipeline,
    appsrc: AppSrc,
    appsink: AppSink,
}

impl Decoder {
    fn new(element: &'static str) -> Result<Decoder, String> {
        let description = format!(
            "appsrc name=src is-live=false format=time stream-type=stream ! \
             h264parse ! {element} ! videoconvert ! video/x-raw,format=BGRA ! \
             appsink name=sink sync=false max-buffers=16 drop=true emit-signals=false"
        );
        let pipeline: gst::Pipeline = gst::parse::launch(&description)
            .map_err(|error| format!("cannot build the preview pipeline: {error}"))?
            .downcast()
            .map_err(|_| "the preview description did not produce a pipeline")?;
        let appsrc: AppSrc = pipeline
            .by_name("src")
            .ok_or("the preview pipeline has no appsrc")?
            .downcast()
            .map_err(|_| "preview src is not an appsrc")?;
        let appsink: AppSink = pipeline
            .by_name("sink")
            .ok_or("the preview pipeline has no appsink")?
            .downcast()
            .map_err(|_| "preview sink is not an appsink")?;

        // Byte stream, not AVC: caps with `stream-format=avc` must carry
        // `codec_data`, which h264parse refuses to do without.
        let caps = gst::Caps::builder("video/x-h264").field("stream-format", "byte-stream").field("alignment", "au").build();
        appsrc.set_caps(Some(&caps));
        appsrc.set_format(gst::Format::Time);
        appsrc.set_is_live(false);
        appsrc.set_stream_type(AppStreamType::Stream);
        pipeline.set_state(gst::State::Playing).map_err(|error| format!("cannot start the preview decoder: {error:?}"))?;
        // No state wait: with appsrc the pipeline reports the change only once
        // data flows, so `state()` would sit out its whole timeout. Buffers
        // pushed while it settles are queued, not dropped.

        Ok(Decoder { pipeline, appsrc, appsink })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Decodes from the previous keyframe up to the requested frame and flushes
/// with end-of-stream. Deliberately stateless: a decoder that is kept alive
/// across requests has to be resumed exactly where it stopped or it decodes
/// garbage, and getting that wrong is far more expensive than a pipeline
/// rebuild. The overlay is not part of this cut; when it is, this is the place
/// to add caching and prefetch.
#[derive(Default)]
pub struct PreviewEngine;

impl PreviewEngine {
    pub fn frame(&mut self, snapshot: &Snapshot, index: usize, width: u32) -> Result<RawImage, String> {
        Ok(self.decode(snapshot, index)?.render(width))
    }

    fn decode(&mut self, snapshot: &Snapshot, target: usize) -> Result<Decoded, String> {
        if snapshot.packets.is_empty() {
            return Err("the rolling buffer is empty".into());
        }
        let target = target.min(snapshot.packets.len() - 1);
        let target_pts = snapshot.packets[target].pts;
        let start = snapshot.keyframe_index(target);

        let element = codec::choose_decoder()?;
        let decoder = Decoder::new(element)?;

        let mut wanted: Option<(i64, u32, u32, Vec<u8>)> = None;
        for index in start..=target {
            let packet = &snapshot.packets[index];
            let annex_b = h264::to_annex_b(&packet.data).ok_or_else(|| format!("ring packet {index} is not AVCC"))?;
            let mut buffer = gst::Buffer::from_slice(annex_b);
            buffer
                .get_mut()
                .ok_or("cannot write the buffer timestamp")?
                .set_pts(gst::ClockTime::from_nseconds(packet.pts as u64));
            decoder.appsrc.push_buffer(buffer).map_err(|error| format!("cannot feed the preview decoder: {error:?}"))?;
            while let Some(sample) = decoder.appsink.try_pull_sample(gst::ClockTime::from_mseconds(0)) {
                if let Some(frame) = sample_frame(&sample) {
                    if frame.0 == target_pts {
                        wanted = Some(frame);
                    }
                }
            }
        }

        // End-of-stream is the only thing that flushes the last access unit:
        // both h264parse and the decoder hold a frame until they see the next
        // one.
        decoder.appsrc.end_of_stream().map_err(|error| format!("cannot flush the preview decoder: {error:?}"))?;

        let deadline = Instant::now() + Duration::from_secs(5);
        while wanted.is_none() {
            if Instant::now() >= deadline {
                break;
            }
            match decoder.appsink.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
                Some(sample) => {
                    if let Some(frame) = sample_frame(&sample) {
                        if frame.0 == target_pts {
                            wanted = Some(frame);
                        }
                    }
                }
                None => {
                    if decoder.appsink.is_eos() {
                        break;
                    }
                }
            }
        }

        let detail = if wanted.is_none() {
            decoder
                .pipeline
                .bus()
                .and_then(|bus| bus.pop())
                .map(|message| match message.view() {
                    gst::MessageView::Error(error) => format!(": {} ({:?})", error.error(), error.debug()),
                    _ => String::new(),
                })
                .unwrap_or_default()
        } else {
            String::new()
        };

        let _ = decoder.pipeline.set_state(gst::State::Null);

        let (_, width, height, pixels) =
            wanted.ok_or_else(|| format!("the preview decoder produced no frame at {target}{detail}"))?;
        Ok(Decoded { width, height, pixels })
    }

    /// Keyframes decode without their neighbours, so thumbnails are
    /// independent: each distinct one is decoded once, spread over a few
    /// decoders in parallel.
    pub fn thumbnails(&mut self, snapshot: &Snapshot, indices: &[usize], width: u32) -> Result<Vec<RawImage>, String> {
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
                            .map(|&index| engine.frame(snapshot, index, width).map(|image| (index, image)))
                            .collect()
                    })
                })
                .collect();
            handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| Err("thumbnail decoder panicked".into()))).collect()
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
}

fn sample_frame(sample: &gst::Sample) -> Option<(i64, u32, u32, Vec<u8>)> {
    let buffer = sample.buffer()?;
    let pts = buffer.pts()?.nseconds() as i64;
    let structure = sample.caps()?.structure(0)?;
    let width = structure.get::<i32>("width").ok()? as u32;
    let height = structure.get::<i32>("height").ok()? as u32;
    let map = buffer.map_readable().ok()?;
    Some((pts, width, height, map.as_slice().to_vec()))
}

/// Box-filtered BGRA downscale, threaded by output row. Preview frames cross a
/// socket, so the smaller size is what matters; the exported clip is never
/// scaled here.
pub fn scale_bgra(src_w: u32, src_h: u32, pixels: &[u8], width: u32) -> RawImage {
    let (src_w, src_h) = (src_w as usize, src_h as usize);
    let (out_w, out_h) = if width == 0 || width as usize >= src_w {
        (src_w, src_h)
    } else {
        let w = width.max(1) as usize;
        (w, ((src_h * w + src_w / 2) / src_w).max(1))
    };

    let mut output = vec![0u8; out_w * out_h * 4];
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let rows_per = out_h.div_ceil(threads);

    std::thread::scope(|scope| {
        for (chunk_index, chunk) in output.chunks_mut(rows_per * out_w * 4).enumerate() {
            scope.spawn(move || {
                let first_row = chunk_index * rows_per;
                for (row_index, row) in chunk.chunks_mut(out_w * 4).enumerate() {
                    let dy = first_row + row_index;
                    if out_w == src_w {
                        let start = dy * src_w * 4;
                        row.copy_from_slice(&pixels[start..start + out_w * 4]);
                        continue;
                    }
                    let y0 = dy * src_h / out_h;
                    let y1 = ((dy + 1) * src_h / out_h).max(y0 + 1).min(src_h);
                    for dx in 0..out_w {
                        let x0 = dx * src_w / out_w;
                        let x1 = ((dx + 1) * src_w / out_w).max(x0 + 1).min(src_w);
                        let (mut b, mut g, mut r, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
                        for sy in y0..y1 {
                            let row_start = (sy * src_w + x0) * 4;
                            for pixel in pixels[row_start..row_start + (x1 - x0) * 4].chunks_exact(4) {
                                b += pixel[0] as u32;
                                g += pixel[1] as u32;
                                r += pixel[2] as u32;
                                a += pixel[3] as u32;
                                n += 1;
                            }
                        }
                        let dst = dx * 4;
                        row[dst] = (b / n) as u8;
                        row[dst + 1] = (g / n) as u8;
                        row[dst + 2] = (r / n) as u8;
                        row[dst + 3] = (a / n) as u8;
                    }
                }
            });
        }
    });

    RawImage { width: out_w as u32, height: out_h as u32, pixels: output }
}
