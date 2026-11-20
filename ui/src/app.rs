//! The rewind overlay: a dimmed desktop, the frame under the playhead, and one
//! floating transport bar.
//!
//! Deliberately only three things. The capture source, the recent clips and the
//! export destination all live in the menu bar item; repeating them here would
//! be decoration.
//!
//! Laid out from the viewport size rather than with nested flex containers,
//! because the timeline has to turn a window-space mouse position into a
//! timestamp and that only stays correct if the geometry is explicit.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::*;

use crate::ipc::{self, Client, RawImage, Reply};
use crate::protocol::{ClipRange, Command, Request};

// MARK: - Geometry

const MARGIN: f32 = 12.0;
const GAP: f32 = 8.0;
const BAR_HEIGHT: f32 = 102.0;
const BAR_PADDING: f32 = 6.0;

/// Past this the timeline is too long to scrub precisely or take in at a
/// glance, so on wide screens the bar is centred instead of spanning the edge.
const MAX_BAR_WIDTH: f32 = 1100.0;

/// Room for half a tick label beyond each end of the track, so "-0s" is not
/// clipped by the bar's right edge.
const RULER_INSET: f32 = 22.0;
const LABEL_WIDTH: f32 = 38.0;

/// Toolbar row: grouped controls in recessed pills.
const TOOLBAR_Y: f32 = 6.0;
const TOOLBAR_H: f32 = 30.0;

/// Ruler: bare numbers on the panel, with the playhead badge riding on it.
const RULER_Y: f32 = 38.0;
const RULER_H: f32 = 22.0;

/// Content row: the recessed strip the range block lives in.
const ROW_Y: f32 = 62.0;
const ROW_H: f32 = 34.0;
const BLOCK_INSET: f32 = 4.0;

const HANDLE_REACH: f32 = 10.0;

/// Trackpad travel per second of footage. 120 pt is about 4 pt per frame at
/// 30 fps: a deliberate swipe moves a few seconds, a small one steps frames.
/// Independent of the timeline's width, so a long buffer does not make it
/// twitchier.
const SCRUB_POINTS_PER_SECOND: f64 = 120.0;
const BADGE_WIDTH: f32 = 66.0;

/// Entrance: the preview starts full-bleed and shrinks into place, then the bar
/// slides up from below the screen edge. ~300 ms in total.
const MORPH_MS: u64 = 260;
const MORPH_STEPS: u64 = 18;

/// Capture is native-resolution (33 MB per frame on a 4K display). A 2K preview
/// costs ~11 MB and about 40 ms round trip, which keeps scrubbing usable; the
/// exported clip is full resolution either way.
const PREVIEW_WIDTH: i64 = 2048;

/// The timeline strip: one thumbnail per slot, decoded from keyframes.
const THUMBNAIL_COUNT: i64 = 16;
const THUMBNAIL_WIDTH: i64 = 160;
const EDGE_FRAME_WIDTH: i64 = 640;



// MARK: - Palette

const SCRIM: u32 = 0x08080CDB;
const PANEL: u32 = 0x1B1B1EF5;
const SUNKEN: u32 = 0x0D0D0FFF;
const BORDER: u32 = 0xFFFFFF12;
const TEXT: u32 = 0xE9E9EDFF;
const MUTED: u32 = 0x8A8A93FF;
const ACCENT: u32 = 0x2F7CF6FF;
const ACCENT_FILL: u32 = 0x2F7CF659;
const PLAYHEAD: u32 = 0xE5484DFF;
const GROUP: u32 = 0xFFFFFF0A;
const DANGER: u32 = 0xFF5A4DFF;
const PENDING: u32 = 0xF0A030FF;

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Playhead,
    From,
    To,
    /// Moving the whole selection. The range is moved by the cursor's offset
    /// from where the drag started, applied to the range as it was then, so
    /// its length cannot drift while dragging.
    Range { anchor: f64, from: f64, to: f64 },
}

enum Status {
    Info(String),
    Error(String),
}

pub struct Rewind {
    focus: FocusHandle,
    client: Client,
    span: f64,
    /// The moment "now" refers to, pinned when the overlay opens. Capture keeps
    /// running behind the overlay; measuring from the live end of the buffer
    /// made playback run at double speed and the paused preview drift.
    anchor: Option<f64>,
    /// Capture frame rate, so frame stepping lands on real frames.
    fps: f64,
    /// Entrance progress: 0 is a full-bleed preview with the bar off-screen,
    /// 1 is the resting layout.
    progress: f32,
    /// Top inset the system keeps clear (menu bar, camera housing), so the
    /// preview is never covered by the notch.
    safe_top: f32,
    /// First and last frame of the range, shown side by side while it is being
    /// dragged.
    edge_frames: Option<(Arc<RenderImage>, Arc<RenderImage>)>,
    /// One edge request in flight at a time; the latest range is remembered so
    /// a fast drag collapses into a single follow-up.
    edge_in_flight: bool,
    edge_pending: Option<(f64, f64)>,
    /// Timeline strip, one per slot, oldest first.
    thumbnails: Vec<Arc<RenderImage>>,
    /// Aspect of a thumbnail, used to cover-crop it into its slot.
    thumbnail_aspect: f32,
    /// Screen rect (AppKit coordinates) of the display being captured.
    screen: Option<(f64, f64, f64, f64)>,
    playhead: f64,
    sel_from: f64,
    sel_to: f64,
    playing: bool,
    frame: Option<Arc<RenderImage>>,
    frame_size: (u32, u32),
    requested: Option<f64>,
    in_flight: bool,
    drag: Option<Drag>,
    exporting: bool,
    status: Option<Status>,
    ticker: Option<Task<()>>,
    /// Wall clock of the previous playback tick. Advance is derived from the
    /// time actually elapsed rather than from a tick count, so a late or early
    /// timer cannot speed playback up or slow it down.
    playback_clock: Option<Instant>,
    last_clip: Option<String>,
    /// A close is in flight: the frame is being copied before the overlay hides.
    closing: bool,
    /// The last copy failed; the next close hides without trying again, so a
    /// dead daemon cannot trap the user in the overlay.
    copy_failed: bool,
}

