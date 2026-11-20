//! The rewind overlay: a dimmed full-screen surface with the frame under the
//! playhead, a timeline you can drag a range out of, and a floating toolbar.
//!
//! Deliberately laid out from the viewport size rather than with nested flex
//! containers, because the timeline has to turn a window-space mouse position
//! into a timestamp and that only stays correct if the geometry is explicit.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::*;

use crate::ipc::{self, Client, RawImage, Reply};
use crate::protocol::{ClipRange, Command, Request};

const SIDE: f32 = 56.0;
const TIMELINE_HEIGHT: f32 = 44.0;
const BOTTOM: f32 = 150.0;
const HANDLE_REACH: f32 = 12.0;

const SCRIM: u32 = 0x0B0B0DCC;
const PANEL: u32 = 0x1C1C1FF2;
const PANEL_BORDER: u32 = 0xFFFFFF1F;
const TRACK: u32 = 0xFFFFFF26;
const ACCENT: u32 = 0x0A84FFFF;
const TEXT: u32 = 0xFFFFFFEB;
const TEXT_DIM: u32 = 0xFFFFFF99;
const DANGER: u32 = 0xFF453AFF;

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
}

impl Rewind {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let client = Client::new();
        let response = ipc::call(&client, Request::new(Command::Status));
        let state = response.as_ref().ok().and_then(|response| response.state.as_ref());

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

    fn set_playhead(&mut self, seconds: f64, cx: &mut Context<Self>) {
        self.playhead = seconds.clamp(0.0, self.span);
        self.request_frame(cx);
        cx.notify();
    }

    fn nudge(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.playing = false;
        self.ticker = None;
        let next = self.playhead + delta;
        self.set_playhead(next, cx);
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
                    request.width = Some(0);
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
            Ok(_) => self.status = Some(Status::Error("expected a frame".into())),
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
                            reveal(&clip.path);
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

    fn timeline_down(
        &mut self,
        x: f32,
        left: f32,
        width: f32,
        cx: &mut Context<Self>,
    ) {
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
            let seconds = self.time_at(x, left, width);
            self.playhead = seconds;
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
                self.request_frame(cx);
            }
            Drag::From => {
                self.sel_from = seconds.clamp(self.sel_to + 0.1, self.span);
            }
            Drag::To => {
                self.sel_to = seconds.clamp(0.0, self.sel_from - 0.1);
            }
        }
        cx.notify();
    }
}

// MARK: - Rendering

impl Render for Rewind {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let width: f32 = viewport.width.into();
        let height: f32 = viewport.height.into();

        let left = SIDE;
        let track_width = (width - SIDE * 2.0).max(1.0);

        let stage_width = (width - SIDE * 2.0).max(1.0);
        let stage_height = (height - BOTTOM - SIDE).max(1.0);
        let (frame_width, frame_height) = self.frame_size;
        let (image_width, image_height) = fit(
            frame_width as f32,
            frame_height as f32,
            stage_width,
            stage_height,
        );

        let span = self.span;
        let x_for = |seconds: f64| {
            left + track_width * (1.0 - (seconds / span).clamp(0.0, 1.0)) as f32
        };

        let from_x = x_for(self.sel_from);
        let to_x = x_for(self.sel_to);
        let playhead_x = x_for(self.playhead);

        let stage = match &self.frame {
            Some(image) => div()
                .w(px(image_width))
                .h(px(image_height))
                .rounded(px(10.0))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(PANEL_BORDER))
                .child(
                    img(ImageSource::Render(image.clone()))
                        .w(px(image_width))
                        .h(px(image_height)),
                )
                .into_any_element(),

