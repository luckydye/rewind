# Rewind

Always-on background screen recording with a rolling buffer: when something
worth keeping happens, save the last N seconds without having been recording.

## Architecture

Three pieces, each doing only what the others cannot.

```
┌──────────────────────────────────┐         ┌───────────────────────────────┐
│ rewindd  (Swift)                 │  unix   │ rewind-ui  (Rust + GPUI)      │
│                                  │ socket  │                               │
│ menu bar status item             │◄───────►│ full-screen overlay:          │
│ SCStream capture                 │  JSON   │ scrim, frame, timeline,       │
│ VideoToolbox HEVC encode         │ + BGRA  │ range selection, toolbar      │
│ rolling encoded-frame ring       │         │                               │
│ preview decode + clip export     │         │ spawned on Cmd+Shift+S,       │
│ global hotkey (Carbon)           │         │ exits on Escape               │
└──────────────────────────────────┘         └───────────────────────────────┘
                    ▲
                    │  rewindctl (debugging)
```

`rewindd` is the only process that exists while nothing is happening, so it
carries the whole steady-state cost. It is a Swift executable because
ScreenCaptureKit and VideoToolbox are Objective-C frameworks with no viable
Rust path — wrapping them would mean writing the same code behind an FFI
boundary plus a Swift runtime to link against.

It also owns the menu bar item. GPUI has no status-item API, and a "background
recorder" that lives in the menu bar would otherwise need a second process with
its own run loop just to draw an icon.

The UI is Rust/GPUI because the rewind experience is where custom rendering
pays for itself. It is a separate process, launched on demand, so a UI bug or
crash cannot interrupt recording.

Frames never leave `rewindd`: capture, encode, ring buffer and export all happen
in one process. The socket carries only control messages and metadata.

## Why this is cheap

The expensive version of background recording is the one that pulls raw pixels
into your process. This pipeline does not:

- `SCStream` delivers IOSurface-backed buffers, and `SCFrameStatusIdle` means
  **no frame is produced at all** when the screen has not changed.
- `VTCompressionSession` encodes HEVC on the media engine, not the CPU cores.
- The ring holds *encoded* samples in RAM (~25 MB/min at 1080p for real desktop
  content), so there are no continuous disk writes.
- Export re-encodes nothing: the clip is cut at a keyframe and muxed as-is.

### Capture quality

Two settings decide whether the export is readable, and both were wrong at
first:

- **Resolution.** `CGDisplayPixelsWide` returns *points* on a Retina display,
  so capturing "1080p" from a 2x screen meant 1662×1080 out of a 3600×2338
  display — under half the detail per axis, lost before the encoder saw a
  frame. Capture now uses `CGDisplayMode.pixelWidth/Height`, i.e. native.
- **Bitrate.** The encoder was capped at 5 Mbit/s and, on top of that, a
  `DataRateLimits` ceiling close to its target made VT undershoot. Screen
  content is mostly flat with islands of very high-frequency detail (text),
  which needs far more bits per pixel than camera footage. The target is now
  0.18 bits per pixel per frame (~45 Mbit/s at 4K30) with no hard ceiling;
  the ring's byte limit bounds memory instead.

Worst-case synthetic content at 4K30 went from ~1.4 to 36.9 Mbit/s. A static
desktop still spends only a few Mbit/s, which is correct — nothing changed.
The catch is memory: ~280 MB for a full 60 s buffer of the worst case, and
proportionally less in practice. `--height 1080 --bitrate 5000000` restores
the old, lighter profile.

The overlay previews at 2K rather than native, because a native 4K frame is
33 MB and takes ~50 ms per round trip; the exported clip is unaffected.

### Measured

`scripts/measure.sh`, 36 s per case, forced to 1080p (`--height 1080`), `debug`
build, CPU and energy impact sampled from `top`. Native 4K capture is ~4× the
pixels, so expect more than these numbers:

