//! Length-prefixed reply parsing for `rewindd`.
//!
//! Replies are either JSON or a raw BGRA image, tagged with a leading kind byte.

use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::protocol::{Request, Response};

/// Matches `RewindKit.Frame.maximumSize`, raised for native-resolution frames.
const MAXIMUM_FRAME_SIZE: usize = 64 * 1024 * 1024;

/// Matches `RewindKit.Reply.imageKind` / `jsonKind`.
const KIND_JSON: u8 = 0;
const KIND_IMAGE: u8 = 1;
const KIND_IMAGES: u8 = 2;

#[derive(Debug, Clone)]
pub struct RawImage {
    pub width: u32,
    pub height: u32,
    /// Tightly packed BGRA.
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum Reply {
    Json(Response),
    Image(RawImage),
    Images(Vec<RawImage>),
}

#[derive(Debug)]
pub enum Error {
    Connect(PathBuf, io::Error),
    Io(io::Error),
    FrameTooLarge(usize),
    Malformed,
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    Rejected(String),
    UnexpectedReply,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Connect(path, error) => {
                write!(f, "cannot connect to {}: {error}", path.display())
            }
            Error::Io(error) => write!(f, "socket error: {error}"),
            Error::FrameTooLarge(size) => write!(f, "frame of {size} bytes is too large"),
            Error::Malformed => write!(f, "malformed frame"),
            Error::Encode(error) => write!(f, "cannot encode request: {error}"),
            Error::Decode(error) => write!(f, "cannot decode response: {error}"),
            Error::Rejected(message) => write!(f, "rewindd rejected the request: {message}"),
            Error::UnexpectedReply => write!(f, "unexpected reply kind"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(unix)]
pub fn default_socket_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME is unset");
    PathBuf::from(home).join("Library/Application Support/Rewind/rewindd.sock")
}

/// Must match `paths::pipe_name` in the Windows daemon.
#[cfg(windows)]
pub fn default_socket_path() -> PathBuf {
    let user: String = std::env::var("USERNAME")
        .unwrap_or_else(|_| "user".into())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    PathBuf::from(format!(r"\\.\pipe\rewind-{user}-rewindd"))
}

#[cfg(unix)]
fn connect(path: &Path) -> io::Result<impl Read + Write> {
    let stream = std::os::unix::net::UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
    Ok(stream)
}

/// A named pipe opens like a file. Every instance can be busy for a moment
/// while the daemon creates the next one, which is retried, not reported.
#[cfg(windows)]
fn connect(path: &Path) -> io::Result<impl Read + Write> {
    const ERROR_PIPE_BUSY: i32 = 231;
    let started = std::time::Instant::now();
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(path) {
            Err(error)
                if error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    && started.elapsed() < Duration::from_secs(5) =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            result => return result,
        }
    }
}

#[derive(Clone)]
pub struct Client {
    path: PathBuf,
}

impl Client {
    pub fn new() -> Self {
        Self {
            path: default_socket_path(),
        }
    }

    pub fn send(&self, request: &Request) -> Result<Reply, Error> {
        let mut stream = connect(&self.path).map_err(|e| Error::Connect(self.path.clone(), e))?;

        let payload = serde_json::to_vec(request).map_err(Error::Encode)?;
        let length = u32::try_from(payload.len()).map_err(|_| Error::FrameTooLarge(payload.len()))?;
        stream
            .write_all(&length.to_be_bytes())
            .and_then(|()| stream.write_all(&payload))
            .map_err(Error::Io)?;

        let mut header = [0u8; 4];
        stream.read_exact(&mut header).map_err(Error::Io)?;
        let length = u32::from_be_bytes(header) as usize;
        if length > MAXIMUM_FRAME_SIZE {
            return Err(Error::FrameTooLarge(length));
        }

        // Read straight into the final buffer: a native frame is ~7 MB and the
        // preview path asks for one per displayed frame.
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).map_err(Error::Io)?;
        decode(body)
    }
}

fn decode(body: Vec<u8>) -> Result<Reply, Error> {
    let mut buffer = body;
    if buffer.is_empty() {
        return Err(Error::Malformed);
    }

    let kind = buffer[0];
    match kind {
        KIND_JSON => serde_json::from_slice(&buffer[1..])
            .map(Reply::Json)
            .map_err(Error::Decode),

        KIND_IMAGE => {
            if buffer.len() < 9 {
                return Err(Error::Malformed);
            }
            let width = u32::from_be_bytes(buffer[1..5].try_into().expect("4 bytes"));
            let height = u32::from_be_bytes(buffer[5..9].try_into().expect("4 bytes"));

            // Drop the 1-byte kind and the 8-byte size header in place, so the
            // pixel payload is never copied a second time.
            let pixels = buffer.split_off(9);
            if pixels.len() != (width as usize) * (height as usize) * 4 {
                return Err(Error::Malformed);
            }
            Ok(Reply::Image(RawImage {
                width,
                height,
                pixels,
            }))
        }

        KIND_IMAGES => {
            if buffer.len() < 5 {
                return Err(Error::Malformed);
            }
            let payload = &buffer[1..];
            let count = u32::from_be_bytes(payload[0..4].try_into().expect("4 bytes")) as usize;
            let mut offset = 4;
            let mut images = Vec::with_capacity(count);
            for _ in 0..count {
                if payload.len() < offset + 8 {
                    return Err(Error::Malformed);
                }
                let width = u32::from_be_bytes(
                    payload[offset..offset + 4].try_into().expect("4 bytes"),
                );
                let height = u32::from_be_bytes(
                    payload[offset + 4..offset + 8].try_into().expect("4 bytes"),
                );
                offset += 8;
                let size = width as usize * height as usize * 4;
                if payload.len() < offset + size {
                    return Err(Error::Malformed);
                }
                images.push(RawImage {
                    width,
                    height,
                    pixels: payload[offset..offset + size].to_vec(),
                });
                offset += size;
            }
            Ok(Reply::Images(images))
        }

        _ => Err(Error::Malformed),
    }
}

/// Sends and turns `ok: false` into an error, which is what every caller wants.
pub fn call(client: &Client, request: Request) -> Result<Response, Error> {
    match client.send(&request)? {
        Reply::Json(response) => match (response.ok, &response.error) {
            (true, _) => Ok(response),
            (false, Some(message)) => Err(Error::Rejected(message.clone())),
            (false, None) => Err(Error::Malformed),
        },
        Reply::Image(_) | Reply::Images(_) => Err(Error::UnexpectedReply),
    }
}
