//! Re-encodes a range that has to change pixels: an annotation layer burnt in,
//! a crop, or both. Decoding happens through `PreviewEngine`; this is the
//! encoder side, fed with BGRA frames and drained for AVCC access units.

use std::sync::Arc;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc, AppStreamType};

use crate::codec::{self, Settings};
use crate::h264;
use crate::protocol::RawImage;
use crate::ring::{Packet, ParameterSets, Snapshot};

pub struct Reencoder {
    pipeline: gst::Pipeline,
    appsrc: AppSrc,
    appsink: AppSink,
    width: u32,
    height: u32,
}

impl Reencoder {
    pub fn new(size: (u32, u32), fps: u32, bitrate: u32) -> Result<Reencoder, String> {
        let encoder = codec::choose_encoder()?;
        let settings = Settings { bitrate, fps, width: size.0, height: size.1 };
        let description = format!(
            "appsrc name=src is-live=false format=time stream-type=stream ! \
             video/x-raw,format=BGRA,width={w},height={h},framerate={fps}/1 ! \
             videoconvert ! video/x-raw,format=NV12 ! {encoder} ! \
             h264parse config-interval=-1 ! video/x-h264,stream-format=avc,alignment=au ! \
             appsink name=sink sync=false emit-signals=false",
            w = size.0,
            h = size.1,
            encoder = codec::encoder_description(encoder, &settings),
        );
        let pipeline: gst::Pipeline = gst::parse::launch(&description)
            .map_err(|error| format!("cannot build the re-encode pipeline: {error}"))?
            .downcast()
            .map_err(|_| "the re-encode description did not produce a pipeline")?;
        let appsrc: AppSrc = pipeline
            .by_name("src")
            .ok_or("the re-encode pipeline has no appsrc")?
            .downcast()
            .map_err(|_| "re-encode src is not an appsrc")?;
        let appsink: AppSink = pipeline
            .by_name("sink")
            .ok_or("the re-encode pipeline has no appsink")?
            .downcast()
            .map_err(|_| "re-encode sink is not an appsink")?;

        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "BGRA")
            .field("width", size.0 as i32)
            .field("height", size.1 as i32)
            .field("framerate", gst::Fraction::new(fps as i32, 1))
            .build();
        appsrc.set_caps(Some(&caps));
        appsrc.set_format(gst::Format::Time);
        appsrc.set_is_live(false);
        appsrc.set_stream_type(AppStreamType::Stream);
        pipeline.set_state(gst::State::Playing).map_err(|error| format!("cannot start the re-encoder: {error:?}"))?;

        Ok(Reencoder { pipeline, appsrc, appsink, width: size.0, height: size.1 })
    }

    pub fn push(&self, frame: &RawImage, pts: i64) -> Result<(), String> {
        if frame.width != self.width || frame.height != self.height {
            return Err(format!(
                "frame is {}×{}, the re-encoder was built for {}×{}",
                frame.width, frame.height, self.width, self.height
            ));
        }
        let mut buffer = gst::Buffer::from_slice(frame.pixels.clone());
        buffer
            .get_mut()
            .ok_or("cannot write the re-encoder timestamp")?
            .set_pts(gst::ClockTime::from_nseconds(pts as u64));
        self.appsrc.push_buffer(buffer).map_err(|error| format!("cannot feed the re-encoder: {error:?}"))?;
        Ok(())
    }

    pub fn finish(self) -> Result<Snapshot, String> {
        self.appsrc.end_of_stream().map_err(|error| format!("cannot finish the re-encoder: {error:?}"))?;

        let mut packets = Vec::new();
        let mut parameters: Option<ParameterSets> = None;
        loop {
            let Some(sample) = self.appsink.try_pull_sample(gst::ClockTime::from_seconds(5)) else {
                break;
            };
            let buffer = sample.buffer().ok_or("re-encode produced an empty sample")?;
            let pts = buffer.pts().ok_or("re-encode produced a sample without a timestamp")?.nseconds() as i64;
            let data = buffer.map_readable().map_err(|_| "cannot read a re-encoded buffer")?;
            let unit = h264::from_avcc(data.as_slice()).ok_or("the re-encoder produced a buffer that is not AVCC")?;
            drop(data);

            if let (Some(sps), Some(pps)) = (unit.sps, unit.pps) {
                if parameters.is_none() {
                    parameters = Some(ParameterSets { sps, pps });
                }
            }
            packets.push(Arc::new(Packet { pts, keyframe: unit.keyframe, data: unit.avcc }));
        }

        let parameters = parameters.ok_or("the re-encoder produced no SPS/PPS")?;
        if packets.is_empty() {
            return Err("the re-encoder produced no frames".into());
        }
        self.pipeline.set_state(gst::State::Null).map_err(|error| format!("cannot stop the re-encoder: {error:?}"))?;
        Ok(Snapshot { packets, parameters })
    }
}

impl Drop for Reencoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
