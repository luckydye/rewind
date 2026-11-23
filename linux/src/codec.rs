//! Which H.264 elements the pipeline uses. GStreamer is the encoder, the video
//! processor and the decoder; which plugin provides them depends on the GPU
//! and on what the distribution installed, so the choice is made once, at
//! startup, and reported by `rewindd doctor`.
//!
//! This is capability selection, not a fallback chain: if the chosen element
//! cannot run, the pipeline fails loudly instead of quietly encoding
//! somewhere else.

use gstreamer as gst;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    /// VAAPI on Intel, AMD and (with libva) virtual GPUs.
    Vah264,
    /// The older VAAPI element, still what Debian 12 ships.
    VaapiH264,
    /// NVIDIA's NVENC through nvcodec.
    NvH264,
    /// Software x264, the floor that always works.
    X264,
}

const ENCODERS: [Encoder; 4] = [Encoder::Vah264, Encoder::VaapiH264, Encoder::NvH264, Encoder::X264];

const DECODERS: [&str; 5] = ["vah264dec", "vaapih264dec", "nvh264dec", "avdec_h264", "openh264dec"];

impl Encoder {
    pub fn element(self) -> &'static str {
        match self {
            Encoder::Vah264 => "vah264enc",
            Encoder::VaapiH264 => "vaapih264enc",
            Encoder::NvH264 => "nvh264enc",
            Encoder::X264 => "x264enc",
        }
    }

    /// VAAPI is the only path that can keep the portal's DMA-BUF on the GPU
    /// through conversion and encoding; everything else needs a system-memory
    /// copy from `pipewiresrc`.
    pub fn uses_vaapi(self) -> bool {
        matches!(self, Encoder::Vah264 | Encoder::VaapiH264)
    }
}

fn present(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

/// The best encoder this machine has, by name.
pub fn choose_encoder() -> Result<Encoder, String> {
    ENCODERS.into_iter().find(|encoder| present(encoder.element())).ok_or_else(|| {
        "no H.264 encoder: install gstreamer1.0-plugins-ugly (x264enc), gstreamer1.0-vaapi or the NVIDIA codec plugins".into()
    })
}

pub fn choose_decoder() -> Result<&'static str, String> {
    DECODERS
        .into_iter()
        .find(|name| present(name))
        .ok_or_else(|| "no H.264 decoder: install gstreamer1.0-libav (avdec_h264) or gstreamer1.0-plugins-bad (openh264dec)".into())
}

pub fn encoders() -> Vec<&'static str> {
    ENCODERS.iter().map(|encoder| encoder.element()).filter(|name| present(name)).collect()
}

pub fn decoders() -> Vec<&'static str> {
    DECODERS.into_iter().filter(|name| present(name)).collect()
}

/// The bitrate is in bit/s everywhere else; GStreamer's encoders take kbit/s.
pub struct Settings {
    pub bitrate: u32,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
}

/// The element plus the properties that matter: no B-frames (the ring and the
/// MP4 writer assume decode order is presentation order), a keyframe at most
/// once a second (an idle screen produces no frames, so a GOP counted in
/// frames would stretch to minutes), and a variable bitrate around the target.
pub fn encoder_description(encoder: Encoder, settings: &Settings) -> String {
    let kbit = (settings.bitrate / 1000).max(1);
    let gop = settings.fps.max(1);
    match encoder {
        Encoder::Vah264 => format!("vah264enc name=enc bitrate={kbit} key-int-max={gop} rate-control=vbr"),
        Encoder::VaapiH264 => format!("vaapih264enc name=enc bitrate={kbit} keyframe-period={gop} max-bframes=0 rate-control=vbr"),
        Encoder::NvH264 => format!("nvh264enc name=enc bitrate={kbit} gop-size={gop} rc-mode=cbr zerolatency=true"),
        Encoder::X264 => format!(
            "x264enc name=enc bitrate={kbit} key-int-max={gop} bframes=0 tune=zerolatency speed-preset=veryfast"
        ),
    }
}

/// The element that turns the portal's packed RGB into NV12 at the target
/// size. `vapostproc` does both on the GPU; `videoscale`/`videoconvert` are the
/// system-memory path.
pub fn converter_description(encoder: Encoder) -> &'static str {
    if encoder.uses_vaapi() { "vapostproc" } else { "videoconvert ! videoscale" }
}
