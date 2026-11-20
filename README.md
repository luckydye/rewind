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
rewindctl config --fps 30 --buffer 60 --display 0 --app com.apple.Safari
rewindctl displays | apps
```

Clips default to `~/Movies/Rewind/`.

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
| drag the timeline | scrub; the preview follows (the playhead stays inside the range) |
| drag a handle | set the export range |
| Space | play the selection, looping |
| ← / → | nudge the playhead |
| Return | export |
| Escape | close |

Export writes to `~/Movies/Rewind/`; the bar then offers a Reveal button rather
than opening Finder unprompted, because stealing focus would break Escape.

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
`listDisplays`, `listApplications`, `previewFrame`, `openUI`, `quit`.

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

## Roadmap

- [x] capture → HEVC → keyframe-aligned ring → export
- [x] daemon: unix-socket IPC, menu bar item, persisted config, login item
- [x] preview decode path: on-demand frames and batches over the socket
- [x] `rewind-ui`: GPUI overlay, timeline scrubber, range export, live preview
- [x] global hotkey in the daemon (GPUI cannot register one)
- [ ] drag a clip out to Finder, copy a file reference to the clipboard
- [ ] power policy: AC/battery, low power mode, thermal state, screen lock
- [ ] audio: system audio tap, optional microphone
- [ ] thumbnails for the timeline (daemon renders, UI displays)
