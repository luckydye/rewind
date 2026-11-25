# macOS

Rewind for macOS is a menu bar daemon (`rewindd`, built as `Rewind.app`) and a
GPUI overlay (`rewind-ui`). It requires macOS 15 or newer.

- [Build and install](#build-and-install)
- [The daemon](#the-daemon)
- [The overlay](#the-overlay)
- [Export formats](#export-formats)
- [Performance](#performance)
- [Standalone capture](#standalone-capture)
- [Protocol](#protocol)

## Build and install

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

For a local identity that survives rebuilds:

```sh
make signing-identity
make install SIGN_IDENTITY="Rewind Local Signing"
```

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

Several things are non-obvious in the implementation:

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

## Export formats

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

## Performance

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

### Preview round trips

Measured at 1662×1080 on an idle-ish desktop:

| | round trip |
|---|---|
| scrub forward, 0.1 s steps | 12–14 ms |
| same frame repeatedly (static screen) | 14–32 ms |
| scrub backward, jumping GOPs | 15–95 ms, depending on GOP distance |
| first request after the decoder is cold | 40–240 ms, session creation |

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

## Protocol

The socket, message framing, JSON commands and preview-frame format are shared
across platforms. See [protocol.md](protocol.md).
