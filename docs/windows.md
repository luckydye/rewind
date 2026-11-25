# Windows

`win/` is the Windows daemon, written in Rust. It is Rust rather than Swift
for the opposite of the reason the macOS daemon is Swift: every API it needs
is reachable from the `windows` crate, and none of the macOS frameworks exist
on Windows. It speaks the same protocol, so `rewind-ui` is the same overlay
on both platforms, with a Windows `activate.rs` and a different transport.

- [Build and install](#build-and-install)
- [How it maps to macOS](#how-it-maps-to-macos)
- [Decisions worth knowing](#decisions-worth-knowing)
- [Performance](#performance)
- [Not handled yet](#not-handled-yet)

## Build and install

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
so the annotation path can be tested without drawing anything. Without `--out`
it only reports timing.

The overlay opens with **Alt+Shift+S**. The tray menu also has buffer presets:
30 seconds, 1 minute, 2 minutes, and 5 minutes.

## How it maps to macOS

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

## Decisions worth knowing

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

## Performance

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

## Not handled yet

- Single-application capture. `listApplications` is empty and
  `applicationBundleID` is ignored; `GraphicsCaptureItem` can capture a
  window, which is the way in.
- Picking the encoder by adapter LUID on multi-GPU machines.
- Exclusive-fullscreen games, which can capture black.
- Hardware (DXVA) preview decode. The native frame costs 186 ms, mostly
  software decode plus NV12 → BGRA conversion on the CPU.
- A change of display resolution rebuilds the whole pipeline, which empties
  the buffer.

## Protocol

Windows uses the same message format over a per-user named pipe. See
[protocol.md](protocol.md).