impl Rewind {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            client: Client::new(),
            span: 1.0,
            anchor: None,
            fps: 30.0,
            progress: 1.0,
            safe_top: 0.0,
            edge_frames: None,
            edge_in_flight: false,
            edge_pending: None,
            thumbnails: Vec::new(),
            thumbnail_aspect: 16.0 / 9.0,
            screen: None,
            playhead: 0.0,
            sel_from: 0.0,
            sel_to: 0.0,
            playing: false,
            frame: None,
            frame_size: (16, 9),
            requested: None,
            in_flight: false,
            drag: None,
            exporting: false,
            status: None,
            ticker: None,
            playback_clock: None,
            last_clip: None,
            closing: false,
            copy_failed: false,
        }
    }

    /// Every show starts from the daemon's current buffer: a fresh "now"
    /// anchor, a fresh default range, and the frame under the playhead.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let response = ipc::call(&self.client, Request::new(Command::Status));
        let state = response
            .as_ref()
            .ok()
            .and_then(|response| response.state.as_ref());

        self.span = state
            .map(|state| state.available_seconds.max(0.5))
            .unwrap_or(1.0);
        self.anchor = state.and_then(|state| state.newest_timestamp);
        self.fps = state
            .map(|state| state.config.fps as f64)
            .unwrap_or(30.0)
            .max(1.0);
        self.safe_top = state
            .and_then(|state| state.display.as_ref())
            .map(|display| display.safe_area_top as f32)
            .unwrap_or(0.0);
        self.screen = state.and_then(|state| state.display.as_ref()).and_then(|display| {
            (display.screen_width > 0.0).then_some((
                display.screen_x,
                display.screen_y,
                display.screen_width,
                display.screen_height,
            ))
        });
        self.sel_from = self.span.min(10.0);
        self.sel_to = 0.0;
        self.playhead = self.sel_from;
        self.status = response
            .as_ref()
            .err()
            .map(|error| Status::Error(error.to_string()));
        self.frame = None;
        self.edge_frames = None;
        self.thumbnails = Vec::new();
        self.drag = None;
        self.closing = false;
        self.copy_failed = false;
        self.request_frame(cx);
        self.request_edge_frames(cx);
        self.fetch_thumbnails(cx);
        cx.notify();
    }

    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh(cx);
        // Open on the display actually being captured, not always the primary.
        if let Some((x, y, width, height)) = self.screen {
            crate::activate::place(x, y, width, height);
        }
        window.focus(&self.focus);
        // Fade at the window-server level so it costs no frames here: the
        // overlay is transparent, ordered front, then animated to opaque.
        crate::activate::prepare_fade();
        crate::activate::activate();
        crate::activate::start_fade();
        // Activation races window ordering and is sometimes dropped; asserting
        // it once more shortly after makes the overlay reliably take the keys.
        cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            crate::activate::activate();
        })
        .detach();

        // Full bleed until the first frame arrives; `received` starts the morph
        // so it never animates a placeholder.
        self.progress = 0.0;
    }

    /// First and last frame of the range. Coalesced like the main preview: one
    /// request in flight, and the latest range wins.
    fn request_edge_frames(&mut self, cx: &mut Context<Self>) {
        let range = (self.sel_from, self.sel_to);
        if self.edge_in_flight {
            self.edge_pending = Some(range);
            return;
        }

        self.edge_in_flight = true;
        let anchor = self.anchor;
        let width = EDGE_FRAME_WIDTH;
        let client = self.client.clone();

        cx.spawn(async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move {
                    let mut request = Request::new(Command::EdgeFrames);
                    request.range = Some(ClipRange {
                        from: range.0,
                        to: range.1,
                    });
                    request.width = Some(width);
                    request.anchor = anchor;
                    client.send(&request)
                })
                .await;

            this.update(cx, |view, cx| {
                view.edge_in_flight = false;
                if let Ok(Reply::Images(images)) = reply {
                    if let (Some(first), Some(last)) = (images.first(), images.last()) {
                        if let (Ok(first), Ok(last)) =
                            (build_image(first.clone()), build_image(last.clone()))
                        {
                            view.edge_frames = Some((first, last));
                        }
                    }
                }
                if let Some(pending) = view.edge_pending.take() {
                    view.sel_from = pending.0;
                    view.sel_to = pending.1;
                    view.request_edge_frames(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The strip is fetched once per show, in one request, from keyframes.
    fn fetch_thumbnails(&mut self, cx: &mut Context<Self>) {
        let anchor = self.anchor;
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move {
                    let mut request = Request::new(Command::Thumbnails);
                    request.count = Some(THUMBNAIL_COUNT);
                    request.width = Some(THUMBNAIL_WIDTH);
                    request.anchor = anchor;
                    client.send(&request)
                })
                .await;

            this.update(cx, |view, cx| {
                if let Ok(Reply::Images(images)) = reply {
                    if let Some(first) = images.first() {
                        view.thumbnail_aspect = first.width as f32 / first.height.max(1) as f32;
                    }
                    view.thumbnails = images
                        .into_iter()
                        .filter_map(|image| build_image(image).ok())
                        .collect();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Bounded step task that drives the entrance geometry: eighteen frames,
    /// then it stops. GPUI's `with_animation` never advanced here.
    fn animate_in(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            for step in 1..=MORPH_STEPS {
                cx.background_executor()
                    .timer(Duration::from_millis(MORPH_MS / MORPH_STEPS))
                    .await;
                let finished = this
                    .update(cx, |view, cx| {
                        view.progress = step as f32 / MORPH_STEPS as f32;
                        cx.notify();
                        step == MORPH_STEPS
                    })
                    .unwrap_or(true);
                if finished {
                    break;
                }
            }
        })
        .detach();
    }

    /// The overlay stays resident between uses so the hotkey is instant; this
    /// is its "close". The frame is dropped so a hidden overlay holds no
    /// 11 MB image.
    fn hide(&mut self, cx: &mut Context<Self>) {
        self.stop_playback();
        self.frame = None;
        self.drag = None;
        self.closing = false;
        crate::activate::hide();
        cx.notify();
    }

    // MARK: - Timing

    fn time_at(&self, x: f32, left: f32, width: f32) -> f64 {
        let fraction = ((x - left) / width).clamp(0.0, 1.0) as f64;
        ((1.0 - fraction) * self.span).clamp(0.0, self.span)
    }

    fn x_of(&self, seconds_ago: f64, left: f32, width: f32) -> f32 {
        let fraction = 1.0 - (seconds_ago / self.span).clamp(0.0, 1.0);
        left + width * fraction as f32
    }

    fn selection_duration(&self) -> f64 {
        (self.sel_from - self.sel_to).max(0.0)
    }


    /// The playhead seeks anywhere in the buffer; the range is what is
    /// exported. Clamping the playhead into the range made clicking outside it
    /// do nothing at all.
    fn set_playhead(&mut self, seconds: f64, cx: &mut Context<Self>) {
        self.playhead = seconds.clamp(0.0, self.span);
        self.request_frame(cx);
        cx.notify();
    }

    fn nudge(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.stop_playback();
        // Left is earlier on the timeline, i.e. further back in time.
        let next = self.playhead + delta;
        self.set_playhead(next, cx);
    }

    fn jump_to_start(&mut self, cx: &mut Context<Self>) {
        self.stop_playback();
        self.set_playhead(self.sel_from, cx);
    }

    fn jump_to_end(&mut self, cx: &mut Context<Self>) {
        self.stop_playback();
        self.set_playhead(self.sel_to, cx);
    }

    fn reset_range(&mut self, cx: &mut Context<Self>) {
        self.sel_from = self.span;
        self.sel_to = 0.0;
        self.playhead = self.span;
        self.request_frame(cx);
        cx.notify();
    }

    // MARK: - Playback

    fn stop_playback(&mut self) {
        self.playing = false;
        self.ticker = None;
        self.playback_clock = None;
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if self.playing {
            self.stop_playback();
            cx.notify();
            return;
        }

        // At the very end there is nothing left to play: start the range over.
        if self.playhead <= 0.001 {
            self.playhead = self.sel_from;
        }
        self.playing = true;
        self.playback_clock = Some(Instant::now());
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;

                let keep_going = this
                    .update(cx, |view, cx| {
                        if !view.playing {
                            return false;
                        }

                        let now = Instant::now();
                        let elapsed = view
                            .playback_clock
                            .map(|clock| now.duration_since(clock).as_secs_f64())
                            .unwrap_or(0.0);
                        view.playback_clock = Some(now);

                        let before = view.playhead;
                        view.playhead -= elapsed;

                        // Loop only when the playhead crosses the range end.
                        // Starting after the range plays on towards now
                        // instead of snapping back; starting before it plays
                        // into the range and then loops.
                        if before >= view.sel_to && view.playhead < view.sel_to {
                            // Carry the overshoot so looping does not drift.
                            let overshoot = view.sel_to - view.playhead;
                            let duration = view.selection_duration().max(0.01);
                            view.playhead = view.sel_from - (overshoot % duration);
                        } else if view.playhead <= 0.0 {
                            view.playhead = 0.0;
                            view.stop_playback();
                            view.request_frame(cx);
                            cx.notify();
                            return false;
                        }

                        view.request_frame(cx);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);

                if !keep_going {
                    break;
                }
            }
        }));
        cx.notify();
    }

    // MARK: - Frames

    fn request_frame(&mut self, cx: &mut Context<Self>) {
        if self.in_flight {
            // The completion handler re-requests if the playhead moved on.
            return;
        }

        self.in_flight = true;
        self.requested = Some(self.playhead);
        let seconds = self.playhead;
        let anchor = self.anchor;
        let client = self.client.clone();

        cx.spawn(async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move {
                    let mut request = Request::new(Command::PreviewFrame);
                    request.seconds = Some(seconds);
                    request.anchor = anchor;
                    request.width = Some(PREVIEW_WIDTH);
                    client.send(&request)
                })
                .await;

            this.update(cx, |view, cx| view.received(reply, cx)).ok();
        })
        .detach();
    }

    fn received(&mut self, reply: Result<Reply, ipc::Error>, cx: &mut Context<Self>) {
        self.in_flight = false;

        match reply {
            Ok(Reply::Image(image)) => {
                self.frame_size = (image.width, image.height);
                match build_image(image) {
                    Ok(image) => {
                        self.frame = Some(image);
                        if matches!(self.status, Some(Status::Error(_))) {
                            self.status = None;
                        }
                        // The entrance animates real content, so it starts when
                        // the first frame lands rather than on the placeholder.
                        if self.progress == 0.0 {
                            self.animate_in(cx);
                        }
                        // The strip is a full second of decoding on the daemon's
                        // actor; asking for it later keeps it from delaying the
                        // first frame.
                        if self.thumbnails.is_empty() {
                            self.fetch_thumbnails(cx);
                        }
                        self.request_edge_frames(cx);
                    }
                    Err(message) => self.status = Some(Status::Error(message)),
                }
            }
            Ok(Reply::Images(_)) => {
                self.status = Some(Status::Error("expected a frame".into()));
            }
            Ok(Reply::Json(response)) => {
                self.status = Some(Status::Error(
                    response
                        .error
                        .unwrap_or_else(|| "the daemon sent no frame".into()),
                ));
            }
            Err(error) => {
                // Nothing to animate into; settle into the resting layout.
                self.progress = 1.0;
                self.status = Some(Status::Error(error.to_string()));
            }
        }

        let stale = self
            .requested
            .map(|requested| (requested - self.playhead).abs() > 0.0005)
            .unwrap_or(true);
        if stale {
            self.request_frame(cx);
        }
        cx.notify();
    }

    // MARK: - Export

    fn export(&mut self, cx: &mut Context<Self>) {
        self.render_clip(false, cx);
    }

    fn copy_clip(&mut self, cx: &mut Context<Self>) {
        self.render_clip(true, cx);
    }

    /// Export to ~/Movies/Rewind, or with `copy` render to the cache and put
    /// the file on the clipboard. Both are the same render on the daemon side.
    fn render_clip(&mut self, copy: bool, cx: &mut Context<Self>) {
        if self.exporting {
            return;
        }
        if self.selection_duration() < 0.05 {
            self.status = Some(Status::Error("select at least a moment".into()));
            cx.notify();
            return;
        }

        self.exporting = true;
        self.stop_playback();
        self.status = Some(Status::Info("rendering…".into()));

        let range = ClipRange {
            from: self.sel_from,
            to: self.sel_to,
        };
        let anchor = self.anchor;
        let client = self.client.clone();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut request = Request::new(if copy {
                        Command::CopyClip
                    } else {
                        Command::ExportClip
                    });
                    request.range = Some(range);
                    request.anchor = anchor;
                    ipc::call(&client, request)
                })
                .await;

            this.update(cx, |view, cx| {
                view.exporting = false;
                view.status = Some(match result {
                    Ok(response) => match response.clip {
                        Some(clip) if copy => Status::Info(format!(
                            "Copied {:.1}s clip · {:.1} MB",
                            clip.duration,
                            clip.bytes as f64 / 1_048_576.0
                        )),
                        Some(clip) => {
                            view.last_clip = Some(clip.path);
                            Status::Info(format!(
                                "Saved {:.1}s · {:.1} MB",
                                clip.duration,
                                clip.bytes as f64 / 1_048_576.0
                            ))
                        }
                        None => Status::Error("no clip was returned".into()),
                    },
                    Err(error) => Status::Error(error.to_string()),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();

        cx.notify();
    }

    /// Two-finger horizontal scrubbing. Inverted, like scrolling a page: swiping
    /// right pulls the timeline back in time. Mostly vertical swipes are
    /// ignored so resting fingers or a diagonal flick do not seek.
    fn on_scroll(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let seconds = match event.delta {
            ScrollDelta::Pixels(delta) => {
                let x = f32::from(delta.x) as f64;
                let y = f32::from(delta.y) as f64;
                if x.abs() <= y.abs() {
                    return;
                }
                x / SCRUB_POINTS_PER_SECOND
            }
            // A notched mouse wheel: one notch is one frame, either axis.
            ScrollDelta::Lines(delta) => {
                let lines = if delta.x != 0.0 { delta.x } else { delta.y };
                lines as f64 / self.fps
            }
        };
        if seconds == 0.0 {
            return;
        }

        self.stop_playback();
        // Inverted: the timeline moves with the fingers, like scrolling a page.
        let next = self.playhead + seconds;
        self.set_playhead(next, cx);
        cx.stop_propagation();
    }

    // MARK: - Close

    /// Closing copies the frame under the playhead to the clipboard at native
    /// resolution, then exits once the daemon confirms.
    fn close(&mut self, cx: &mut Context<Self>) {
        if self.copy_failed {
            self.hide(cx);
            return;
        }
        if self.closing {
            return;
        }

        self.closing = true;
        self.stop_playback();
        self.status = Some(Status::Info("copying frame…".into()));
        let seconds = self.playhead;
        let anchor = self.anchor;
        let client = self.client.clone();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut request = Request::new(Command::CopyFrame);
                    request.seconds = Some(seconds);
                    request.anchor = anchor;
                    ipc::call(&client, request)
                })
                .await;

            this.update(cx, |view, cx| match result {
                Ok(_) => view.hide(cx),
                Err(error) => {
                    view.closing = false;
                    view.copy_failed = true;
                    view.status = Some(Status::Error(format!(
                        "copy failed: {error} — close again to dismiss"
                    )));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // MARK: - Input

    /// Editing-tool conventions. Times are seconds *ago*, so moving left on
    /// the timeline is a positive delta.
    ///
    /// | key            | action                                 |
    /// |----------------|----------------------------------------|
    /// | Space          | play / pause                           |
    /// | ← / →          | one frame back / forward               |
    /// | ⇧← / ⇧→        | one second back / forward              |
    /// | ⌥← / ⌥→        | move the whole range by one second     |
    /// | ↑ / ↓          | jump to range start / end              |
    /// | Home / End     | jump to the oldest frame / now         |
    /// | I / O          | set range start / end at the playhead  |
    /// | A              | select the whole buffer                |
    /// | Return         | export                                 |
    /// | ⌘C             | copy the range as a video file         |
    /// | Esc            | close                                  |
    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        let frame = 1.0 / self.fps;
        let step = if modifiers.shift { 1.0 } else { frame };

        match event.keystroke.key.as_str() {
            "escape" => self.close(cx),
            "space" => self.toggle_play(cx),
            "enter" => self.export(cx),
            "c" if modifiers.platform => self.copy_clip(cx),
            "left" if modifiers.alt => self.shift_range(1.0, cx),
            "right" if modifiers.alt => self.shift_range(-1.0, cx),
            "left" => self.nudge(step, cx),
            "right" => self.nudge(-step, cx),
            "up" => self.jump_to_start(cx),
            "down" => self.jump_to_end(cx),
            "home" => {
                self.stop_playback();
                self.set_playhead(self.span, cx);
            }
            "end" => {
                self.stop_playback();
                self.set_playhead(0.0, cx);
            }
            "i" => self.mark_start(cx),
            "o" => self.mark_end(cx),
            "a" => self.reset_range(cx),
            _ => return,
        }
        // Reporting the key as handled is what stops AppKit offering it again:
        // unhandled, one press arrived as a key equivalent and a key down (and
        // sometimes more), so every arrow stepped two or three frames.
        cx.stop_propagation();
    }

    /// Marking past the other end pushes it along, as in editing tools, rather
    /// than silently refusing the mark.
    fn mark_start(&mut self, cx: &mut Context<Self>) {
        self.sel_from = self.playhead.clamp(0.1, self.span);
        self.sel_to = self.sel_to.min(self.sel_from - 0.1).max(0.0);
        cx.notify();
    }

    fn mark_end(&mut self, cx: &mut Context<Self>) {
        self.sel_to = self.playhead.clamp(0.0, self.span - 0.1);
        self.sel_from = self.sel_from.max(self.sel_to + 0.1).min(self.span);
        cx.notify();
    }

    fn shift_range(&mut self, delta: f64, cx: &mut Context<Self>) {
        let delta = delta.clamp(-self.sel_to, self.span - self.sel_from);
        self.sel_from += delta;
        self.sel_to += delta;
        cx.notify();
    }

    /// The ruler always seeks; the row edits the range. Mixing the two made
    /// the playhead impossible to grab, because it starts on the range edge and
    /// any click near it trimmed or moved the range instead.
    fn timeline_down(
        &mut self,
        x: f32,
        on_ruler: bool,
        left: f32,
        width: f32,
        cx: &mut Context<Self>,
    ) {
        self.stop_playback();
        // Grabbing the timeline mid-animation should not fight it.
        self.progress = 1.0;

        if on_ruler {
            self.drag = Some(Drag::Playhead);
            self.playhead = self.time_at(x, left, width);
            self.request_frame(cx);
            cx.notify();
            return;
        }

        let to_handle = (x - self.x_of(self.sel_to, left, width)).abs();
        let from_handle = (x - self.x_of(self.sel_from, left, width)).abs();

        if to_handle <= HANDLE_REACH && to_handle <= from_handle {
            self.drag = Some(Drag::To);
        } else if from_handle <= HANDLE_REACH {
            self.drag = Some(Drag::From);
        } else {
            let seconds = self.time_at(x, left, width);
            if seconds <= self.sel_from && seconds >= self.sel_to {
                // Inside the range: move the whole block.
                self.drag = Some(Drag::Range {
                    anchor: seconds,
                    from: self.sel_from,
                    to: self.sel_to,
                });
            } else {
                self.drag = Some(Drag::Playhead);
                self.playhead = seconds;
                self.request_frame(cx);
            }
        }
        cx.notify();
    }

    fn timeline_drag(&mut self, x: f32, left: f32, width: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else { return };
        let seconds = self.time_at(x, left, width);
        let edits_range = !matches!(drag, Drag::Playhead);

        match drag {
            Drag::Playhead => {
                self.playhead = seconds;
                self.request_frame(cx);
            }
            Drag::From => {
                self.sel_from = seconds.clamp(self.sel_to + 0.1, self.span);
            }
            Drag::To => {
                self.sel_to = seconds.clamp(0.0, self.sel_from - 0.1);
            }
            Drag::Range { anchor, from, to } => {
                let delta = (seconds - anchor).clamp(-to, self.span - from);
                self.sel_from = from + delta;
                self.sel_to = to + delta;
            }
        }

        if edits_range {
            self.request_edge_frames(cx);
        }
        cx.notify();
    }
}

// MARK: - Rendering

impl Render for Rewind {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Nothing else in the overlay is focusable, and the window can lose key
        // status when another app activates itself, which silently drops Space,
        // Enter and Escape. Re-assert focus on every frame.
        if !self.focus.is_focused(window) {
            window.focus(&self.focus);
        }

        let viewport = window.viewport_size();
        let width: f32 = viewport.width.into();
        let height: f32 = viewport.height.into();

        let bar_top = height - MARGIN - BAR_HEIGHT;
        let bar_width = (width - MARGIN * 2.0).min(MAX_BAR_WIDTH);
        let bar_left = (width - bar_width) / 2.0;
        // track_left is relative to the bar (used for layout); track_origin is
        // absolute in the window (used for pointer maths). Mixing the two put
        // the playhead a bar-margin away from the cursor.
        let track_left = BAR_PADDING + RULER_INSET;
        let track_origin = bar_left + track_left;
        let track_width = (bar_width - (BAR_PADDING + RULER_INSET) * 2.0).max(1.0);

        let stage_left = MARGIN;
        let stage_top = (self.safe_top + MARGIN * 0.5).max(MARGIN);
        let stage_width = (width - MARGIN * 2.0).max(1.0);
        let stage_height = (bar_top - GAP - stage_top).max(1.0);

        let (frame_width, frame_height) = self.frame_size;
        let (fit_width, fit_height) = fit(
            frame_width as f32,
            frame_height as f32,
            stage_width,
            stage_height,
        );

        // Anchor the morph: the image starts covering the screen and shrinks to
        // the fitted rect. Both rects keep the frame's aspect, so it is never
        // stretched, only cropped by the window while it is larger than it.
        let eased = 1.0 - (1.0 - self.progress).powi(3);
        let (cover_width, cover_height) =
            cover(frame_width as f32, frame_height as f32, width, height);

        let start_left = (width - cover_width) / 2.0;
        let start_top = (height - cover_height) / 2.0;
        let end_left = stage_left + (stage_width - fit_width) / 2.0;
        let end_top = stage_top + (stage_height - fit_height) / 2.0;

        let image_width = cover_width + (fit_width - cover_width) * eased;
        let image_height = cover_height + (fit_height - cover_height) * eased;
        let image_left = start_left + (end_left - start_left) * eased;
        let image_top = start_top + (end_top - start_top) * eased;
        let image_radius = 14.0 * eased;

        // The bar follows, arriving from below the screen edge.
        let bar_progress = ((eased - 0.35) / 0.65).clamp(0.0, 1.0);
        let bar_draw_top = bar_top + (1.0 - bar_progress) * (BAR_HEIGHT + MARGIN * 2.0);

        let span = self.span;
        let x_for =
            |seconds: f64| track_left + track_width * (1.0 - (seconds / span).clamp(0.0, 1.0)) as f32;

        let from_x = x_for(self.sel_from);
        let to_x = x_for(self.sel_to);
        let playhead_x = x_for(self.playhead);

        // While the range is being dragged, show what it contains: its first and
        // last frame, side by side.
        let dragging_range = matches!(
            self.drag,
            Some(Drag::Range { .. }) | Some(Drag::From) | Some(Drag::To)
        );
        let stage = if dragging_range && self.edge_frames.is_some() {
            self.edge_pair(stage_left, stage_top, stage_width, stage_height)
        } else {
            match &self.frame {
            Some(image) => div()
                .absolute()
                .left(px(image_left))
                .top(px(image_top))
                .w(px(image_width))
                .h(px(image_height))
                .rounded(px(image_radius))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(BORDER))
                .child(
                    img(ImageSource::Render(image.clone()))
                        .w(px(image_width))
                        .h(px(image_height)),
                )
                .into_any_element(),

            // Before the first frame there is nothing to animate: draw no panel
            // at all, so the overlay opens onto the dimmed desktop and the video
            // grows out of it instead of a grey slab appearing first.
            None if self.progress < 1.0 => div().into_any_element(),

            None => div()
                .absolute()
                .left(px(image_left))
                .top(px(image_top))
                .w(px(image_width))
                .h(px(image_height))
                .rounded(px(image_radius))
                .bg(rgba(PANEL))
                .border_1()
                .border_color(rgba(BORDER))
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(rgba(MUTED))
                .child("No frame")
                .into_any_element(),
            }
        };

        // Moves and releases are handled on the root, which covers the whole
        // window, so a drag keeps tracking after the pointer leaves the bar.
        div()
            .relative()
            .size_full()
            .bg(rgba(SCRIM))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_move(cx.listener(move |view, event: &MouseMoveEvent, _window, cx| {
                if view.drag.is_some() && event.pressed_button == Some(MouseButton::Left) {
                    view.timeline_drag(event.position.x.into(), track_origin, track_width, cx)
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _: &MouseUpEvent, _window, cx| {
                    if view.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(stage)
            .child(self.transport_bar(
                bar_top,
                bar_draw_top,
                bar_left,
                bar_width,
                track_left,
                track_origin,
                track_width,
                from_x,
                to_x,
                playhead_x,
                cx,
            ))
    }
}

// MARK: - The bar

impl Rewind {
    #[allow(clippy::too_many_arguments)]
    fn transport_bar(
        &self,
        bar_top: f32,
        bar_draw_top: f32,
        bar_left: f32,
        bar_width: f32,
        track_left: f32,
        track_origin: f32,
        track_width: f32,
        from_x: f32,
        to_x: f32,
        playhead_x: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (status_text, status_colour) = match &self.status {
            Some(Status::Info(text)) => (text.clone(), rgba(PENDING)),
            Some(Status::Error(text)) => (text.clone(), rgba(DANGER)),
            None => (String::new(), rgba(MUTED)),
        };

        let transport = group()
            .child(icon_button("◀◀").on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, _window, cx| view.jump_to_start(cx)),
            ))
            .child(
                icon_button(if self.playing { "❚❚" } else { "▶" })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _: &MouseDownEvent, _window, cx| view.toggle_play(cx)),
                    ),
            )
            .child(icon_button("▶▶").on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, _window, cx| view.jump_to_end(cx)),
            ));

        let mut left = div()
            .absolute()
            .left(px(BAR_PADDING))
            .top(px(TOOLBAR_Y))
            .h(px(TOOLBAR_H))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(transport)
            .child(
                div()
                    .text_xs()
                    .text_color(rgba(MUTED))
                    .child(format!("{:.1}s selected", self.selection_duration())),
            );
        if !status_text.is_empty() {
            left = left.child(div().text_xs().text_color(status_colour).child(status_text));
        }

        let mut secondary = group();
        if let Some(path) = &self.last_clip {
            let path = path.clone();
            secondary = secondary.child(
                label_button("Reveal").on_mouse_down(MouseButton::Left, move |_, _, _| reveal(&path)),
            );
        }
        secondary = secondary
            .child(label_button("Reset").on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, _window, cx| view.reset_range(cx)),
            ))
            .child(label_button("Copy").on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, _window, cx| view.copy_clip(cx)),
            ));

        let right = div()
            .absolute()
            .right(px(BAR_PADDING))
            .top(px(TOOLBAR_Y))
            .h(px(TOOLBAR_H))
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(secondary)
            .child(
                group().bg(rgba(ACCENT)).child(
                    label_button(if self.exporting { "Exporting…" } else { "Export" })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _: &MouseDownEvent, _window, cx| view.export(cx)),
                        ),
                ),
            )
            .child(icon_button("✕").on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, _window, cx| view.close(cx)),
            ));

        let block_top = ROW_Y + BLOCK_INSET;
        let block_height = ROW_H - BLOCK_INSET * 2.0;

        panel()
            .absolute()
            .left(px(bar_left))
            .top(px(bar_draw_top))
            .w(px(bar_width))
            .h(px(BAR_HEIGHT))
            .child(left)
            .child(right)
            .child(self.ruler(track_left, track_width))
            .child(
                div()
                    .absolute()
                    .left(px(BAR_PADDING))
                    .top(px(ROW_Y))
                    .w(px(bar_width - BAR_PADDING * 2.0))
                    .h(px(ROW_H))
                    .rounded(px(8.0))
                    .bg(rgba(SUNKEN)),
            )
            .child(self.thumbnail_strip(track_left, track_width))
            .child(
                // The range block. Grabbing its body moves the whole range.
                div()
                    .absolute()
                    .left(px(from_x))
                    .top(px(block_top))
                    .w(px((to_x - from_x).max(2.0)))
                    .h(px(block_height))
                    .rounded(px(6.0))
                    .bg(rgba(ACCENT_FILL))
                    .border_1()
                    .border_color(rgba(ACCENT)),
            )
            .child(trim_handle(from_x + 5.0, block_top, block_height))
            .child(trim_handle(to_x - 5.0, block_top, block_height))
            .child(
                div()
                    .absolute()
                    .left(px(playhead_x - 1.0))
                    .top(px(RULER_Y + RULER_H / 2.0))
                    .w(px(2.0))
                    .h(px(ROW_Y + ROW_H - RULER_Y - RULER_H / 2.0))
                    .bg(rgba(PLAYHEAD)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(playhead_x - BADGE_WIDTH / 2.0))
                    .top(px(RULER_Y))
                    .w(px(BADGE_WIDTH))
                    .h(px(RULER_H))
                    .rounded(px(6.0))
                    .bg(rgba(PLAYHEAD))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(rgba(0xFFFFFFFF))
                    .child(format!("−{:.2}s", self.playhead)),
            )
            .child(
                // Ruler and row together are the seek / drag surface.
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(px(RULER_Y))
                    .w(px(bar_width))
                    .h(px(BAR_HEIGHT - RULER_Y))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, event: &MouseDownEvent, _window, cx| {
                            let y: f32 = event.position.y.into();
                            let on_ruler = y < bar_top + ROW_Y;
                            view.timeline_down(
                                event.position.x.into(),
                                on_ruler,
                                track_origin,
                                track_width,
                                cx,
                            )
                        }),
                    ),
            )
            .into_any_element()
    }

    /// The range's first and last frame, side by side, each fitted to half the
    /// stage.
    fn edge_pair(&self, left: f32, top: f32, width: f32, height: f32) -> AnyElement {
        let Some((first, last)) = &self.edge_frames else {
            return div().into_any_element();
        };

        let half = (width - GAP) / 2.0;
        let mut row = div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(width))
            .h(px(height));

        for (slot, image) in [first.clone(), last.clone()].into_iter().enumerate() {
            let (image_width, image_height) = fit(
                self.frame_size.0 as f32,
                self.frame_size.1 as f32,
                half,
                height,
            );
            let x = slot as f32 * (half + GAP) + (half - image_width) / 2.0;
            let y = (height - image_height) / 2.0;

            row = row.child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(image_width))
                    .h(px(image_height))
                    .rounded(px(10.0))
                    .overflow_hidden()
                    .border_1()
                    .border_color(rgba(BORDER))
                    .child(
                        img(ImageSource::Render(image.clone()))
                            .w(px(image_width))
                            .h(px(image_height)),
                    ),
            );
        }

        row.into_any_element()
    }

    /// Filmstrip behind the range block. Each slot is the same width, so the
    /// strip lines up with the ruler above it.
    fn thumbnail_strip(&self, track_left: f32, strip_width: f32) -> AnyElement {
        let mut strip = div()
            .absolute()
            .left(px(track_left))
            .top(px(ROW_Y))
            .w(px(strip_width))
            .h(px(ROW_H))
            .rounded(px(10.0))
            .overflow_hidden();

        if !self.thumbnails.is_empty() {
            let slot = strip_width / self.thumbnails.len() as f32;
            // Cover the slot rather than stretch it: the slot is wider than the
            // frame, so the frame is scaled to the slot width and the overflow
            // is clipped by the strip.
            let covered = slot / self.thumbnail_aspect.max(0.1);
            let overshoot = (covered - ROW_H) / 2.0;
            for (index, thumbnail) in self.thumbnails.iter().enumerate() {
                strip = strip.child(
                    div()
                        .absolute()
                        .left(px(index as f32 * slot))
                        .top(px(-overshoot))
                        .w(px(slot + 1.0))
                        .h(px(covered))
                        .child(
                            img(ImageSource::Render(thumbnail.clone()))
                                .w(px(slot + 1.0))
                                .h(px(covered)),
                        ),
                );
            }
        }

        strip.opacity(0.85).into_any_element()
    }

    /// Numbers are centred on their tick in a fixed-width box, which keeps the
    /// last one inside the bar instead of hanging off the edge.
    fn ruler(&self, track_left: f32, track_width: f32) -> AnyElement {
        let step = ruler_step(self.span);
        let mut ruler = div()
            .absolute()
            .left(px(track_left))
            .top(px(RULER_Y))
            .h(px(RULER_H))
            .w(px(track_width));

        let mut ticks = 0.0;
        while ticks <= self.span + 0.001 {
            let x = track_width * (1.0 - (ticks / self.span).clamp(0.0, 1.0)) as f32;
            ruler = ruler.child(
                div()
                    .absolute()
                    .left(px(x - LABEL_WIDTH / 2.0))
                    .top(px(0.0))
                    .w(px(LABEL_WIDTH))
                    .h(px(RULER_H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(rems(0.625))
                    .text_color(rgba(MUTED))
                    .child(format!("−{:.0}s", ticks)),
            );
            ticks += step;
        }

        ruler.into_any_element()
    }
}

// MARK: - Bits

fn panel() -> Div {
    div()
        .rounded(px(12.0))
        .bg(rgba(PANEL))
        .border_1()
        .border_color(rgba(BORDER))
        .shadow_lg()
}

/// A recessed pill that groups related controls, as in the reference.
fn group() -> Div {
    div()
        .h(px(TOOLBAR_H))
        .px(px(2.0))
        .rounded(px(8.0))
        .bg(rgba(GROUP))
        .flex()
        .items_center()
        .gap(px(2.0))
}

fn icon_button(glyph: &'static str) -> Div {
    div()
        .w(px(28.0))
        .h(px(26.0))
        .rounded(px(7.0))
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(rgba(TEXT))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0xFFFFFF14)))
        .child(glyph)
}

