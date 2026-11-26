# Development

## Repository layout

| path | what |
|---|---|
| `daemon/` | macOS daemon, menu bar item and CLI — Swift |
| `ui/` | the overlay — Rust + GPUI, shared by macOS and Windows |
| `win/` | Windows daemon and CLI — Rust |
| `linux/` | Wayland daemon and CLI — Rust |
| `resources/` | `Info.plist` and the app icon |
| `scripts/` | packaging, signing, measurement and integration checks |
| `docs/` | the documentation in this folder |

`rewindd` (macOS) and `Rewind.exe` (Windows) are the long-lived processes.
The overlay is launched by the daemon on demand and talks to it over the
protocol described in [protocol.md](protocol.md).

## Prerequisites

- macOS: Xcode command line tools and Swift 6; `make` for the bundle targets.
- Windows: Rust stable and the MSVC toolchain.
- Linux: Rust stable, GStreamer development packages and a desktop portal; see
  [linux.md](linux.md#build).
- The overlay builds with Rust stable.

## Make targets (macOS)

```sh
make build       # rewind-ui + rewindd + rewind, debug
make ui          # only the overlay, debug
make ui-release  # only the overlay, release
make daemon      # build, then run the menu bar daemon
make run         # standalone capture, no menu bar
make status      # rewind status against a running daemon
make list        # capture sources
make doctor      # screen recording permission, displays, hardware encoder
make bundle      # dist/Rewind.app
make install     # build release, install to /Applications, start at login
make uninstall
make measure     # scripts/measure.sh
make clean
```

The daemon launches `rewind-ui` from its own directory, so `make build` copies
the overlay binary next to `rewindd`. Copying over a running executable keeps
its inode, which makes the kernel kill the new binary on a stale signature
cache, so the build removes the old file first.

## Code signing

`make bundle` signs with the `Rewind Local Signing` identity when one exists,
otherwise ad-hoc (`-`). An ad-hoc signature changes with every build, and so
does the Screen Recording grant. Create a stable local identity once:

```sh
make signing-identity
make install SIGN_IDENTITY="Rewind Local Signing"
```

`errSecInternalComponent` during signing means `codesign` could not unlock the
identity's key: run `make install` from your own terminal and choose
**Always Allow** when the keychain asks.

## Windows and Linux

```powershell
.\scripts\windows.ps1 bundle
.\scripts\windows.ps1 install
.\scripts\windows.ps1 uninstall
```

```sh
cd linux
cargo build --release
```

For development, run the daemon from a terminal and it will find the overlay
binary in `ui/target/<profile>/`. Environment traces:

| variable | effect |
|---|---|
| `REWIND_TRACE=1` | log preview requests: cache hit or miss, decoder position, lock wait |
| `REWIND_UI_TRACE=1` | log overlay round trips and render cadence |
| `REWIND_OVERLAY_CAPTURABLE=1` | let the overlay record itself, for debugging capture exclusion |

The daemon passes its environment to the overlay it spawns, so both traces
land in the daemon's stderr.

## Integration checks

There is deliberately no unit test suite; the interesting failures live where
the pieces meet. Two scripts exercise the real binaries:

- `scripts/test-export.sh` exports PNG, MP4 and GIF — plain, cropped,
  annotated and both — from a running daemon and checks pixel size, frame
  count, GIF timing and looping, and that the drawing lands where it was drawn,
  the right way up, inside the crop. It needs a running daemon, `ffmpeg`, and a
  screen `ffplay` can draw on.
- `scripts/measure.sh` samples CPU and energy impact from `top` while
  `rewindd run` captures idle and animated desktops at two profiles.

Run them before a release, alongside the platform-specific checks documented
in [macos.md](macos.md), [windows.md](windows.md) and [linux.md](linux.md).

## Releases

Pushing a `v*` tag runs `.github/workflows/release.yml`, which:

1. stamps the tag into `resources/Info.plist`,
2. builds and archives `Rewind.app` for macOS arm64 (`make bundle`),
3. builds and archives the Windows x64 bundle (`scripts/windows.ps1 bundle`),
4. uploads both zips with SHA-256 files to a single GitHub release.

Re-running a tag build replaces the assets instead of creating a second
release.
