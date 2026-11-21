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

use crate::icons;
use crate::ipc::{self, Client, RawImage, Reply};
use crate::paint::{Brush, BrushKind, Paint, Point, Rgb, StrokeSession, from_srgb_hex};
use crate::protocol::{ClipRange, Command, Crop, ExportFormat, Request};
use crate::settings::{self, Output, Settings};
use crate::trace;

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
/// One control height for every button, segment and group, so nothing in the
/// toolbar is a different size from its neighbours.
const CONTROL_H: f32 = 26.0;
const GROUP_H: f32 = 30.0;
const CONTROL_RADIUS: f32 = 8.0;
const CONTROL_PADDING: f32 = 14.0;

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
const MORPH_MS: u64 = 190;
const MORPH_STEPS: u64 = 18;

/// Capture is native-resolution (33 MB per frame on a 4K display). A 2K preview
/// costs ~11 MB and about 40 ms round trip, which keeps scrubbing usable; the
/// exported clip is full resolution either way.
const PREVIEW_WIDTH: i64 = 2048;
/// While playing, motion hides softness and the frame is the dominant cost:
/// 1280 wide is 3.7 MB against 9.4 MB, so playback moves ~2.5x less data.
const PLAYBACK_WIDTH: i64 = 1280;

/// The timeline strip: one thumbnail per slot, decoded from keyframes.
const THUMBNAIL_COUNT: i64 = 16;
const THUMBNAIL_WIDTH: i64 = 160;
const EDGE_FRAME_WIDTH: i64 = 640;

/// The drawing toolbar: a column of pills down the left edge, centred in the
/// space above the transport bar. Laid out explicitly so the weight slider can
/// turn a window-space pointer into a width.
const TOOLBAR_PADDING: f32 = 4.0;
/// Same control size, radius and spacing as the transport bar's buttons.
const TOOL_BUTTON: f32 = CONTROL_H;
const TOOL_ICON: f32 = 14.0;
const TOOL_SPACING: f32 = 2.0;
const TOOLBAR_WIDTH: f32 = TOOL_BUTTON + TOOLBAR_PADDING * 2.0;
const TOOLBAR_GAP: f32 = GAP;
const TOOLBAR_RADIUS: f32 = CONTROL_RADIUS + TOOLBAR_PADDING;
const SLIDER_HEIGHT: f32 = 120.0;
const COLOR_BUTTON: f32 = TOOLBAR_WIDTH;
const SWATCH: f32 = 18.0;
const TOOLS_HEIGHT: f32 = TOOLBAR_PADDING * 2.0 + TOOL_BUTTON * 3.0 + TOOL_SPACING * 2.0;
const HISTORY_HEIGHT: f32 = TOOLS_HEIGHT;
const WEIGHT_HEIGHT: f32 = TOOLBAR_PADDING * 2.0 + SLIDER_HEIGHT;
const TOOLBAR_HEIGHT: f32 =
    TOOLS_HEIGHT + HISTORY_HEIGHT + WEIGHT_HEIGHT + COLOR_BUTTON + TOOLBAR_GAP * 3.0;
const DEFAULT_BRUSH_WIDTH: f32 = 9.0;
const MIN_BRUSH_WIDTH: f32 = 1.0;
const MAX_BRUSH_WIDTH: f32 = 160.0;

/// Ink paints in linear RGB; these are sRGB swatches chosen to stay readable
/// on both light and dark captures.
const DRAW_COLORS: [u32; 8] = [
    0xFFFFFF, 0x16181D, 0xE5484D, 0xF07A24, 0xF5C542, 0x2FA84F, 0x2F7CF6, 0x9A5CF5,
];

// MARK: - Palette

/// The backdrop's colour, without alpha: its darkness follows the entrance.
const SCRIM: u32 = 0x08080C;
/// While the first frame loads the desktop is only lightly dimmed; it deepens
/// as the preview grows into place.
const SCRIM_LOADING_ALPHA: f32 = 0.28;
const SCRIM_ALPHA: f32 = 0.72;
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

/// Which action is currently rendering, so only that button says it is busy.
#[derive(Clone, Copy, PartialEq)]
enum Busy {
    Export,
    Copy,
}

/// Which range frame the stage shows while dragging.
#[derive(Clone, Copy, PartialEq)]
enum EdgeMode {
    /// The whole range is moving, so both ends matter.
    Both,
    /// Only one edge is moving: show just that end.
    First,
    Last,
}

/// Which corner of a crop a drag is pulling.
#[derive(Clone, Copy, PartialEq)]
enum CropHandle {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// What a press on the preview does in crop mode.
#[derive(Clone, Copy)]
enum CropDrag {
    /// Laying a new rectangle from where the press began. `previous` comes
    /// back if the press turns out to be a click rather than a drag.
    New { anchor: (f64, f64), previous: Option<Crop> },
    /// Moving the rectangle as it was.
    Move { anchor: (f64, f64), start: Crop },
    /// Pulling one corner of the rectangle as it was.
    Resize { handle: CropHandle, start: Crop },
}

/// How near a corner a press counts as grabbing it, in points.
const CROP_HANDLE_REACH: f32 = 12.0;
const MIN_CROP: f64 = 0.02;

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Playhead,
    From,
    To,
    /// Moving the whole selection. The range is moved by the cursor's offset
    /// from where the drag started, applied to the range as it was then, so
    /// its length cannot drift while dragging.
    Range {
        anchor: f64,
        from: f64,
        to: f64,
    },
}

enum Status {
    Info(String),
    Error(String),
}

/// Copy and Export close the overlay at once and render behind it; the same
/// window comes back as this small card at the top of the screen until the
/// render has finished and been reported.
struct Toast {
    text: String,
    error: bool,
    /// The saved file, revealed when the toast is clicked.
    path: Option<String>,
}

const TOAST_WIDTH: f32 = 340.0;
const TOAST_HEIGHT: f32 = 52.0;
/// Below the top edge, and below the menu bar on macOS.
const TOAST_TOP: f32 = 12.0;
const TOAST_DONE: Duration = Duration::from_millis(3000);
const TOAST_FAILED: Duration = Duration::from_millis(7000);
const PROGRESS_POLL: Duration = Duration::from_millis(150);

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
    /// Exact encoded size of the current range, from the daemon.
    estimated_bytes: Option<i64>,
    /// Entrance progress: 0 is a full-bleed preview with the bar off-screen,
    /// 1 is the resting layout.
    progress: f32,
    /// Top inset the system keeps clear (menu bar, camera housing), so the
    /// preview is never covered by the notch.
    safe_top: f32,
    /// Images waiting for a render that no longer references them. Dropping one
    /// while it is still in the painted tree lets GPUI re-upload it with no
    /// owner, which leaks a frame per use.
    graveyard: Vec<Arc<RenderImage>>,
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
    /// Whether the in-flight request asked for a lean (playback or scrub) frame
    /// rather than the sharp one.
    requested_lean: bool,
    in_flight: bool,
    drag: Option<Drag>,
    busy: Option<Busy>,
    status: Option<Status>,
    ticker: Option<Task<()>>,
    /// Wall clock of the previous playback tick. Advance is derived from the
    /// time actually elapsed rather than from a tick count, so a late or early
    /// timer cannot speed playback up or slow it down.
    playback_clock: Option<Instant>,
    last_clip: Option<String>,
    /// Remembered between captures, with the crop; see `settings.rs`.
    output: Output,
    /// The settings file could not be read or written. Shown each time the
    /// overlay opens until a save succeeds.
    settings_error: Option<String>,
    /// The background render's card, while it renders and briefly after.
    toast: Option<Toast>,
    /// The window is currently the toast rather than the overlay.
    toast_shown: bool,
    /// Bumped by every render and every show, so a dismiss scheduled for an
    /// earlier toast cannot take down a newer one or the reopened overlay.
    toast_generation: u64,
    /// How far the background render is, when the daemon reports it.
    render_progress: Option<f32>,
    /// The overlay is up, or opening: not hidden, not exiting. A render that
    /// finishes while it is reports in the bar instead of a toast.
    overlay_open: bool,
    /// The window's scale at the last render, to size the toast in pixels.
    scale: f32,
    /// Which drawing tool is selected; the toolbar is always available.
    draw_eraser: bool,
    /// The weight slider is being dragged: the pointer's window-space y when
    /// the drag began, and the slider's position then. The slider follows the
    /// pointer's movement rather than its position, so grabbing it never jumps.
    weight_drag: Option<(f32, f32)>,
    /// The swatches beside the colour button are showing.
    colors_open: bool,
    /// The part of the capture copied and exported. A rectangle counts as
    /// soon as it is drawn; there is no separate apply step.
    crop: Option<Crop>,
    crop_drag: Option<CropDrag>,
    /// The crop tool is selected. Drawing is suspended while it is on.
    cropping: bool,
    /// The overlay is playing its exit: 0 is resting, 1 is off-screen.
    exiting: bool,
    exit_progress: f32,
    /// Bumped by every show and every dismiss, so a fade that finishes late
    /// cannot hide an overlay that was reopened.
    fade_generation: u64,
    brush_color: Rgb,
    /// A swatch was picked. Until then the ink follows the system theme, which
    /// can change while the overlay stays resident.
    color_chosen: bool,
    brush_width: f32,
    /// Completed annotations and the stroke currently under the pointer, in
    /// normalised capture coordinates so they can render at any size.
    paint: Paint,
    live: Option<StrokeSession>,
    live_started: Option<Instant>,
    /// The annotations, painted over the preview. Rebuilt when a stroke moves.
    overlay: Option<Arc<RenderImage>>,
    /// The ink stroke under the pointer, rasterised over just the area it
    /// covers and placed in annotation-layer pixels: x, y, width, height.
    live_overlay: Option<(Arc<RenderImage>, (u32, u32, u32, u32))>,
    /// On-screen rect of the preview, in window coordinates, so a pointer can
    /// be mapped back onto the capture.
    stage_image: (f32, f32, f32, f32),
    /// The capture's encoded size, used to rasterise annotations for export.
    native_size: (u32, u32),
    /// Remaining attempts to take keyboard focus after showing. Focus has to be
    /// re-asserted from a render, the only place a `Window` is available, but
    /// doing that unconditionally invalidates the window every frame and pins a
    /// core. Bounded, it recovers from a previous app holding focus and stops.
    focus_until: Option<Instant>,
    closing: bool,
    /// The last copy failed; the next close hides without trying again, so a
    /// dead daemon cannot trap the user in the overlay.
    copy_failed: bool,
}

