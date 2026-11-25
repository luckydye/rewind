# Architecture

Rewind is three pieces, each doing only what the others cannot.

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

The Windows and Wayland daemons follow the same shape — hardware encode where
available, an encoded ring, passthrough export — using what each platform
provides. See [windows.md](windows.md) and [linux.md](linux.md).

## Capture quality (macOS)

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

Platform measurements live with each platform:

- [macOS performance](macos.md#performance)
- [Windows performance](windows.md#performance)
- [Linux performance](linux.md#performance)
