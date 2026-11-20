//! Mirrors `RewindKit/Messages.swift` and `RewindKit/Config.swift`.
//!
//! The two sides are hand-written against the same wire format, so every field
//! here must keep its exact JSON name. `camelCase` matches Swift's default;
//! acronyms need an explicit rename because Swift emits `displayID`, not
//! `displayId`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureResolution {
    Automatic,
    Best,
    Nominal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderConfig {
    pub buffer_seconds: f64,
    pub height: i64,
    pub fps: i64,
    pub bitrate: i64,
    pub display_index: i64,
    #[serde(default)]
    pub application_bundle_id: Option<String>,
    pub capture_resolution: CaptureResolution,
    pub shows_cursor: bool,
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
    CopyFrame,
    CopyClip,
    #[serde(rename = "openUI")]
    OpenUi,
    Quit,
}

/// A slice of the rolling buffer, measured in seconds before now.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipRange {
    pub from: f64,
    pub to: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub command: Command,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<RecorderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<ClipRange>,
}

impl Request {
    pub fn new(command: Command) -> Self {
        Self {
            command,
            config: None,
            seconds: None,
            path: None,
            width: None,
            range: None,
        }
    }
}

/// Mirrors the daemon's state in full, including fields the overlay does not
/// read yet; the wire format is the contract, not this crate's usage.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub path: String,
    pub duration: f64,
    pub bytes: i64,
    pub frames: i64,
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub ok: bool,
    pub error: Option<String>,
    pub state: Option<State>,
    pub clip: Option<Clip>,
    pub displays: Option<Vec<DisplayInfo>>,
    pub applications: Option<Vec<ApplicationInfo>>,
}