impl Rewind {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (remembered, settings_error) = match settings::load() {
            Ok(settings) => (settings, None),
            Err(error) => (Settings::default(), Some(format!("settings unreadable: {error}"))),
        };
        Self {
            focus: cx.focus_handle(),
            client: Client::new(),
            span: 1.0,
            anchor: None,
            fps: 30.0,
            estimated_bytes: None,
            progress: 1.0,
            safe_top: 0.0,
            graveyard: Vec::new(),
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
            requested_lean: false,
            in_flight: false,
            drag: None,
            busy: None,
            status: None,
            settings_error,
            ticker: None,
            playback_clock: None,
            output: remembered.output,
            toast: None,
            toast_shown: false,
            toast_generation: 0,
            render_progress: None,
            overlay_open: false,
            scale: 1.0,
            draw_eraser: false,
            weight_drag: None,
            colors_open: false,
            crop: remembered.crop,
            crop_drag: None,
            cropping: false,
            exiting: false,
            exit_progress: 0.0,
            fade_generation: 0,
            brush_color: theme_ink(cx.window_appearance()),
            color_chosen: false,
            brush_width: DEFAULT_BRUSH_WIDTH,
            paint: Paint::new(),
            live: None,
            live_started: None,
            overlay: None,
            live_overlay: None,
            stage_image: (0.0, 0.0, 1.0, 1.0),
            native_size: (0, 0),
            last_clip: None,
            focus_until: None,
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
        self.native_size = state
            .map(|state| (state.width.max(0) as u32, state.height.max(0) as u32))
            .unwrap_or((0, 0));
        self.safe_top = state
            .and_then(|state| state.display.as_ref())
            .map(|display| display.safe_area_top as f32)
            .unwrap_or(0.0);
        self.screen = state
            .and_then(|state| state.display.as_ref())
            .and_then(|display| {
            (display.screen_width > 0.0).then_some((
                display.screen_x,
                display.screen_y,
                display.screen_width,
                display.screen_height,
            ))
        });
        self.sel_from = self.span.min(10.0);
        self.sel_to = 0.0;
        // Open on the newest frame, not on the start of the default range:
        // "what just happened" is the frame you want to see first.
        self.playhead = 0.0;
        self.status = response
            .as_ref()
            .err()
            .map(|error| error.to_string())
            .or_else(|| self.settings_error.clone())
            .map(Status::Error);
        // Dropped through GPUI, not just discarded: a frame still in the sprite
        // atlas stays resident even when nothing references it.
        self.release_images(cx);
        self.frame = None;
        self.drag = None;
        self.closing = false;
        self.copy_failed = false;
        self.paint.clear();
        self.live = None;
        self.live_started = None;
        self.overlay = None;
        self.live_overlay = None;
        self.draw_eraser = false;
        self.cropping = false;
        self.crop_drag = None;
        self.request_frame(cx);
        self.request_edge_frames(cx);
        self.request_estimate(cx);
        self.fetch_thumbnails(cx);
        cx.notify();
    }

    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Show first, load second: `refresh` asks the daemon for the buffer
        // state, and the daemon may still be finishing the last clipboard copy.
        // Waiting for that before ordering the window cost up to a second.
        self.frame = None;
        self.drag = None;
        self.release_images(cx);
        self.progress = 0.0;
        self.exiting = false;
        self.exit_progress = 0.0;
        self.fade_generation = self.fade_generation.wrapping_add(1);
        // The toast's window becomes the overlay again; a render still running
        // reports in the bar instead.
        self.toast_generation = self.toast_generation.wrapping_add(1);
        self.toast = None;
        let was_toast = self.toast_shown;
        if was_toast {
            self.toast_shown = false;
            crate::activate::end_toast();
        }
        self.overlay_open = true;
        if !self.color_chosen {
            self.brush_color = theme_ink(cx.window_appearance());
        }
        self.closing = false;
        self.copy_failed = false;

        // Open on the display actually being captured, not always the primary.
        // Coming from the toast the size changes, and GPUI only hears of that
        // outside an update (see `show_toast`).
        if was_toast {
            self.restore_overlay_size(cx);
        } else if let Some((x, y, width, height)) = self.screen {
            crate::activate::place(x, y, width, height);
        }
        window.focus(&self.focus);
        // Stay transparent until the first real frame lands. The window keeps
        // its last drawn surface while hidden, so starting the fade here showed
        // one frame of the previous overlay before the new one was drawn.
        crate::activate::prepare_fade();
        crate::activate::activate();

        self.refresh(cx);
        if self.busy.is_some() && self.status.is_none() {
            self.status = Some(Status::Info(self.rendering_text()));
        }
        // Activation races window ordering and is often dropped. Re-asserting it
        // is not enough on its own: the focus retry lives in `render`, which only
        // runs when something asks for a frame, so nudge a few renders too and
        // stop as soon as focus sticks.
        cx.spawn(async move |this, cx| {
            for _ in 0..12 {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                crate::activate::activate();
                let settled = this
                    .update(cx, |view, cx| {
                        cx.notify();
                        view.focus_until.is_none()
                            && crate::activate::is_active()
                            && crate::activate::is_key_window()
                    })
                    .unwrap_or(true);
                if settled {
                    break;
                }
            }
        })
        .detach();

