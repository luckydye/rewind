//! `rewindd run | list | doctor | login-item`, for measurement and setup.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gstreamer as gst;

use crate::capture::Capture;
use crate::client;
use crate::codec;
use crate::daemon;
use crate::mp4;
use crate::paths;
use crate::protocol::{Command, RecorderConfig, Reply, Request};
use crate::ring::TICKS_PER_SECOND;

pub const USAGE: &str = "\
rewindd — background capture daemon for Rewind (Wayland)

USAGE:
  rewindd                  run the daemon (socket, recording, supervisor)
  rewindd login-item on|off
                           add or remove the XDG autostart entry
  rewindd list             which screen is being captured
  rewindd doctor           portal, PipeWire, encoders and decoders
  rewindd run [options]    capture in the foreground, for debugging

RUN OPTIONS:
  --buffer <seconds>       rolling buffer length (default 60)
  --clip <seconds>         length of the clip exported at exit (default 8)
  --export <path>          export a clip when the run ends
  --seconds <seconds>      stop after this long instead of waiting for Ctrl+C
  --height <pixels>        output height; 0 = the screen's own size (default 0)
  --fps <n>                capture frame rate (default 30)
  --bitrate <bits/s>       encoder target; 0 = 0.18 bits/pixel/frame (default 0)
  --display <index>        accepted for parity; the portal picker chooses
  --stats <seconds>        statistics interval, 0 to disable (default 5)
  --no-cursor              omit the cursor from the capture

The first start shows the desktop's screen-sharing dialog. Its answer is
remembered, so later starts go straight to recording where the portal keeps a
restore token.";

struct RunOptions {
    config: RecorderConfig,
    clip: f64,
    export: Option<PathBuf>,
    seconds: Option<f64>,
    stats: f64,
}

fn parse_run(args: &[String]) -> Result<RunOptions, String> {
    let mut options = RunOptions { config: RecorderConfig::default(), clip: 8.0, export: None, seconds: None, stats: 5.0 };
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let mut value = || -> Result<&String, String> {
            i += 1;
            args.get(i).ok_or(format!("missing value for {flag}"))
        };
        fn num<T: std::str::FromStr>(flag: &str, raw: &str) -> Result<T, String> {
            raw.parse().map_err(|_| format!("invalid value '{raw}' for {flag}"))
        }
        match flag {
            "--buffer" => options.config.buffer_seconds = num(flag, value()?)?,
            "--clip" => options.clip = num(flag, value()?)?,
            "--export" => options.export = Some(PathBuf::from(value()?)),
            "--seconds" => options.seconds = Some(num(flag, value()?)?),
            "--height" => options.config.height = num(flag, value()?)?,
            "--fps" => options.config.fps = num(flag, value()?)?,
            "--bitrate" => options.config.bitrate = num(flag, value()?)?,
            "--display" => options.config.display_index = num(flag, value()?)?,
            "--stats" => options.stats = num(flag, value()?)?,
            "--no-cursor" => options.config.shows_cursor = false,
            other => return Err(format!("unknown option '{other}'")),
        }
        i += 1;
    }
    if options.config.fps <= 0 {
        return Err("--fps must be positive".into());
    }
    Ok(options)
}

static STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

pub fn main(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        None | Some("daemon") => daemon::run(),
        Some("list") => list(),
        Some("doctor") => doctor(),
        Some("login-item") => match args.get(1).map(String::as_str) {
            Some("on") => login_item(true),
            Some("off") => login_item(false),
            _ => Err("usage: rewindd login-item on|off".into()),
        },
        Some("run") => parse_run(&args[1..]).and_then(run),
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command '{other}'\n\n{USAGE}")),
    }
}

fn list() -> Result<(), String> {
    let response = match client::call(&Request::new(Command::Status)) {
        Ok(Reply::Json(response)) if response.ok => response,
        _ => {
            println!("rewindd is not running. Start it, then choose a screen in the portal's dialog.");
            return Ok(());
        }
    };
    match response.state.and_then(|state| state.display) {
        Some(display) => println!("  [0] {}", display.name()),
        None => println!("  nothing is being captured"),
    }
    Ok(())
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|directory| directory.join(name).is_file()))
}

