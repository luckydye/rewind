//! Wire format shared with `rewind-ui`. Mirrors `RewindKit/Messages.swift`,
//! `RewindKit/Config.swift` and `RewindKit/Reply.swift` field for field: the
//! UI is the same binary on both platforms and does not know which daemon it
//! is talking to.
//!
//! Framing: big-endian u32 length, one kind byte, then JSON (`kind = 0`), one
//! BGRA image (`kind = 1`) or a batch of images (`kind = 2`).

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

pub const MAXIMUM_FRAME_SIZE: usize = 64 * 1024 * 1024;
const KIND_JSON: u8 = 0;
const KIND_IMAGE: u8 = 1;
const KIND_IMAGES: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CaptureResolution {
    #[default]
    Automatic,
    Best,
    Nominal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RecorderConfig {
    pub buffer_seconds: f64,
    /// Output height; 0 is the display's native resolution.
    pub height: i64,
    pub fps: i64,
    /// Bits per second; 0 derives 0.18 bits per pixel per frame.
    pub bitrate: i64,
    pub display_index: i64,
    /// Accepted for wire compatibility; single-application capture is not
    /// implemented on Windows, so this is ignored.
    #[serde(rename = "applicationBundleID", skip_serializing_if = "Option::is_none")]
    pub application_bundle_id: Option<String>,
    pub capture_resolution: CaptureResolution,
    pub shows_cursor: bool,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        RecorderConfig {
            buffer_seconds: 60.0,
            height: 0,
            fps: 30,
            bitrate: 0,
            display_index: 0,
            application_bundle_id: None,
            capture_resolution: CaptureResolution::Automatic,
            shows_cursor: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayInfo {
    pub index: i64,
    #[serde(rename = "displayID")]
    pub display_id: u32,
    pub width: i64,
    pub height: i64,
    pub is_main: bool,
    /// Nothing is kept clear at the top of a Windows display.
    pub safe_area_top: f64,
    /// On Windows: the monitor rect in physical virtual-screen pixels, origin
    /// top-left. The overlay hands these straight to `SetWindowPos`.
    pub screen_x: f64,
    pub screen_y: f64,
    pub screen_width: f64,
    pub screen_height: f64,
}

impl DisplayInfo {
    pub fn name(&self) -> String {
        format!("Display {} — {}×{}", self.index, self.width, self.height)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationInfo {
    pub name: String,
    pub bundle_identifier: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Command {
    Status,
    Start,
    Stop,
    SetConfig,
    ExportClip,
    ListDisplays,
    ListApplications,
    PreviewFrame,
    Thumbnails,
    EdgeFrames,
    Estimate,
    CopyFrame,
    CopyClip,
    #[serde(rename = "openUI")]
    OpenUi,
    Quit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ClipRange {
    pub from: f64,
    pub to: f64,
}

impl ClipRange {
    pub fn duration(&self) -> f64 {
        self.from - self.to
    }
}

/// A rectangle of the capture, as fractions of its width and height, so it
/// does not depend on whether a preview or a native frame is cropped.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Crop {
    /// Whole pixels of a `width` by `height` frame, `(x, y, w, h)`. Even
    /// dimensions keep the H.264 encoder happy; the rectangle is shrunk to fit.
    /// Same rounding as `Crop.swift`.
    pub fn pixels(&self, width: u32, height: u32) -> (u32, u32, u32, u32) {
        let (fw, fh) = (width as f64, height as f64);
        let x = (self.x * fw).round().clamp(0.0, fw - 2.0);
        let y = (self.y * fh).round().clamp(0.0, fh - 2.0);
        let w = (self.width * fw).round();
        let h = (self.height * fh).round();
        let w = (w - w % 2.0).min(fw - x).max(2.0);
        let h = (h - h % 2.0).min(fh - y).max(2.0);
        // `min` with an odd remainder can make it odd again.
        let even = |v: f64| (v as u32) & !1;
        (x as u32, y as u32, even(w).max(2), even(h).max(2))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub command: Command,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<RecorderConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<ClipRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<f64>,
    /// Raw RGBA annotation layer (big-endian u32 width, u32 height, then
    /// straight-alpha pixels) to composite into a copied frame or a clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_path: Option<String>,
    /// The part of the capture to keep, if it was cropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<Crop>,
}

impl Request {
    pub fn new(command: Command) -> Self {
        Request {
            command,
            config: None,
            seconds: None,
            path: None,
            width: None,
            count: None,
            range: None,
            anchor: None,
            overlay_path: None,
            crop: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub recording: bool,
    pub config: RecorderConfig,
    pub display: Option<DisplayInfo>,
    pub width: i64,
    pub height: i64,
    pub buffer_seconds: f64,
    pub available_seconds: f64,
    pub buffer_bytes: i64,
    pub bitrate: f64,
    pub frames_captured: i64,
    pub frames_encoded: i64,
    pub dropped_frames: i64,
    pub idle_notices: i64,
    pub on_battery: bool,
    pub last_error: Option<String>,
    pub newest_timestamp: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub path: String,
    pub duration: f64,
    pub bytes: i64,
    pub frames: i64,
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<State>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<Clip>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub displays: Option<Vec<DisplayInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applications: Option<Vec<ApplicationInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<i64>,
}

impl Response {
    pub fn ok() -> Self {
        Response { ok: true, ..Default::default() }
    }

    pub fn failure(message: impl Into<String>) -> Self {
        Response { ok: false, error: Some(message.into()), ..Default::default() }
    }
}

/// Tightly packed BGRA.
pub struct RawImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub enum Reply {
    Json(Response),
    Image(RawImage),
    Images(Vec<RawImage>),
}

impl Reply {
    /// Writes header and pixels straight to the pipe, so a multi-megabyte
    /// frame is never assembled into a second buffer.
    pub fn write_to(&self, out: &mut impl Write) -> io::Result<()> {
        match self {
            Reply::Json(response) => {
                let json = serde_json::to_vec(response).map_err(io::Error::other)?;
                out.write_all(&((json.len() + 1) as u32).to_be_bytes())?;
                out.write_all(&[KIND_JSON])?;
                out.write_all(&json)?;
            }
            Reply::Image(image) => {
                out.write_all(&((1 + 8 + image.pixels.len()) as u32).to_be_bytes())?;
                out.write_all(&[KIND_IMAGE])?;
                out.write_all(&image.width.to_be_bytes())?;
                out.write_all(&image.height.to_be_bytes())?;
                out.write_all(&image.pixels)?;
            }
            Reply::Images(images) => {
                let body: usize = images.iter().map(|i| 8 + i.pixels.len()).sum();
                out.write_all(&((1 + 4 + body) as u32).to_be_bytes())?;
                out.write_all(&[KIND_IMAGES])?;
                out.write_all(&(images.len() as u32).to_be_bytes())?;
                for image in images {
                    out.write_all(&image.width.to_be_bytes())?;
                    out.write_all(&image.height.to_be_bytes())?;
                    out.write_all(&image.pixels)?;
                }
            }
        }
        out.flush()
    }

    pub fn read_from(input: &mut impl Read) -> io::Result<Reply> {
        let body = read_frame(input)?;
        let malformed = || io::Error::new(io::ErrorKind::InvalidData, "malformed reply");
        let (&kind, rest) = body.split_first().ok_or_else(malformed)?;
        let u32_at = |bytes: &[u8], at: usize| -> io::Result<u32> {
            bytes
                .get(at..at + 4)
                .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
                .ok_or_else(malformed)
        };
        match kind {
            KIND_JSON => serde_json::from_slice(rest).map(Reply::Json).map_err(io::Error::other),
            KIND_IMAGE => {
                let (width, height) = (u32_at(rest, 0)?, u32_at(rest, 4)?);
                Ok(Reply::Image(RawImage { width, height, pixels: rest[8..].to_vec() }))
            }
            KIND_IMAGES => {
                let count = u32_at(rest, 0)? as usize;
                let mut offset = 4;
                let mut images = Vec::with_capacity(count);
                for _ in 0..count {
                    let (width, height) = (u32_at(rest, offset)?, u32_at(rest, offset + 4)?);
                    offset += 8;
                    let size = width as usize * height as usize * 4;
                    let pixels = rest.get(offset..offset + size).ok_or_else(malformed)?.to_vec();
                    offset += size;
                    images.push(RawImage { width, height, pixels });
                }
                Ok(Reply::Images(images))
            }
            _ => Err(malformed()),
        }
    }
}

pub fn read_frame(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut header = [0u8; 4];
    input.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if length > MAXIMUM_FRAME_SIZE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut body = vec![0u8; length];
    input.read_exact(&mut body)?;
    Ok(body)
}

pub fn write_request(out: &mut impl Write, request: &Request) -> io::Result<()> {
    let json = serde_json::to_vec(request).map_err(io::Error::other)?;
    out.write_all(&(json.len() as u32).to_be_bytes())?;
    out.write_all(&json)?;
    out.flush()
}