        // Another app may still hold activation; the retry is bounded.
        // Another app may still hold activation, and taking focus is what makes
        // the keys land here. Bounded by time rather than by a render count,
        // which can all elapse before the window is key-able.
        self.focus_until = Some(Instant::now() + Duration::from_millis(2500));
        // Full bleed until the first frame arrives; `received` starts the morph
        // so it never animates a placeholder.
        self.progress = 0.0;
        self.closing = false;
        self.copy_failed = false;
    }

    /// Exact encoded size of the range: cheap on the daemon side and kept
    /// separate from the edge frames so a slow decode cannot delay it.
    fn request_estimate(&mut self, cx: &mut Context<Self>) {
        let range = (self.sel_from, self.sel_to);
        let anchor = self.anchor;
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move {
                    let mut request = Request::new(Command::Estimate);
                    request.range = Some(ClipRange {
                        from: range.0,
                        to: range.1,
                    });
                    request.anchor = anchor;
                    ipc::call(&client, request)
                })
                .await;

            this.update(cx, |view, cx| {
                if let Ok(response) = reply {
                    view.estimated_bytes = response.estimated_bytes;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
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
                            if let Some((old_first, old_last)) = view.edge_frames.take() {
                                view.graveyard.push(old_first);
                                view.graveyard.push(old_last);
                            }
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
                    for thumbnail in view.thumbnails.drain(..) {
                        view.graveyard.push(thumbnail);
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
    /// A frame never arrived: show the resting layout and its error rather
    /// than leaving an invisible window up.
    fn reveal_without_frame(&mut self) {
        if self.progress == 0.0 {
            self.progress = 1.0;
            crate::activate::start_fade();
        }
    }

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
    /// GPUI keeps every `RenderImage` it paints in a sprite atlas until told
    /// otherwise, so a new preview frame per scrub step grows without bound
    /// until the old ones are dropped.
    fn release_images(&mut self, _cx: &mut Context<Self>) {
        if let Some(frame) = self.frame.take() {
            self.graveyard.push(frame);
        }
        if let Some(overlay) = self.overlay.take() {
            self.graveyard.push(overlay);
        }
        if let Some((live, _)) = self.live_overlay.take() {
            self.graveyard.push(live);
        }
        for thumbnail in self.thumbnails.drain(..) {
            self.graveyard.push(thumbnail);
        }
        if let Some((first, last)) = self.edge_frames.take() {
            self.graveyard.push(first);
            self.graveyard.push(last);
        }
    }

    /// Dismisses the overlay. The window-server fade is short and subtle, and
    /// the images are only dropped once it is gone: hiding immediately made
    /// Escape blink rather than fade.
    fn hide(&mut self, cx: &mut Context<Self>) {
        if self.exiting {
            return;
        }
        self.stop_playback();
        self.drag = None;
        self.closing = false;
        self.cropping = false;
        self.exiting = true;
        self.overlay_open = false;
        let generation = self.fade_generation.wrapping_add(1);
        self.fade_generation = generation;

        cx.spawn(async move |this, cx| {
            const STEPS: u64 = 10;
            for step in 1..=STEPS {
                cx.background_executor()
                    .timer(Duration::from_millis(8))
                    .await;
                let still_current = this
                    .update(cx, |view, cx| {
                        if view.fade_generation != generation {
                            return false;
                        }
                        view.exit_progress = step as f32 / STEPS as f32;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !still_current {
                    return;
                }
            }
            this.update(cx, |view, cx| {
                if view.fade_generation == generation {
                    view.finish_hide(cx);
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn finish_hide(&mut self, cx: &mut Context<Self>) {
        crate::activate::hide();
        self.exit_progress = 0.0;
        self.release_images(cx);
        self.frame = None;
        self.overlay = None;
        self.live_overlay = None;
        self.live = None;
        self.exiting = false;
        if self.toast.is_some() && !self.overlay_open {
            self.show_toast(cx);
        }
        cx.notify();
    }

    // MARK: - Toast

    /// Brings the hidden window back as the toast: small, at the top centre of
    /// the captured display, and never taking focus from the app in front.
    ///
    /// Every resize of the window happens in a task, between updates. GPUI
    /// learns of a new size by updating the window from the platform's resize
    /// callback, and that update fails while the window is already being
    /// updated, so a resize made from inside one is lost: the toast was drawn
    /// with the full-screen viewport, its contents far below the visible card.
    fn show_toast(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some((screen, scale)) = this
                .update(cx, |view, _| {
                    (view.toast.is_some() && !view.overlay_open && !view.toast_shown).then(|| {
                        view.toast_shown = true;
                        (view.screen, view.scale)
                    })
                })
                .ok()
                .flatten()
            else {
                return;
            };
            crate::activate::show_toast(
                screen,
                TOAST_WIDTH as f64,
                TOAST_HEIGHT as f64,
                TOAST_TOP as f64,
                scale as f64,
            );
            // Same reveal as the overlay: nothing on screen until the toast
            // itself has been drawn, so the last overlay frame never flashes.
            this.update(cx, |_, cx| {
                crate::activate::start_fade();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        self.toast = None;
        if self.toast_shown {
            self.toast_shown = false;
            crate::activate::hide();
            crate::activate::end_toast();
            // Back to covering the display while hidden, so the next show
            // opens at full size; outside an update, as in `show_toast`.
            self.restore_overlay_size(cx);
        }
        cx.notify();
    }

    fn restore_overlay_size(&self, cx: &mut Context<Self>) {
        let screen = self.screen;
        cx.spawn(async move |_, _| match screen {
            Some((x, y, width, height)) => crate::activate::place(x, y, width, height),
            None => crate::activate::make_borderless(),
        })
        .detach();
    }

    fn dismiss_toast_after(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let generation = self.toast_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |view, cx| {
                if view.toast_generation == generation && view.busy.is_none() {
                    view.dismiss_toast(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Asks the daemon how far the render is until it has finished. The
    /// render's own request is blocked for its whole length, so this is a
    /// second connection; the daemon serves each on its own thread.
    fn poll_progress(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_POLL).await;
                let busy = this.update(cx, |view, _| view.busy.is_some()).unwrap_or(false);
                if !busy {
                    return;
                }
                let client = client.clone();
                let progress = cx
                    .background_executor()
                    .spawn(async move {
                        ipc::call(&client, Request::new(Command::Status))
                            .ok()
                            .and_then(|response| response.state)
                            .and_then(|state| state.export_progress)
                    })
                    .await;
                let still = this
                    .update(cx, |view, cx| {
                        if view.busy.is_none() {
                            return false;
                        }
                        if let Some(progress) = progress {
                            view.render_progress = Some(progress.clamp(0.0, 1.0) as f32);
                            if view.overlay_open {
                                view.status = Some(Status::Info(view.rendering_text()));
                            }
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !still {
                    return;
                }
            }
        })
        .detach();
    }

    fn rendering_text(&self) -> String {
        match self.render_progress {
            Some(progress) => format!("rendering… {:.0}%", progress * 100.0),
            None => "rendering…".into(),
        }
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

    /// Exact size of the encoded frames the range would export, computed by the
    /// daemon from the ring without decoding anything.
    fn estimated_size(&self) -> String {
        if !self.paint.is_empty() {
            return if self.crop.is_some() {
                " · annotated · cropped".into()
            } else {
                " · annotated".into()
            };
        }
        if self.crop.is_some() {
            return " · cropped".into();
        }
        // The estimate is the encoded MP4 frames; it says nothing about a GIF.
        if self.output != Output::Mp4 {
            return String::new();
        }
        match self.estimated_bytes {
            Some(bytes) if bytes > 0 => format!(" · ~{:.1} MB", bytes as f64 / 1_048_576.0),
            _ => String::new(),
        }
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
        self.request_edge_frames(cx);
        self.request_estimate(cx);
        cx.notify();
    }

    // MARK: - Drawing

    fn select_eraser(&mut self, eraser: bool, cx: &mut Context<Self>) {
        self.draw_eraser = eraser;
        self.cropping = false;
        cx.notify();
    }

    // MARK: - Cropping

    /// Selects the crop tool. An existing crop gets its handles back for
    /// moving or resizing.
    fn begin_crop(&mut self, cx: &mut Context<Self>) {
        self.cropping = true;
        self.colors_open = false;
        self.crop_drag = None;
        cx.notify();
    }

    /// Back to drawing; the crop stays as it is.
    fn end_crop(&mut self, cx: &mut Context<Self>) {
        self.cropping = false;
        self.crop_drag = None;
        cx.notify();
    }

    /// A window-space pointer as fractions of the preview.
    fn normalised_point(&self, x: f32, y: f32) -> (f64, f64) {
        let (left, top, width, height) = self.stage_image;
        (
            ((x - left) / width.max(1.0)).clamp(0.0, 1.0) as f64,
            ((y - top) / height.max(1.0)).clamp(0.0, 1.0) as f64,
        )
    }

    /// The corner of `crop` within reach of the pointer, in preview points.
    fn crop_handle_at(&self, crop: Crop, x: f64, y: f64) -> Option<CropHandle> {
        let (width, height) = (self.stage_image.2 as f64, self.stage_image.3 as f64);
        let reach = CROP_HANDLE_REACH as f64;
        [
            (CropHandle::TopLeft, crop.x, crop.y),
            (CropHandle::TopRight, crop.x + crop.width, crop.y),
            (CropHandle::BottomLeft, crop.x, crop.y + crop.height),
            (CropHandle::BottomRight, crop.x + crop.width, crop.y + crop.height),
        ]
        .into_iter()
        .find(|(_, cx, cy)| {
            let (dx, dy) = (x * width - cx * width, y * height - cy * height);
            (dx * dx + dy * dy).sqrt() <= reach
        })
        .map(|(handle, _, _)| handle)
    }

    fn crop_down(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        let (x, y) = self.normalised_point(x, y);
        if let Some(crop) = self.crop {
            if let Some(handle) = self.crop_handle_at(crop, x, y) {
                self.crop_drag = Some(CropDrag::Resize { handle, start: crop });
                cx.notify();
                return;
            }
            let inside = x >= crop.x
                && x <= crop.x + crop.width
                && y >= crop.y
                && y <= crop.y + crop.height;
            if inside {
                self.crop_drag = Some(CropDrag::Move {
                    anchor: (x, y),
                    start: crop,
                });
                cx.notify();
                return;
            }
        }
        self.crop_drag = Some(CropDrag::New { anchor: (x, y), previous: self.crop });
        self.crop = Some(Crop {
            x,
            y,
            width: 0.0,
            height: 0.0,
        });
        cx.notify();
    }

    fn crop_move(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.crop_drag else {
            return;
        };
        let (x, y) = self.normalised_point(x, y);
        self.crop = Some(match drag {
            CropDrag::New { anchor, .. } => Crop {
                x: anchor.0.min(x),
                y: anchor.1.min(y),
                width: (x - anchor.0).abs(),
                height: (y - anchor.1).abs(),
            },
            CropDrag::Move { anchor, start } => Crop {
                x: (start.x + x - anchor.0).clamp(0.0, 1.0 - start.width),
                y: (start.y + y - anchor.1).clamp(0.0, 1.0 - start.height),
                ..start
            },
            CropDrag::Resize { handle, start } => {
                let (mut left, mut top) = (start.x, start.y);
                let (mut right, mut bottom) = (start.x + start.width, start.y + start.height);
                match handle {
                    CropHandle::TopLeft => {
                        left = x.min(right - MIN_CROP);
                        top = y.min(bottom - MIN_CROP);
                    }
                    CropHandle::TopRight => {
                        right = x.max(left + MIN_CROP);
                        top = y.min(bottom - MIN_CROP);
                    }
                    CropHandle::BottomLeft => {
                        left = x.min(right - MIN_CROP);
                        bottom = y.max(top + MIN_CROP);
                    }
                    CropHandle::BottomRight => {
                        right = x.max(left + MIN_CROP);
                        bottom = y.max(top + MIN_CROP);
                    }
                }
                let (left, right) = (left.clamp(0.0, 1.0), right.clamp(0.0, 1.0));
                let (top, bottom) = (top.clamp(0.0, 1.0), bottom.clamp(0.0, 1.0));
                Crop {
                    x: left,
                    y: top,
                    width: right - left,
                    height: bottom - top,
                }
            }
        });
        cx.notify();
    }

    /// A press that never moved is not a crop; the rectangle that was there
    /// before it stays.
    fn crop_up(&mut self, cx: &mut Context<Self>) {
        if let Some(CropDrag::New { previous, .. }) = self.crop_drag
            && self
                .crop
                .is_some_and(|crop| crop.width < MIN_CROP || crop.height < MIN_CROP)
        {
            self.crop = previous;
        }
        if self.crop_drag.take().is_some() {
            self.remember();
        }
        cx.notify();
    }

    fn reset_crop(&mut self, cx: &mut Context<Self>) {
        self.crop = None;
        self.crop_drag = None;
        self.remember();
        cx.notify();
    }

    fn set_output(&mut self, output: Output, cx: &mut Context<Self>) {
        self.output = output;
        self.remember();
        cx.notify();
    }

    /// Saves the format and crop for the next capture. Called once a change is
    /// finished, not on every pointer move of a crop drag.
    fn remember(&mut self) {
        let result = settings::save(&Settings {
            output: self.output,
            crop: self.crop,
        });
        self.settings_error = result.err().map(|error| format!("settings not saved: {error}"));
        if let Some(error) = &self.settings_error {
            self.status = Some(Status::Error(error.clone()));
        }
    }

    /// Moves the weight by how far the pointer travelled since the drag began:
    /// the slider's full height spans the whole range. Logarithmic, so the
    /// fine widths people annotate with get most of the travel.
    fn drag_weight(&mut self, y: f32, cx: &mut Context<Self>) {
        let Some((start_y, start_fraction)) = self.weight_drag else {
            return;
        };
        let fraction = (start_fraction + (start_y - y) / SLIDER_HEIGHT).clamp(0.0, 1.0);
        let (min, max) = (MIN_BRUSH_WIDTH.ln(), MAX_BRUSH_WIDTH.ln());
        self.brush_width = (min + (max - min) * fraction).exp().round();
        cx.notify();
    }

    fn weight_fraction(&self) -> f32 {
        let (min, max) = (MIN_BRUSH_WIDTH.ln(), MAX_BRUSH_WIDTH.ln());
        ((self.brush_width.ln() - min) / (max - min)).clamp(0.0, 1.0)
    }

    fn nudge_brush_width(&mut self, direction: f32, cx: &mut Context<Self>) {
        let next = if direction > 0.0 {
            self.brush_width * 1.15 + 1.0
        } else {
            self.brush_width / 1.15 - 1.0
        };
        self.brush_width = next.clamp(MIN_BRUSH_WIDTH, MAX_BRUSH_WIDTH);
        cx.notify();
    }

    fn undo_stroke(&mut self, cx: &mut Context<Self>) {
        if self.live.take().is_some() {
            self.live_started = None;
            self.drop_live_overlay();
        } else {
            self.paint.undo();
        }
        self.rebuild_overlay(cx);
    }

    fn redo_stroke(&mut self, cx: &mut Context<Self>) {
        if self.live.is_none() {
            self.paint.redo();
            self.rebuild_overlay(cx);
        }
    }

    /// On-screen size of the annotation layer. Normalised strokes make the
    /// preview and the native export the same document rendered twice.
    fn overlay_size(&self) -> (u32, u32) {
        let (width, height) = if self.native_size.0 > 0 && self.native_size.1 > 0 {
            self.native_size
        } else {
            self.frame_size
        };
        assert!(width > 0 && height > 0, "the capture has no size");
        let scaled = width.min(PREVIEW_WIDTH as u32);
        let scaled_height = ((scaled as f64 * height as f64 / width as f64).round() as u32).max(1);
        (scaled, scaled_height)
    }

    /// While a stroke is under the pointer it is previewed at half resolution:
    /// the area it covers is re-rasterised every event, and this quarters that
    /// work. On release the stroke is rebuilt into the full layer.
    fn live_size(&self) -> (u32, u32) {
        let (width, height) = self.overlay_size();
        ((width / 2).max(1), (height / 2).max(1))
    }

    /// Native size of the capture, which is what the daemon composites against.
    fn export_size(&self) -> (u32, u32) {
        if self.native_size.0 > 0 && self.native_size.1 > 0 {
            self.native_size
        } else {
            (self.frame_size.0.max(1), self.frame_size.1.max(1))
        }
    }

    /// The finished strokes. Rebuilt when they change, not per pointer move:
    /// a full layer costs tens of milliseconds, and while the UI thread is
    /// busy AppKit merges mouse events, which makes strokes coarse.
    fn rebuild_overlay(&mut self, cx: &mut Context<Self>) {
        let erasing = self.live.as_ref().filter(|live| live.kind() == BrushKind::Eraser);
        if self.paint.is_empty() && erasing.is_none() {
            if let Some(overlay) = self.overlay.take() {
                self.graveyard.push(overlay);
            }
            cx.notify();
            return;
        }

        let (width, height) = self.overlay_size();
        let pixels = rgba_to_bgra(self.paint.render_rgba(width, height, erasing));
        match build_image(RawImage {
            width,
            height,
            pixels,
        }) {
            Ok(image) => {
                if let Some(previous) = self.overlay.replace(image) {
                    self.graveyard.push(previous);
                }
            }
            Err(message) => self.status = Some(Status::Error(message)),
        }
        cx.notify();
    }

    /// The stroke under the pointer. Ink only redraws the area it covers; an
    /// eraser has to redraw the strokes it removes, so it rebuilds the layer.
    fn rebuild_live(&mut self, cx: &mut Context<Self>) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        if live.kind() == BrushKind::Eraser {
            self.rebuild_overlay(cx);
            return;
        }
        let (width, height) = self.live_size();
        let Some(region) = live.render_region(width, height) else {
            return;
        };
        let rect = (region.x, region.y, region.width, region.height);
        match build_image(RawImage {
            width: region.width,
            height: region.height,
            pixels: rgba_to_bgra(region.pixels),
        }) {
            Ok(image) => {
                if let Some((previous, _)) = self.live_overlay.replace((image, rect)) {
                    self.graveyard.push(previous);
                }
            }
            Err(message) => self.status = Some(Status::Error(message)),
        }
        cx.notify();
    }

    fn drop_live_overlay(&mut self) {
        if let Some((live, _)) = self.live_overlay.take() {
            self.graveyard.push(live);
        }
    }

    /// Maps a window-space pointer onto the preview, in points from its
    /// top-left corner. The brush dynamics are tuned in pointer units, so the
    /// gesture runs in these and the stroke is scaled when it is rasterised.
    fn stage_point(&self, x: f32, y: f32) -> (f64, f64) {
        let (left, top, width, height) = self.stage_image;
        (
            (x - left).clamp(0.0, width) as f64,
            (y - top).clamp(0.0, height) as f64,
        )
    }

    fn draw_begin(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        self.colors_open = false;
        if self.exiting || self.live.is_some() || self.frame.is_none() || self.progress < 1.0 {
            return;
        }
        let (px, py) = self.stage_point(x, y);
        let kind = if self.draw_eraser {
            BrushKind::Eraser
        } else {
            BrushKind::Stroke
        };
        let mut point = Point::at(px, py);
        point.time = Some(0.0);
        self.stop_playback();
        self.live_started = Some(Instant::now());
        self.live = Some(StrokeSession::new(
            kind,
            Brush::ink(self.brush_width as f64),
            self.brush_color,
            point,
            (self.stage_image.2 as f64, self.stage_image.3 as f64),
        ));
        self.rebuild_live(cx);
    }

    fn draw_move(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        let (px, py) = self.stage_point(x, y);
        let time = self
            .live_started
            .map(|started| started.elapsed().as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let mut point = Point::at(px, py);
        point.time = Some(time);
        live.push(&[point]);
        self.rebuild_live(cx);
    }

    fn finish_stroke(&mut self, cx: &mut Context<Self>) {
        let Some(live) = self.live.take() else {
            return;
        };
        self.live_started = None;
        self.paint.push(live.finish());
        self.drop_live_overlay();
        self.rebuild_overlay(cx);
    }

    fn stop_playback(&mut self) {
        self.playing = false;
        self.ticker = None;
        self.playback_clock = None;
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if self.playing {
            self.stop_playback();
            // The paused frame should be the sharp one.
            self.request_frame(cx);
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
        let lean = self.playing || self.drag.is_some();
        self.requested_lean = lean;
        let client = self.client.clone();

        cx.spawn(async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move {
                    let mut request = Request::new(Command::PreviewFrame);
                    request.seconds = Some(seconds);
                    request.anchor = anchor;
                    request.width = Some(if lean { PLAYBACK_WIDTH } else { PREVIEW_WIDTH });
                    let traced = trace::start();
                    let reply = client.send(&request);
                    trace::round_trip(traced);
                    reply
                })
                .await;

            let traced = trace::start();
            this.update(cx, |view, cx| view.received(reply, cx)).ok();
            trace::received(traced);
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
                        if let Some(previous) = self.frame.replace(image) {
                            self.graveyard.push(previous);
                        }
                        if matches!(self.status, Some(Status::Error(_))) {
                            self.status = None;
                        }
                        // The entrance animates real content, so it starts when
                        // the first frame lands rather than on the placeholder.
                        if self.progress == 0.0 {
                            crate::activate::start_fade();
                            self.animate_in(cx);
                        }
                        // The strip is a full second of decoding on the daemon's
                        // actor; asking for it later keeps it from delaying the
                        // first frame.
                        if self.thumbnails.is_empty() {
                            self.fetch_thumbnails(cx);
                        }
                    }
                    Err(message) => {
                        self.status = Some(Status::Error(message));
                        self.reveal_without_frame();
                    }
                }
            }
            Ok(Reply::Images(_)) => {
                self.status = Some(Status::Error("expected a frame".into()));
                self.reveal_without_frame();
            }
            Ok(Reply::Json(response)) => {
                self.status = Some(Status::Error(
                    response
                        .error
                        .unwrap_or_else(|| "the daemon sent no frame".into()),
                ));
                self.reveal_without_frame();
            }
            Err(error) => {
                // Nothing to animate into; settle into the resting layout.
                self.status = Some(Status::Error(error.to_string()));
                self.reveal_without_frame();
            }
        }

        // Releasing while a lean request is in flight would otherwise leave the
        // soft scrub frame on screen until the next seek: nothing is stale, so
        // nothing asks again.
        let lean = self.playing || self.drag.is_some();
        if self.requested_lean && !lean {
            self.request_frame(cx);
            return;
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
    /// Exporting with Image selected saves the frame under the playhead.
    ///
    /// The overlay closes at once and the render runs behind it, reported by
    /// a toast: waiting in the overlay for a re-encode could take seconds.
    fn render_clip(&mut self, copy: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            self.status = Some(Status::Info("still rendering the last one…".into()));
            cx.notify();
            return;
        }
        let frame = !copy && self.output == Output::Image;
        if !frame && self.selection_duration() < 0.05 {
            self.status = Some(Status::Error("select at least a moment".into()));
            cx.notify();
            return;
        }

        self.busy = Some(if copy { Busy::Copy } else { Busy::Export });
        self.stop_playback();
        if self.live.is_some() {
            self.finish_stroke(cx);
        }

        let range = ClipRange {
            from: self.sel_from,
            to: self.sel_to,
        };
        let seconds = self.playhead;
        let format = self.output.clip_format();
        let anchor = self.anchor;
        let client = self.client.clone();
        let annotations = (!self.paint.is_empty()).then(|| self.paint.clone());
        let size = self.export_size();
        let crop = self.crop;

        let kind = if frame {
            "PNG"
        } else if format == ExportFormat::Gif {
            "GIF"
        } else {
            "clip"
        };
        self.render_progress = None;
        self.toast_generation = self.toast_generation.wrapping_add(1);
        self.toast = Some(Toast {
            text: format!("{} {kind}…", if copy { "Copying" } else { "Saving" }),
            error: false,
            path: None,
        });
        self.status = None;
        self.hide(cx);
        self.poll_progress(cx);

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let overlay = write_overlay(annotations.as_ref(), size)?;
                    let mut request = Request::new(if frame {
                        Command::ExportFrame
                    } else if copy {
                        Command::CopyClip
                    } else {
                        Command::ExportClip
                    });
                    if frame {
                        request.seconds = Some(seconds);
                    } else {
                        request.range = Some(range);
                        request.format = Some(format);
                    }
                    request.anchor = anchor;
                    request.overlay_path = overlay
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned());
                    request.crop = crop;
                    let result = ipc::call(&client, request).map_err(|error| error.to_string());
                    if let Some(path) = overlay {
                        let _ = std::fs::remove_file(path);
                    }
                    result
                })
                .await;

            this.update(cx, |view, cx| {
                view.busy = None;
                view.render_progress = None;
                let (text, error, path) = match result {
                    Ok(response) => match response.clip {
                        Some(clip) if copy => (
                            format!(
                                "Copied {:.1}s {kind} · {:.1} MB",
                                clip.duration,
                                clip.bytes as f64 / 1_048_576.0
                            ),
                            false,
                            None,
                        ),
                        Some(clip) if frame => (
                            format!(
                                "Saved {}×{} PNG · {:.1} MB",
                                clip.width,
                                clip.height,
                                clip.bytes as f64 / 1_048_576.0
                            ),
                            false,
                            Some(clip.path),
                        ),
                        Some(clip) => (
                            format!(
                                "Saved {:.1}s {kind} · {:.1} MB",
                                clip.duration,
                                clip.bytes as f64 / 1_048_576.0
                            ),
                            false,
                            Some(clip.path),
                        ),
                        None => ("no clip was returned".into(), true, None),
                    },
                    Err(error) => (
                        format!("{} failed: {error}", if copy { "Copy" } else { "Export" }),
                        true,
                        None,
                    ),
                };
                if let Some(path) = &path {
                    view.last_clip = Some(path.clone());
                }
                if view.overlay_open {
                    // Reopened while rendering: report in the bar, as the
                    // overlay always has.
                    view.toast = None;
                    view.status = Some(if error { Status::Error(text) } else { Status::Info(text) });
                } else {
                    view.toast = Some(Toast { text, error, path });
                    view.dismiss_toast_after(if error { TOAST_FAILED } else { TOAST_DONE }, cx);
                }
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
    fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    /// Closing copies what the Image / MP4 / GIF switch says: the frame under
    /// the playhead at native resolution, or the range as a clip rendered
    /// behind a toast. The overlay hides at once either way.
    fn close(&mut self, cx: &mut Context<Self>) {
        if self.copy_failed {
            self.hide(cx);
            return;
        }
        if self.closing {
            return;
        }
        self.stop_playback();

        // A clip copies in the background behind a toast, like Export.
        if self.output != Output::Image {
            self.render_clip(true, cx);
            return;
        }
        self.closing = true;

        // Hide now and copy behind: waiting for the daemon to decode and encode
        // made the overlay sit there for half a second after Escape.
        let annotations = (!self.paint.is_empty()).then(|| self.paint.clone());
        let size = self.export_size();
        let crop = self.crop;
        self.hide(cx);
        let seconds = self.playhead;
        let anchor = self.anchor;
        let client = self.client.clone();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let overlay = write_overlay(annotations.as_ref(), size)?;
                    let mut request = Request::new(Command::CopyFrame);
                    request.seconds = Some(seconds);
                    request.anchor = anchor;
                    request.overlay_path = overlay
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned());
                    request.crop = crop;
                    let result = ipc::call(&client, request).map_err(|error| error.to_string());
                    if let Some(path) = overlay {
                        let _ = std::fs::remove_file(path);
                    }
                    result
                })
                .await;

            this.update(cx, |view, cx| {
                // The overlay is already hidden, so a late failure can only be
                // reported the next time it opens.
                if let Err(error) = result {
                    view.copy_failed = true;
                    view.status = Some(Status::Error(format!("last copy failed: {error}")));
                }
                cx.notify();
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
    /// | , / .          | one frame back / forward               |
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
        // The crop is already in effect, so Escape only puts the tool down;
        // everything else, Enter included, works as usual.
        if self.cropping && event.keystroke.key == "escape" {
            self.end_crop(cx);
            cx.stop_propagation();
            return;
        }
        let modifiers = &event.keystroke.modifiers;
        let frame = 1.0 / self.fps;
        let step = if modifiers.shift { 1.0 } else { frame };

        match event.keystroke.key.as_str() {
            "escape" => self.hide(cx),
            "space" => self.toggle_play(cx),
            // Enter copies whatever the switch says and dismisses; Escape just
            // dismisses.
            // `secondary` is Cmd on macOS and Ctrl elsewhere.
            "enter" if modifiers.secondary() => self.export(cx),
            "enter" => self.close(cx),
            "c" if modifiers.secondary() => self.copy_clip(cx),
            "left" if modifiers.alt => self.shift_range(1.0, cx),
            "right" if modifiers.alt => self.shift_range(-1.0, cx),
            "left" => self.nudge(step, cx),
            "right" => self.nudge(-step, cx),
            // Editing convention: comma steps back a frame, period forward.
            "," | "comma" => self.nudge(frame, cx),
            "." | "period" => self.nudge(-frame, cx),
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
            "b" => self.select_eraser(false, cx),
            "e" => self.select_eraser(true, cx),
            "[" | "bracketleft" => self.nudge_brush_width(-1.0, cx),
            "]" | "bracketright" => self.nudge_brush_width(1.0, cx),
            "z" if modifiers.platform && modifiers.shift => self.redo_stroke(cx),
            "z" if modifiers.platform => self.undo_stroke(cx),
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
        self.request_estimate(cx);
        cx.notify();
    }

    fn mark_end(&mut self, cx: &mut Context<Self>) {
        self.sel_to = self.playhead.clamp(0.0, self.span - 0.1);
        self.sel_from = self.sel_from.max(self.sel_to + 0.1).min(self.span);
        self.request_estimate(cx);
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
            self.request_estimate(cx);
        }
        cx.notify();
    }
}

// MARK: - Rendering

impl Render for Rewind {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let traced = trace::start();
        // Anything queued from the last frame is no longer referenced by the
        // tree about to be replaced, so it can go now.
        for image in std::mem::take(&mut self.graveyard) {
            cx.drop_image(image, Some(window));
        }
        trace::render(traced);

        // Windows: the window is cloaked until the first real frame is on
        // screen. This render draws it; uncloak one frame later, once it has
        // been presented, so the previous session's surface never shows.
        if crate::activate::take_reveal() {
            // `request_animation_frame` needs a view being rendered, which a
            // frame callback is not; `refresh` asks for the next frame without.
            window.on_next_frame(|window, _| {
                window.on_next_frame(|_, _| crate::activate::reveal());
                window.refresh();
            });
            window.request_animation_frame();
        }

        self.scale = window.scale_factor();
        if self.toast_shown {
            return self.toast_view(cx);
        }

        // Nothing else in the overlay is focusable, and the window can lose key
        // status when another app activates itself, which silently drops Space,
        // Enter and Escape. `focus_until` bounds the retries: re-asserting focus
        // unconditionally invalidates the window every frame and pins a core.
        if let Some(until) = self.focus_until {
            let ready = self.focus.is_focused(window)
                && crate::activate::is_active()
                && crate::activate::is_key_window();
            if ready || Instant::now() >= until {
                self.focus_until = None;
            } else {
                window.focus(&self.focus);
                crate::activate::activate();
            }
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
        let exit = self.exit_progress;
        // Cubic ease-in: it leaves gently and then gets out of the way.
        let exit_ease = exit * exit * exit;
        let scrim_alpha = ((SCRIM_LOADING_ALPHA + (SCRIM_ALPHA - SCRIM_LOADING_ALPHA) * eased)
            * (1.0 - exit_ease)
            * 255.0)
            .round() as u32;
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

        // Exit: the preview settles inward while the furniture slides away.
        // Geometric rather than a fade, so dismissing reads as the overlay
        // leaving rather than the screen blinking.
        let (image_width, image_height, image_left, image_top) = {
            let zoom = 1.0 - 0.07 * exit_ease;
            let width = image_width * zoom;
            let height = image_height * zoom;
            (
                width,
                height,
                image_left + (image_width - width) / 2.0,
                image_top + (image_height - height) / 2.0,
            )
        };

        // The bar follows, arriving from below the screen edge and leaving the
        // same way.
        let bar_progress = ((eased - 0.35) / 0.65).clamp(0.0, 1.0);
        let bar_draw_top = bar_top
            + (1.0 - bar_progress) * (BAR_HEIGHT + MARGIN * 2.0)
            + exit_ease * (BAR_HEIGHT + MARGIN * 2.0);

        let span = self.span;
        let x_for = |seconds: f64| {
            track_left + track_width * (1.0 - (seconds / span).clamp(0.0, 1.0)) as f32
        };

        let from_x = x_for(self.sel_from);
        let to_x = x_for(self.sel_to);
        let playhead_x = x_for(self.playhead);

        // While the range is being dragged, show what it covers: both ends when
        // the whole range moves, just the end being dragged when one edge does.
        let edge_mode = match self.drag {
            Some(Drag::Range { .. }) => Some(EdgeMode::Both),
            Some(Drag::From) => Some(EdgeMode::First),
            Some(Drag::To) => Some(EdgeMode::Last),
            _ => None,
        };
        let stage = if let (Some(mode), Some(frames)) = (edge_mode, self.edge_frames.as_ref()) {
            self.edge_view(
                mode,
                frames,
                stage_left,
                stage_top,
                stage_width,
                stage_height,
            )
        } else {
            match &self.frame {
                Some(image) => {
                    self.stage_image = (image_left, image_top, image_width, image_height);
                    let overlay = self.overlay.clone();
                    // Live-layer pixels to stage points.
                    let layer = self.live_size();
                    let live = self.live_overlay.clone().map(|(image, (x, y, w, h))| {
                        let sx = image_width / layer.0 as f32;
                        let sy = image_height / layer.1 as f32;
                        (image, (x as f32 * sx, y as f32 * sy, w as f32 * sx, h as f32 * sy))
                    });
                    div()
                        .absolute()
                        .left(px(image_left))
                        .top(px(image_top))
                        .w(px(image_width))
                        .h(px(image_height))
                        .rounded(px(image_radius))
                        .overflow_hidden()
                        .border_1()
                        .border_color(rgba(BORDER))
                        // A little fade with the zoom: the capture recedes
                        // rather than blinking out.
                        .opacity(1.0 - 0.5 * exit_ease)
                        .child(
                            img(ImageSource::Render(image.clone()))
                                .object_fit(ObjectFit::Fill)
                                .w(px(image_width))
                                .h(px(image_height)),
                        )
                        .when_some(overlay, |this, overlay| {
                            this.child(
                                div()
                                    .absolute()
                                    .left(px(0.0))
                                    .top(px(0.0))
                                    .w(px(image_width))
                                    .h(px(image_height))
                                    .child(
                                        img(ImageSource::Render(overlay))
                                            .object_fit(ObjectFit::Fill)
                                            .w(px(image_width))
                                            .h(px(image_height)),
                                    ),
                            )
                        })
                        .when_some(live, |this, (image, (x, y, w, h))| {
                            this.child(
                                div()
                                    .absolute()
                                    .left(px(x))
                                    .top(px(y))
                                    .w(px(w))
                                    .h(px(h))
                                    .child(
                                        img(ImageSource::Render(image))
                                            .object_fit(ObjectFit::Fill)
                                            .w(px(w))
                                            .h(px(h)),
                                    ),
                            )
                        })
                        .when_some(self.crop, |this, crop| {
                            let x0 = crop.x as f32 * image_width;
                            let y0 = crop.y as f32 * image_height;
                            let w = crop.width as f32 * image_width;
                            let h = crop.height as f32 * image_height;
                            let shade = rgba(0x08080CB0);
                            let mut view = this
                                .child(shade_rect(0.0, 0.0, image_width, y0, shade))
                                .child(shade_rect(0.0, y0 + h, image_width, image_height - y0 - h, shade))
                                .child(shade_rect(0.0, y0, x0, h, shade))
                                .child(shade_rect(x0 + w, y0, image_width - x0 - w, h, shade))
                                .child(
                                    div()
                                        .absolute()
                                        .left(px(x0))
                                        .top(px(y0))
                                        .w(px(w))
                                        .h(px(h))
                                        .border_2()
                                        .border_color(rgba(0xFFFFFFFF)),
                                );
                            if self.cropping {
                                for (hx, hy) in [(x0, y0), (x0 + w, y0), (x0, y0 + h), (x0 + w, y0 + h)] {
                                    view = view.child(
                                        div()
                                            .absolute()
                                            .left(px(hx - 4.0))
                                            .top(px(hy - 4.0))
                                            .size(px(8.0))
                                            .rounded_full()
                                            .bg(rgba(0xFFFFFFFF)),
                                    );
                                }
                            }
                            view
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, event: &MouseDownEvent, _window, cx| {
                                let (x, y) = (event.position.x.into(), event.position.y.into());
                                if view.cropping {
                                    view.crop_down(x, y, cx);
                                } else {
                                    view.draw_begin(x, y, cx);
                                }
                            }),
                        )
                        .into_any_element()
                }

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

        // Centred in the space between the top inset and the transport bar.
        let toolbar_top = stage_top + ((bar_top - GAP - stage_top) - TOOLBAR_HEIGHT).max(0.0) / 2.0;
        // Slides in from the left edge on the same timing as the bar rising
        // from below.
        let toolbar_left = MARGIN
            - (1.0 - bar_progress) * (TOOLBAR_WIDTH + MARGIN * 2.0)
            - exit_ease * (TOOLBAR_WIDTH + MARGIN * 2.0);
        let palette = self.brush_palette(toolbar_left, toolbar_top, cx);
        let crop_pill = self.cropping.then(|| self.crop_actions(bar_top, cx));

        // Moves and releases are handled on the root, which covers the whole
        // window, so a drag keeps tracking after the pointer leaves the bar.
        div()
            .relative()
            .size_full()
            .bg(rgba(SCRIM << 8 | scrim_alpha))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_move(
                cx.listener(move |view, event: &MouseMoveEvent, _window, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        if view.weight_drag.is_some() {
                            view.drag_weight(event.position.y.into(), cx);
                        } else if view.cropping && view.crop_drag.is_some() {
                            view.crop_move(event.position.x.into(), event.position.y.into(), cx);
                        } else if view.live.is_some() {
                            view.draw_move(event.position.x.into(), event.position.y.into(), cx);
                        } else if view.drag.is_some() {
                            view.timeline_drag(
                                event.position.x.into(),
                                track_origin,
                                track_width,
                                cx,
                            );
                        }
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _: &MouseUpEvent, _window, cx| {
                    if view.weight_drag.take().is_some() {
                        return;
                    }
                    if view.cropping {
                        view.crop_up(cx);
                        return;
                    }
                    if view.live.is_some() {
                        view.finish_stroke(cx);
                        return;
                    }
                    if view.drag.take().is_some() {
                        // The drag used the lean frame; restore the sharp one now
                        // that nothing is moving.
                        view.request_frame(cx);
                        cx.notify();
                    }
                }),
            )
            .child(stage)
            .when_some(crop_pill, |this, pill| this.child(pill))
            .child(palette)
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
            .into_any_element()
    }
}

// MARK: - The bar

impl Rewind {
    /// The whole window while a render runs behind the closed overlay: what is
    /// being made, how far along it is, then the result. A saved file is
    /// revealed by clicking; any click dismisses.
    fn toast_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(toast) = &self.toast else {
            return div().into_any_element();
        };
        let busy = self.busy.is_some();
        let detail = if busy {
            self.render_progress
                .map(|progress| format!("{:.0}%", progress * 100.0))
                .unwrap_or_default()
        } else if toast.path.is_some() {
            "Reveal".to_string()
        } else {
            String::new()
        };
        let progress = self.render_progress.filter(|_| busy);

        panel()
            .size_full()
            .px(px(16.0))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(7.0))
            .when(!busy, |this| this.cursor_pointer())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .text_color(rgba(if toast.error { DANGER } else { TEXT }))
                            .child(toast.text.clone()),
                    )
                    .child(div().text_xs().text_color(rgba(MUTED)).child(detail)),
            )
            .when_some(progress, |this, progress| {
                this.child(
                    div()
                        .w_full()
                        .h(px(3.0))
                        .rounded_full()
                        .bg(rgba(GROUP))
                        .child(
                            div()
                                .h_full()
                                .w(relative(progress))
                                .rounded_full()
                                .bg(rgba(ACCENT)),
                        ),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, _window, cx| {
                    if view.busy.is_some() {
                        return;
                    }
                    if let Some(path) = view.toast.as_ref().and_then(|toast| toast.path.clone()) {
                        reveal(&path);
                    }
                    view.dismiss_toast(cx);
                }),
            )
            .into_any_element()
    }

    /// The hint while the crop tool is selected. A drawn crop is already in
    /// effect, so the only action is taking it away.
    fn crop_actions(&self, bar_top: f32, cx: &mut Context<Self>) -> AnyElement {
        let hint = if self.crop.is_some() {
            "Drag the crop or its corners · Esc to draw"
        } else {
            "Drag over the capture to crop · Esc to draw"
        };
        div()
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(bar_top - GAP - 38.0))
            .flex()
            .justify_center()
            .child(
                panel()
                    .h(px(38.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        div()
                            .px(px(6.0))
                            .text_xs()
                            .text_color(rgba(MUTED))
                            .child(hint),
                    )
                    .when(self.crop.is_some(), |pill| {
                        pill.child(label_button("Reset").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _: &MouseDownEvent, _window, cx| {
                                view.reset_crop(cx)
                            }),
                        ))
                    }),
            )
            .into_any_element()
    }

    /// The drawing toolbar: separate pills for the tools, history, weight and
    /// colour, stacked down the left edge.
    fn brush_palette(&self, left: f32, top: f32, cx: &mut Context<Self>) -> AnyElement {
        let tools = toolbar_pill()
            .child(
                tool_button(icons::PEN, !self.draw_eraser && !self.cropping, true).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, _: &MouseDownEvent, _window, cx| {
                        view.select_eraser(false, cx)
                    }),
                ),
            )
            .child(
                tool_button(icons::ERASER, self.draw_eraser && !self.cropping, true).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, _: &MouseDownEvent, _window, cx| {
                        view.select_eraser(true, cx)
                    }),
                ),
            )
            .child(
                tool_button(icons::CROP, self.cropping, true).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, _: &MouseDownEvent, _window, cx| view.begin_crop(cx)),
                ),
            );

        let can_undo = self.live.is_some() || self.paint.can_undo();
        let can_redo = self.live.is_none() && self.paint.can_redo();
        let history = toolbar_pill()
            .child(tool_button(icons::UNDO, false, can_undo).on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, _window, cx| view.undo_stroke(cx)),
            ))
            .child(tool_button(icons::REDO, false, can_redo).on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, _window, cx| view.redo_stroke(cx)),
            ));

        let fill = SLIDER_HEIGHT * self.weight_fraction();
        // Pressed is the drag, not the element's own active state: a drag keeps
        // going after the pointer leaves the slider.
        let pressed = self.weight_drag.is_some();
        let slider = div()
            .id("weight")
            .group("weight")
            .relative()
            .w(px(TOOL_BUTTON))
            .h(px(SLIDER_HEIGHT))
            .rounded(px(CONTROL_RADIUS))
            .overflow_hidden()
            .bg(rgba(SUNKEN))
            .border_1()
            .border_color(rgba(if pressed { 0xFFFFFF47 } else { BORDER }))
            .when(!pressed, |this| this.hover(|style| style.border_color(rgba(0xFFFFFF29))))
            .cursor(CursorStyle::ResizeUpDown)
            .child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .bottom(px(0.0))
                    .w_full()
                    .h(px(fill))
                    // overflow_hidden clips to a rectangle, not the track's
                    // rounded corners, so the fill rounds its own; its top
                    // rounds too once it reaches the track's top curve.
                    .rounded_b(px(CONTROL_RADIUS - 1.0))
                    .when(fill > SLIDER_HEIGHT - CONTROL_RADIUS, |this| {
                        this.rounded_t(px(CONTROL_RADIUS - 1.0))
                    })
                    .bg(rgba(if pressed { 0x7C7C86FF } else { 0x5A5A62FF }))
                    .when(!pressed, |this| {
                        this.group_hover("weight", |style| style.bg(rgba(0x6A6A73FF)))
                    }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(6.0))
                    .w_full()
                    .flex()
                    .justify_center()
                    .text_size(rems(0.625))
                    .text_color(rgba(if pressed { TEXT } else { MUTED }))
                    .group_hover("weight", |style| style.text_color(rgba(TEXT)))
                    .child(format!("{:.0}", self.brush_width)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, event: &MouseDownEvent, _window, _cx| {
                    view.weight_drag = Some((event.position.y.into(), view.weight_fraction()));
                }),
            );
        let weight = toolbar_pill().child(slider);

        let (hex, _) = DRAW_COLORS
            .iter()
            .map(|hex| (*hex, from_srgb_hex(*hex)))
            .find(|(_, color)| *color == self.brush_color)
            .expect("the brush colour is one of the swatches");
        let open = self.colors_open;
        let color = div()
            .id("color")
            .group("color")
            .size(px(COLOR_BUTTON))
            .rounded_full()
            .bg(rgba(if open { 0x2A2A30FF } else { PANEL }))
            .border_1()
            .border_color(rgba(if open { 0xFFFFFF47 } else { BORDER }))
            .shadow_lg()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(rgba(0x26262BFF)).border_color(rgba(0xFFFFFF29)))
            .child(
                div()
                    .size(px(COLOR_BUTTON - 2.0 * TOOLBAR_PADDING - 2.0))
                    .rounded_full()
                    .bg(rgba(hex << 8 | 0xFF))
                    .border_1()
                    .border_color(rgba(if open { 0xFFFFFF80 } else { 0xFFFFFF33 }))
                    .group_hover("color", |style| style.border_color(rgba(0xFFFFFF80))),
            )
            .active(|style| style.opacity(0.85))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, _window, cx| {
                    view.colors_open = !view.colors_open;
                    cx.notify();
                }),
            );

        let column = div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(TOOLBAR_WIDTH))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(TOOLBAR_GAP))
            .child(tools)
            .child(history)
            .child(weight)
            .child(color)
            // Runs after the buttons' own handlers and keeps the press from
            // reaching the stage below. Occluding instead would also hide moves
            // and releases from the root, which ends drags and strokes.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

        if !self.colors_open {
            return column.into_any_element();
        }

        // The swatches open beside the colour button, two rows of four.
        let mut swatches = panel()
            .absolute()
            .left(px(left + TOOLBAR_WIDTH + TOOLBAR_GAP))
            .top(px(top + TOOLBAR_HEIGHT - COLOR_BUTTON - 4.0))
            .p(px(TOOLBAR_PADDING + 4.0))
            .rounded(px(TOOLBAR_RADIUS))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .flex()
            .flex_col()
            .gap(px(6.0));
        for row in DRAW_COLORS.chunks(4) {
            let mut line = div().flex().gap(px(6.0));
            for hex in row {
                let color = from_srgb_hex(*hex);
                let selected = color == self.brush_color;
                line = line.child(
                    div()
                        .id(SharedString::from(format!("swatch-{hex:06x}")))
                        .size(px(SWATCH))
                        .rounded_full()
                        .bg(rgba(*hex << 8 | 0xFF))
                        .border_2()
                        .border_color(rgba(if selected { 0xFFFFFFFF } else { 0xFFFFFF26 }))
                        .cursor_pointer()
                        .when(!selected, |this| {
                            this.hover(|style| style.border_color(rgba(0xFFFFFF99)))
                        })
                        .active(|style| style.opacity(0.75))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |view, _: &MouseDownEvent, _window, cx| {
                                view.brush_color = color;
                                view.color_chosen = true;
                                view.colors_open = false;
                                cx.notify();
                            }),
                        ),
                );
            }
            swatches = swatches.child(line);
        }

        div()
            .absolute()
            .size_full()
            .child(column)
            .child(swatches)
            .into_any_element()
    }

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
            .h(px(GROUP_H))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(transport)
            .child(
                div()
                    .text_xs()
                    .text_color(rgba(MUTED))
                    .child(format!(
                        "{:.1}s selected{}",
                        self.selection_duration(),
                        self.estimated_size()
                    )),
            );
        if !status_text.is_empty() {
            left = left.child(div().text_xs().text_color(status_colour).child(status_text));
        }
        if let Some(path) = &self.last_clip {
            let path = path.clone();
            left = left.child(
                label_button("Reveal").on_mouse_down(MouseButton::Left, move |_, _, _| {
                    reveal(&path)
                }),
            );
        }

        let output_switch = [Output::Image, Output::Mp4, Output::Gif].into_iter().fold(
            group(),
            |switch, output| {
                switch.child(segment(output.label(), self.output == output).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |view, _: &MouseDownEvent, _window, cx| {
                        view.set_output(output, cx)
                    }),
                ))
            },
        );

        let secondary = secondary_button("Copy", "Enter", self.busy == Some(Busy::Copy))
            .on_mouse_down(
                MouseButton::Left,
                // Same as Enter, whose shortcut it shows: copies what the
                // Image / MP4 / GIF switch says, then closes.
                cx.listener(|view, _: &MouseDownEvent, _window, cx| view.close(cx)),
            );

        let right = div()
            .absolute()
            .right(px(BAR_PADDING))
            .top(px(TOOLBAR_Y))
            .h(px(GROUP_H))
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(output_switch)
            .child(secondary)
            .child(
                primary_button("Export", EXPORT_SHORTCUT, self.busy == Some(Busy::Export))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _: &MouseDownEvent, _window, cx| view.export(cx)),
                    ),
            )
            .child(icon_button("✕").on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _: &MouseUpEvent, _window, cx| view.hide(cx)),
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

    /// The range's end frame(s) in the stage: both side by side while the whole
    /// range moves, one filling the stage while a single edge moves.
    fn edge_view(
        &self,
        mode: EdgeMode,
        frames: &(Arc<RenderImage>, Arc<RenderImage>),
        left: f32,
        top: f32,
        width: f32,
        height: f32,
    ) -> AnyElement {
        let slots: &[(usize, &Arc<RenderImage>)] = match mode {
            EdgeMode::Both => &[(0, &frames.0), (1, &frames.1)],
            EdgeMode::First => &[(0, &frames.0)],
            EdgeMode::Last => &[(0, &frames.1)],
        };

        let count = slots.len() as f32;
        let slot_width = (width - GAP * (count - 1.0)) / count;
        let mut row = div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(width))
            .h(px(height));

        for (slot, (_, image)) in slots.iter().enumerate() {
            let (image_width, image_height) = fit(
                self.frame_size.0 as f32,
                self.frame_size.1 as f32,
                slot_width,
                height,
            );
            let x = slot as f32 * (slot_width + GAP) + (slot_width - image_width) / 2.0;
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
                        img(ImageSource::Render(Arc::clone(image)))
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
        .h(px(GROUP_H))
        .px(px(2.0))
        .rounded(px(CONTROL_RADIUS + 1.0))
        .bg(rgba(GROUP))
        .flex()
        .items_center()
        .gap(px(2.0))
}

