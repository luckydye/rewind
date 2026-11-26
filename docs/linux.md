# Linux (Wayland)

`linux/` is the Wayland daemon, written in Rust like the Windows one. It
speaks the same protocol, so `rewind` and (once its window code is wired)
`rewind-ui` are the same clients. This cut is the daemon and the CLI; the
overlay, tray and hotkey are not built yet.

Wayland has no screen-capture API. Every capture starts at
`org.freedesktop.portal.ScreenCast`, which shows the desktop's own source
picker and hands back one PipeWire node plus an fd. `pipewiresrc` consumes
them; GStreamer is the video processor, the H.264 encoder and the decoder.
There is no OS media framework to lean on, so the encoder is whichever plugin
the machine has: `vah264enc`/`vaapih264enc` on Intel and AMD, `nvh264enc` on
NVIDIA, `x264enc` as the software floor. `rewindd doctor` reports what was
found.

- [Build](#build)
- [Run](#run)
- [How it maps to macOS](#how-it-maps-to-macos)
- [Decisions worth knowing](#decisions-worth-knowing)
- [Performance](#performance)
- [Not handled yet](#not-handled-yet)

## Build

```sh
sudo apt install rustup build-essential pkg-config \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav \
  gstreamer1.0-pipewire pipewire wireplumber \
  xdg-desktop-portal xdg-desktop-portal-wlr wl-clipboard
rustup default stable

cd linux
cargo build --release
sudo cp target/release/rewindd target/release/rewind /usr/local/bin/
```

On Hyprland use `xdg-desktop-portal-hyprland` instead of `-wlr`; on GNOME use
`xdg-desktop-portal-gnome`. On Arch:
`gstreamer gst-plugins-{base,good,bad,ugly} gst-libav gst-plugin-pipewire
pipewire wireplumber xdg-desktop-portal-gnome wl-clipboard` (or
`xdg-desktop-portal-wlr`/`-hyprland`).

## Run

```sh
rewindd                  # capture, socket, supervisor; no tray, no hotkey
rewindd doctor           # portal, PipeWire plugin, encoders, decoders
rewindd list             # the screen being captured
rewindd run --seconds 10 --export /tmp/clip.mp4

rewind status
rewind start | stop | quit
rewind export --seconds 30 --out ~/Videos/clip.mp4
rewind export --format gif --seconds 10
rewind export --format png --at 2.5
rewind export --crop 0.1,0.1,0.5,0.5 --overlay marks.rgba
rewind preview --ago 2 --width 1280 --out frame.bmp
rewind config --fps 15 --height 600
rewind config --buffer 120
```

`--buffer` is in seconds; 60 is the default. The 512 MB ring cap still applies.

The first start opens the portal's picker. Its answer is remembered as a
restore token in `$XDG_CONFIG_HOME/rewind/portal-token`, so the next launch
goes straight to recording where the portal supports tokens (Hyprland, GNOME
and KDE do; `xdg-desktop-portal-wlr` may not, and then it asks again). A
config change restarts capture, which means the picker can appear again; the
`setConfig` reply does not wait for it.

`rewindd login-item on` writes `~/.config/autostart/rewind.desktop`. There is
no global hotkey: bind `rewind start` (or a future `rewind open`) in the
compositor's key binding settings, which is the supported Wayland way.

## How it maps to macOS

| macOS | Linux |
|---|---|
| `SCStream` | `org.freedesktop.portal.ScreenCast` → PipeWire node |
| VideoToolbox HEVC | GStreamer VAAPI / NVENC / x264, H.264 |
| `FrameRing` | the same `ring.rs` policy, nanosecond timestamps |
| `AVAssetWriter` passthrough | the same `mp4.rs` writer |
| `PreviewEngine` (VideoToolbox) | GStreamer decode to BGRA |
| unix socket, mode 0600 | `$XDG_RUNTIME_DIR/rewind/rewindd.sock`, mode 0600 |
| one byte on `ui.sock` | the same `ui.sock` path under the runtime dir |
| menu bar item | none yet |
| Carbon ⌘⇧S | none yet; bind `rewind` yourself |
| `NSPasteboard` | `wl-copy`: `text/uri-list` for a clip, `image/png` for a frame |
| `~/Movies/Rewind` | `~/Videos/Rewind` |
| `config.json` in Application Support | `$XDG_CONFIG_HOME/rewind/config.json` |
| `SMAppService` login item | XDG autostart entry |

## Decisions worth knowing

- **The picker is the price of admission.** A daemon cannot start capture
  silently on Wayland, and there is no way to enumerate or select an output
  without it. `--display` is accepted for parity but the portal decides.
- **Preview is stateless in this cut.** Each request builds a decoder, feeds
  it from the previous keyframe and flushes with EOS, so the last frame always
  comes out. Keeping a decoder alive across requests is what the macOS and
  Windows daemons do for fast scrubbing, but the state has to resume exactly
  where it stopped; getting that wrong was costing frames. When the overlay
  lands, this is where caching and prefetch go.
- **Export re-encodes when it must.** A plain clip is muxed as encoded, from
  the keyframe before the requested start. A crop or an annotation layer
  decodes every frame, composites on the CPU, and re-encodes through the same
  GStreamer encoder (`transcode.rs`).
- **No capture exclusion.** Wayland has no `NSWindowSharingNone` equivalent,
  so an overlay would appear in the recording. There is nothing to do about it
  at the daemon level; the overlay cut has to decide whether to pause capture
  while it is up.
- **Idle cost depends on the compositor.** A wlroots portal sends frames only
  when the output changes, which keeps the idle ring empty and costs almost
  nothing. A compositor that sends at a fixed cadence will fill the ring
  instead; `videorate drop-only` caps the rate but cannot invent damage
  detection.
- **`wl-copy` stays alive by design.** It forks a process that owns the
  clipboard selection, so stderr is not piped and the child is not waited on.
  Piping it makes the command hang until the clipboard is taken.

## Performance

Measured on the Debian 13/Hyprland arm64 test VM, 1920×1200, release build,
software x264 (no VAAPI in the VM), cursor movement as the only change:

| case | result |
|---|---|
| live capture, 30 fps | 3–5 % of one core, 0.02 Mbit/s on a mostly black screen |
| `preview --width 640`, 1920×1200 source | 68–130 ms per frame |
| 24 thumbnails, 160 px wide | ~120 ms |
| 2 s crop export to 960×600, 51 frames | 2.4 s |
| 2 s annotated export to 1152×720, 51 frames | 4.7 s |
| 4 s GIF at 1280 px, 15 fps | 24 s (the `image` crate's GIF quantiser is single-threaded) |

## Not handled yet

- The overlay, tray and hotkey (this cut is the daemon and the CLI).
- Hardware preview decode on VAAPI: the decoder is chosen by presence, but
  `vah264dec` outputs VA surfaces that `videoconvert` pulls to system memory.
- The portal gives one stream. Multi-monitor means one picker choice, not a
  display index.
- A compositor that revokes the stream ends capture and does not retry, so the
  picker is not re-opened behind the user's back.

## Protocol

Linux uses the same socket protocol under `$XDG_RUNTIME_DIR`. See
[protocol.md](protocol.md).