| case | CPU % | energy | frames | bitrate |
|---|---|---|---|---|
| idle desktop, 1080p30 | 0.2 | 0.2 | 97 / 36 s | 0.3 Mbps |
| idle desktop, 720p15 | 0.2 | 0.2 | 36 / 36 s | 0.2 Mbps |
| animated, 1080p30 | 2.5 | 2.5 | 1035 / 36 s | 3.3 Mbps |
| animated, 720p15 | 0.6 | 0.6 | 535 / 36 s | 2.5 Mbps |

`energy` is Activity Monitor's energy-impact figure, not watts. The animated
case uses `testsrc2`, which is close to worst-case entropy for a video encoder;
real desktop content sits far closer to the idle row. For honest watt numbers
run `sudo powermetrics` alongside a capture.

Two things to know about the idle row: the compositor still calls back at the
capture frame rate (the sample counts above are idle notifications, not
frames), so it is 0.2 rather than 0.0. And capture stops producing frames
entirely when the display blanks, which is what keeps a sleeping laptop free.

## Building

```sh
make build      # rewindd + rewindctl (debug)
make bundle     # dist/Rewind.app, ad-hoc signed, for the login item
make doctor     # screen recording permission, display count, hardware encoder
```

### Install

```sh
make install     # builds release, installs /Applications/Rewind.app,
                 # registers it as a login item, starts it
make uninstall   # removes the login item and the app
```

`make install` stops any development copy first. Because the bundle is signed
ad-hoc, its Screen Recording grant is tied to that exact build, so macOS asks
once after each install — allow it, then reopen the overlay. A Developer ID
signature would keep the grant across rebuilds.

Running the bare binary from a terminal inherits the terminal's Screen
Recording permission, which is the friction-free way to develop. An ad-hoc
signed bundle gets its own permission identity that changes on every rebuild,
so it re-prompts — that only matters for distribution, where you would sign
with a stable Developer ID instead.

## The daemon

```sh
make daemon                 # menu bar item + socket, records immediately
```

Menu bar: buffered seconds, start/stop, capture source (a display, or a single
application), start at login, quit. The login item only appears in a bundled
build, since `SMAppService.mainApp` needs a bundle identifier.

Configuration is persisted to `~/Library/Application Support/Rewind/config.json`
and restored on launch.

Debugging client — every daemon feature is reachable without touching the menu:

```sh
rewindctl status
rewindctl status --json
rewindctl start | stop | quit
rewindctl export --seconds 30 --out ~/Desktop/clip.mp4
rewindctl export --seconds 10 --format gif [--gif-fps 15] [--gif-width 1280]
rewindctl export --format png --at 2.5        # the frame 2.5 s ago
rewindctl config --fps 30 --buffer 60 --display 0 --app com.apple.Safari
rewindctl displays | apps
```

Clips, GIFs and PNGs default to `~/Movies/Rewind/`.

## The overlay

`Cmd+Shift+S` (registered by the daemon with Carbon's `RegisterEventHotKey`,
which needs no accessibility permission) spawns `rewind-ui`. It opens a
full-screen, transparent window above the menu bar: the desktop is dimmed, the
frame under the playhead fills the middle, and a single floating bar at the
bottom carries the transport, the readout, the actions and the timeline.

Three things, on purpose. The capture source, the recent clips and the export
destination live in the menu bar item; an earlier version repeated all of them
inside the overlay as status pills and a side panel, which was decoration.

| input | |
|---|---|
| drag on the ruler | seek |
| two-finger swipe left / right | scrub (120 pt per second, ~4 pt per frame) |
| drag the range block | move the range |
| drag a range edge | trim |
| Space | play / pause |
| ← / → | one frame back / forward |
| , / . | one frame back / forward |
| ⇧← / ⇧→ | one second back / forward |
| ⌥← / ⌥→ | move the range by one second |
| ↑ / ↓ | jump to range start / end |
| Home / End | jump to the oldest frame / now |
| I / O | set range start / end at the playhead |
| A | select the whole buffer |
| B | Ink brush |
| E | eraser |
| [ / ] | thinner / thicker brush |
| ⌘Z | undo the last stroke |
| ⇧⌘Z | redo |
| Return / Copy | copy per the Image / MP4 / GIF switch, then close |
| ⌘Return / Export | save per the switch: the frame as PNG, or the range |
| ⌘C | copy the range to the clipboard as a file (GIF if selected, else MP4) |
| Escape | close without copying |

