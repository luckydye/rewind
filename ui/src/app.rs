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

const MARGIN: f32 = 24.0;
const GAP: f32 = 16.0;
const BAR_HEIGHT: f32 = 152.0;
const BAR_PADDING: f32 = 16.0;

/// Room for half a tick label beyond each end of the track, so "-0s" is not
/// clipped by the bar's right edge.
const RULER_INSET: f32 = 32.0;
const LABEL_WIDTH: f32 = 44.0;

/// Toolbar row: grouped controls in recessed pills.
const TOOLBAR_Y: f32 = 10.0;
const TOOLBAR_H: f32 = 44.0;

/// Ruler: bare numbers on the panel, with the playhead badge riding on it.
const RULER_Y: f32 = 62.0;
const RULER_H: f32 = 26.0;

/// Content row: the recessed strip the range block lives in.
const ROW_Y: f32 = 94.0;
const ROW_H: f32 = 44.0;
const BLOCK_INSET: f32 = 5.0;

const HANDLE_REACH: f32 = 10.0;
const BADGE_WIDTH: f32 = 58.0;

/// Capture is native-resolution (33 MB per frame on a 4K display). A 2K preview
/// costs ~11 MB and about 40 ms round trip, which keeps scrubbing usable; the
/// exported clip is full resolution either way.
const PREVIEW_WIDTH: i64 = 2048;

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
    activated: bool,
    last_clip: Option<String>,
}

impl Rewind {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let client = Client::new();
        let response = ipc::call(&client, Request::new(Command::Status));
        let state = response
            .as_ref()
            .ok()
            .and_then(|response| response.state.as_ref());

        let span = state
            .map(|state| state.available_seconds.max(0.5))
            .unwrap_or(1.0);
        let sel_from = span.min(10.0);
        let status = response
            .as_ref()
            .err()
            .map(|error| Status::Error(error.to_string()));

        let mut view = Self {
            focus: cx.focus_handle(),
            client,
            span,
            playhead: sel_from,
            sel_from,
            sel_to: 0.0,
            playing: false,
            frame: None,
            frame_size: (16, 9),
            requested: None,
            in_flight: false,
            drag: None,
            exporting: false,
            status,
            ticker: None,
            playback_clock: None,
            activated: false,
            last_clip: None,
        };

        view.request_frame(cx);
        view
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

