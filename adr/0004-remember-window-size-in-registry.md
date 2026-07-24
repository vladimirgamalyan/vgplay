# 0004. Remember the window size across launches (HKCU)

Status: Accepted

## Context

The player window became resizable (ADR 0003), but every launch reopened at the
hard-coded default (640×260). A user who prefers a larger or smaller player had
to resize it every single time. We want the last chosen size to persist across
launches.

Two questions had to be settled:

- **Where to store it.** vgplay had no persistent state of its own before this.
  The realistic options were a config file (e.g. under `%APPDATA%`) or the
  Windows registry under HKCU. The app already talks to HKCU via `winreg` for
  file-type associations (`register`/`unregister`), and it is a Windows-only tool.
- **What unit to store.** `Window::inner_size()` returns *physical* pixels, which
  differ per monitor DPI. Persisting physical pixels would make a window sized on
  a 150% display reopen 1.5× larger on a 100% display.

## Decision

We will remember the window's inner size in the registry, keyed under
`HKCU\Software\vgplay` as two `REG_DWORD` values (`WindowWidth`, `WindowHeight`):

- **Registry, not a config file.** It reuses the `winreg` dependency and the HKCU
  hive already in play for associations, needs no directory/file creation or
  format choice, and matches the app's Windows-only nature. `--unregister` is left
  as-is (it removes associations); the size key is tiny, harmless, and orphaned
  cleanup is not worth a behavior change.
- **Store the logical size.** We save `inner_size().to_logical(scale_factor)` and
  reopen with `LogicalSize`, so the remembered size is DPI-independent.
- **Save on exit, not on every resize.** The size is written once, at both exit
  points (`CloseRequested` and Escape), rather than on each `Resized` event — no
  registry churn while the user drags the window edge.
- **Skip maximized.** A maximized window's size is the screen, not a size the user
  picked, so `persist_window_size` returns early when `is_maximized()` — the last
  normal size stays remembered. Restoring the maximized *state* is out of scope.
- **Fail soft.** Missing, zero, or unreadable values fall back to the 640×260
  default; `min_inner_size` (360×180) still clamps anything too small.

## Consequences

- The player reopens at the user's last chosen size; first run and any read
  failure still get the sensible default.
- vgplay now writes a key under `HKCU\Software\vgplay`. It is not removed by
  `--unregister`; a user wanting a truly clean uninstall would delete that key
  manually. Acceptable for a two-DWORD preference.
- Only the size is persisted — not window position, nor maximized state. These
  were deliberately left out as speculative; they can be added later if wanted.
- Persistence is Windows-only (the `winreg`-backed functions are `cfg(windows)`,
  with no-op stubs elsewhere), consistent with the rest of the platform glue.
