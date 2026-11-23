//! What the overlay remembers between captures: the output format and the
//! crop. Kept in its own file rather than the daemon's `config.json`, because
//! changing the recorder config restarts capture.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::protocol::{Crop, ExportFormat};

/// What Copy puts on the clipboard and Export saves: the frame under the
/// playhead as PNG, or the range as MP4 or GIF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    #[default]
    Image,
    Mp4,
    Gif,
}

impl Output {
    pub fn label(self) -> &'static str {
        match self {
            Output::Image => "Image",
            Output::Mp4 => "MP4",
            Output::Gif => "GIF",
        }
    }

    /// The clip format for range actions. ⌘C always copies the range, so with
    /// Image selected it falls back to MP4.
    pub fn clip_format(self) -> ExportFormat {
        match self {
            Output::Gif => ExportFormat::Gif,
            Output::Image | Output::Mp4 => ExportFormat::Mp4,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub output: Output,
    /// Fractions of the capture, so it survives a change of resolution.
    pub crop: Option<Crop>,
}

#[cfg(target_os = "macos")]
fn path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME is unset");
    PathBuf::from(home).join("Library/Application Support/Rewind/overlay.json")
}

/// Beside the daemon's `config.json`, as on the other platforms.
#[cfg(target_os = "linux")]
fn path() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").expect("HOME is unset")).join(".config"));
    config.join("rewind").join("overlay.json")
}

#[cfg(windows)]
fn path() -> PathBuf {
    let app_data = std::env::var("APPDATA").expect("APPDATA is unset");
    PathBuf::from(app_data).join("Rewind").join("overlay.json")
}

/// No file yet means the defaults; an unreadable one is an error.
pub fn load() -> Result<Settings, String> {
    let path = path();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

pub fn save(settings: &Settings) -> Result<(), String> {
    let path = path();
    let parent = path.parent().expect("settings path has a parent");
    std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let json = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
    std::fs::write(&path, json).map_err(|error| format!("{}: {error}", path.display()))
}