The overlay is preloaded hidden when the daemon starts and stays resident, so
the hotkey only shows a window that already exists (~70 ms instead of ~220 ms
for a cold process launch, plus Metal setup). While hidden it decodes nothing
and cost 0.00 s of CPU over 10 s; its frame is dropped on hide. The daemon
signals it with one byte on `~/Library/Application Support/Rewind/ui.sock`, and
only spawns a process if nothing answers there.

Export writes to `~/Movies/Rewind/`; the bar then offers a Reveal button rather
than opening Finder unprompted, because stealing focus would break Escape.

Drawing is one Ink brush with the same dynamics as the canvas: speed and pen
pressure set the width, strokes are smoothed with a B-spline, and the first
stretch of a mouse stroke grows in instead of starting as a blob. The drawing
toolbar always floats down the left edge of the capture; `B` picks Ink, `E` the
eraser, `[` / `]` change weight, `⌘Z` undoes and `⇧⌘Z` redoes. The crop tool
has no Apply step: a rectangle counts as soon as it is drawn, and copies and
exports use it right away. Drag it or its corners to adjust, Reset to remove it,
Escape or another tool to put the crop tool down. The crop and the
Image / MP4 / GIF choice carry over to the next capture, and across restarts,
via `overlay.json` beside the daemon's config; drawings do not. Annotations are
normalised to the capture, so the preview, a copied image and an exported clip
all get the same drawing: the UI rasterises the layer at native resolution,
`rewindd` composites it into the copied frame, and annotated clips are
re-encoded through a Core Image video composition so the marks land in the
file rather than only on screen.

The overlay is excluded from screen capture (`NSWindowSharingNone`), so the
recorder never films itself and the preview cannot recurse into a picture-in-
picture of itself. Build with `REWIND_OVERLAY_CAPTURABLE=1` to lift that while
debugging the overlay itself.

Three things are non-obvious in the implementation:

- **The window cannot be laid out with nested flex containers.** The timeline
  has to turn a window-space mouse position into a timestamp, so its geometry
  is computed from the viewport size and kept explicit.
- **The window is not borderless out of the box.** GPUI always sets
  `NSWindowStyleMaskTitled`, so the overlay came up with traffic lights,
  rounded corners, and content pushed down by a titlebar. `ui/src/activate.rs`
  strips the style mask to `Borderless` and pins the frame to the display.
  A plain `titlebar: None` is not enough — GPUI still creates a titled window.
- **The window has to be raised above the menu bar.** GPUI's `PopUp` kind sits
  under it, which left a live menu bar above the dimmed screen.
- **Activation has to be done by hand, twice.** GPUI 0.2.2 calls
  `-[NSApplication activateIgnoringOtherApps:]`, a no-op since macOS 14, so the
  overlay drew above everything while every keystroke went to the app that was
  frontmost before. The modern `-[NSApplication activate]` fixes it, but it
  races window ordering and is silently dropped maybe one time in three, so it
  is re-asserted from the first render with `makeKeyAndOrderFront:`. Making the
  UI an `.app` bundle does *not* fix it — that was measured, not assumed.

Known gaps: dragging a clip out to Finder needs `NSFilePromiseProvider`, which
GPUI cannot reach; the overlay covers the primary display only; and a range
export can start up to a second early because it cuts on the previous keyframe.

### Export formats

One switch in the bar, Image / MP4 / GIF, decides what both Copy and Export
produce, so there is a single idea of "the output" rather than one per button.

- **Image** — the frame under the playhead, native resolution, PNG.
- **MP4** — the range, muxed as encoded; re-encoded only when annotated or
  cropped.
