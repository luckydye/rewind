<p align="center">
  <img src="docs/assets/icon.png" width="96" height="96" alt="Rewind">
</p>

<h1 align="center">Rewind</h1>

<p align="center">
  <strong>The last minute of your screen, always ready.</strong>
</p>

<p align="center">
  Press one shortcut to scrub, trim, and export what just happened.
</p>

<p align="center">
  <a href="https://github.com/luckydye/rewind/releases"><img src="https://img.shields.io/github/v/release/luckydye/rewind?style=flat-square&color=000000" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/macOS-15%2B-000000?style=flat-square&logo=apple&logoColor=white" alt="macOS 15+">
  <img src="https://img.shields.io/badge/Windows-0078D4?style=flat-square&logo=windows&logoColor=white" alt="Windows">
  <img src="https://img.shields.io/badge/Linux-Wayland-FCC624?style=flat-square&logo=linux&logoColor=black" alt="Linux Wayland">
  <img src="https://img.shields.io/badge/100%25-local-2ea44f?style=flat-square" alt="100% local">
</p>

Rewind keeps a rolling buffer of your screen in memory. There is no record
button, no growing file on disk, and no cloud upload.

## Features

- **Always buffering**: a configurable ring, 60 seconds by default.
- **Frame-accurate**: scrub, step frames, and set in and out points.
- **Annotate and crop**: draw on the frame or crop it before exporting.
- **PNG, MP4, or GIF**: Copy and Export both follow the selected format.
- **Low overhead**: hardware encoding, about 0.2% CPU idle at 1080p.
- **Local only**: no account, telemetry, or network path.

## Install

**macOS 15+** (arm64)

Download the release, or build and install it:

```sh
make install
```

Press `⌘⇧S` to open the overlay.

**Windows** (x64)

Unzip the release and run `Rewind.exe`, or:

```powershell
.\scripts\windows.ps1 install
```

Press `Alt+Shift+S`.

**Linux** (Wayland)

The daemon and CLI work today. The overlay is not built yet.

```sh
cd linux
cargo build --release
sudo cp target/release/{rewindd,rewindctl} /usr/local/bin/
rewindd
```

## Shortcuts

| | macOS | Windows |
|---|---|---|
| Open | `⌘⇧S` | `Alt+Shift+S` |
| Play / pause | `Space` | `Space` |
| Frame back / forward | `←` / `→` | `←` / `→` |
| Copy | `Return` | `Return` |
| Export | `⌘Return` | `Ctrl+Return` |
| Close | `Escape` | `Escape` |

Full list: [overlay shortcuts](docs/macos.md#the-overlay).

## Platform support

| | macOS | Windows | Linux |
|---|---|---|---|
| Capture | ✅ | ✅ | ✅ |
| Overlay, tray, hotkey | ✅ | ✅ | — |
| PNG / MP4 / GIF | ✅ | ✅ | ✅ |
| Ink and crop | ✅ | ✅ | Via CLI |
| Encoder | HEVC | H.264 | H.264 |

## Documentation

- [Architecture](docs/architecture.md)
- [macOS](docs/macos.md)
- [Windows](docs/windows.md)
- [Linux (Wayland)](docs/linux.md)
- [Protocol](docs/protocol.md)
- [Development](docs/development.md)
- [Roadmap](docs/roadmap.md)
