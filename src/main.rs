// vgplay — a fast, minimal sound player for Windows (winit + softbuffer + CPU).
// Double-click a sound file in Explorer and a compact player window appears
// immediately: a play/stop button and a repeat toggle, drawn by hand on the CPU
// path (no widget toolkit). Audio is initialized lazily — only when a file is
// actually opened — so launch stays cheap. A second launch while a player is
// already open reuses that window (Windows): the file is handed to the running
// instance, which stops any current sound and plays the new one.
#![windows_subsystem = "windows"]

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use winit::event::{ElementState, Event, MouseButton, WindowEvent};
use winit::event_loop::EventLoopBuilder;
use winit::keyboard::{Key, NamedKey};
use winit::window::{Icon, WindowBuilder, WindowButtons};

const BG: u32 = 0x00F5_F5F5; // window background (softbuffer: 0x00RRGGBB)

const SOUND_EXTS: &[&str] = &["wav", "mp3", "flac", "ogg"];

fn load_app_icon() -> Option<Icon> {
    let rgba = image::load_from_memory(include_bytes!("../assets/icon.png"))
        .ok()?
        .to_rgba8();
    let (w, h) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), w, h).ok()
}

// Absolute path from a possibly-relative CLI arg, without touching the filesystem or
// adding a \\?\ verbatim prefix (which canonicalize would). Explorer already passes
// absolute paths; this covers a relative path typed on the command line.
fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

// ── Console for CLI subcommands (a windows-subsystem build has no console of its own) ──
#[cfg(windows)]
fn attach_console() {
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(dw_process_id: u32) -> i32;
    }
    unsafe {
        AttachConsole(0xFFFF_FFFF); // ATTACH_PARENT_PROCESS
    }
}
#[cfg(not(windows))]
fn attach_console() {}

#[cfg(windows)]
fn notify_assoc_changed() {
    #[link(name = "shell32")]
    extern "system" {
        fn SHChangeNotify(
            w_event_id: i32,
            u_flags: u32,
            dw_item1: *const core::ffi::c_void,
            dw_item2: *const core::ffi::c_void,
        );
    }
    unsafe {
        // SHCNE_ASSOCCHANGED, SHCNF_IDLIST
        SHChangeNotify(0x0800_0000, 0x0000, core::ptr::null(), core::ptr::null());
    }
}

// Cloak/uncloak the window at the DWM level. A cloaked window is composited (its surface
// exists, so a GDI present lands in it) but not displayed. This lets us reveal the window
// only after its first frame is painted, with no white flash on show.
#[cfg(windows)]
fn set_cloak(window: &winit::window::Window, cloak: bool) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut core::ffi::c_void,
            attr: u32,
            value: *const core::ffi::c_void,
            size: u32,
        ) -> i32;
    }
    const DWMWA_CLOAK: u32 = 13;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return;
    };
    let value: i32 = cloak as i32;
    unsafe {
        DwmSetWindowAttribute(
            h.hwnd.get() as *mut core::ffi::c_void,
            DWMWA_CLOAK,
            &value as *const i32 as *const core::ffi::c_void,
            core::mem::size_of::<i32>() as u32,
        );
    }
}
#[cfg(not(windows))]
fn set_cloak(_window: &winit::window::Window, _cloak: bool) {}

// Set the window class background brush to `rgb` (0xRRGGBB). If anything ever erases the
// window before our first paint (e.g. a stray WM_ERASEBKGND), it fills that color instead
// of white — a backstop to the DWM cloak in set_cloak. The brush lives for the process; the
// OS reclaims it on exit.
#[cfg(windows)]
fn set_class_background(window: &winit::window::Window, rgb: u32) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "gdi32")]
    extern "system" {
        fn CreateSolidBrush(color: u32) -> *mut core::ffi::c_void;
    }
    #[link(name = "user32")]
    extern "system" {
        fn SetClassLongPtrW(hwnd: *mut core::ffi::c_void, index: i32, value: isize) -> isize;
    }
    const GCLP_HBRBACKGROUND: i32 = -10;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return;
    };
    // COLORREF is 0x00BBGGRR; swap R and B from our 0xRRGGBB.
    let color = ((rgb & 0xFF) << 16) | (rgb & 0x00FF00) | ((rgb >> 16) & 0xFF);
    unsafe {
        let brush = CreateSolidBrush(color);
        SetClassLongPtrW(
            h.hwnd.get() as *mut core::ffi::c_void,
            GCLP_HBRBACKGROUND,
            brush as isize,
        );
    }
}
#[cfg(not(windows))]
fn set_class_background(_window: &winit::window::Window, _rgb: u32) {}

