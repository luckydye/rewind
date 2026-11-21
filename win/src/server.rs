//! Named-pipe server: the Windows twin of `rewindd.sock`. Same framing, one
//! request in, one reply out per connection.
//!
//! The pipe's DACL grants access to the current user only, the equivalent of
//! the socket's mode 0600, and remote clients are rejected outright.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::Arc;

use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::protocol::{self, Reply, Request, Response};

const BUFFER: u32 = 1 << 20;

/// Security descriptor allowing only the user running the daemon.
struct OwnerOnly(PSECURITY_DESCRIPTOR);

unsafe impl Send for OwnerOnly {}
unsafe impl Sync for OwnerOnly {}

impl OwnerOnly {
    fn new() -> Result<Self, String> {
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(|e| e.message())?;
            let mut needed = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
            let mut buffer = vec![0u8; needed as usize];
            let result = GetTokenInformation(token, TokenUser, Some(buffer.as_mut_ptr() as *mut _), needed, &mut needed);
            let _ = CloseHandle(token);
            result.map_err(|e| e.message())?;
            let user = &*(buffer.as_ptr() as *const TOKEN_USER);

            let mut sid = PWSTR::null();
            ConvertSidToStringSidW(user.User.Sid, &mut sid).map_err(|e| e.message())?;
            let sddl = HSTRING::from(format!("D:P(A;;GA;;;{})", sid.to_string().unwrap_or_default()));
            let _ = LocalFree(Some(HLOCAL(sid.0 as *mut _)));

            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut descriptor, None)
                .map_err(|e| e.message())?;
            Ok(OwnerOnly(descriptor))
        }
    }
}

fn create_instance(name: &HSTRING, security: &OwnerOnly, first: bool) -> Result<HANDLE, String> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0.0,
        bInheritHandle: false.into(),
    };
    let mut mode = PIPE_ACCESS_DUPLEX;
    if first {
        mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER,
            BUFFER,
            0,
            Some(&attributes),
        )
    };
    if handle.is_invalid() {
        let error = windows::core::Error::from_thread();
        return Err(if first {
            format!("another rewindd is already running ({})", error.message())
        } else {
            error.message()
        });
    }
    Ok(handle)
}

/// Claims the pipe name (failing if another daemon holds it) and serves
/// requests on a background thread, one thread per connection.
pub fn start(handler: Arc<dyn Fn(Request) -> Reply + Send + Sync>) -> Result<(), String> {
    let name = HSTRING::from(crate::paths::pipe_name());
    let security = OwnerOnly::new()?;
    let first = create_instance(&name, &security, true)?;
    let first = first.0 as isize;

    std::thread::Builder::new()
        .name("ipc".into())
        .spawn(move || {
            let mut next = Some(HANDLE(first as *mut _));
            loop {
                let pipe = match next.take() {
                    Some(pipe) => pipe,
                    None => match create_instance(&name, &security, false) {
                        Ok(pipe) => pipe,
                        Err(error) => {
                            eprintln!("rewindd: cannot create a pipe instance: {error}");
                            std::thread::sleep(std::time::Duration::from_secs(1));
                            continue;
                        }
                    },
                };
                let connected = unsafe { ConnectNamedPipe(pipe, None) };
                let connected = match connected {
                    Ok(()) => true,
                    Err(error) => error.code() == ERROR_PIPE_CONNECTED.to_hresult(),
                };
                if !connected {
                    unsafe {
                        let _ = CloseHandle(pipe);
                    }
                    continue;
                }
                let handler = handler.clone();
                let raw = pipe.0 as isize;
                std::thread::spawn(move || serve(HANDLE(raw as *mut _), &*handler));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn serve(pipe: HANDLE, handler: &(dyn Fn(Request) -> Reply + Send + Sync)) {
    // The File owns the handle from here and closes it on drop.
    let mut file = unsafe { File::from_raw_handle(pipe.0) };
    let reply = match protocol::read_frame(&mut file) {
        Ok(body) => match serde_json::from_slice::<Request>(&body) {
            Ok(request) => handler(request),
            Err(error) => Reply::Json(Response::failure(format!("malformed request: {error}"))),
        },
        Err(_) => return,
    };
    {
        let mut out = BufWriter::with_capacity(BUFFER as usize, &mut file);
        // A client that went away mid-reply is its own problem.
        let _ = reply.write_to(&mut out);
        let _ = out.flush();
    }
    unsafe {
        let _ = FlushFileBuffers(pipe);
        let _ = DisconnectNamedPipe(pipe);
    }
}

/// Client side, shared by `rewindctl`. A busy pipe (every instance mid-
/// request) is retried briefly rather than reported.
pub fn connect() -> std::io::Result<File> {
    let name = crate::paths::pipe_name();
    let started = std::time::Instant::now();
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(&name) {
            Ok(file) => return Ok(file),
            Err(error) if error.raw_os_error() == Some(231) && started.elapsed().as_secs() < 5 => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn call(request: &Request) -> std::io::Result<Reply> {
    let mut pipe = connect()?;
    protocol::write_request(&mut pipe, request)?;
    Reply::read_from(&mut pipe)
}