- **GIF** — the range, decoded, drawn on, cropped, scaled to at most 1280 px
  wide and sampled at 15 fps. Capture is variable-rate (an unchanged screen
  produces no frames), so instead of resampling on a fixed clock the first
  frame in each 1/15 s slot is kept and held until the next one; a mostly idle
  screen makes a small GIF. Full rate at native size would be hundreds of
  megabytes; crop first when detail matters. Quantisation is the platform's:
  ImageIO on macOS, WIC (median-cut palette per frame, no dithering) on
  Windows.

### Protocol

Unix socket at `~/Library/Application Support/Rewind/rewindd.sock`, mode 0600.
Frames are a big-endian `UInt32` byte count, a one-byte kind, then either UTF-8
JSON (`kind = 0`) or a raw image (`kind = 1`). One request in, one response out;
there are no subscriptions, so a client that dies mid-request cannot leave the
daemon in a half-written state.

```jsonc
// request
{ "command": "exportClip", "seconds": 30, "path": null }

// response (every field except ok is optional)
{
  "ok": true,
  "state": { "recording": true, "config": { … }, "availableSeconds": 47.2, … },
  "clip": { "path": "/Users/…/Rewind 2026-11-20 20.14.02.mp4",
            "duration": 30.1, "bytes": 8123456, "frames": 903,
            "width": 1662, "height": 1080 }
}
```

Commands: `status`, `start`, `stop`, `setConfig`, `exportClip`,
`exportFrame`, `copyClip`, `copyFrame`, `listDisplays`, `listApplications`,
`previewFrame`, `openUI`, `quit`.

`exportClip` and `copyClip` take `"format": "mp4" | "gif"` (default `mp4`);
for GIF, `fps` (default 15) and `width`, the maximum width (default 1280).
`exportFrame` writes the frame `seconds` ago as PNG.

`exportClip` takes a range measured in seconds before now:

```jsonc
{ "command": "exportClip", "range": { "from": 12.0, "to": 4.0 } }
```

### Preview frames

GPUI cannot decode HEVC, so the daemon decodes and the UI receives pixels.
The frame is **full resolution, lossless BGRA**, written as a binary reply —
no downscale, no JPEG, no base64:

```
[u32 length][u8 kind = 1][u32 width][u32 height][BGRA pixels]
```

That is deliberate. An earlier version downscaled to 960 px and JPEG-encoded
at quality 0.65 to save bandwidth; it turned readable UI text into mush, which
is the one thing a screen recorder must never do. At native resolution a frame
is ~7 MB, which a unix socket moves in a few milliseconds and which is cheaper
than JPEG encoding the same pixels.

`seconds` on the request is an offset *backwards from now*, so a timeline can
ask for a position without knowing absolute timestamps. `width` defaults to 0,
meaning native; only pass a width if you genuinely want a smaller frame (the
scale path is a CoreGraphics draw and is slower than shipping native, so it is
rarely worth it).

The decoder keeps its state between requests: scrubbing forward costs one
decode per frame, and only a backward seek costs a re-decode from the keyframe.
Measured at 1662×1080 on an idle-ish desktop:

| | round trip |
|---|---|
| scrub forward, 0.1 s steps | 12–14 ms |
| same frame repeatedly (static screen) | 14–32 ms |
| scrub backward, jumping GOPs | 15–95 ms, depending on GOP distance |
| first request after the decoder is cold | 40–240 ms, session creation |

Two things made that possible, and both were worth more than the transport:
keyframe search walks backwards instead of scanning 1800 frames calling
`CMSampleBufferGetSampleAttachmentsArray`, and the server writes the header and
the pixel buffer straight to the socket instead of assembling an intermediate
`Data`.

The remaining cost is the ~7 MB copy per frame. Sending the encoded HEVC and
decoding in the UI with `videotoolbox` would cut it by roughly 3×, at the price
of a Rust-side decoder; it is not needed at 30 fps, but it is the obvious next
step if it ever is.

