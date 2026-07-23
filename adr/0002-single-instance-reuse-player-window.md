# 0002. A running player reuses its window for newly opened sounds

Status: Accepted

## Context

Each vgplay launch opened its own window and event loop. Double-clicking a
second sound while a player was already open stacked a second window, then a
third, and so on — every file its own window, each playing over the last. For a
"click a sound, hear it" tool that is noisy: the user usually wants *this* sound
now, in the window that is already there, with the previous one stopped.

We want a second launch to hand its file to the running player, which stops
whatever it is playing and plays the new file in the same window. That needs
two things on Windows: arbitration (who is the one live instance?) and a way to
pass a path from the new process into the running process's event loop.

Alternatives considered:

- **WM_COPYDATA to the existing window.** The classic Win32 single-instance
  path. But winit 0.29 owns the window's `WndProc` and exposes no supported hook
  for arbitrary messages; intercepting `WM_COPYDATA` means subclassing the HWND
  and interoperating with winit's own message handling — fragile against winit
  upgrades.
- **A named mutex plus shared memory / a message file.** Mutex arbitration is
  standard, but it needs a *separate* channel to actually move the path, and a
  polled file is hacky.
- **Leave it as one window per file.** Simplest, but it is the behavior we were
  explicitly asked to change.

## Decision

We will make vgplay single-instance *for playback* on Windows using a per-user
**named pipe** for both arbitration and message passing, and winit's
`EventLoopProxy` to inject the received path into the running event loop:

- The pipe is `\\.\pipe\vgplay-<username>`, scoped by user so separate logged-in
  sessions get separate players.
- On opening a file, an instance first tries to connect to the pipe and send the
  path (`try_handoff`); if a server accepts it, the new process exits. Otherwise
  it creates the pipe with `FILE_FLAG_FIRST_PIPE_INSTANCE` and becomes the
  primary. That flag makes creation fail with `ERROR_ACCESS_DENIED` if a server
  already exists, which — with a short retry loop — closes the startup race
  without a separate mutex.
- The primary runs a background accept loop that reads the UTF-8 path a client
  sends and forwards it via `EventLoopProxy::send_event`. A new `Event::UserEvent`
  arm stops current playback, plays the new file, retitles the window, and pulls
  it to the foreground (the handing-off process calls `AllowSetForegroundWindow`
  so the focus change is permitted).

This is scoped to `run_sound`; the `--register` / `--unregister` / `--help`
subcommands are unaffected. On non-Windows builds the arbitration is compiled
out and each file opens its own window as before.

## Consequences

- One player window that always plays the most recently opened sound; the
  previous sound stops. No window stacking.
- The IPC is hand-written Win32 (`CreateNamedPipeW` / `CreateFileW` /
  `ConnectNamedPipe`), consistent with the file's existing style of local
  `extern "system"` blocks — no new crates, no widget/IPC dependency, launch
  stays cheap.
- Arbitration and messaging share one primitive (the pipe), so there is no
  separate mutex to keep in sync. The `FILE_FLAG_FIRST_PIPE_INSTANCE` race guard
  plus a 5-iteration retry handle simultaneous launches.
- The behavior is per-user and Windows-only by construction. If cross-session or
  cross-platform single-instance is ever wanted, this must be revisited.
- We gave up the ability to have several independent player windows at once. If
  that is later desired (e.g. compare two sounds side by side), it would need an
  opt-out and would supersede this decision.
