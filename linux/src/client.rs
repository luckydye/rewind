//! The client half of the socket, shared by `rewindctl` and `rewindd list`.

use std::io;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::paths;
use crate::protocol::{self, Reply, Request};

pub fn call(request: &Request) -> Result<Reply, String> {
    let path = paths::socket_path();
    let mut stream =
        UnixStream::connect(&path).map_err(|error| format!("cannot connect to {} ({error}); is rewindd running?", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(600))).map_err(io_error)?;
    stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(io_error)?;

    protocol::write_request(&mut stream, request).map_err(io_error)?;
    Reply::read_from(&mut stream).map_err(io_error)
}

fn io_error(error: io::Error) -> String {
    format!("socket error: {error}")
}
