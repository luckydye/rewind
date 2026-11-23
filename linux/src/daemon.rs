//! The resident daemon: socket server, recorder, supervisor. No tray and no
//! global hotkey — Wayland has neither in a form a background process can rely
//! on, so the desktop's own key binding calls `rewindctl` instead.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::config_store;
use crate::paths;
use crate::recorder::Recorder;
use crate::router::{self, Context};
use crate::server;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn on_signal(_: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
}

pub fn run() -> Result<(), String> {
    let recorder = Recorder::new(config_store::load());
    let stop = Arc::new(AtomicBool::new(false));
    let context = Arc::new(Context {
        recorder: recorder.clone(),
        quit: Box::new({
            let stop = stop.clone();
            move || stop.store(true, Ordering::SeqCst)
        }),
    });

    install_signal_handlers();

    {
        let recorder = recorder.clone();
        std::thread::Builder::new()
            .name("supervisor".into())
            .spawn(move || {
                recorder.start();
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    recorder.supervise();
                    if SHUTDOWN.load(Ordering::SeqCst) {
                        return;
                    }
                }
            })
            .map_err(|error| error.to_string())?;
    }

    eprintln!("rewindd: listening on {}", paths::socket_path().display());
    let handler: server::Handler = {
        let context = context.clone();
        Arc::new(move |request| router::handle(request, &context))
    };
    server::serve(handler, || stop.load(Ordering::SeqCst) || SHUTDOWN.load(Ordering::SeqCst))?;

    recorder.stop();
    Ok(())
}

/// Spawns the overlay. This cut builds the daemon and the CLI only, so the
/// binary may not exist yet; that is reported rather than hidden.
pub fn open_ui() -> Result<(), String> {
    let program = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("rewind-ui")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from("rewind-ui"));

    std::process::Command::new(&program)
        .arg("--show")
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot start {}: {error}", program.display()))
}