fn print_help() {
    println!(
        "vgplay — a fast sound player\n\n\
         Usage:\n  \
         vgplay <file>          open a sound (wav, mp3, flac, ogg)\n  \
         vgplay --register      register associations (HKCU, no admin)\n  \
         vgplay --unregister    remove associations\n  \
         vgplay --help          this help"
    );
}

// Registers vgplay as a candidate handler for sounds in HKCU (no admin rights).
// The path comes from current_exe() — register the installed .exe.
#[cfg(windows)]
fn register() -> std::io::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;
    let exe = std::env::current_exe()?;
    let exe_str = exe.to_string_lossy().replace('/', "\\");
    let cmd = format!("\"{exe_str}\" \"%1\"");
    let classes = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey("Software\\Classes")?
        .0;

    classes
        .create_subkey("vgplay.sound")?
        .0
        .set_value("", &"Sound (vgplay)")?;
    classes
        .create_subkey("vgplay.sound\\DefaultIcon")?
        .0
        .set_value("", &format!("{exe_str},0"))?;
    classes
        .create_subkey("vgplay.sound\\shell\\open\\command")?
        .0
        .set_value("", &cmd)?;

    // Applications\vgplay.exe entry — so the app shows up nicely in "Open with".
    classes
        .create_subkey("Applications\\vgplay.exe\\shell\\open\\command")?
        .0
        .set_value("", &cmd)?;
    classes
        .create_subkey("Applications\\vgplay.exe")?
        .0
        .set_value("FriendlyAppName", &"vgplay")?;
    let supported = classes
        .create_subkey("Applications\\vgplay.exe\\SupportedTypes")?
        .0;

    for ext in SOUND_EXTS {
        // Add vgplay as a candidate for the extension WITHOUT touching the default (no association hijacking).
        classes
            .create_subkey(format!(".{ext}\\OpenWithProgids"))?
            .0
            .set_value("vgplay.sound", &"")?;
        supported.set_value(format!(".{ext}"), &"")?;
    }
    notify_assoc_changed();
    Ok(())
}

#[cfg(windows)]
fn unregister() -> std::io::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;
    let classes = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Software\\Classes", KEY_ALL_ACCESS)?;
    let _ = classes.delete_subkey_all("vgplay.sound");
    let _ = classes.delete_subkey_all("Applications\\vgplay.exe");
    for ext in SOUND_EXTS {
        if let Ok(owp) =
            classes.open_subkey_with_flags(format!(".{ext}\\OpenWithProgids"), KEY_SET_VALUE)
        {
            let _ = owp.delete_value("vgplay.sound");
        }
    }
    notify_assoc_changed();
    Ok(())
}

// ── Sound player UI (hand-drawn; no widget toolkit) ──────────────────────────
// The sound window shows two round buttons: play/stop (left) and a repeat toggle
// (right). Shapes are supersampled for anti-aliasing and the whole UI is redrawn
// each frame — both cheap at this size.

const UI_ACCENT: u32 = 0x003B_82F6; // primary/enabled button fill (blue)
const UI_ON_ICON: u32 = 0x00FF_FFFF; // icon on an accent (enabled) button
const UI_OFF_FILL: u32 = 0x00E4_E4E4; // repeat button background when off
const UI_OFF_ICON: u32 = 0x009A_A0A6; // icon on an off button

const SS: u32 = 4; // supersampling factor per axis for edge anti-aliasing