## Standalone capture

`rewindd run` drives the same pipeline without the menu bar, for measurement
and for checking the encode path in isolation:

```sh
rewindd run --seconds 10 --stats 3 --clip 5 --export /tmp/clip.mp4
```

| flag | default | |
|---|---|---|
| `--buffer` | 60 | rolling buffer length in seconds |
| `--clip` | 8 | length of the clip exported at exit |
| `--height` | 0 | output height; 0 captures at the display's native resolution |
| `--fps` | 30 | capture frame rate |
| `--bitrate` | 0 | encoder target in bit/s; 0 derives 0.18 bits/pixel/frame |
| `--display` | 0 | index from `rewindd list` |
| `--app` | — | bundle identifier, filters to one application |
| `--capture-resolution` | automatic | `automatic` / `best` / `nominal` |
| `--stats` | 5 | statistics interval, 0 disables |
| `--no-cursor` | off | omit the cursor |

## Windows

`win/` is the Windows daemon, written in Rust. It is Rust rather than Swift
for the opposite of the reason the macOS daemon is Swift: every API it needs
is reachable from the `windows` crate, and none of the macOS frameworks exist
on Windows. It speaks the same protocol, so `rewind-ui` is the same overlay
on both platforms, with a Windows `activate.rs` and a different transport.

```powershell
.\scripts\windows.ps1 bundle      # dist\Rewind\: Rewind.exe, rewindd.exe, rewindctl.exe, rewind-ui.exe
.\scripts\windows.ps1 install     # %LOCALAPPDATA%\Programs\Rewind, Start Menu, Start at Login, started
.\scripts\windows.ps1 uninstall
```

For development, run `win\target\debug\rewindd.exe` from a terminal: its log
goes there, and it finds `ui\target\<profile>\rewind-ui.exe` on its own.
`rewindctl` has the same commands as on macOS, plus `copy`, `copy-frame`,
`thumbnails` and `preview --out frame.bmp`. `export`, `copy` and
`copy-frame` take `--overlay <rgba file>` and `--crop x,y,w,h` (fractions),
so the annotation path can be tested without drawing anything. Without `--out` it only reports timing.

| macOS | Windows |
|---|---|
| `SCStream` | Windows.Graphics.Capture: monitor capture into D3D11 textures, no frames while the screen is unchanged |
| (VideoToolbox takes BGRA) | D3D11 video processor converts BGRA to NV12 and scales for `--height`, on the GPU |
| VideoToolbox HEVC | Media Foundation hardware H.264 MFT (NVENC / AMF / QuickSync), fed D3D11 textures directly |
| `FrameRing` | `ring.rs`, same eviction and keyframe cut |
| `AVAssetWriter` passthrough | `mp4.rs`, a ~250-line single-track MP4 writer; samples are muxed as encoded |
| `PreviewEngine` (VideoToolbox) | `preview.rs`: Microsoft H.264 decoder MFT, NV12 → BGRA (BT.709) on the CPU, box-filtered when a width is asked for |
| unix socket, mode 0600 | named pipe `\\.\pipe\rewind-<user>-rewindd`, DACL for the current user only, remote clients rejected |
| one byte on `ui.sock` | auto-reset named event `Local\Rewind.Overlay.<user>` |
| menu bar item | notification-area icon: left click opens, right click is the same menu |
| Carbon hotkey ⌘⇧S | `RegisterHotKey` **Alt+Shift+S** |
| `SMAppService` login item | `HKCU\…\Run` value pointing at `Rewind.exe` |
| `NSPasteboard` | `CF_DIB` for Copy Frame, `CF_HDROP` (the file) for Copy Clip |
| `~/Movies/Rewind`, Caches | `%USERPROFILE%\Videos\Rewind`, `%LOCALAPPDATA%\Rewind\Clipboard` |
| `config.json` in Application Support | `%APPDATA%\Rewind\config.json` |

