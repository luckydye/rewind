//! `$XDG_CONFIG_HOME/rewind/config.json`, restored on launch, and the portal's
//! restore token beside it: the token is what turns "pick a screen" into no
//! dialog at all on the next launch.

use crate::paths;
use crate::protocol::RecorderConfig;

fn write_atomically(path: &std::path::Path, bytes: &[u8]) {
    let Some(dir) = path.parent() else { return };
    let _ = std::fs::create_dir_all(dir);
    let temporary = path.with_extension("tmp");
    if std::fs::write(&temporary, bytes).is_ok() {
        let _ = std::fs::rename(&temporary, path);
    }
}

pub fn load() -> RecorderConfig {
    std::fs::read(paths::config_dir().join("config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(config: &RecorderConfig) {
    if let Ok(json) = serde_json::to_vec_pretty(config) {
        write_atomically(&paths::config_dir().join("config.json"), &json);
    }
}

fn token_path() -> std::path::PathBuf {
    paths::config_dir().join("portal-token")
}

pub fn load_restore_token() -> Option<String> {
    std::fs::read_to_string(token_path())
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

pub fn save_restore_token(token: &str) {
    write_atomically(&token_path(), token.as_bytes());
}

pub fn clear_restore_token() {
    let _ = std::fs::remove_file(token_path());
}
