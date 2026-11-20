# Rewind

Always-on background screen recording with a rolling buffer: when something
worth keeping happens, save the last N seconds without having been recording.

## Architecture

Three pieces, each doing only what the other cannot.

```
┌─────────────────────────────┐        ┌──────────────────────────────┐
│ rewindd  (Swift)            │  unix  │ rewind-ui  (Rust + GPUI)     │
│                             │ socket │                              │
│ menu bar status item        │◄──────►│ rewind mode: timeline,       │
│ SCStream capture            │  JSON  │ scrubber, clip library       │
│ VideoToolbox HEVC encode    │ frames │                              │
│ rolling encoded-frame ring  │        │ launched on demand,          │
│ clip export (AVAssetWriter) │        │ exits when dismissed         │
└─────────────────────────────┘        └──────────────────────────────┘
```

`rewindd` is the only process that exists while nothing is happening, so it
carries the whole steady-state cost. It is a Swift executable because
ScreenCaptureKit and VideoToolbox are Objective-C frameworks with no viable
Rust path — wrapping them would mean writing the same code behind an FFI
boundary plus a Swift runtime to link against.

The UI is Rust/GPUI because the rewind experience is where custom rendering
pays for itself. It is a separate process, launched on demand, so a UI bug or
crash cannot interrupt recording.

Frames never leave `rewindd`: capture, encode, ring buffer and export all happen
in one process, and the socket carries only control messages and metadata.

## Why this is cheap

The expensive version of background recording is the one that pulls raw pixels
into your process. This pipeline does not:

- `SCStream` delivers IOSurface-backed buffers, and `SCFrameStatusIdle` means
  **no frame is produced at all** when the screen has not changed.
- `VTCompressionSession` encodes HEVC on the media engine, not the CPU cores.
- The ring holds *encoded* samples in RAM (~25 MB/min at 1080p for real desktop
  content), so there are no continuous disk writes.
- Export re-encodes nothing: the clip is cut at a keyframe and muxed as-is.

### Measured

`scripts/measure.sh`, 36 s per case, built-in display (1800×1169), `debug`
build, CPU and energy impact sampled from `top`:

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
make build      # builds rewindd
make doctor     # screen recording permission, display count, hardware encoder
make list       # displays and capturable applications
```

## Recording

```sh
# 10 second run, then export the last 5 seconds
.build/debug/rewindd run --seconds 10 --clip 5 --export /tmp/clip.mp4

# capture one application instead of the whole display
.build/debug/rewindd run --app com.apple.Safari

# full option list
.build/debug/rewindd --help
```

| flag | default | |
|---|---|---|
| `--buffer` | 60 | rolling buffer length in seconds |
| `--clip` | 8 | exported clip length |
| `--height` | 1080 | output height, width follows display aspect |
| `--fps` | 30 | capture frame rate |
| `--bitrate` | 5000000 | encoder ceiling, bit/s |
| `--display` | 0 | index from `rewindd list` |
| `--app` | — | bundle identifier, filters to one application |
| `--capture-resolution` | automatic | `automatic` / `best` / `nominal` |
| `--stats` | 5 | statistics interval, 0 disables |
| `--no-cursor` | off | omit the cursor |

## Roadmap

- [x] capture → HEVC → keyframe-aligned ring → export
- [ ] `rewindd` as a real daemon: unix-socket IPC, menu bar item, login item
- [ ] power policy: AC/battery, low power mode, thermal state, screen lock
- [ ] `rewind-ui`: GPUI timeline scrubber and clip library
- [ ] audio: system audio tap, optional microphone
- [ ] thumbnails for the timeline (daemon renders, UI displays)