fn label_button(label: &'static str) -> Div {
    div()
        .h(px(26.0))
        .px(px(10.0))
        .rounded(px(7.0))
        .flex()
        .items_center()
        .text_xs()
        .text_color(rgba(TEXT))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0xFFFFFF14)))
        .child(label)
}

fn trim_handle(x: f32, top: f32, height: f32) -> AnyElement {
    let bar = height * 0.5;
    div()
        .absolute()
        .left(px(x - 2.0))
        .top(px(top + (height - bar) / 2.0))
        .w(px(4.0))
        .h(px(bar))
        .rounded_full()
        .bg(rgba(TEXT))
        .into_any_element()
}

fn ruler_step(span: f64) -> f64 {
    for candidate in [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0] {
        if span / candidate <= 14.0 {
            return candidate;
        }
    }
    120.0
}

/// Largest rect with the frame's aspect that covers the whole screen.
fn cover(frame_width: f32, frame_height: f32, width: f32, height: f32) -> (f32, f32) {
    if frame_width <= 0.0 || frame_height <= 0.0 {
        return (width, height);
    }
    let scale = (width / frame_width).max(height / frame_height);
    (frame_width * scale, frame_height * scale)
}

fn fit(frame_width: f32, frame_height: f32, max_width: f32, max_height: f32) -> (f32, f32) {
    if frame_width <= 0.0 || frame_height <= 0.0 {
        return (max_width, max_height);
    }
    let scale = (max_width / frame_width)
        .min(max_height / frame_height)
        .min(1.0);
    (frame_width * scale, frame_height * scale)
}