There are two daemon executables. `Rewind.exe` is built for the GUI
subsystem, so Start at Login opens no console window. `rewindd.exe` is the
console build, used for the CLI and for development.

Decisions worth knowing:

- **Alt+Shift+S, not Ctrl+Shift+S.** Ctrl+Shift+S is Save As in most apps,
  Win+Shift+S is the Snipping Tool, and Ctrl+Alt is AltGr on many European
  layouts.
- **Focus is handed over, not taken.** Windows only lets the foreground
  process give the foreground away. The daemon is that process right after
  the hotkey or a tray click, so it calls `AllowSetForegroundWindow` before
  signalling the overlay. If that races, the overlay attaches its input queue
  to the foreground thread for one `SetForegroundWindow` call. Hiding the
  overlay returns activation to the previous app on its own.
- **The overlay covers its monitor before the first show.** GPUI sizes
  windows in logical pixels. Above 100% scaling that is smaller than the
  screen, and the first show happens before the daemon has reported which
  display is captured.
- **The ring stops evicting what an open overlay can reach.** The overlay
  pins "now" when it opens, while capture keeps running and the ring keeps
  evicting at 1 s per second. After a few seconds in the overlay, a point
  50 s back sits at the eviction edge. Playing forward at 1× then keeps
  pace with eviction at 1×, so the frames under the playhead are deleted
  as they are needed: one keyframe a second, ~1 fps. Every anchored
  request now pins the ring, and nothing newer than anchor minus buffer
  length is evicted. The pin lapses 60 s after the last request, and the
  512 MB byte limit still applies. The reported available duration is
  capped at the buffer length, so a reopened overlay never sizes its
  timeline to frames the new pin will release.

  Driving the real overlay at the eviction edge (open, wait 12 s, Home,
  play) went from 1.3 fps to 23–27 fps.
- **Playback decodes ahead.** The overlay plays back by asking for one
  frame at a time, so any slow decode is a visible stall. Two of them made
  playback jaggy:
  - **Keyframe crossings:** the decoder was restarted at every keyframe it
    crossed. It now simply keeps going, since the stream is continuous.
  - **IDR decodes:** an IDR takes the software decoder roughly ten times as
    long as a P-frame (~50 ms at 1440p).

  While requests move forward, a prefetch worker keeps up to 8 frames
  decoded past the one last shown, in a cache with its own lock. A frame
  that is already decoded never waits on the decoder, and the worker steps
  aside while a request does wait. `rewindctl play --from 50` replays the
  overlay's request pattern. `REWIND_TRACE=1` on the daemon logs each
  request: cache hit or miss, the decoder's position, and lock wait.
  `REWIND_UI_TRACE=1` makes the overlay log its round trips and render
  cadence. The daemon passes its environment to the overlay it spawns, so
  both traces land in the daemon's stderr.
- **Thumbnails decode in parallel.** Each thumbnail is an independent
  keyframe, so each distinct one is decoded once across up to six decoders:
  250 ms for 24 at 1440p instead of 1.1 s in sequence.
- **The overlay is cloaked until its first frame is on screen.** A hidden
  window keeps its last drawn surface, so showing it flashed the previous
  session's finished UI before the entrance animation. macOS avoids this
  by starting at alpha 0. GPUI composites through DirectComposition,
  which cannot take a layered alpha, so the window is shown cloaked
  (`DWMWA_CLOAK`) instead: it is shown and takes the foreground, but
  nothing reaches the screen.

  GPUI draws only on `WM_PAINT`, which a cloaked window never receives. So
  while cloaked, a pump posts `RDW_INTERNALPAINT` every 8 ms. When the
  first real frame has been rendered and presented (two `on_next_frame`
  hops later), the window uncloaks. After 1.5 s it uncloaks regardless.
  Recorded at 60 fps with the overlay made capturable, the first visible
  frame is the start of the entrance animation.
