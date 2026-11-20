mod ipc;
mod protocol;

use std::fs;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use protocol::{Command, Request};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let Err(message) = run(&arguments) {
        eprintln!("rewind-ui: {message}");
        std::process::exit(1);
    }
}

fn run(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let client = ipc::Client::new();

    match arguments.first().map(String::as_str) {
        Some("status") => {
            let response = ipc::call(&client, Request::new(Command::Status))?;
            let state = response.state.ok_or("status returned no state")?;
            println!(
                "recording    {}\ndisplay      {}\noutput       {}×{} @ {}fps\nbuffer       {:.1}s / {}s   {:.1} MB",
                if state.recording { "yes" } else { "no" },
                state
                    .display
                    .as_ref()
                    .map(|d| d.name())
                    .unwrap_or_else(|| "none".into()),
                state.width,
                state.height,
                state.config.fps,
                state.available_seconds,
                state.buffer_seconds,
                state.buffer_bytes as f64 / 1_048_576.0,
            );
        }

        Some("preview") => {
            let ago = flag(arguments, "--ago").unwrap_or(2.0);
            let width = flag(arguments, "--width").unwrap_or(960.0) as i64;
            let out = option(arguments, "--out").unwrap_or_else(|| "/tmp/preview.jpg".into());

            let started = std::time::Instant::now();
            let mut request = Request::new(Command::PreviewFrame);
            request.seconds = Some(ago);
            request.width = Some(width);
            let response = ipc::call(&client, request)?;
            let encoded = response.previews.ok_or("no previews in response")?;
            let elapsed = started.elapsed();

            let frames: Vec<Vec<u8>> = encoded
                .iter()
                .map(|text| STANDARD.decode(text))
                .collect::<Result<_, _>>()?;

            fs::write(&out, &frames[0])?;
            println!(
                "wrote {} ({} bytes) in {:.0} ms",
                out,
                frames[0].len(),
                elapsed.as_secs_f64() * 1000.0
            );
        }

        Some("export") => {
            let seconds = flag(arguments, "--seconds").unwrap_or(30.0);
            let out = option(arguments, "--out");

            let mut request = Request::new(Command::ExportClip);
            request.seconds = Some(seconds);
            request.path = out;
            let response = ipc::call(&client, request)?;
            let clip = response.clip.ok_or("export returned no clip")?;
            println!(
                "saved {} — {:.2}s, {:.1} MB, {} frames, {}×{}",
                clip.path,
                clip.duration,
                clip.bytes as f64 / 1_048_576.0,
                clip.frames,
                clip.width,
                clip.height
            );
        }

        Some("displays") => {
            let response = ipc::call(&client, Request::new(Command::ListDisplays))?;
            for display in response.displays.unwrap_or_default() {
                println!("  [{}] {}", display.index, display.name());
            }
        }

        Some("apps") => {
            let response = ipc::call(&client, Request::new(Command::ListApplications))?;
            for application in response.applications.unwrap_or_default() {
                println!("  {} — {}", application.name, application.bundle_identifier);
            }
        }

        Some("quit") => {
            ipc::call(&client, Request::new(Command::Quit))?;
            println!("rewindd is shutting down");
        }

        Some(other) => return Err(format!("unknown command '{other}'").into()),
        None => {
            return Err("usage: rewind-ui status | preview | export | displays | apps | quit".into());
        }
    }

    Ok(())
}

fn option(arguments: &[String], name: &str) -> Option<String> {
    arguments
        .iter()
        .position(|argument| argument == name)
        .and_then(|index| arguments.get(index + 1))
        .cloned()
}

fn flag(arguments: &[String], name: &str) -> Option<f64> {
    option(arguments, name).and_then(|value| value.parse().ok())
}
