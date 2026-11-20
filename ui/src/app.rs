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
use std::time::Duration;

use gpui::prelude::*;
use gpui::*;

use crate::ipc::{self, Client, RawImage, Reply};
use crate::protocol::{ClipRange, Command, Request};

// MARK: - Geometry

const MARGIN: f32 = 24.0;
const GAP: f32 = 16.0;
const BAR_HEIGHT: f32 = 112.0;
const BAR_PADDING: f32 = 18.0;

/// Room for half a tick label beyond each end of the track, so "-0s" is not
/// clipped by the bar's right edge.
const RULER_INSET: f32 = 32.0;
const LABEL_WIDTH: f32 = 44.0;

const TRACK_Y: f32 = 76.0;
const TRACK_H: f32 = 8.0;
const HANDLE_REACH: f32 = 12.0;

/// Capture is native-resolution (33 MB per frame on a 4K display). A 2K preview
/// costs ~11 MB and about 40 ms round trip, which keeps scrubbing usable; the
/// exported clip is full resolution either way.
const PREVIEW_WIDTH: i64 = 2048;

// MARK: - Palette

const SCRIM: u32 = 0x08080CDB;
const PANEL: u32 = 0x1B1B1EF5;
const BORDER: u32 = 0xFFFFFF12;
const TEXT: u32 = 0xE9E9EDFF;
const MUTED: u32 = 0x8A8A93FF;
const ACCENT: u32 = 0x2F7CF6FF;
const ACCENT_SOFT: u32 = 0x2F7CF64D;
const DANGER: u32 = 0xFF5A4DFF;
const PENDING: u32 = 0xF0A030FF;
const TRACK: u32 = 0xFFFFFF1A;

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Playhead,
    From,
    To,
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

    fn position_in_selection(&self) -> f64 {
        (self.sel_from - self.playhead).clamp(0.0, self.selection_duration())
    }

    /// The playhead belongs to the range being exported; letting it drift
    /// outside made the readout clamp to zero and disagree with the timeline.
    fn clamp_playhead(&mut self) {
        self.playhead = self.playhead.clamp(self.sel_to, self.sel_from);
    }

    fn set_playhead(&mut self, seconds: f64, cx: &mut Context<Self>) {
        self.playhead = seconds;
        self.clamp_playhead();
        self.request_frame(cx);
        cx.notify();
    }

    fn nudge(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.playing = false;
        self.ticker = None;
        // Left is earlier on the timeline, i.e. further back in time.
        let next = self.playhead + delta;
        self.set_playhead(next, cx);
    }

    fn jump_to_start(&mut self, cx: &mut Context<Self>) {
        self.playing = false;
        self.ticker = None;
        self.set_playhead(self.sel_from, cx);
    }

    fn jump_to_end(&mut self, cx: &mut Context<Self>) {
        self.playing = false;
        self.ticker = None;
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

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        self.playing = !self.playing;
        if !self.playing {
            self.ticker = None;
            cx.notify();
            return;
        }

        if self.playhead <= self.sel_to + 0.001 {
            self.playhead = self.sel_from;
        }

        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;

                let keep_going = this
                    .update(cx, |view, cx| {
                        if !view.playing {
                            return false;
                        }
                        view.playhead -= 1.0 / 30.0;
                        if view.playhead < view.sel_to {
                            view.playhead = view.sel_from;
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
        self.playing = false;
        self.ticker = None;
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
        self.playing = false;
        self.ticker = None;

        let to_handle = (x - self.x_of(self.sel_to, left, width)).abs();
        let from_handle = (x - self.x_of(self.sel_from, left, width)).abs();

        if to_handle <= HANDLE_REACH && to_handle <= from_handle {
            self.drag = Some(Drag::To);
        } else if from_handle <= HANDLE_REACH {
            self.drag = Some(Drag::From);
        } else {
            self.drag = Some(Drag::Playhead);
            self.playhead = self.time_at(x, left, width);
            self.clamp_playhead();
            self.request_frame(cx);
        }
        cx.notify();
    }

    fn timeline_drag(&mut self, x: f32, left: f32, width: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else { return };
        let seconds = self.time_at(x, left, width);

        match drag {
            Drag::Playhead => {
                self.playhead = seconds;
                self.clamp_playhead();
                self.request_frame(cx);
            }
            Drag::From => {
                self.sel_from = seconds.clamp(self.sel_to + 0.1, self.span);
                self.clamp_playhead();
            }
            Drag::To => {
                self.sel_to = seconds.clamp(0.0, self.sel_from - 0.1);
                self.clamp_playhead();
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

        let viewport = window.viewport_size();
        let width: f32 = viewport.width.into();
        let height: f32 = viewport.height.into();

        let bar_top = height - MARGIN - BAR_HEIGHT;
        let bar_width = width - MARGIN * 2.0;
        let track_left = MARGIN + BAR_PADDING + RULER_INSET;
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

        div()
            .relative()
            .size_full()
            .bg(rgba(SCRIM))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .child(stage)
            .child(self.transport_bar(
                bar_top,
                bar_width,
                track_left,
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

        let mut bar = panel()
            .absolute()
            .left(px(MARGIN))
            .top(px(bar_top))
            .w(px(bar_width))
            .h(px(BAR_HEIGHT))
            // Transport
            .child(
                div()
                    .absolute()
                    .left(px(BAR_PADDING))
                    .top(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(transport_button("◀◀").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _: &MouseDownEvent, _window, cx| view.jump_to_start(cx)),
                    ))
                    .child(
                        transport_button(if self.playing { "❚❚" } else { "▶" })
                            .bg(rgba(ACCENT_SOFT))
                            .text_color(rgba(ACCENT))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|view, _: &MouseDownEvent, _window, cx| {
                                    view.toggle_play(cx)
                                }),
                            ),
                    )
                    .child(transport_button("▶▶").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _: &MouseDownEvent, _window, cx| view.jump_to_end(cx)),
                    ))
                    .child(
                        div()
                            .ml(px(12.0))
                            .text_sm()
                            .text_color(rgba(TEXT))
                            .child(format!(
                                "{:.1}s / {:.1}s",
                                self.position_in_selection(),
                                self.selection_duration()
                            )),
                    ),
            );

        if !status_text.is_empty() {
            bar = bar.child(
                div()
                    .absolute()
                    .left(px(BAR_PADDING + 296.0))
                    .top(px(20.0))
                    .text_xs()
                    .text_color(status_colour)
                    .child(status_text),
            );
        }

        let mut actions = div()
            .absolute()
            .right(px(BAR_PADDING))
            .top(px(12.0))
            .flex()
            .items_center()
            .gap(px(8.0));

        if let Some(path) = &self.last_clip {
            let path = path.clone();
            actions = actions.child(text_button("Reveal").on_mouse_down(
                MouseButton::Left,
                move |_, _, _| reveal(&path),
            ));
        }

        bar = bar
            .child(
                actions
                    .child(text_button("Reset").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _: &MouseDownEvent, _window, cx| view.reset_range(cx)),
                    ))
                    .child(
                        text_button("Export")
                            .bg(rgba(ACCENT))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|view, _: &MouseDownEvent, _window, cx| {
                                    view.export(cx)
                                }),
                            ),
                    )
                    .child(
                        transport_button("✕")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_view, _: &MouseDownEvent, _window, cx| cx.quit()),
                            ),
                    ),
            )
            .child(self.ruler(track_left, track_width))
            .child(
                div()
                    .absolute()
                    .left(px(track_left))
                    .top(px(TRACK_Y))
                    .w(px(track_width))
                    .h(px(TRACK_H))
                    .rounded_full()
                    .bg(rgba(TRACK)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(from_x))
                    .top(px(TRACK_Y))
                    .w(px((to_x - from_x).max(1.0)))
                    .h(px(TRACK_H))
                    .rounded_full()
                    .bg(rgba(ACCENT)),
            )
            .child(handle(from_x))
            .child(handle(to_x))
            .child(
                div()
                    .absolute()
                    .left(px(playhead_x - 1.0))
                    .top(px(TRACK_Y - 6.0))
                    .w(px(2.0))
                    .h(px(TRACK_H + 12.0))
                    .rounded_full()
                    .bg(rgba(TEXT)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(track_left))
                    .top(px(TRACK_Y - 14.0))
                    .w(px(track_width))
                    .h(px(TRACK_H + 28.0))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, event: &MouseDownEvent, _window, cx| {
                            view.timeline_down(event.position.x.into(), track_left, track_width, cx)
                        }),
                    )
                    .on_mouse_move(
                        cx.listener(move |view, event: &MouseMoveEvent, _window, cx| {
                            view.timeline_drag(event.position.x.into(), track_left, track_width, cx)
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|view, _: &MouseUpEvent, _window, cx| {
                            view.drag = None;
                            cx.notify();
                        }),
                    ),
            );

        bar.into_any_element()
    }

    /// Labels are centred on their tick in a fixed-width box, which is what
    /// keeps the last one inside the bar instead of hanging off the edge.
    fn ruler(&self, track_left: f32, track_width: f32) -> AnyElement {
        let step = ruler_step(self.span);
        let mut ruler = div()
            .absolute()
            .left(px(track_left))
            .top(px(48.0))
            .h(px(18.0))
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
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.0))
                    .child(div().w(px(1.0)).h(px(3.0)).bg(rgba(0xFFFFFF26)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgba(MUTED))
                            .child(format!("-{:.0}s", ticks)),
                    ),
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

fn transport_button(glyph: &'static str) -> Div {
    div()
        .w(px(36.0))
        .h(px(30.0))
        .rounded(px(9.0))
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(rgba(TEXT))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0xFFFFFF14)))
        .child(glyph)
}

fn text_button(label: &'static str) -> Div {
    div()
        .px(px(14.0))
        .py(px(7.0))
        .rounded(px(9.0))
        .bg(rgba(0xFFFFFF12))
        .text_sm()
        .text_color(rgba(TEXT))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0xFFFFFF20)))
        .child(label)
}

fn handle(x: f32) -> AnyElement {
    div()
        .absolute()
        .left(px(x - 7.0))
        .top(px(TRACK_Y - 3.0))
        .w(px(14.0))
        .h(px(14.0))
        .rounded_full()
        .bg(rgba(TEXT))
        .border_2()
        .border_color(rgba(PANEL))
        .into_any_element()
}

fn ruler_step(span: f64) -> f64 {
    for candidate in [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0] {
        if span / candidate <= 6.0 {
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