fn doctor() -> Result<(), String> {
    let joined = |names: Vec<&str>, none: &str| if names.is_empty() { none.to_string() } else { names.join(", ") };

    match portal_sources() {
        Ok(sources) => println!("screen cast portal: available ({sources})"),
        Err(error) => println!("screen cast portal: NOT available ({error})"),
    }
    println!("pipewiresrc: {}", if gst::ElementFactory::find("pipewiresrc").is_some() { "present" } else { "MISSING (install gstreamer1.0-pipewire)" });
    println!("H.264 encoders: {}", joined(codec::encoders(), "none"));
    println!("H.264 decoders: {}", joined(codec::decoders(), "none"));
    println!("GStreamer: {}", gst::version_string());
    println!("clipboard: {}", if on_path("wl-copy") { "wl-copy present" } else { "wl-copy MISSING (install wl-clipboard)" });
    println!("start at login: {}", if paths::autostart_file().is_file() { "on" } else { "off" });
    Ok(())
}

fn portal_sources() -> Result<String, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the portal runtime: {error}"))?;
    runtime.block_on(async {
        let screencast = ashpd::desktop::screencast::Screencast::new().await.map_err(|error| error.to_string())?;
        let sources = screencast.available_source_types().await.map_err(|error| error.to_string())?;
        Ok(format!("{sources:?}"))
    })
}

fn login_item(enabled: bool) -> Result<(), String> {
    let path = paths::autostart_file();
    if !enabled {
        std::fs::remove_file(&path).map_err(|error| format!("cannot remove {}: {error}", path.display()))?;
        println!("start at login: off");
        return Ok(());
    }

    let exe = std::env::current_exe().map_err(|error| format!("cannot locate rewindd: {error}"))?;
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Rewind\nComment=Always-on background screen recorder\nExec=\"{}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    std::fs::write(&path, entry).map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    println!("start at login: on");
    Ok(())
}

fn cpu_seconds() -> f64 {
    let mut usage = unsafe { std::mem::zeroed::<libc::rusage>() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let time = |value: libc::timeval| value.tv_sec as f64 + value.tv_usec as f64 / 1e6;
    time(usage.ru_utime) + time(usage.ru_stime)
}

fn run(options: RunOptions) -> Result<(), String> {
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }

    let capture = Capture::start(&options.config)?;
    let (width, height) = capture.info.size();
    let (source_width, source_height) = capture.info.source;
    println!(
        "capturing {} {}×{} -> {}×{} @ {} fps, {:.1} Mbit/s target, {} s buffer",
        capture.info.name(),
        source_width,
        source_height,
        width,
        height,
        capture.fps,
        capture.bitrate as f64 / 1e6,
        options.config.buffer_seconds
    );
    println!("encoder: {}", capture.info.encoder);

    let started = Instant::now();
    let mut last_stats = Instant::now();
    let mut last_cpu = cpu_seconds();
    while !STOP.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
        if options.seconds.is_some_and(|seconds| started.elapsed().as_secs_f64() >= seconds) {
            break;
        }
        if options.stats > 0.0 && last_stats.elapsed().as_secs_f64() >= options.stats {
            let wall = last_stats.elapsed().as_secs_f64();
            let cpu = cpu_seconds();
            print_stats(started, &capture, (cpu - last_cpu) / wall * 100.0);
            last_cpu = cpu;
            last_stats = Instant::now();
        }
    }

    let snapshot = capture.ring.snapshot(options.clip, 0.0, None)?;
    let fps = capture.fps;
    let cpu = cpu_seconds() / started.elapsed().as_secs_f64() * 100.0;
    print_stats(started, &capture, cpu);
    drop(capture);

    if let Some(path) = options.export {
        let written = mp4::write(
            &path,
            &mp4::Track {
                width,
                height,
                parameters: &snapshot.parameters,
                samples: &snapshot.packets,
                frame_ticks: TICKS_PER_SECOND / fps as i64,
            },
        )
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        println!(
            "exported {}: {:.2} s, {} frames ({} keyframes), {width}×{height}, {:.2} MB",
            path.display(),
            snapshot.duration(),
            snapshot.packets.len(),
            snapshot.packets.iter().filter(|packet| packet.keyframe).count(),
            written as f64 / 1e6
        );
    }
    Ok(())
}

fn print_stats(started: Instant, capture: &Capture, cpu_percent: f64) {
    let stats = capture.ring.statistics();
    let bitrate = if stats.duration > 0.0 { stats.bytes as f64 * 8.0 / stats.duration / 1e6 } else { 0.0 };
    let o = Ordering::Relaxed;
    let counters = &capture.counters;
    println!(
        "[{:6.1}s] delivered {} encoded {} errors {} | buffer {:.1}s {} frames {:.1} MB {:.2} Mbit/s | cpu {:.1}%",
        started.elapsed().as_secs_f64(),
        counters.delivered.load(o),
        counters.encoded.load(o),
        counters.errors.load(o),
        stats.duration,
        stats.frames,
        stats.bytes as f64 / 1e6,
        bitrate,
        cpu_percent
    );
}
