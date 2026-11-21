//! Debugging client: every daemon feature without touching the tray. Same
//! commands as the macOS `rewindctl`.

use std::time::Instant;

use rewindd::protocol::{ClipRange, Command, Crop, Reply, Request, Response};
use rewindd::server;

const USAGE: &str = "\
rewindctl — talk to a running rewindd

USAGE:
  rewindctl status [--json]
  rewindctl start | stop | quit | open
  rewindctl export [--seconds N | --from A --to B] [--out PATH] [--overlay RGBA] [--crop x,y,w,h]
  rewindctl copy [--seconds N | --from A --to B] [--overlay RGBA] [--crop x,y,w,h]
  rewindctl copy-frame [--ago SECONDS] [--overlay RGBA] [--crop x,y,w,h]
  rewindctl config [--buffer S] [--fps N] [--height PX] [--display N] [--bitrate B] [--cursor on|off]
  rewindctl preview [--ago SECONDS] [--width PX] [--out frame.bmp]
  rewindctl thumbnails [--count N] [--width PX]
  rewindctl displays";

fn option(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn number<T: std::str::FromStr>(args: &[String], name: &str) -> Result<Option<T>, String> {
    option(args, name).map(|raw| raw.parse().map_err(|_| format!("invalid value '{raw}' for {name}"))).transpose()
}

fn call(request: Request) -> Result<Response, String> {
    match server::call(&request).map_err(|e| format!("cannot reach rewindd ({}): {e}", rewindd::paths::pipe_name()))? {
        Reply::Json(response) if response.ok => Ok(response),
        Reply::Json(response) => Err(response.error.unwrap_or_else(|| "request failed".into())),
        _ => Err("unexpected image reply".into()),
    }
}

fn range(args: &[String]) -> Result<ClipRange, String> {
    let seconds = number(args, "--seconds")?.unwrap_or(30.0);
    Ok(ClipRange { from: number(args, "--from")?.unwrap_or(seconds), to: number(args, "--to")?.unwrap_or(0.0) })
}

/// `--crop x,y,w,h` as fractions of the capture.
fn crop(args: &[String]) -> Result<Option<Crop>, String> {
    let Some(raw) = option(args, "--crop") else { return Ok(None) };
    let parts: Vec<f64> = raw.split(',').map(|p| p.trim().parse()).collect::<Result<_, _>>().map_err(|_| format!("invalid --crop '{raw}'"))?;
    match parts[..] {
        [x, y, width, height] => Ok(Some(Crop { x, y, width, height })),
        _ => Err("--crop takes x,y,w,h as fractions".into()),
    }
}

fn print_state(response: &Response) {
    let Some(state) = &response.state else { return };
    println!("recording    {}", if state.recording { "yes" } else { "no" });
    println!("display      {}", state.display.as_ref().map(|d| d.name()).unwrap_or_else(|| "none".into()));
    println!("output       {}×{} @ {} fps", state.width, state.height, state.config.fps);
    println!(
        "buffer       {:.1}s / {}s   {:.1} MB   {:.2} Mbit/s",
        state.available_seconds,
        state.buffer_seconds,
        state.buffer_bytes as f64 / 1_048_576.0,
        state.bitrate / 1e6
    );
    println!(
        "frames       captured {}  encoded {}  dropped {}  skipped {}",
        state.frames_captured, state.frames_encoded, state.dropped_frames, state.idle_notices
    );
    if let Some(error) = &state.last_error {
        println!("last error   {error}");
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("status") => {
            let response = call(Request::new(Command::Status))?;
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string_pretty(&response.state).unwrap_or_default());
            } else {
                print_state(&response);
            }
        }
        Some("start") => print_state(&call(Request::new(Command::Start))?),
        Some("stop") => print_state(&call(Request::new(Command::Stop))?),
        Some("open") => {
            call(Request::new(Command::OpenUi))?;
        }
        Some("quit") => {
            call(Request::new(Command::Quit))?;
            println!("rewindd is shutting down");
        }
        Some("copy-frame") => {
            let mut request = Request::new(Command::CopyFrame);
            request.seconds = Some(number(args, "--ago")?.unwrap_or(0.0));
            request.overlay_path = option(args, "--overlay");
            request.crop = crop(args)?;
            call(request)?;
            println!("copied the frame");
        }
        Some(command @ ("export" | "copy")) => {
            let mut request = Request::new(if command == "export" { Command::ExportClip } else { Command::CopyClip });
            request.range = Some(range(args)?);
            request.path = option(args, "--out");
            request.overlay_path = option(args, "--overlay");
            request.crop = crop(args)?;
            let started = Instant::now();
            let clip = call(request)?.clip.ok_or("no clip in the reply")?;
            println!(
                "{} {} — {:.2}s, {:.1} MB, {} frames, {}×{} ({:.0} ms)",
                if command == "export" { "saved" } else { "copied" },
                clip.path,
                clip.duration,
                clip.bytes as f64 / 1_048_576.0,
                clip.frames,
                clip.width,
                clip.height,
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
        Some("config") => {
            let mut config = call(Request::new(Command::Status))?.state.ok_or("no state")?.config;
            if let Some(v) = number(args, "--buffer")? { config.buffer_seconds = v }
            if let Some(v) = number(args, "--fps")? { config.fps = v }
            if let Some(v) = number(args, "--height")? { config.height = v }
            if let Some(v) = number(args, "--bitrate")? { config.bitrate = v }
            if let Some(v) = number(args, "--display")? { config.display_index = v }
            if let Some(v) = option(args, "--cursor") { config.shows_cursor = v != "off" }
            let mut request = Request::new(Command::SetConfig);
            request.config = Some(config);
            print_state(&call(request)?);
        }
        Some("preview") => {
            let mut request = Request::new(Command::PreviewFrame);
            request.seconds = Some(number(args, "--ago")?.unwrap_or(0.0));
            request.width = Some(number(args, "--width")?.unwrap_or(0));
            let started = Instant::now();
            let reply = server::call(&request).map_err(|e| e.to_string())?;
            let elapsed = started.elapsed();
            let image = match reply {
                Reply::Image(image) => image,
                Reply::Json(response) => return Err(response.error.unwrap_or_else(|| "no image".into())),
                Reply::Images(_) => return Err("unexpected reply".into()),
            };
            println!("{}×{} BGRA, {:.0} ms round trip", image.width, image.height, elapsed.as_secs_f64() * 1000.0);
            if let Some(out) = option(args, "--out") {
                std::fs::write(&out, bmp(&image)).map_err(|e| e.to_string())?;
                println!("wrote {out}");
            }
        }
        Some("thumbnails") => {
            let mut request = Request::new(Command::Thumbnails);
            request.count = Some(number(args, "--count")?.unwrap_or(24));
            request.width = Some(number(args, "--width")?.unwrap_or(160));
            let started = Instant::now();
            match server::call(&request).map_err(|e| e.to_string())? {
                Reply::Images(images) => println!(
                    "{} thumbnails of {}×{}, {:.0} ms",
                    images.len(),
                    images.first().map(|i| i.width).unwrap_or(0),
                    images.first().map(|i| i.height).unwrap_or(0),
                    started.elapsed().as_secs_f64() * 1000.0
                ),
                Reply::Json(response) => return Err(response.error.unwrap_or_default()),
                Reply::Image(_) => return Err("unexpected reply".into()),
            }
        }
        Some("displays") => {
            for display in call(Request::new(Command::ListDisplays))?.displays.unwrap_or_default() {
                println!("  [{}] {}{}", display.index, display.name(), if display.is_main { " (main)" } else { "" });
            }
        }
        Some("help" | "--help" | "-h") | None => println!("{USAGE}"),
        Some(other) => return Err(format!("unknown command '{other}'\n\n{USAGE}")),
    }
    Ok(())
}

/// A 32-bit bottom-up BMP: viewable anywhere, no encoder dependency.
fn bmp(image: &rewindd::protocol::RawImage) -> Vec<u8> {
    let (w, h) = (image.width as usize, image.height as usize);
    let size = 54 + w * h * 4;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for row in image.pixels.chunks_exact(w * 4).rev() {
        out.extend_from_slice(row);
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(message) = run(&args) {
        eprintln!("rewindctl: {message}");
        std::process::exit(1);
    }
}