/// One of the four panes that dim everything outside the crop.
fn shade_rect(x: f32, y: f32, width: f32, height: f32, color: Rgba) -> Div {
    div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(width.max(0.0)))
        .h(px(height.max(0.0)))
        .bg(color)
}

/// One pill of the drawing toolbar.
fn toolbar_pill() -> Div {
    div()
        .w(px(TOOLBAR_WIDTH))
        .p(px(TOOLBAR_PADDING))
        .rounded(px(TOOLBAR_RADIUS))
        .bg(rgba(PANEL))
        .border_1()
        .border_color(rgba(BORDER))
        .shadow_lg()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(TOOL_SPACING))
}

/// One drawing-tool button. The selected tool is a light tile with a dark
/// icon, as in the reference; disabled ones are dimmed and inert.
fn tool_button(icon: &'static str, active: bool, enabled: bool) -> Div {
    let colour = if active {
        0x16161AFF
    } else if enabled {
        TEXT
    } else {
        0xFFFFFF40
    };
    let button = div()
        .size(px(TOOL_BUTTON))
        .rounded(px(CONTROL_RADIUS))
        .flex()
        .items_center()
        .justify_center()
        .child(svg().path(icon).size(px(TOOL_ICON)).text_color(rgba(colour)));
    match (active, enabled) {
        (true, _) => button.bg(rgba(0xE9E9EDFF)),
        (false, true) => button
            .cursor_pointer()
            .hover(|style| style.bg(rgba(0xFFFFFF14))),
        (false, false) => button,
    }
}