// Alpha-blend `fg` over `bg` (both 0x00RRGGBB) with coverage `a` in 0..1.
#[inline]
fn blend(bg: u32, fg: u32, a: f32) -> u32 {
    let a = a.clamp(0.0, 1.0);
    let mix = |sh: u32| {
        let b = ((bg >> sh) & 0xFF) as f32;
        let f = ((fg >> sh) & 0xFF) as f32;
        (b + (f - b) * a).round() as u32
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

// Fill pixels of the bounding box `(x0,y0,x1,y1)` where `inside(px,py)` holds,
// anti-aliased by SS×SS supersampling, blending `color` over the buffer.
fn fill_aa<F: Fn(f32, f32) -> bool>(
    buf: &mut [u32],
    w: u32,
    h: u32,
    bb: (f32, f32, f32, f32),
    color: u32,
    inside: F,
) {
    let x0 = bb.0.floor().max(0.0) as u32;
    let y0 = bb.1.floor().max(0.0) as u32;
    let x1 = (bb.2.ceil().max(0.0) as u32).min(w);
    let y1 = (bb.3.ceil().max(0.0) as u32).min(h);
    let inv = 1.0 / (SS * SS) as f32;
    for y in y0..y1 {
        for x in x0..x1 {
            let mut hits = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    if inside(px, py) {
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                let i = (y * w + x) as usize;
                buf[i] = blend(buf[i], color, hits as f32 * inv);
            }
        }
    }
}

fn point_in_tri(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let s = |u: (f32, f32), v: (f32, f32), o: (f32, f32)| {
        (u.0 - o.0) * (v.1 - o.1) - (v.0 - o.0) * (u.1 - o.1)
    };
    let (d1, d2, d3) = (s(p, a, b), s(p, b, c), s(p, c, a));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

fn disc(buf: &mut [u32], w: u32, h: u32, cx: f32, cy: f32, r: f32, color: u32) {
    fill_aa(buf, w, h, (cx - r, cy - r, cx + r, cy + r), color, move |px, py| {
        let (dx, dy) = (px - cx, py - cy);
        dx * dx + dy * dy <= r * r
    });
}

fn triangle(buf: &mut [u32], w: u32, h: u32, a: (f32, f32), b: (f32, f32), c: (f32, f32), color: u32) {
    let bb = (
        a.0.min(b.0).min(c.0),
        a.1.min(b.1).min(c.1),
        a.0.max(b.0).max(c.0),
        a.1.max(b.1).max(c.1),
    );
    fill_aa(buf, w, h, bb, color, move |px, py| point_in_tri((px, py), a, b, c));
}

// Left button: an accent disc showing what a click will do — a stop square while a
// track is playing, a play triangle while stopped.
fn draw_play_stop(buf: &mut [u32], w: u32, h: u32, c: (f32, f32), d: f32, playing: bool) {
    disc(buf, w, h, c.0, c.1, d * 0.5, UI_ACCENT);
    if playing {
        let s = d * 0.22;
        let bb = (c.0 - s, c.1 - s, c.0 + s, c.1 + s);
        fill_aa(buf, w, h, bb, UI_ON_ICON, move |px, py| {
            (px - c.0).abs() <= s && (py - c.1).abs() <= s
        });
    } else {
        // Right-pointing triangle, nudged right so it reads as optically centered.
        let t = d * 0.28;
        let ox = d * 0.05;
        triangle(
            buf,
            w,
            h,
            (c.0 - t * 0.7 + ox, c.1 - t),
            (c.0 - t * 0.7 + ox, c.1 + t),
            (c.0 + t * 0.9 + ox, c.1),
            UI_ON_ICON,
        );
    }
}

// Right button: a repeat toggle drawn as a circular arrow (annulus arc + arrowhead).
// Accent-filled with a white glyph when on; a muted disc with a gray glyph when off.
fn draw_repeat(buf: &mut [u32], w: u32, h: u32, c: (f32, f32), d: f32, on: bool) {
    let (fill, icon) = if on {
        (UI_ACCENT, UI_ON_ICON)
    } else {
        (UI_OFF_FILL, UI_OFF_ICON)
    };
    disc(buf, w, h, c.0, c.1, d * 0.5, fill);
    // Annulus, drawn everywhere except a wedge at the top (a gap for the arrowhead).
    let r = d * 0.27;
    let th = d * 0.055;
    let (ri, ro) = (r - th, r + th);
    let gap0 = 250f32.to_radians();
    let gap1 = 290f32.to_radians();
    fill_aa(
        buf,
        w,
        h,
        (c.0 - ro, c.1 - ro, c.0 + ro, c.1 + ro),
        icon,
        move |px, py| {
            let (dx, dy) = (px - c.0, py - c.1);
            let dd = dx * dx + dy * dy;
            if dd < ri * ri || dd > ro * ro {
                return false;
            }
            let mut ang = dy.atan2(dx);
            if ang < 0.0 {
                ang += std::f32::consts::TAU;
            }
            !(ang >= gap0 && ang <= gap1)
        },
    );
    // Arrowhead at the east side of the gap, pointing over the top (clockwise sweep).
    let a = gap1;
    let (ca, sa) = (a.cos(), a.sin());
    let p = (c.0 + r * ca, c.1 + r * sa);
    let tang = (sa, -ca); // tangent pointing into the top gap
    let (hl, hw) = (d * 0.12, d * 0.11);
    triangle(
        buf,
        w,
        h,
        (p.0 + tang.0 * hl, p.1 + tang.1 * hl),
        (p.0 + ca * hw, p.1 + sa * hw),
        (p.0 - ca * hw, p.1 - sa * hw),
        icon,
    );
}

// The two buttons' centers and shared radius, in physical pixels, derived from the
// window size. Shared by drawing and hit-testing so the two never drift apart.
fn sound_layout(w: u32, h: u32) -> ((f32, f32), (f32, f32), f32) {
    let (wf, hf) = (w as f32, h as f32);
    let d = hf * 0.42;
    let gap = d * 0.5;
    let x0 = (wf - (d * 2.0 + gap)) * 0.5;
    let cy = hf * 0.5;
    let r = d * 0.5;
    ((x0 + r, cy), (x0 + d * 1.5 + gap, cy), r)
}

enum SoundButton {
    PlayStop,
    Repeat,
}

// Which button, if any, the cursor is over.
fn sound_button_hit(w: u32, h: u32, cursor: (f32, f32)) -> Option<SoundButton> {
    let (c1, c2, r) = sound_layout(w, h);
    let inside = |c: (f32, f32)| {
        let (dx, dy) = (cursor.0 - c.0, cursor.1 - c.1);
        dx * dx + dy * dy <= r * r
    };
    if inside(c1) {
        Some(SoundButton::PlayStop)
    } else if inside(c2) {
        Some(SoundButton::Repeat)
    } else {
        None
    }
}

fn draw_sound_ui(buf: &mut [u32], w: u32, h: u32, playing: bool, repeat: bool) {
    buf.iter_mut().for_each(|p| *p = BG);
    let (c1, c2, r) = sound_layout(w, h);
    let d = r * 2.0;
    draw_play_stop(buf, w, h, c1, d, playing);
    draw_repeat(buf, w, h, c2, d, repeat);
}

// ── Single-instance IPC (Windows) ─────────────────────────────────────────────
// The first player to open owns a per-user named pipe and listens on a background
// thread. A later launch connects to that pipe, sends the file path, and exits;
// the running instance receives the path over an EventLoopProxy and swaps playback
// to the new file. This keeps a single player window that always plays the most
// recently opened sound instead of stacking one window per file.

// Per-user pipe name, as a null-terminated UTF-16 string for the Win32 W APIs.
// Scoped by username so separate logged-in sessions get separate players.
#[cfg(windows)]
fn pipe_name() -> Vec<u16> {
    let user: String = std::env::var("USERNAME")
        .unwrap_or_default()
        .chars()
        .filter(|c| *c != '\\') // backslash is illegal in a pipe name component
        .collect();
    format!(r"\\.\pipe\vgplay-{user}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

// Try to hand `path` to an already-running instance over the named pipe. Returns
// true if a server accepted it (caller should then exit). Returns false when no
// server is listening, so the caller becomes the primary instead.
#[cfg(windows)]
fn try_handoff(path: &Path) -> bool {
    use core::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            sec: *mut c_void,
            disposition: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn WriteFile(h: *mut c_void, buf: *const u8, n: u32, written: *mut u32, ov: *mut c_void)
            -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
        fn WaitNamedPipeW(name: *const u16, timeout: u32) -> i32;
        fn GetLastError() -> u32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn AllowSetForegroundWindow(pid: u32) -> i32;
    }
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const OPEN_EXISTING: u32 = 3;
    const INVALID: *mut c_void = usize::MAX as *mut c_void; // INVALID_HANDLE_VALUE (-1)
    const ERROR_PIPE_BUSY: u32 = 231;
    const ASFW_ANY: u32 = 0xFFFF_FFFF;

    let name = pipe_name();
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    // Retry only the transient "server momentarily busy" case; anything else
    // (no such pipe) means there is no running instance to hand off to.
    for _ in 0..10 {
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                0,
                core::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                core::ptr::null_mut(),
            )
        };
        if h != INVALID {
            unsafe {
                // Let the running instance pull its window to the foreground.
                AllowSetForegroundWindow(ASFW_ANY);
                let mut written = 0u32;
                WriteFile(
                    h,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut written,
                    core::ptr::null_mut(),
                );
                CloseHandle(h);
            }
            return true;
        }
        if unsafe { GetLastError() } == ERROR_PIPE_BUSY {
            unsafe { WaitNamedPipeW(name.as_ptr(), 200) };
            continue;
        }
        return false;
    }
    false
}

#[cfg(windows)]
enum ServerResult {
    Created(usize), // pipe HANDLE as usize (to cross the thread boundary)
    Exists,         // another instance won the race to become the server
    Failed,         // IPC unavailable; run standalone
}

// Try to become the single server by creating the first (and only) instance of
// the named pipe. FILE_FLAG_FIRST_PIPE_INSTANCE makes this fail with
// ERROR_ACCESS_DENIED if a server already owns the name, closing the startup race.
#[cfg(windows)]
fn create_server() -> ServerResult {
    use core::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateNamedPipeW(
            name: *const u16,
            open_mode: u32,
            pipe_mode: u32,
            max_instances: u32,
            out_buf: u32,
            in_buf: u32,
            timeout: u32,
            sec: *mut c_void,
        ) -> *mut c_void;
        fn GetLastError() -> u32;
    }
    const PIPE_ACCESS_INBOUND: u32 = 0x0000_0001;
    const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
    const INVALID: *mut c_void = usize::MAX as *mut c_void;
    const ERROR_ACCESS_DENIED: u32 = 5;

    let name = pipe_name();
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            0, // PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT (all 0)
            1, // one instance is enough: connections are one-shot and rare
            0,
            4096,
            0,
            core::ptr::null_mut(),
        )
    };
    if h != INVALID {
        ServerResult::Created(h as usize)
    } else if unsafe { GetLastError() } == ERROR_ACCESS_DENIED {
        ServerResult::Exists
    } else {
        ServerResult::Failed
    }
}

