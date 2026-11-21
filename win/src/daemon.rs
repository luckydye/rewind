//! The resident daemon: pipe server, recorder, tray, hotkey, overlay preload.
//! Mirrors `Daemon.swift`.

use std::sync::Arc;
use std::time::Duration;

use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::core::BOOL;

use crate::config_store;
use crate::launcher::Launcher;
use crate::recorder::Recorder;
use crate::router::{self, Context};
use crate::server;
use crate::tray;

unsafe extern "system" fn on_console_event(_: u32) -> BOOL {
    tray::post_quit();
    true.into()
}

pub fn run() -> Result<(), String> {
    let recorder = Recorder::new(config_store::load());
    let context = Arc::new(Context {
        recorder: recorder.clone(),
        launcher: Launcher,
        quit: Box::new(tray::post_quit),
    });

    // Claims the pipe first: a second daemon stops here, before it touches
    // the capture device or adds a second tray icon.
    server::start(Arc::new(move |request| router::handle(request, &context)))?;
    eprintln!("rewindd: listening on {}", crate::paths::pipe_name());

    unsafe {
        let _ = SetConsoleCtrlHandler(Some(on_console_event), true);
    }

    {
        let recorder = recorder.clone();
        std::thread::Builder::new()
            .name("supervisor".into())
            .spawn(move || {
                recorder.start();
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    recorder.supervise();
                }
            })
            .map_err(|e| e.to_string())?;
    }

    // Preload the overlay hidden so the first hotkey is as fast as the rest.
    Launcher.preload();

    tray::run(recorder.clone(), Launcher)?;
    recorder.stop();
    Ok(())
}