fn icon_button(glyph: &'static str) -> Div {
    div()
        .w(px(CONTROL_H))
        .h(px(CONTROL_H))
        .rounded(px(CONTROL_RADIUS))
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(rgba(TEXT))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0xFFFFFF14)))
        .child(glyph)
}

/// One half of a two-way switch.
fn segment(label: &'static str, active: bool) -> Div {
    div()
        .h(px(CONTROL_H))
        .px(px(10.0))
        .rounded(px(CONTROL_RADIUS))
        .flex()
        .items_center()
        .text_xs()
        .text_color(rgba(if active { TEXT } else { MUTED }))
        .bg(rgba(if active { 0xFFFFFF1F } else { 0x00000000 }))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0xFFFFFF14)))
        .child(label)
}

/// The one primary action. A tinted accent rather than a solid fill: a full
/// strength blue block at this size dominated the whole bar.
/// The secondary of the pair: neutral, same box as the primary.
fn secondary_button(label: &'static str, shortcut: &'static str, busy: bool) -> Div {
    div()
        .h(px(CONTROL_H))
        .px(px(CONTROL_PADDING))
        .rounded(px(CONTROL_RADIUS))
        .bg(rgba(if busy { 0xFFFFFF1F } else { 0xFFFFFF12 }))
        .flex()
        .items_center()
        .text_xs()
        .text_color(rgba(TEXT))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0xFFFFFF20)))
        .child(button_label(label, shortcut, MUTED))
}

