# Rewind

Always-on background screen recording with a rolling buffer: when something
worth keeping happens, save the last N seconds without having been recording.

## Architecture

Three pieces, each doing only what the others cannot.

```
┌──────────────────────────────────┐         ┌───────────────────────────────┐
│ rewindd  (Swift)                 │  unix   │ rewind-ui  (Rust + GPUI)      │
│                                  │ socket  │                               │
│ menu bar status item             │◄───────►│ rewind mode: timeline,        │
│ SCStream capture                 │  JSON   │ scrubber, clip library        │
│ VideoToolbox HEVC encode         │ frames  │                               │
│ rolling encoded-frame ring       │         │ launched on demand,           │
│ clip export (AVAssetWriter)      │         │ exits when dismissed          │
│ power policy                     │         │                               │
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

### Protocol

Unix socket at `~/Library/Application Support/Rewind/rewindd.sock`, mode 0600.
Frames are a big-endian `UInt32` byte count followed by UTF-8 JSON. One request
in, one response out; there are no subscriptions, so a client that dies
mid-request cannot leave the daemon in a half-written state.

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
`listDisplays`, `listApplications`, `quit`.

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
- [x] daemon: unix-socket IPC, menu bar item, persisted config, login item
- [ ] `rewind-ui`: GPUI timeline scrubber and clip library
- [ ] power policy: AC/battery, low power mode, thermal state, screen lock
- [ ] audio: system audio tap, optional microphone
- [ ] thumbnails for the timeline (daemon renders, UI displays)