                        view.playhead -= elapsed;
                        if view.playhead < view.sel_to {
                            // Carry the overshoot so looping does not drift.
                            let overshoot = view.sel_to - view.playhead;
                            let duration = view.selection_duration().max(0.01);
                            view.playhead = view.sel_from - (overshoot % duration);
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
        let client = self.client.clone();

        cx.spawn(async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move {
                    let mut request = Request::new(Command::PreviewFrame);
                    request.seconds = Some(seconds);
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
                    }
                    Err(message) => self.status = Some(Status::Error(message)),
                }
            }
            Ok(Reply::Json(response)) => {
                self.status = Some(Status::Error(
                    response
                        .error
                        .unwrap_or_else(|| "the daemon sent no frame".into()),
                ));
            }
            Err(error) => self.status = Some(Status::Error(error.to_string())),
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
        let client = self.client.clone();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut request = Request::new(Command::ExportClip);
                    request.range = Some(range);
                    ipc::call(&client, request)
                })
                .await;

            this.update(cx, |view, cx| {
                view.exporting = false;
                view.status = Some(match result {
                    Ok(response) => match response.clip {
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

    // MARK: - Input

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => cx.quit(),
            "space" => self.toggle_play(cx),
            "enter" => self.export(cx),
            "left" => self.nudge(0.25, cx),
            "right" => self.nudge(-0.25, cx),
            _ => {}
        }
    }

    fn timeline_down(&mut self, x: f32, left: f32, width: f32, cx: &mut Context<Self>) {
        self.stop_playback();

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
        cx.notify();
    }
}

// MARK: - Rendering

impl Render for Rewind {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.activated {
            self.activated = true;
            cx.spawn(async move |_, cx| {
                crate::activate::activate();
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                crate::activate::activate();
            })
            .detach();
        }

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
        let bar_width = width - MARGIN * 2.0;
        // track_left is relative to the bar (used for layout); track_origin is
        // absolute in the window (used for pointer maths). Mixing the two put
        // the playhead a bar-margin away from the cursor.
        let track_left = BAR_PADDING + RULER_INSET;
        let track_origin = MARGIN + track_left;
        let track_width = (bar_width - (BAR_PADDING + RULER_INSET) * 2.0).max(1.0);

        let stage_left = MARGIN;
        let stage_top = MARGIN;
        let stage_width = (width - MARGIN * 2.0).max(1.0);
        let stage_height = (bar_top - GAP - stage_top).max(1.0);

        let (frame_width, frame_height) = self.frame_size;
        let (image_width, image_height) = fit(
            frame_width as f32,
            frame_height as f32,
            stage_width,
            stage_height,
        );
        let image_left = stage_left + (stage_width - image_width) / 2.0;
        let image_top = stage_top + (stage_height - image_height) / 2.0;

        let span = self.span;
        let x_for =
            |seconds: f64| track_left + track_width * (1.0 - (seconds / span).clamp(0.0, 1.0)) as f32;

        let from_x = x_for(self.sel_from);
        let to_x = x_for(self.sel_to);
        let playhead_x = x_for(self.playhead);

        let stage = match &self.frame {
            Some(image) => div()
                .absolute()
                .left(px(image_left))
                .top(px(image_top))
                .w(px(image_width))
                .h(px(image_height))
                .rounded(px(14.0))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(BORDER))
                .child(
                    img(ImageSource::Render(image.clone()))
                        .w(px(image_width))
                        .h(px(image_height)),
                )
                .into_any_element(),

            None => div()
                .absolute()
                .left(px(image_left))
                .top(px(image_top))
                .w(px(image_width))
                .h(px(image_height))
                .rounded(px(14.0))
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
        };

        // Moves and releases are handled on the root, which covers the whole
        // window, so a drag keeps tracking after the pointer leaves the bar.
        div()
            .relative()
            .size_full()
            .bg(rgba(SCRIM))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
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
                    .text_sm()
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
            .gap(px(16.0))
            .child(transport)
            .child(
                div()
                    .text_sm()
                    .text_color(rgba(MUTED))
                    .child(format!("{:.1}s selected", self.selection_duration())),
            );
        if !status_text.is_empty() {
            left = left.child(div().text_sm().text_color(status_colour).child(status_text));
        }

        let mut secondary = group();
        if let Some(path) = &self.last_clip {
            let path = path.clone();
            secondary = secondary.child(
                label_button("Reveal").on_mouse_down(MouseButton::Left, move |_, _, _| reveal(&path)),
            );
        }
        secondary = secondary.child(label_button("Reset").on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, _: &MouseDownEvent, _window, cx| view.reset_range(cx)),
        ));

        let right = div()
            .absolute()
            .right(px(BAR_PADDING))
            .top(px(TOOLBAR_Y))
            .h(px(TOOLBAR_H))
            .flex()
            .items_center()
            .gap(px(8.0))
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
                cx.listener(|_view, _: &MouseDownEvent, _window, cx| cx.quit()),
            ));

        let block_top = ROW_Y + BLOCK_INSET;
        let block_height = ROW_H - BLOCK_INSET * 2.0;

        panel()
            .absolute()
            .left(px(MARGIN))
            .top(px(bar_top))
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
                    .rounded(px(12.0))
                    .bg(rgba(SUNKEN)),
            )
            .child(
                // The range block. Grabbing its body moves the whole range.
                div()
                    .absolute()
                    .left(px(from_x))
                    .top(px(block_top))
                    .w(px((to_x - from_x).max(2.0)))
                    .h(px(block_height))
                    .rounded(px(8.0))
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
                    .rounded(px(8.0))
                    .bg(rgba(PLAYHEAD))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(rgba(0xFFFFFFFF))
                    .child(format!("−{:.1}s", self.playhead)),
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
                            view.timeline_down(event.position.x.into(), track_origin, track_width, cx)
                        }),
                    ),
            )
            .into_any_element()
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
                    .text_xs()
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
        .rounded(px(20.0))
        .bg(rgba(PANEL))
        .border_1()
        .border_color(rgba(BORDER))
        .shadow_lg()
}

/// A recessed pill that groups related controls, as in the reference.
fn group() -> Div {
    div()
        .h(px(TOOLBAR_H))
        .px(px(4.0))
        .rounded(px(14.0))
        .bg(rgba(GROUP))
        .flex()
        .items_center()
        .gap(px(2.0))
}

fn icon_button(glyph: &'static str) -> Div {
    div()
        .w(px(36.0))
        .h(px(36.0))
        .rounded(px(10.0))
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
        .h(px(36.0))
        .px(px(14.0))
        .rounded(px(10.0))
        .flex()
        .items_center()
        .text_sm()
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

pub fn run() {
    Application::new().run(|cx| {
        let display = cx.primary_display();
        let bounds = display
            .as_ref()
            .map(|display| display.bounds())
            .unwrap_or_else(|| Bounds::new(point(px(0.0), px(0.0)), size(px(1440.0), px(900.0))));
        let display_id = display.as_ref().map(|display| display.id());
        let display_size = bounds.size;

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: None,
            }),
            focus: true,
            show: true,
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

        cx.open_window(options, |window, cx| {
            let view = cx.new(Rewind::new);
            window.focus(&view.read(cx).focus);
            view
        })
        .expect("could not open the rewind window");

        crate::activate::make_borderless(
            f32::from(display_size.width),
            f32::from(display_size.height),
        );

        cx.activate(true);
    });
}