            None => div()
                .w(px(image_width))
                .h(px(image_height))
                .rounded(px(10.0))
                .bg(rgba(PANEL))
                .border_1()
                .border_color(rgba(PANEL_BORDER))
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(rgba(TEXT_DIM))
                .child("No frame")
                .into_any_element(),
        };

        let timeline = div()
            .relative()
            .w_full()
            .h(px(TIMELINE_HEIGHT))
            .on_mouse_down(
                MouseButton::Left,
                _cx.listener(move |view, event: &MouseDownEvent, _window, cx| {
                    view.timeline_down(event.position.x.into(), left, track_width, cx)
                }),
            )
            .on_mouse_move(
                _cx.listener(move |view, event: &MouseMoveEvent, _window, cx| {
                    view.timeline_drag(event.position.x.into(), left, track_width, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                _cx.listener(|view, _event: &MouseUpEvent, _window, cx| {
                    view.drag = None;
                    cx.notify();
                }),
            )
            // Track
            .child(
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(TIMELINE_HEIGHT / 2.0 - 3.0))
                    .w(px(track_width))
                    .h(px(6.0))
                    .rounded_full()
                    .bg(rgba(TRACK)),
            )
            // Selection
            .child(
                div()
                    .absolute()
                    .left(px(from_x))
                    .top(px(TIMELINE_HEIGHT / 2.0 - 3.0))
                    .w(px((to_x - from_x).max(1.0)))
                    .h(px(6.0))
                    .rounded_full()
                    .bg(rgba(ACCENT)),
            )
            // Range handles
            .child(handle(from_x))
            .child(handle(to_x))
            // Playhead
            .child(
                div()
                    .absolute()
                    .left(px(playhead_x - 1.0))
                    .top(px(TIMELINE_HEIGHT / 2.0 - 11.0))
                    .w(px(2.0))
                    .h(px(22.0))
                    .rounded_full()
                    .bg(rgba(TEXT)),
            );

        let toolbar = div()
            .absolute()
            .bottom(px(32.0))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(10.0))
                    .py(px(9.0))
                    .rounded(px(14.0))
                    .bg(rgba(PANEL))
                    .border_1()
                    .border_color(rgba(PANEL_BORDER))
                    .shadow_lg()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(34.0))
                            .h(px(30.0))
                            .rounded(px(9.0))
                            .text_sm()
                            .text_color(rgba(TEXT))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgba(0xFFFFFF14)))
                            .on_mouse_down(
                                MouseButton::Left,
                                _cx.listener(|view, _event: &MouseDownEvent, _window, cx| {
                                    view.toggle_play(cx)
                                }),
                            )
                            .child(if self.playing { "❚❚" } else { "▶" }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgba(TEXT_DIM))
                            .child(format_seconds(self.playhead)),
                    )
                    .child(separator())
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgba(TEXT))
                            .child(format!("{:.1}s", self.selection_duration())),
                    )
                    .child(button("Export", true).on_mouse_down(
                        MouseButton::Left,
                        _cx.listener(|view, _event: &MouseDownEvent, _window, cx| view.export(cx)),
                    ))
                    .child(button("Close", false).on_mouse_down(
                        MouseButton::Left,
                        _cx.listener(|_view, _event: &MouseDownEvent, _window, cx| cx.quit()),
                    )),
            );

        let mut root = div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgba(SCRIM))
            .track_focus(&self.focus)
            .on_key_down(_cx.listener(Self::on_key))
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(stage),
            )
            .child(div().w_full().px(px(SIDE)).child(timeline));

        if let Some(status) = &self.status {
            let (text, color) = match status {
                Status::Info(text) => (text.clone(), rgba(TEXT_DIM)),
                Status::Error(text) => (text.clone(), rgba(DANGER)),
            };
            root = root.child(
                div()
                    .absolute()
                    .bottom(px(96.0))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .text_xs()
                    .text_color(color)
                    .child(text),
            );
        }

        root.child(toolbar)
    }
}

// MARK: - Bits

fn handle(x: f32) -> AnyElement {
    div()
        .absolute()
        .left(px(x - 6.0))
        .top(px(TIMELINE_HEIGHT / 2.0 - 6.0))
        .w(px(12.0))
        .h(px(12.0))
        .rounded_full()
        .bg(rgba(TEXT))
        .border_1()
        .border_color(rgba(PANEL))
        .into_any_element()
}

fn separator() -> AnyElement {
    div()
        .w(px(1.0))
        .h(px(18.0))
        .bg(rgba(PANEL_BORDER))
        .into_any_element()
}

fn button(label: &'static str, primary: bool) -> Div {
    let background = if primary { rgba(ACCENT) } else { rgba(0xFFFFFF14) };
    let hovered = if primary {
        rgba(0x0A84FFE6)
    } else {
        rgba(0xFFFFFF26)
    };

    div()
        .px(px(14.0))
        .py(px(7.0))
        .rounded(px(9.0))
        .bg(background)
        .text_sm()
        .text_color(rgba(TEXT))
        .cursor_pointer()
        .hover(move |style| style.bg(hovered))
        .child(label)
}

fn fit(frame_width: f32, frame_height: f32, max_width: f32, max_height: f32) -> (f32, f32) {
    if frame_width <= 0.0 || frame_height <= 0.0 {
        return (max_width, max_height);
    }
    let scale = (max_width / frame_width).min(max_height / frame_height).min(1.0);
    (frame_width * scale, frame_height * scale)
}

fn format_seconds(seconds: f64) -> String {
    format!("{:.1}s", seconds)
}

fn build_image(image: RawImage) -> Result<Arc<RenderImage>, String> {
    let buffer = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(
        image.width,
        image.height,
        image.pixels,
    )
    .ok_or_else(|| "frame size does not match its pixels".to_string())?;

    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

fn reveal(path: &str) {
    let _ = std::process::Command::new("open").args(["-R", path]).spawn();
}

// MARK: - Entry

pub fn run() {
    Application::new().run(|cx| {
        let display = cx.primary_display();
        let bounds = display
            .as_ref()
            .map(|display| display.bounds())
            .unwrap_or_else(|| {
                Bounds::new(point(px(0.0), px(0.0)), size(px(1440.0), px(900.0)))
            });
        let display_id = display.as_ref().map(|display| display.id());

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

        // A PopUp window draws above everything but does not take focus by
        // itself, so without this every keystroke still goes to whatever was
        // frontmost before the overlay opened.
        cx.activate(true);
        crate::activate::activate();
    });
}
