# Protocol

Every client talks to the daemon over a local socket. One request in, one
response out; there are no subscriptions, so a client that dies mid-request
cannot leave the daemon in a half-written state.

| platform | transport |
|---|---|
| macOS | unix socket at `~/Library/Application Support/Rewind/rewindd.sock`, mode 0600 |
| Windows | named pipe `\\.\pipe\rewind-<user>-rewindd`, DACL for the current user only, remote clients rejected |
| Linux | `$XDG_RUNTIME_DIR/rewind/rewindd.sock`, mode 0600 |

Frames are a big-endian `UInt32` byte count, a one-byte kind, then either UTF-8
JSON (`kind = 0`) or a raw image (`kind = 1`).

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
`previewFrame`, `openUI`, `quit`. The Windows and Linux daemons add
`thumbnails`.

`exportClip` and `copyClip` take `"format": "mp4" | "gif"` (default `mp4`);
for GIF, `fps` (default 15) and `width`, the maximum width (default 1280).
`exportFrame` writes the frame `seconds` ago as PNG.

`exportClip` takes a range measured in seconds before now:

```jsonc
{ "command": "exportClip", "range": { "from": 12.0, "to": 4.0 } }
```

## Preview frames

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

Two things made the macOS path fast, and both were worth more than the
transport: keyframe search walks backwards instead of scanning 1800 frames
calling `CMSampleBufferGetSampleAttachmentsArray`, and the server writes the
header and the pixel buffer straight to the socket instead of assembling an
intermediate `Data`.

The remaining cost is the ~7 MB copy per frame. Sending the encoded HEVC and
decoding in the UI with `videotoolbox` would cut it by roughly 3×, at the price
of a Rust-side decoder; it is not needed at 30 fps, but it is the obvious next
step if it ever is. The Windows and Linux daemons use their own platform
decoders behind the same reply format.