/// The primary-weight button. `busy` keeps its label and shows progress on the
/// button that was actually pressed.
fn primary_button(label: &'static str, shortcut: &'static str, busy: bool) -> Div {
    div()
        .h(px(CONTROL_H))
        .px(px(CONTROL_PADDING))
        .rounded(px(CONTROL_RADIUS))
        .bg(rgba(if busy { 0x2F7CF666 } else { 0x2F7CF63D }))
        .flex()
        .items_center()
        .text_xs()
        .text_color(rgba(0x7FB2FFFF))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0x2F7CF659)))
        .child(button_label(label, shortcut, 0x7FB2FF99))
}

/// Label plus its shortcut, the shortcut a step quieter.
fn button_label(label: &'static str, shortcut: &'static str, hint: u32) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(7.0))
        .child(label)
        .child(div().text_color(rgba(hint)).child(shortcut))
}

fn label_button(label: &'static str) -> Div {
    div()
        .h(px(CONTROL_H))
        .px(px(CONTROL_PADDING))
        .rounded(px(CONTROL_RADIUS))
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
    // Always fill the stage, upscaling if the frame is smaller than it: the
    // playback preview is 1280 wide and the stage is wider, and capping the
    // scale at 1.0 made the picture shrink as soon as playback started.
    let scale = (max_width / frame_width).min(max_height / frame_height);
    (frame_width * scale, frame_height * scale)
}