fn build_image(image: RawImage) -> Result<Arc<RenderImage>, String> {
    let buffer =
        image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(image.width, image.height, image.pixels)
            .ok_or_else(|| "frame size does not match its pixels".to_string())?;

    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

fn reveal(path: &str) {
    let _ = std::process::Command::new("open")
        .args(["-R", path])
        .spawn();
}

// MARK: - Entry

/// Leaves a stale socket file from a killed overlay behind would block the
/// bind forever, so it is removed unless something actually answers on it.
fn bind_listener(path: &std::path::Path) -> Option<std::os::unix::net::UnixListener> {
    if path.exists() {
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => return None,
            Err(_) => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    std::os::unix::net::UnixListener::bind(path).ok()
}

fn ui_socket_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").expect("HOME is unset");
    std::path::PathBuf::from(home)
        .join("Library/Application Support/Rewind")
        .join("ui.sock")
}

/// The overlay stays resident so the hotkey only has to show a window that
/// already exists. A resident process costs nothing while hidden if no frames
/// are decoded, which is why the frame is dropped on hide.
pub fn run(show: bool) {
    Application::new().run(move |cx| {
        let display = cx.primary_display();
        let bounds = display
            .as_ref()
            .map(|display| display.bounds())
            .unwrap_or_else(|| Bounds::new(point(px(0.0), px(0.0)), size(px(1440.0), px(900.0))));
        let display_id = display.as_ref().map(|display| display.id());

        let path = ui_socket_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Some(listener) = bind_listener(&path) else {
            // Another overlay is already listening; it will be the one shown.
            cx.quit();
            return;
        };
        let listener = std::sync::Arc::new(listener);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: None,
            }),
            focus: true,
            show: false,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id,
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: None,
            window_min_size: None,
            window_decorations: None,
            tabbing_identifier: None,
        };

        let handle = cx
            .open_window(options, |_window, cx| cx.new(Rewind::new))
            .expect("could not open the rewind window");

        crate::activate::make_borderless();
        crate::activate::make_accessory();

        if show {
            handle
                .update(cx, |view, window, cx| view.show(window, cx))
                .ok();
        }

        cx.spawn(async move |cx| loop {
            let listener = listener.clone();
            let incoming = cx
                .background_executor()
                .spawn(async move { listener.accept().is_ok() })
                .await;
            if !incoming {
                break;
            }
            let _ = handle.update(cx, |view, window, cx| view.show(window, cx));
        })
        .detach();
    });
}