- **Annotations and crops are burnt in on the CPU and re-encoded on the
  GPU.** Copy Frame composites the overlay's RGBA layer onto the native
  frame, then crops, as `AnnotationOverlay.swift` and `Crop.swift` do. A clip
  with either one is decoded frame by frame, with its own decoder so the
  overlay's preview state is untouched. Each frame is composited and cropped,
  uploaded to a texture, and sent through the same video processor and
  hardware H.264 encoder as live capture (`transcode.rs`). The result keeps
  the capture's codec settings, colour and one-second keyframes. Without
  annotations or a crop, a clip is still muxed as encoded, with no
  re-encode.

  Re-encoding at 1440p takes about 20 ms per frame: 2.7 s for a 5 s clip
  with 159 frames, 1.2 s for 57 frames cropped to 852×720. Every frame in
  comes out (the low-latency encoder emits one output per input), and the
  clips decode completely in Chromium.

- **H.264, not HEVC.** Windows decodes HEVC only with the HEVC Video
  Extensions from the Store, so HEVC clips would not play on many machines,
  and the preview decoder would fail on them too. `doctor` reports both.
- **Hardware MFTs are asynchronous.** They request input and announce output
  through events. An `IMFAsyncCallback` forwards each event into the same
  channel as captured frames, so the encoder thread blocks on a single
  `recv()` and never polls.
- **Keyframes are forced by time, not only by GOP length.** No frames arrive
  while the screen is idle, so a 30-frame GOP could stretch to minutes. The
  encoder forces an IDR when a second has passed since the last one, so a clip
  still starts at most a second early.
- **The OS limits the frame rate.** `SetMinUpdateInterval` (Windows 11 24H2+)
  stops the compositor calling back faster than `--fps`. Older builds deliver
  at the display refresh rate, and the extra frames are skipped in-process.
- **The capture border is hidden** via `GraphicsCaptureAccess` on Windows 11.
  Older builds show a yellow border.
- **The low-latency mode is required.** It produces one output per input with
  no B-frames, so decode order equals presentation order. The NV12 texture
  pool relies on that one-to-one output to know when a texture is free again.

Measured on an RTX 3060 Ti at 2560×1440, release build, 20 s per case, with
a video playing (so not an idle desktop). CPU is the process's share of one
core:

| case | CPU % | frames | bitrate |
|---|---|---|---|
| native 1440p30 | 1.6–3.3 | 556 / 20 s | 8.2 Mbps |
| 1080p30 | 2.2–3.5 | 561 / 20 s | 4.2 Mbps |
| 720p15 | 1.2–2.6 | 301 / 20 s | 2.0 Mbps |

No frames were dropped and every captured frame was encoded. A requested 10 s
clip came out at 10.9 s with 11 keyframes, because the cut lands on the
previous keyframe. Exported clips decode completely in Chromium (0 corrupted
frames, seekable), and UI text stays sharp at native resolution.

Preview round trips over the pipe, release build, at 2560×1440:

| | round trip |
|---|---|
| scrub forward, 0.1 s steps, 1280 wide | 24–67 ms |
| same frame repeatedly | 22 ms |
| native 2560×1440 frame | 186 ms |
| scrub backward, 1280 wide | 69–79 ms |
| playback 50 s back, 1280 wide, paced like the overlay | median 8 ms, frame gaps p99 21 ms |
| 24 thumbnails / 64 thumbnails | 250 ms / 356 ms |

Before the prefetch, playback 50 s back stalled for 60–90 ms once a second,
at every keyframe. Now a stall over 50 ms happens only right after a seek,
while the decoder catches up.

While hidden, the resident overlay used 0.016 s of CPU over 10 s, with an
83 MB working set (GPUI and Direct3D).

Not handled yet:

- Single-application capture. `listApplications` is empty and
  `applicationBundleID` is ignored; `GraphicsCaptureItem` can capture a
  window, which is the way in.
- Picking the encoder by adapter LUID on multi-GPU machines.
- Exclusive-fullscreen games, which can capture black.
- Hardware (DXVA) preview decode. The native frame costs 186 ms, mostly
  software decode plus NV12 → BGRA conversion on the CPU.