/// The paint engine produces RGBA (which is what the daemon composites for
/// export); GPUI's `RenderImage` takes BGRA, like the daemon's preview frames.
fn rgba_to_bgra(mut pixels: Vec<u8>) -> Vec<u8> {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    pixels
}

/// Ink that stands out on what the system theme puts on screen: white on a
/// dark desktop, black on a light one.
fn theme_ink(appearance: WindowAppearance) -> Rgb {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => from_srgb_hex(DRAW_COLORS[0]),
        WindowAppearance::Light | WindowAppearance::VibrantLight => from_srgb_hex(DRAW_COLORS[1]),
    }
}

fn build_image(image: RawImage) -> Result<Arc<RenderImage>, String> {
    let buffer =
        image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(image.width, image.height, image.pixels)
            .ok_or_else(|| "frame size does not match its pixels".to_string())?;

    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// Writes the annotations for the daemon to composite: two big-endian u32
/// dimensions followed by straight RGBA. A file keeps the IPC protocol JSON,
/// even though a native annotation layer is tens of megabytes.
fn write_overlay(
    paint: Option<&Paint>,
    size: (u32, u32),
) -> Result<Option<std::path::PathBuf>, String> {
    let Some(paint) = paint else {
        return Ok(None);
    };
    let (width, height) = (size.0.max(1), size.1.max(1));
    let pixels = paint.render_rgba(width, height, None);
    let path = std::env::temp_dir().join(format!(
        "rewind-annotations-{}-{}.rgba",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));

    let mut data = Vec::with_capacity(8 + pixels.len());
    data.extend_from_slice(&width.to_be_bytes());
    data.extend_from_slice(&height.to_be_bytes());
    data.extend_from_slice(&pixels);
    std::fs::write(&path, data).map_err(|error| format!("cannot write annotations: {error}"))?;
    Ok(Some(path))
}

#[cfg(target_os = "macos")]
const EXPORT_SHORTCUT: &str = "⌘Enter";
#[cfg(not(target_os = "macos"))]
const EXPORT_SHORTCUT: &str = "Ctrl+Enter";

#[cfg(target_os = "macos")]
fn reveal(path: &str) {
    let _ = std::process::Command::new("open")
        .args(["-R", path])
        .spawn();
}

#[cfg(windows)]
fn reveal(path: &str) {
    // `/select,` must be one argument with the path glued on, or Explorer
    // opens the parent folder without selecting anything.
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer")
        .raw_arg(format!("/select,\"{path}\""))
        .spawn();
}

// MARK: - Entry

/// The overlay stays resident so the hotkey only has to show a window that
/// already exists. A resident process costs nothing while hidden if no frames
/// are decoded, which is why the frame is dropped on hide.
pub fn run(show: bool) {
    Application::new().with_assets(crate::icons::Icons).run(move |cx| {
        let display = cx.primary_display();
        let bounds = display
            .as_ref()
            .map(|display| display.bounds())
            .unwrap_or_else(|| Bounds::new(point(px(0.0), px(0.0)), size(px(1440.0), px(900.0))));
        let display_id = display.as_ref().map(|display| display.id());

        let Some(listener) = crate::signal::Listener::bind() else {
            // Another overlay is already listening; it will be the one shown.
            cx.quit();
            return;
        };
        let listener = std::sync::Arc::new(listener);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: None,
                // On Windows a "transparent" titlebar makes GPUI answer
                // WM_NCCALCSIZE itself and inset the client area by the
                // resize-frame thickness (8 px left/right/bottom, 1 px top at
                // 100%), even after the frame styles are stripped: the overlay
                // drew short of every edge. Left opaque, Windows computes the
                // client area, and a WS_POPUP window's is the whole window.
                appears_transparent: !cfg!(windows),
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

        handle
            .update(cx, |_, window, _| crate::activate::remember(window))
            .ok();
        crate::activate::make_borderless();
        crate::activate::make_accessory();

        if show {
            handle
                .update(cx, |view, window, cx| view.show(window, cx))
                .ok();
        }

        cx.spawn(async move |cx| {
            loop {
                let listener = listener.clone();
                let incoming = cx
                    .background_executor()
                    .spawn(async move { listener.accept() })
                    .await;
                if !incoming {
                    break;
                }
                let _ = handle.update(cx, |view, window, cx| view.show(window, cx));
            }
        })
        .detach();
    });
}
