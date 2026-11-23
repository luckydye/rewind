//! The Wayland clipboard has no file-copy API: a client serves the selection
//! itself, so `wl-copy` stays alive holding it. Copying a clip is therefore a
//! `text/uri-list` source, and copying a frame an `image/png` source.

use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::image_file;
use crate::paths;
use crate::protocol::RawImage;

fn wl_copy(mime: &str, data: &[u8]) -> Result<(), String> {
    let mut child = Command::new("wl-copy")
        .args(["--type", mime])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        // `wl-copy` forks a process that owns the selection and keeps it until
        // something else takes it. A piped stderr would stay open in that
        // child, and waiting for EOF would wait forever.
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot run wl-copy (install wl-clipboard): {error}"))?;

    child
        .stdin
        .take()
        .ok_or("wl-copy closed its input")?
        .write_all(data)
        .map_err(|error| format!("cannot write to wl-copy: {error}"))?;

    let status = child.wait().map_err(|error| format!("wl-copy failed: {error}"))?;
    if !status.success() {
        return Err(format!("wl-copy exited with {status}"));
    }
    Ok(())
}

fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        match byte {
            b'/' | b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => uri.push(byte as char),
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

pub fn copy_file(path: &Path) -> Result<(), String> {
    wl_copy("text/uri-list", format!("{}\n", file_uri(path)).as_bytes())
}

/// The file a `copyClip` request writes before `wl-copy` starts serving it.
pub fn new_cached_clip_path(extension: &str) -> std::path::PathBuf {
    let directory = paths::clipboard_dir();
    let _ = std::fs::create_dir_all(&directory);
    directory.join(paths::timestamped_clip_name(extension))
}

pub fn copy_image(image: &RawImage) -> Result<(), String> {
    wl_copy("image/png", &image_file::png_bytes(image)?)
}
