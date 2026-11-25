# Roadmap

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
