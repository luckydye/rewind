//! `%APPDATA%\Rewind\config.json`, restored on launch.

use crate::paths;
use crate::protocol::RecorderConfig;

pub fn load() -> RecorderConfig {
    std::fs::read(paths::config_dir().join("config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(config: &RecorderConfig) {
    let dir = paths::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_vec_pretty(config) {
        // Write then rename, so a crash mid-write cannot leave half a file.
        let temporary = dir.join("config.json.tmp");
        if std::fs::write(&temporary, json).is_ok() {
            let _ = std::fs::rename(&temporary, dir.join("config.json"));
        }
    }
}