// Background accept loop for the primary instance. Blocks on each client, reads
// the UTF-8 path it sent, and forwards it into the event loop via the proxy.
#[cfg(windows)]
fn ipc_server(handle: usize, proxy: winit::event_loop::EventLoopProxy<PathBuf>) {
    use core::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn ConnectNamedPipe(h: *mut c_void, ov: *mut c_void) -> i32;
        fn DisconnectNamedPipe(h: *mut c_void) -> i32;
        fn ReadFile(h: *mut c_void, buf: *mut u8, n: u32, read: *mut u32, ov: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }
    const ERROR_PIPE_CONNECTED: u32 = 535; // a client connected before ConnectNamedPipe

    let h = handle as *mut c_void;
    loop {
        let connected = unsafe { ConnectNamedPipe(h, core::ptr::null_mut()) } != 0
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if connected {
            // Read until the client closes its end (byte-stream framing).
            let mut data = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let mut read = 0u32;
                let ok = unsafe {
                    ReadFile(
                        h,
                        buf.as_mut_ptr(),
                        buf.len() as u32,
                        &mut read,
                        core::ptr::null_mut(),
                    )
                };
                if ok == 0 || read == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..read as usize]);
            }
            if let Ok(text) = String::from_utf8(data) {
                if !text.is_empty() && proxy.send_event(PathBuf::from(text)).is_err() {
                    return; // the event loop is gone; stop serving
                }
            }
        }
        unsafe { DisconnectNamedPipe(h) };
    }
}