- A change of display resolution rebuilds the whole pipeline, which empties
  the buffer.

## Linux (Wayland)

`linux/` is the Wayland daemon, written in Rust like the Windows one. It
speaks the same protocol, so `rewindctl` and (once its window code is wired)
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

### Build

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
sudo cp target/release/rewindd target/release/rewindctl /usr/local/bin/
```

On Hyprland use `xdg-desktop-portal-hyprland` instead of `-wlr`; on GNOME use
`xdg-desktop-portal-gnome`. On Arch:
`gstreamer gst-plugins-{base,good,bad,ugly} gst-libav gst-plugin-pipewire
pipewire wireplumber xdg-desktop-portal-gnome wl-clipboard` (or
`xdg-desktop-portal-wlr`/`-hyprland`).

### Run

```sh
rewindd                  # capture, socket, supervisor; no tray, no hotkey
rewindd doctor           # portal, PipeWire plugin, encoders, decoders
rewindd list             # the screen being captured
rewindd run --seconds 10 --export /tmp/clip.mp4

rewindctl status
rewindctl start | stop | quit
rewindctl export --seconds 30 --out ~/Videos/clip.mp4
rewindctl export --format gif --seconds 10
rewindctl export --format png --at 2.5
rewindctl export --crop 0.1,0.1,0.5,0.5 --overlay marks.rgba
rewindctl preview --ago 2 --width 1280 --out frame.bmp
rewindctl config --fps 15 --height 600
```

The first start opens the portal's picker. Its answer is remembered as a
restore token in `$XDG_CONFIG_HOME/rewind/portal-token`, so the next launch
goes straight to recording where the portal supports tokens (Hyprland, GNOME
and KDE do; `xdg-desktop-portal-wlr` may not, and then it asks again). A
config change restarts capture, which means the picker can appear again; the
`setConfig` reply does not wait for it.

`rewindd login-item on` writes `~/.config/autostart/rewind.desktop`. There is
no global hotkey: bind `rewindctl start` (or a future `rewindctl open`) in the
compositor's key binding settings, which is the supported Wayland way.

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
| Carbon ⌘⇧S | none yet; bind `rewindctl` yourself |
| `NSPasteboard` | `wl-copy`: `text/uri-list` for a clip, `image/png` for a frame |
| `~/Movies/Rewind` | `~/Videos/Rewind` |
| `config.json` in Application Support | `$XDG_CONFIG_HOME/rewind/config.json` |
| `SMAppService` login item | XDG autostart entry |

### Decisions worth knowing

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

Not handled yet:

- The overlay, tray and hotkey (this cut is the daemon and the CLI).
- Hardware preview decode on VAAPI: the decoder is chosen by presence, but
  `vah264dec` outputs VA surfaces that `videoconvert` pulls to system memory.
- The portal gives one stream. Multi-monitor means one picker choice, not a
  display index.
- A compositor that revokes the stream ends capture and does not retry, so the
  picker is not re-opened behind the user's back.

## Roadmap

- [x] capture → HEVC → keyframe-aligned ring → export
- [x] daemon: unix-socket IPC, menu bar item, persisted config, login item
- [x] preview decode path: on-demand frames and batches over the socket
- [x] `rewind-ui`: GPUI overlay, timeline scrubber, range export, live preview
- [x] global hotkey in the daemon (GPUI cannot register one)
- [x] Windows daemon: Windows.Graphics.Capture, Media Foundation H.264, named-pipe IPC
- [x] Wayland daemon: portal + PipeWire capture, GStreamer H.264, unix-socket IPC
- [ ] Wayland overlay, tray icon and compositor-bound shortcut
- [ ] drag a clip out to Finder, copy a file reference to the clipboard
- [ ] power policy: AC/battery, low power mode, thermal state, screen lock
- [ ] audio: system audio tap, optional microphone
- [ ] thumbnails for the timeline (daemon renders, UI displays)
