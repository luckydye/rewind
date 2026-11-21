//! `rewindd run | list | doctor | login-item`, for measurement and setup.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Graphics::Capture::GraphicsCaptureSession;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Media::MediaFoundation::{MFVideoFormat_H264, MFVideoFormat_HEVC};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::core::BOOL;

use crate::d3d;
use crate::encoder;
use crate::login;
use crate::mp4;
use crate::pipeline::Pipeline;
use crate::protocol::RecorderConfig;
use crate::ring::TICKS_PER_SECOND;

pub const USAGE: &str = "\
rewindd — background capture daemon for Rewind (Windows)

USAGE:
  rewindd                  run the daemon (tray icon + named pipe)
  rewindd login-item on|off
                           register or remove Start at Login
  rewindd list             list displays
  rewindd doctor           capture support, hardware encoders, decoders
  rewindd run [options]    capture in the foreground, for debugging

RUN OPTIONS:
  --buffer <seconds>       rolling buffer length (default 60)
  --clip <seconds>         length of the clip exported at exit (default 8)
  --export <path>          export a clip when the run ends
  --seconds <seconds>      stop after this long instead of waiting for Ctrl+C
  --height <pixels>        output height; 0 = native display resolution (default 0)
  --fps <n>                capture frame rate (default 30)
  --bitrate <bits/s>       encoder target; 0 = 0.18 bits/pixel/frame (default 0)
  --display <index>        display index from 'rewindd list' (default 0)
  --stats <seconds>        statistics interval, 0 to disable (default 5)
  --no-cursor              omit the cursor from the capture";

struct RunOptions {
    config: RecorderConfig,
    clip: f64,
    export: Option<PathBuf>,
    seconds: Option<f64>,
    stats: f64,
}

fn parse_run(args: &[String]) -> Result<RunOptions, String> {
    let mut o = RunOptions { config: RecorderConfig::default(), clip: 8.0, export: None, seconds: None, stats: 5.0 };
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
            "--buffer" => o.config.buffer_seconds = num(flag, value()?)?,
            "--clip" => o.clip = num(flag, value()?)?,
            "--export" => o.export = Some(PathBuf::from(value()?)),
            "--seconds" => o.seconds = Some(num(flag, value()?)?),
            "--height" => o.config.height = num(flag, value()?)?,
            "--fps" => o.config.fps = num(flag, value()?)?,
            "--bitrate" => o.config.bitrate = num(flag, value()?)?,
            "--display" => o.config.display_index = num(flag, value()?)?,
            "--stats" => o.stats = num(flag, value()?)?,
            "--no-cursor" => o.config.shows_cursor = false,
            other => return Err(format!("unknown option '{other}'")),
        }
        i += 1;
    }
    if o.config.fps <= 0 {
        return Err("--fps must be positive".into());
    }
    Ok(o)
}

static STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn on_console_event(_: u32) -> BOOL {
    STOP.store(true, Ordering::SeqCst);
    true.into()
}

pub fn main(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        None | Some("daemon") => crate::daemon::run(),
        Some("list") => {
            list();
            Ok(())
        }
        Some("doctor") => {
            doctor();
            Ok(())
        }
        Some("login-item") => match args.get(1).map(String::as_str) {
            Some("on") => login::set(true).map(|()| println!("start at login: on")),
            Some("off") => login::set(false).map(|()| println!("start at login: off")),
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

fn list() {
    for (index, m) in d3d::monitors().iter().enumerate() {
        let r = m.rect;
        println!(
            "  [{index}] {}  {}x{} at ({}, {}){}",
            m.name,
            r.right - r.left,
            r.bottom - r.top,
            r.left,
            r.top,
            if m.primary { "  (main)" } else { "" }
        );
    }
}

fn doctor() {
    let joined = |names: Vec<String>, none: &str| if names.is_empty() { none.to_string() } else { names.join(", ") };
    let supported = GraphicsCaptureSession::IsSupported().unwrap_or(false);
    println!("Windows.Graphics.Capture: {}", if supported { "supported" } else { "NOT supported" });
    println!("displays: {}", d3d::monitors().len());
    println!("hardware H.264 encoders: {}", joined(encoder::hardware_encoders(MFVideoFormat_H264), "none"));
    println!("hardware HEVC encoders: {}", joined(encoder::hardware_encoders(MFVideoFormat_HEVC), "none"));
    println!("H.264 decoders: {}", joined(encoder::decoders(MFVideoFormat_H264), "none"));
    println!(
        "HEVC decoders: {}",
        joined(encoder::decoders(MFVideoFormat_HEVC), "none (needs the HEVC Video Extensions from the Store)")
    );
    println!("start at login: {}", if login::is_enabled() { "on" } else { "off" });
}

fn cpu_seconds() -> f64 {
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user);
    }
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    (ticks(kernel) + ticks(user)) as f64 / TICKS_PER_SECOND as f64
}

