//! Request -> reply. Mirrors `Router.swift` command for command.

use std::path::PathBuf;

use crate::clipboard;
use crate::launcher::Launcher;
use crate::protocol::{ClipRange, Command, Reply, Request, Response};
use crate::recorder::{self, Recorder};

pub struct Context {
    pub recorder: std::sync::Arc<Recorder>,
    pub launcher: Launcher,
    /// Ends the daemon; posted to the tray's message loop.
    pub quit: Box<dyn Fn() + Send + Sync>,
}

pub fn handle(request: Request, cx: &Context) -> Reply {
    match route(request, cx) {
        Ok(reply) => reply,
        Err(message) => Reply::Json(Response::failure(message)),
    }
}

fn with_state(cx: &Context) -> Response {
    Response { state: Some(cx.recorder.state()), ..Response::ok() }
}

fn route(request: Request, cx: &Context) -> Result<Reply, String> {
    let recorder = &cx.recorder;
    let width = |default: i64| request.width.unwrap_or(default).max(0) as u32;

    Ok(match request.command {
        Command::Status => Reply::Json(with_state(cx)),
        Command::Start => Reply::Json(Response { state: Some(recorder.start()), ..Response::ok() }),
        Command::Stop => Reply::Json(Response { state: Some(recorder.stop()), ..Response::ok() }),

        Command::SetConfig => {
            let config = request.config.ok_or("setConfig requires a config")?;
            Reply::Json(Response { state: Some(recorder.apply(config)), ..Response::ok() })
        }

        Command::ExportClip => {
            let range = request.range.unwrap_or(ClipRange { from: request.seconds.unwrap_or(30.0), to: 0.0 });
            if range.to < 0.0 || range.duration() <= 0.0 {
                return Err("range needs from > to >= 0".into());
            }
            let clip = recorder.export(
                range.from,
                range.to,
                request.path.clone().map(PathBuf::from),
                request.export_options()?,
                request.anchor,
                request.overlay_path.as_deref(),
                request.crop.as_ref(),
            )?;
            Reply::Json(Response { clip: Some(clip), ..with_state(cx) })
        }

        Command::CopyClip => {
            let range = request.range.filter(|r| r.to >= 0.0 && r.duration() > 0.0)
                .ok_or("copyClip needs a range with from > to >= 0")?;
            let options = request.export_options()?;
            let clip = recorder.export(
                range.from,
                range.to,
                Some(clipboard::new_cached_clip_path(options.format.extension())),
                options,
                request.anchor,
                request.overlay_path.as_deref(),
                request.crop.as_ref(),
            )?;
            clipboard::copy_file(std::path::Path::new(&clip.path))?;
            Reply::Json(Response { clip: Some(clip), ..with_state(cx) })
        }

        Command::ExportFrame => {
            let clip = recorder.export_frame(
                request.seconds.unwrap_or(0.0),
                request.path.clone().map(PathBuf::from),
                request.anchor,
                request.overlay_path.as_deref(),
                request.crop.as_ref(),
            )?;
            Reply::Json(Response { clip: Some(clip), ..with_state(cx) })
        }

        Command::OpenUi => {
            cx.launcher.open();
            Reply::Json(Response::ok())
        }

        Command::ListDisplays => Reply::Json(Response { displays: Some(recorder::displays()), ..Response::ok() }),
        Command::ListApplications => Reply::Json(Response { applications: Some(recorder::applications()), ..Response::ok() }),

        Command::Thumbnails => Reply::Images(recorder.thumbnails(
            request.count.unwrap_or(24).max(1) as usize,
            width(160),
            request.anchor,
        )?),

        Command::Estimate => {
            let range = request.range.filter(|r| r.duration() > 0.0).ok_or("estimate needs a range")?;
            Reply::Json(Response {
                estimated_bytes: Some(recorder.estimate(range.from, range.to, request.anchor)?),
                ..Response::ok()
            })
        }

        Command::EdgeFrames => Reply::Images(recorder.edge_frames(
            request.range.map(|r| r.from).or(request.seconds).unwrap_or(0.0),
            request.range.map(|r| r.to).unwrap_or(0.0),
            width(480),
            request.anchor,
        )?),

        Command::PreviewFrame => Reply::Image(recorder.preview(request.seconds.unwrap_or(0.0), width(0), request.anchor)?),

        Command::CopyFrame => {
            // Native resolution regardless of what the overlay previews at.
            let image = recorder.composed_frame(
                request.seconds.unwrap_or(0.0),
                request.anchor,
                request.overlay_path.as_deref(),
                request.crop.as_ref(),
            )?;
            clipboard::copy_image(&image)?;
            Reply::Json(Response::ok())
        }

        Command::Quit => {
            (cx.quit)();
            Reply::Json(Response::ok())
        }
    })
}
