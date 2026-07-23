# vgplay

A fast, minimal sound player for Windows, built for **instant startup**. Double-click
a sound file in Explorer and a compact player window is on screen in tens of
milliseconds — no splash, no runtime, no clutter.

> The name is a working title and may change.

vgplay was split out of the [vgiew](../vgiew) image viewer, which the two apps
originally shared a binary with; see vgiew's ADR 0014 for the reasoning.

## Status

- **Sound player — early MVP.** Plays WAV, MP3, FLAC, OGG. The window shows a
  play/stop button and a repeat toggle; playback starts automatically on open.

## Why it's fast

- Native Rust, a single self-contained `.exe`, no runtime to spin up.
- The window and first frame appear immediately; the audio device and decoder are
  opened lazily, only when a file is actually played.
- No-flash reveal via a DWM cloak — the window appears already painted.
- CPU-drawn UI via `softbuffer` — no GPU context to initialize, no widget toolkit.
  The two round buttons are pure geometry, supersampled for anti-aliasing.

## Controls

| Control | Action |
|---------|--------|
| Play/Stop button | stop ends playback; play restarts from the beginning |
| Repeat toggle | loop the file when it reaches the end |
| `Esc` | close |

## Build

Requires the Rust toolchain (`stable-x86_64-pc-windows-msvc`) and the MSVC C++ build
tools (Visual Studio Build Tools).

```powershell
cargo build --release
# run:
target\release\vgplay.exe path\to\sound.mp3
```

## Install and bind to double-click

`install.ps1` builds a release, installs into a stable per-user path
(`%LOCALAPPDATA%\Programs\vgplay`, no admin required), and registers vgplay as a
handler for sound files:

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1
```

Then, one time, set it as the default: right-click a sound file → **Open with →
Choose another app → vgplay → Always** (Windows 11 requires this manual confirmation
and does not allow setting a default silently). Because the install path is stable,
every later release is just `install.ps1` again — the association keeps working.

To install into `Program Files` instead (needs an elevated terminal):

```powershell
powershell -File install.ps1 -InstallDir "C:\Program Files\vgplay"
```

Remove everything with `uninstall.ps1`.

## Development

- `vgplay --register` / `--unregister` — add/remove HKCU associations (no admin).
- `vgplay --help` — CLI help.

## Requirements

Windows 10/11, x64.

## License

MIT — see [LICENSE](LICENSE).
