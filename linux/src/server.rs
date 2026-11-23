//! The socket server. Same framing as the other platforms, one request in and
//! one reply out per connection, mode 0600 under `$XDG_RUNTIME_DIR`.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::time::Duration;

use crate::paths;
use crate::protocol::{self, Reply, Request};

pub type Handler = Arc<dyn Fn(Request) -> Reply + Send + Sync>;

pub fn serve(handler: Handler, stop: impl Fn() -> bool) -> Result<(), String> {
    let path = paths::socket_path();
    let directory = path.parent().ok_or("the socket path has no directory")?;
    std::fs::create_dir_all(directory).map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    std::fs::set_permissions(directory, PermissionsExt::from_mode(0o700))
        .map_err(|error| format!("cannot restrict {}: {error}", directory.display()))?;

    // A live socket means a second daemon; a leftover file does not.
    if UnixStream::connect(&path).is_ok() {
        return Err(format!("another rewindd is listening on {}", path.display()));
    }
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path).map_err(|error| format!("cannot bind {}: {error}", path.display()))?;
    std::fs::set_permissions(&path, PermissionsExt::from_mode(0o600))
        .map_err(|error| format!("cannot restrict {}: {error}", path.display()))?;
    listener.set_nonblocking(true).map_err(|error| format!("cannot poll {}: {error}", path.display()))?;

    while !stop() {
        match listener.accept() {
            Ok((stream, _)) => {
                let handler = handler.clone();
                std::thread::spawn(move || {
                    if let Err(error) = handle(stream, handler) {
                        eprintln!("rewindd: client: {error}");
                    }
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                return Err(format!("cannot accept a client: {error}"));
            }
        }
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn handle(mut stream: UnixStream, handler: Handler) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    // An annotated 4K clip re-encodes every frame; the reply waits for it.
    stream.set_write_timeout(Some(Duration::from_secs(600)))?;

    let body = protocol::read_frame(&mut stream)?;
    let request: Request = serde_json::from_slice(&body).map_err(io::Error::other)?;
    handler(request).write_to(&mut stream)
}
