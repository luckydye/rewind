//! `REWIND_UI_TRACE=1`: timings of the preview loop on stderr — the round trip
//! to the daemon, handling the reply, and how long and how often `render`
//! runs. Costs one atomic load per call when off.

use std::sync::OnceLock;
use std::sync::Mutex;
use std::time::Instant;

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("REWIND_UI_TRACE").is_some())
}

pub fn start() -> Option<Instant> {
    enabled().then(Instant::now)
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

pub fn round_trip(started: Option<Instant>) {
    if let Some(started) = started {
        eprintln!("ui: round trip {:.1} ms", ms(started));
    }
}

pub fn received(started: Option<Instant>) {
    if let Some(started) = started {
        eprintln!("ui: received {:.1} ms", ms(started));
    }
}

pub fn render(started: Option<Instant>) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    if let Some(started) = started {
        let mut last = LAST.lock().unwrap();
        let interval = last.map(|l| started.duration_since(l).as_secs_f64() * 1000.0).unwrap_or(0.0);
        *last = Some(started);
        eprintln!("ui: render every {interval:.1} ms, drop {:.1} ms", ms(started));
    }
}