fn run(o: RunOptions) -> Result<(), String> {
    let monitors = d3d::monitors();
    let monitor = monitors
        .get(o.config.display_index.max(0) as usize)
        .ok_or(format!("no display {}; see 'rewindd list'", o.config.display_index))?;
    let pipeline = Pipeline::start(&o.config, monitor)?;

    println!(
        "capturing {} {}x{} -> {}x{} @ {} fps, {:.1} Mbit/s target, {} s buffer",
        pipeline.monitor_name,
        pipeline.source.0,
        pipeline.source.1,
        pipeline.width,
        pipeline.height,
        pipeline.fps,
        pipeline.bitrate as f64 / 1e6,
        o.config.buffer_seconds
    );
    println!(
        "encoder: {}; border: {}; OS frame-rate limit: {}",
        pipeline.encoder_name,
        if pipeline.border { "shown" } else { "hidden" },
        if pipeline.rate_limited_by_os { "yes" } else { "no (frames dropped in-process)" }
    );
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(on_console_event), true);
    }

    let started = Instant::now();
    let mut last_stats = Instant::now();
    let mut last_cpu = cpu_seconds();
    while !STOP.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
        if o.seconds.is_some_and(|s| started.elapsed().as_secs_f64() >= s) {
            break;
        }
        if o.stats > 0.0 && last_stats.elapsed().as_secs_f64() >= o.stats {
            let wall = last_stats.elapsed().as_secs_f64();
            let cpu = cpu_seconds();
            print_stats(started, &pipeline, (cpu - last_cpu) / wall * 100.0);
            last_cpu = cpu;
            last_stats = Instant::now();
        }
    }

    let ring = pipeline.ring.clone();
    let (width, height, fps) = (pipeline.width, pipeline.height, pipeline.fps);
    let cpu = cpu_seconds() / started.elapsed().as_secs_f64() * 100.0;
    print_stats(started, &pipeline, cpu);
    drop(pipeline);

    if let Some(path) = o.export {
        let snapshot = ring.snapshot(o.clip, 0.0, None)?;
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
            "exported {}: {:.2} s, {} frames ({} keyframes), {}x{height}, {:.2} MB",
            path.display(),
            snapshot.duration(),
            snapshot.packets.len(),
            snapshot.packets.iter().filter(|p| p.keyframe).count(),
            width,
            written as f64 / 1e6
        );
    }
    Ok(())
}

fn print_stats(started: Instant, pipeline: &Pipeline, cpu_percent: f64) {
    let s = pipeline.ring.statistics();
    let bitrate = if s.duration > 0.0 { s.bytes as f64 * 8.0 / s.duration / 1e6 } else { 0.0 };
    let o = Ordering::Relaxed;
    let (c, e) = (&pipeline.capture_counters, &pipeline.encoder_counters);
    println!(
        "[{:6.1}s] delivered {} captured {} encoded {} skipped {} dropped {} errors {} | buffer {:.1}s {} frames {:.1} MB {:.2} Mbit/s | cpu {:.1}%",
        started.elapsed().as_secs_f64(),
        c.delivered.load(o),
        c.captured.load(o),
        e.encoded.load(o),
        c.skipped.load(o),
        c.dropped.load(o),
        c.errors.load(o) + e.errors.load(o),
        s.duration,
        s.frames,
        s.bytes as f64 / 1e6,
        bitrate,
        cpu_percent
    );
}