// ── Sound path ──────────────────────────────────────────────────────────────
// A sound file opens a compact player window with a play/stop button and a repeat
// toggle. Audio is initialized lazily here (only when a sound is actually opened),
// so nothing is paid for linking rodio until a file is played.
fn run_sound(path: &Path) {
    let mut path = path.to_path_buf();

    // Single-instance: if another vgplay is already showing a player, hand this
    // file to it and exit. Otherwise become the primary and keep the server pipe
    // to be serviced by the IPC thread once the event loop's proxy exists. The
    // short loop covers the startup race where a rival becomes the server between
    // our handoff attempt and our own create_server call. (Windows only.)
    #[cfg(windows)]
    let server_pipe: Option<usize> = {
        let mut owned = None;
        for _ in 0..5 {
            if try_handoff(&path) {
                return;
            }
            match create_server() {
                ServerResult::Created(h) => {
                    owned = Some(h);
                    break;
                }
                ServerResult::Exists => continue,
                ServerResult::Failed => break,
            }
        }
        owned
    };

    // Best-effort playback: open the default output and keep the stream and player
    // alive for the window's lifetime — dropping them stops playback. Any failure
    // (no device) leaves the window up with working buttons but no sound.
    let audio = rodio::DeviceSinkBuilder::open_default_sink()
        .ok()
        .map(|stream| {
            let player = rodio::Player::connect_new(stream.mixer());
            (stream, player)
        });

    // Queue a fresh decode of the file, playing it from the start. The decoder is
    // lazy (only the header is read here), so this is cheap and does not block.
    fn append_file(player: &rodio::Player, path: &Path) {
        if let Ok(file) = std::fs::File::open(path) {
            if let Ok(source) = rodio::Decoder::try_from(file) {
                player.append(source);
            }
        }
    }

    // `active` is the play/stop state (true while a track is playing or looping);
    // it drives the button glyph. Playback starts automatically on open.
    let mut active = false;
    let mut repeat = false;
    if let Some((_, player)) = &audio {
        append_file(player, &path);
        active = true;
    }

    let event_loop = EventLoopBuilder::<PathBuf>::with_user_event().build().unwrap();
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("—");
    let window = Rc::new(
        WindowBuilder::new()
            .with_title(format!("vgplay — {name}"))
            .with_window_icon(load_app_icon())
            .with_inner_size(winit::dpi::LogicalSize::new(420.0, 120.0))
            .with_resizable(false)
            // Disable the maximize button (grayed): the player is fixed-size, so
            // there is nothing to maximize. Minimize stays active; on Windows the
            // two are a coupled pair, so a disabled maximize shows grayed.
            .with_enabled_buttons(WindowButtons::CLOSE | WindowButtons::MINIMIZE)
            .with_visible(false)
            .build(&event_loop)
            .unwrap(),
    );
    let context = softbuffer::Context::new(window.clone()).unwrap();
    let mut surface = softbuffer::Surface::new(&context, window.clone()).unwrap();

    // No-flash reveal: cloak, show, paint the first frame, uncloak.
    set_class_background(&window, BG);
    set_cloak(&window, true);
    window.set_visible(true);
    {
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        surface
            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
            .unwrap();
        let mut buffer = surface.buffer_mut().unwrap();
        let slice: &mut [u32] = &mut buffer;
        draw_sound_ui(slice, w, h, active, repeat);
        buffer.present().unwrap();
    }
    set_cloak(&window, false);

    let mut cursor = (0.0f32, 0.0f32);

    // If we are the primary instance, start servicing handoffs from later launches.
    #[cfg(windows)]
    if let Some(h) = server_pipe {
        let proxy = event_loop.create_proxy();
        std::thread::spawn(move || ipc_server(h, proxy));
    }

    event_loop
        .run(move |event, elwt| match event {
            Event::UserEvent(new_path) => {
                // Another launch handed us a file: stop whatever is playing and
                // play the new one in this same window, then surface the window.
                path = new_path;
                if let Some((_, player)) = &audio {
                    player.stop();
                    append_file(player, &path);
                    active = true;
                } else {
                    active = false;
                }
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("—");
                window.set_title(&format!("vgplay — {name}"));
                window.set_minimized(false);
                window.focus_window();
                window.request_redraw();
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => elwt.exit(),
                WindowEvent::KeyboardInput { event: key, .. } => {
                    if key.state == ElementState::Pressed
                        && matches!(key.logical_key.as_ref(), Key::Named(NamedKey::Escape))
                    {
                        elwt.exit();
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    cursor = (position.x as f32, position.y as f32);
                    let size = window.inner_size();
                    let hovering = sound_button_hit(size.width, size.height, cursor).is_some();
                    window.set_cursor_icon(if hovering {
                        winit::window::CursorIcon::Pointer
                    } else {
                        winit::window::CursorIcon::Default
                    });
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    if button != MouseButton::Left || state != ElementState::Pressed {
                        return;
                    }
                    let size = window.inner_size();
                    match sound_button_hit(size.width, size.height, cursor) {
                        Some(SoundButton::PlayStop) => {
                            if let Some((_, player)) = &audio {
                                if active {
                                    player.stop();
                                    active = false;
                                } else {
                                    append_file(player, &path);
                                    active = true;
                                }
                                window.request_redraw();
                            }
                        }
                        Some(SoundButton::Repeat) => {
                            repeat = !repeat;
                            window.request_redraw();
                        }
                        None => {}
                    }
                }
                WindowEvent::RedrawRequested => {
                    let size = window.inner_size();
                    let (w, h) = (size.width.max(1), size.height.max(1));
                    surface
                        .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
                        .unwrap();
                    let mut buffer = surface.buffer_mut().unwrap();
                    let slice: &mut [u32] = &mut buffer;
                    draw_sound_ui(slice, w, h, active, repeat);
                    buffer.present().unwrap();
                }
                _ => {}
            },
            Event::AboutToWait => {
                // Detect the track's natural end to reset the button — and to loop it
                // when repeat is on. Poll only while active; stay idle otherwise.
                if active {
                    if let Some((_, player)) = &audio {
                        if player.empty() {
                            if repeat {
                                append_file(player, &path);
                            } else {
                                active = false;
                            }
                            window.request_redraw();
                        }
                    }
                }
                elwt.set_control_flow(if active {
                    winit::event_loop::ControlFlow::WaitUntil(
                        std::time::Instant::now() + Duration::from_millis(50),
                    )
                } else {
                    winit::event_loop::ControlFlow::Wait
                });
            }
            _ => {}
        })
        .unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("--register") => {
            attach_console();
            match register() {
                Ok(()) => println!("vgplay: sound associations registered (HKCU)."),
                Err(e) => {
                    eprintln!("vgplay --register: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("--unregister") => {
            attach_console();
            match unregister() {
                Ok(()) => println!("vgplay: associations removed."),
                Err(e) => {
                    eprintln!("vgplay --unregister: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("--help") | Some("-h") => {
            attach_console();
            print_help();
            return;
        }
        _ => {}
    }

    // Any file argument is treated as a sound to play; a failed decode simply leaves
    // the window up with working buttons but no audio. With no argument there is
    // nothing to play, so print help instead of opening an empty player.
    match args.get(1).map(|a| absolutize(Path::new(a))) {
        Some(path) => run_sound(&path),
        None => {
            attach_console();
            print_help();
        }
    }
}
