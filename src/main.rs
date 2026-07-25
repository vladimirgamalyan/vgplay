// vgplay — a fast, minimal sound player for Windows (winit + softbuffer + CPU).
// Double-click a sound file in Explorer and a resizable player window appears
// immediately: a waveform with a moving playback cursor, a play/pause and a repeat
// button, and a status line — all drawn by hand on the CPU path (no widget toolkit).
// Audio is initialized lazily — only when a file is actually opened — so launch
// stays cheap; the waveform is decoded off-thread afterwards so it never delays the
// start. A second launch while a player is already open reuses that window
// (Windows): the file is handed to the running instance, which stops any current
// sound and plays the new one.
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

// Is `path` a sound vgplay opens? Extension-only and case-insensitive — the same
// test the associations register. Used to pick neighbors in a folder and to filter
// what a drag-and-drop delivers.
fn is_sound(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SOUND_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

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

// ── Window size persistence ───────────────────────────────────────────────────
// The player's window size is remembered across launches in HKCU (the same hive
// the associations live in — no separate config file). Stored as the *logical*
// size so it is DPI-independent: a window sized on a 150% monitor reopens the same
// on a 100% one. Missing or garbage values simply fall back to the default size.

#[cfg(windows)]
fn load_window_size() -> Option<(u32, u32)> {
    use winreg::RegKey;
    let key = RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey("Software\\vgplay")
        .ok()?;
    let w: u32 = key.get_value("WindowWidth").ok()?;
    let h: u32 = key.get_value("WindowHeight").ok()?;
    (w > 0 && h > 0).then_some((w, h))
}
#[cfg(not(windows))]
fn load_window_size() -> Option<(u32, u32)> {
    None
}

#[cfg(windows)]
fn save_window_size(w: u32, h: u32) {
    use winreg::RegKey;
    if let Ok((key, _)) =
        RegKey::predef(winreg::enums::HKEY_CURRENT_USER).create_subkey("Software\\vgplay")
    {
        let _ = key.set_value("WindowWidth", &w);
        let _ = key.set_value("WindowHeight", &h);
    }
}
#[cfg(not(windows))]
fn save_window_size(_w: u32, _h: u32) {}

// Persist the window's current logical size, unless it is maximized (a maximized
// window's size is the screen, not a size the user chose — keep the last normal one).
fn persist_window_size(window: &winit::window::Window) {
    if window.is_maximized() {
        return;
    }
    let size = window.inner_size().to_logical::<f64>(window.scale_factor());
    let (w, h) = (size.width.round() as u32, size.height.round() as u32);
    if w > 0 && h > 0 {
        save_window_size(w, h);
    }
}

// ── Sound player UI (hand-drawn; no widget toolkit) ──────────────────────────
// The sound window shows two round buttons: play/stop (left) and a repeat toggle
// (right). Shapes are supersampled for anti-aliasing and the whole UI is redrawn
// each frame — both cheap at this size.

const UI_ACCENT: u32 = 0x003B_82F6; // primary/enabled button fill (blue)
const UI_ON_ICON: u32 = 0x00FF_FFFF; // icon on an accent (enabled) button
const UI_OFF_FILL: u32 = 0x00E4_E4E4; // repeat button background when off
const UI_OFF_ICON: u32 = 0x009A_A0A6; // icon on an off button

const WF_PLAYED: u32 = 0x003B_82F6; // waveform, already-played portion (accent)
const WF_UNPLAYED: u32 = 0x00C2_C7CC; // waveform, not-yet-played portion (gray)
const WF_AXIS: u32 = 0x00E0_E0E0; // faint per-channel center axis line
const PLAYHEAD: u32 = 0x0030_3336; // playback cursor line (dark)
const STATUS_TEXT: u32 = 0x0056_5C63; // status-bar text

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

// Left button: an accent disc showing what a click will do — a pause glyph (two
// bars) while a track is playing, a play triangle while paused or stopped.
fn draw_play_pause(buf: &mut [u32], w: u32, h: u32, c: (f32, f32), d: f32, playing: bool) {
    disc(buf, w, h, c.0, c.1, d * 0.5, UI_ACCENT);
    if playing {
        // Two vertical bars.
        let bw = d * 0.09; // half bar width
        let bh = d * 0.22; // half bar height
        let off = d * 0.13; // bar center offset from disc center
        for sx in [-off, off] {
            let cx = c.0 + sx;
            let bb = (cx - bw, c.1 - bh, cx + bw, c.1 + bh);
            fill_aa(buf, w, h, bb, UI_ON_ICON, move |px, py| {
                (px - cx).abs() <= bw && (py - c.1).abs() <= bh
            });
        }
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

// ── Bitmap font (5×7, hand-drawn) ─────────────────────────────────────────────
// The status bar needs text (time, format). The app has no widget/font toolkit
// (ADR 0001), so glyphs are a tiny 5×7 bitmap drawn as solid pixel blocks, in the
// same "draw geometry by hand" spirit as the buttons. Only the characters the
// status line uses are encoded; anything else renders as blank.
const GLYPH_W: i32 = 5;
const GLYPH_H: i32 = 7;

// Each row is 5 bits, MSB = leftmost column. Unknown chars → blank.
fn glyph(c: char) -> [u8; 7] {
    match c {
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        ':' => [0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000],
        '.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00110, 0b00110],
        '/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        _ => [0; 7], // space and anything unencoded
    }
}

// Fill a solid, clipped rectangle of pixels with `color`.
fn fill_rect(buf: &mut [u32], w: u32, h: u32, x0: i32, y0: i32, x1: i32, y1: i32, color: u32) {
    let x0 = x0.max(0);
    let y0 = y0.max(0);
    let x1 = x1.min(w as i32);
    let y1 = y1.min(h as i32);
    for y in y0..y1 {
        let row = y as u32 * w;
        for x in x0..x1 {
            buf[(row + x as u32) as usize] = color;
        }
    }
}

// Draw `text` at (x, y) with each font pixel a `scale`×`scale` block. Uppercased;
// glyphs are 5×7 with a 1-column gap. Returns nothing; text is drawn solid.
fn draw_text(buf: &mut [u32], w: u32, h: u32, x: i32, y: i32, scale: i32, color: u32, text: &str) {
    let mut cx = x;
    for ch in text.chars() {
        let g = glyph(ch.to_ascii_uppercase());
        for (ry, row) in g.iter().enumerate() {
            for gx in 0..GLYPH_W {
                if row & (1 << (GLYPH_W - 1 - gx)) != 0 {
                    let px = cx + gx * scale;
                    let py = y + ry as i32 * scale;
                    fill_rect(buf, w, h, px, py, px + scale, py + scale, color);
                }
            }
        }
        cx += (GLYPH_W + 1) * scale;
    }
}

// ── Player layout & rendering ─────────────────────────────────────────────────
#[derive(Clone, Copy)]
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    fn contains(&self, p: (f32, f32)) -> bool {
        p.0 >= self.x0 && p.0 <= self.x1 && p.1 >= self.y0 && p.1 <= self.y1
    }
    fn width(&self) -> f32 {
        self.x1 - self.x0
    }
}

// All geometry (waveform area, the two buttons, status text origin), in physical
// pixels, derived from the window size. Shared by drawing and hit-testing so they
// never drift apart. Everything is a fraction of the window, so it is DPI-agnostic.
struct Layout {
    wave: Rect,
    play: (f32, f32, f32),   // cx, cy, r
    repeat: (f32, f32, f32), // cx, cy, r
    status_x: i32,
    text_scale: i32, // preferred scale from bar height; shrunk to fit width at draw
}

fn layout(w: u32, h: u32) -> Layout {
    let (wf, hf) = (w as f32, h as f32);
    let pad = (hf * 0.05).max(6.0);
    let bar_h = (hf * 0.24).clamp(30.0, 68.0);
    let bar_top = hf - bar_h;
    let wave = Rect {
        x0: pad,
        y0: pad,
        x1: wf - pad,
        y1: bar_top - pad * 0.5,
    };
    let cy = bar_top + bar_h * 0.5;
    let r = ((bar_h * 0.5 - 5.0).max(9.0)) * 0.82;
    let gap = r * 0.7;
    let cx1 = pad + r;
    let cx2 = cx1 + r * 2.0 + gap;
    let text_scale = ((bar_h / 20.0) as i32).clamp(1, 4);
    let status_x = (cx2 + r + gap) as i32;
    Layout {
        wave,
        play: (cx1, cy, r),
        repeat: (cx2, cy, r),
        status_x,
        text_scale,
    }
}

// Rendered width of `text` at `scale`, in pixels (glyphs are 5 wide + a 1px gap).
fn text_width(text: &str, scale: i32) -> i32 {
    text.chars().count() as i32 * (GLYPH_W + 1) * scale
}

enum Hit {
    Play,
    Repeat,
    Wave,
    None,
}

fn hit_test(lay: &Layout, p: (f32, f32)) -> Hit {
    let in_disc = |c: (f32, f32, f32)| {
        let (dx, dy) = (p.0 - c.0, p.1 - c.1);
        dx * dx + dy * dy <= c.2 * c.2
    };
    if in_disc(lay.play) {
        Hit::Play
    } else if in_disc(lay.repeat) {
        Hit::Repeat
    } else if lay.wave.contains(p) {
        Hit::Wave
    } else {
        Hit::None
    }
}

// Aggregate the stored envelope buckets covering pixel column `px` of `cols` into a
// single (min, max) pair. Multiple buckets per column at narrow widths; multiple
// columns per bucket at wide widths (block scaling) — both handled by index range.
fn column_minmax(lane: &[(f32, f32)], px: u32, cols: u32) -> (f32, f32) {
    if lane.is_empty() {
        return (0.0, 0.0);
    }
    let n = lane.len() as u64;
    let b0 = (px as u64 * n / cols as u64) as usize;
    let b1 = (((px as u64 + 1) * n / cols as u64) as usize).max(b0 + 1);
    let mut mn = f32::INFINITY;
    let mut mx = f32::NEG_INFINITY;
    for &(lo, hi) in &lane[b0..b1.min(lane.len())] {
        mn = mn.min(lo);
        mx = mx.max(hi);
    }
    if mn > mx {
        (0.0, 0.0)
    } else {
        (mn, mx)
    }
}

// Draw one channel's envelope into `lane_rect`, coloring columns left of `played_x`
// with the played (accent) color and the rest gray.
fn draw_lane(buf: &mut [u32], w: u32, h: u32, lane_rect: Rect, data: &[(f32, f32)], played_x: f32) {
    let cols = lane_rect.width().max(1.0) as u32;
    let cy = (lane_rect.y0 + lane_rect.y1) * 0.5;
    let half = (lane_rect.y1 - lane_rect.y0) * 0.5 * 0.92;
    let x0 = lane_rect.x0 as i32;
    // Faint center axis, so a silent lane still reads as a line.
    fill_rect(buf, w, h, x0, cy as i32, x0 + cols as i32, cy as i32 + 1, WF_AXIS);
    for px in 0..cols {
        let (mn, mx) = column_minmax(data, px, cols);
        let ytop = (cy - mx * half).min(cy);
        let ybot = (cy - mn * half).max(cy);
        let x = x0 + px as i32;
        let color = if (x as f32) < played_x {
            WF_PLAYED
        } else {
            WF_UNPLAYED
        };
        // At least 1px tall so silence still shows a center line.
        let (yt, yb) = (ytop.floor() as i32, ybot.ceil() as i32);
        let yb = yb.max(yt + 1);
        fill_rect(buf, w, h, x, yt, x + 1, yb, color);
    }
}

fn draw_waveform(buf: &mut [u32], w: u32, h: u32, area: Rect, wf: &Waveform, frac: Option<f32>) {
    let nch = wf.lanes.len().min(2).max(1);
    let played_x = frac.map_or(-1.0, |f| area.x0 + f * area.width());
    if nch == 1 {
        draw_lane(buf, w, h, area, &wf.lanes[0], played_x);
        return;
    }
    // Two lanes stacked with a small gap; a faint axis at each lane center.
    let gap = ((area.y1 - area.y0) * 0.06).max(2.0);
    let mid = (area.y0 + area.y1) * 0.5;
    let top = Rect {
        y1: mid - gap * 0.5,
        ..area
    };
    let bot = Rect {
        y0: mid + gap * 0.5,
        ..area
    };
    draw_lane(buf, w, h, top, &wf.lanes[0], played_x);
    draw_lane(buf, w, h, bot, &wf.lanes[1], played_x);
}

// Full UI: background, waveform, playhead, the two buttons, and the status text.
struct UiState<'a> {
    playing: bool, // pause glyph shown when true; play triangle otherwise
    repeat: bool,
    frac: Option<f32>, // playhead position, 0..1
    waveform: Option<&'a Waveform>,
    status: &'a str,
}

fn draw_ui(buf: &mut [u32], w: u32, h: u32, s: &UiState) {
    buf.iter_mut().for_each(|p| *p = BG);
    let lay = layout(w, h);
    if let Some(wf) = s.waveform {
        draw_waveform(buf, w, h, lay.wave, wf, s.frac);
    }
    if let Some(f) = s.frac {
        let x = (lay.wave.x0 + f * lay.wave.width()) as i32;
        let lw = ((h as f32 * 0.004).round() as i32).max(1);
        fill_rect(buf, w, h, x, lay.wave.y0 as i32, x + lw, lay.wave.y1 as i32, PLAYHEAD);
    }
    draw_play_pause(buf, w, h, (lay.play.0, lay.play.1), lay.play.2 * 2.0, s.playing);
    draw_repeat(buf, w, h, (lay.repeat.0, lay.repeat.1), lay.repeat.2 * 2.0, s.repeat);
    // Shrink the status text below the bar-height scale if it would overflow the width.
    let avail = (lay.wave.x1 as i32 - lay.status_x).max(1);
    let fit = avail / text_width(s.status, 1).max(1);
    let scale = fit.clamp(1, lay.text_scale);
    let ty = (lay.play.1 - (GLYPH_H * scale) as f32 * 0.5) as i32;
    draw_text(buf, w, h, lay.status_x, ty, scale, STATUS_TEXT, s.status);
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
fn ipc_server(handle: usize, proxy: winit::event_loop::EventLoopProxy<AppEvent>) {
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
                if !text.is_empty()
                    && proxy
                        .send_event(AppEvent::Open(PathBuf::from(text)))
                        .is_err()
                {
                    return; // the event loop is gone; stop serving
                }
            }
        }
        unsafe { DisconnectNamedPipe(h) };
    }
}

// ── Waveform analysis (background) ────────────────────────────────────────────
// Drawing a waveform needs the whole file decoded; doing that on the launch path
// would delay playback, which must stay instant. So playback starts first (lazy
// decoder, as before) and the file is decoded a *second* time on a background
// thread purely to compute a per-channel min/max envelope. The result is delivered
// back into the event loop; the waveform simply "pops in" a moment later.

// Target stored resolution per channel. The envelope is re-bucketed to the window
// width at draw time, so this only bounds memory (~2·WF_RES pairs per channel),
// not the on-screen detail.
const WF_RES: usize = 2048;

struct Waveform {
    gen: u64,                    // matches the open that requested it (stale results dropped)
    lanes: Vec<Vec<(f32, f32)>>, // per-channel (min, max) envelope
    duration: Duration,
    sample_rate: u32,
    channels: u16,
    bits: Option<u16>, // source bit depth; None for lossy codecs (mp3/vorbis)
}

// The two event-loop messages: a file handed over by a later launch (IPC), and a
// finished waveform analysis.
enum AppEvent {
    Open(PathBuf),
    Waveform(Waveform),
}

// Merge adjacent buckets pairwise, halving the count — used to keep the envelope
// bounded while streaming a file of unknown length.
fn halve(lane: &mut Vec<(f32, f32)>) {
    let mut out = Vec::with_capacity(lane.len() / 2 + 1);
    let mut i = 0;
    while i < lane.len() {
        if i + 1 < lane.len() {
            out.push((lane[i].0.min(lane[i + 1].0), lane[i].1.max(lane[i + 1].1)));
        } else {
            out.push(lane[i]);
        }
        i += 2;
    }
    *lane = out;
}

// Read the source bit depth from the container header (rodio does not expose it).
// This is a cheap header-only probe with symphonia — no full decode. Returns None
// for lossy codecs (mp3, vorbis) that have no fixed bit depth.
fn probe_bits(path: &Path) -> Option<u16> {
    use symphonia::core::codecs::CODEC_TYPE_NULL;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let track = probed
        .format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)?;
    track.codec_params.bits_per_sample.map(|b| b as u16)
}

// Decode the whole file and build a per-channel min/max envelope. Streams the
// samples with adaptive downsampling so memory stays bounded regardless of length;
// duration is derived from the exact decoded frame count. Returns None on any
// decode failure (the window then just shows no waveform).
fn analyze(path: &Path, gen: u64) -> Option<Waveform> {
    use rodio::Source;
    let file = std::fs::File::open(path).ok()?;
    let decoder = rodio::Decoder::try_from(file).ok()?;
    let sample_rate = decoder.sample_rate().get();
    let ch = decoder.channels().get() as usize;
    if ch == 0 {
        return None;
    }
    let bits = probe_bits(path);

    let mut lanes: Vec<Vec<(f32, f32)>> = vec![Vec::with_capacity(2 * WF_RES); ch];
    let mut cur = vec![(f32::INFINITY, f32::NEG_INFINITY); ch]; // current bucket per channel
    let mut frames_in_cur: u64 = 0;
    let mut frames_per_bucket: u64 = 1;
    let mut total_frames: u64 = 0;
    let mut c = 0usize;

    for s in decoder {
        let a = &mut cur[c];
        a.0 = a.0.min(s);
        a.1 = a.1.max(s);
        c += 1;
        if c == ch {
            c = 0;
            frames_in_cur += 1;
            total_frames += 1;
            if frames_in_cur == frames_per_bucket {
                for ci in 0..ch {
                    lanes[ci].push(cur[ci]);
                    cur[ci] = (f32::INFINITY, f32::NEG_INFINITY);
                }
                frames_in_cur = 0;
                if lanes[0].len() >= 2 * WF_RES {
                    for lane in &mut lanes {
                        halve(lane);
                    }
                    frames_per_bucket *= 2;
                }
            }
        }
    }
    // Flush a partial trailing bucket.
    if frames_in_cur > 0 {
        for ci in 0..ch {
            lanes[ci].push(cur[ci]);
        }
    }
    if total_frames == 0 {
        return None;
    }

    let duration = Duration::from_secs_f64(total_frames as f64 / sample_rate as f64);
    Some(Waveform {
        gen,
        lanes,
        duration,
        sample_rate,
        channels: ch as u16,
        bits,
    })
}

fn spawn_analysis(path: PathBuf, gen: u64, proxy: winit::event_loop::EventLoopProxy<AppEvent>) {
    std::thread::spawn(move || {
        if let Some(wf) = analyze(&path, gen) {
            let _ = proxy.send_event(AppEvent::Waveform(wf));
        }
    });
}

// "M:SS" from a duration (minutes are not zero-padded; seconds are).
fn fmt_time(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

// A one-line status: FORMAT, sample rate (full Hz), bit depth (when the codec has
// one), channel layout, position / duration. Fields are separated by three spaces.
fn status_line(path: &Path, wf: Option<&Waveform>, pos: Duration) -> String {
    let fmt = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_uppercase())
        .unwrap_or_default();
    match wf {
        None => fmt,
        Some(wf) => {
            let chans = match wf.channels {
                1 => "MONO".to_string(),
                2 => "STEREO".to_string(),
                n => format!("{}CH", n),
            };
            let mut parts = vec![fmt, format!("{} HZ", wf.sample_rate)];
            if let Some(bits) = wf.bits {
                parts.push(format!("{} BIT", bits));
            }
            parts.push(chans);
            parts.push(format!("{} / {}", fmt_time(pos), fmt_time(wf.duration)));
            parts.join("   ")
        }
    }
}

// ── Neighbor navigation (arrow keys) ─────────────────────────────────────────
// Left/Right step to the previous/next sound in the same folder and play it, as
// if that file had been opened from Explorer. Files are ordered the way Explorer
// orders them so the step matches what the user sees in the folder.

// Compare two files the way Explorer's file list does: a natural, digit-aware,
// case-insensitive order (so "track2" precedes "track10"). StrCmpLogicalW is the
// very comparator Explorer uses, so arrow-key order matches the folder view.
#[cfg(windows)]
fn explorer_cmp(a: &Path, b: &Path) -> std::cmp::Ordering {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "shlwapi")]
    extern "system" {
        fn StrCmpLogicalW(psz1: *const u16, psz2: *const u16) -> i32;
    }
    let wide = |p: &Path| -> Vec<u16> {
        p.file_name()
            .unwrap_or_default()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let (wa, wb) = (wide(a), wide(b));
    unsafe { StrCmpLogicalW(wa.as_ptr(), wb.as_ptr()) }.cmp(&0)
}

// Non-Windows fallback: a plain case-insensitive file-name ordering.
#[cfg(not(windows))]
fn explorer_cmp(a: &Path, b: &Path) -> std::cmp::Ordering {
    let key = |p: &Path| {
        p.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_lowercase()
    };
    key(a).cmp(&key(b))
}

// The sound file adjacent to `path` in its directory, in Explorer's order.
// `delta` is -1 for the previous file, +1 for the next. Returns None when there
// is no neighbor in that direction (navigation stops at the ends, it does not
// wrap) or the directory cannot be read.
fn sibling_sound(path: &Path, delta: i32) -> Option<PathBuf> {
    let dir = path.parent()?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_sound(p))
        .collect();
    files.sort_by(|a, b| explorer_cmp(a, b));
    // Locate the current file by name, case-insensitively: the filesystem is
    // case-insensitive on Windows, so the opened path's casing may differ from
    // the directory entry's.
    let cur = files.iter().position(|p| match (p.file_name(), path.file_name()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    })?;
    let idx = cur as i32 + delta;
    (0..files.len() as i32)
        .contains(&idx)
        .then(|| files[idx as usize].clone())
}

// ── Sound path ──────────────────────────────────────────────────────────────
// A sound file opens a resizable player window: a waveform with a moving playback
// cursor, a play/pause and a repeat button, and a status line. Audio is initialized
// lazily here (only when a sound is actually opened), so nothing is paid for linking
// rodio until a file is played; the waveform is computed off-thread afterwards.
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

    // Toggle play/pause exactly like the on-screen play button: cue a fresh track
    // from the start when stopped, otherwise flip between playing and paused. Shared
    // by the button hit and the Space key so the two never drift apart.
    fn toggle_play(player: &rodio::Player, path: &Path, has_track: &mut bool, paused: &mut bool) {
        if !*has_track {
            append_file(player, path);
            *has_track = true;
            *paused = false;
        } else if *paused {
            player.play();
            *paused = false;
        } else {
            player.pause();
            *paused = true;
        }
    }

    // Player state. `has_track` is true while a source is loaded (playing or paused);
    // `paused` distinguishes the two. Together they drive the play/pause glyph.
    // Playback starts automatically on open. `gen` tags each opened file so a late
    // waveform result from a previous file is ignored (window reuse, ADR 0002).
    let mut has_track = false;
    let mut paused = false;
    let mut repeat = false;
    let mut gen: u64 = 0;
    let mut waveform: Option<Waveform> = None;
    let mut dragging = false; // scrubbing the playhead with the mouse
    let mut drag_frac = 0.0f32;
    let mut pending_drop: Option<PathBuf> = None; // first sound of a dropped batch
    if let Some((_, player)) = &audio {
        append_file(player, &path);
        has_track = true;
    }

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build().unwrap();
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("—");
    let (init_w, init_h) = load_window_size().unwrap_or((640, 260));
    let window = Rc::new(
        WindowBuilder::new()
            .with_title(format!("vgplay — {name}"))
            .with_window_icon(load_app_icon())
            .with_inner_size(winit::dpi::LogicalSize::new(init_w as f64, init_h as f64))
            .with_min_inner_size(winit::dpi::LogicalSize::new(360.0, 180.0))
            .with_resizable(true)
            .with_enabled_buttons(WindowButtons::all())
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
        // First frame: UI with an empty waveform. It fills in when analysis finishes.
        let status = status_line(&path, None, Duration::ZERO);
        draw_ui(
            slice,
            w,
            h,
            &UiState {
                playing: has_track,
                repeat,
                frac: None,
                waveform: None,
                status: &status,
            },
        );
        buffer.present().unwrap();
    }
    set_cloak(&window, false);

    let mut cursor = (0.0f32, 0.0f32);

    // Playback has started; now decode the file off-thread to build the waveform.
    let proxy = event_loop.create_proxy();
    spawn_analysis(path.clone(), gen, proxy.clone());

    // If we are the primary instance, start servicing handoffs from later launches.
    #[cfg(windows)]
    if let Some(h) = server_pipe {
        let ipc_proxy = event_loop.create_proxy();
        std::thread::spawn(move || ipc_server(h, ipc_proxy));
    }

    event_loop
        .run(move |event, elwt| match event {
            Event::UserEvent(AppEvent::Open(new_path)) => {
                // Another launch handed us a file: stop whatever is playing and
                // play the new one in this same window, then surface the window.
                path = new_path;
                gen += 1;
                waveform = None;
                dragging = false;
                paused = false;
                if let Some((_, player)) = &audio {
                    player.stop();
                    append_file(player, &path);
                    // Clear any lingering pause from before the swap: stop()/append()
                    // reset the queue but not rodio's pause flag, so without this the
                    // new track would be silently paused while we show it as playing.
                    player.play();
                    has_track = true;
                } else {
                    has_track = false;
                }
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("—");
                window.set_title(&format!("vgplay — {name}"));
                window.set_minimized(false);
                window.focus_window();
                spawn_analysis(path.clone(), gen, proxy.clone());
                window.request_redraw();
            }
            Event::UserEvent(AppEvent::Waveform(wf)) => {
                // Ignore a result for a file that has since been replaced.
                if wf.gen == gen {
                    waveform = Some(wf);
                    window.request_redraw();
                }
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    persist_window_size(&window);
                    elwt.exit();
                }
                WindowEvent::KeyboardInput { event: key, .. } => {
                    // Ignore auto-repeat so holding Space doesn't rapidly toggle.
                    if key.state == ElementState::Pressed && !key.repeat {
                        match key.logical_key.as_ref() {
                            Key::Named(NamedKey::Escape) => {
                                persist_window_size(&window);
                                elwt.exit();
                            }
                            Key::Named(NamedKey::Space) => {
                                if let Some((_, player)) = &audio {
                                    toggle_play(player, &path, &mut has_track, &mut paused);
                                    window.request_redraw();
                                }
                            }
                            // Left/Right: open and play the previous/next sound in
                            // the folder, reusing the same swap path as an Explorer
                            // open (AppEvent::Open) so behavior is identical.
                            Key::Named(NamedKey::ArrowLeft) => {
                                if let Some(prev) = sibling_sound(&path, -1) {
                                    let _ = proxy.send_event(AppEvent::Open(prev));
                                }
                            }
                            Key::Named(NamedKey::ArrowRight) => {
                                if let Some(next) = sibling_sound(&path, 1) {
                                    let _ = proxy.send_event(AppEvent::Open(next));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // Drag and drop: a drop delivers one DroppedFile event per file.
                // Keep the first sound of the batch and ignore the rest (folders,
                // unsupported types, further files); it is opened in AboutToWait,
                // once the whole batch has arrived.
                WindowEvent::DroppedFile(dropped) => {
                    if pending_drop.is_none() && dropped.is_file() && is_sound(&dropped) {
                        pending_drop = Some(dropped);
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    cursor = (position.x as f32, position.y as f32);
                    let size = window.inner_size();
                    let lay = layout(size.width, size.height);
                    if dragging {
                        drag_frac =
                            ((cursor.0 - lay.wave.x0) / lay.wave.width()).clamp(0.0, 1.0);
                        window.request_redraw();
                    }
                    // The waveform is only interactive once it (and the duration) exist.
                    let hovering = match hit_test(&lay, cursor) {
                        Hit::Play | Hit::Repeat => true,
                        Hit::Wave => waveform.is_some(),
                        Hit::None => false,
                    };
                    window.set_cursor_icon(if hovering {
                        winit::window::CursorIcon::Pointer
                    } else {
                        winit::window::CursorIcon::Default
                    });
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    if button != MouseButton::Left {
                        return;
                    }
                    let size = window.inner_size();
                    let lay = layout(size.width, size.height);
                    match state {
                        ElementState::Pressed => match hit_test(&lay, cursor) {
                            Hit::Play => {
                                if let Some((_, player)) = &audio {
                                    toggle_play(player, &path, &mut has_track, &mut paused);
                                    window.request_redraw();
                                }
                            }
                            Hit::Repeat => {
                                repeat = !repeat;
                                window.request_redraw();
                            }
                            Hit::Wave => {
                                if waveform.is_some() {
                                    dragging = true;
                                    drag_frac = ((cursor.0 - lay.wave.x0) / lay.wave.width())
                                        .clamp(0.0, 1.0);
                                    window.request_redraw();
                                }
                            }
                            Hit::None => {}
                        },
                        ElementState::Released => {
                            if dragging {
                                dragging = false;
                                if let (Some((_, player)), Some(wf)) = (&audio, &waveform) {
                                    let target = wf.duration.mul_f64(drag_frac as f64);
                                    // Scrubbing from a stopped state cues playback there.
                                    if !has_track {
                                        append_file(player, &path);
                                        has_track = true;
                                        paused = false;
                                    }
                                    let _ = player.try_seek(target);
                                }
                                window.request_redraw();
                            }
                        }
                    }
                }
                WindowEvent::RedrawRequested => {
                    let size = window.inner_size();
                    let (w, h) = (size.width.max(1), size.height.max(1));
                    surface
                        .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
                        .unwrap();

                    // Playhead position: the drag target while scrubbing, otherwise the
                    // live playback position (or the start when stopped).
                    let dur = waveform.as_ref().map(|wf| wf.duration);
                    let pos = match &audio {
                        Some((_, player)) if has_track => player.get_pos(),
                        _ => Duration::ZERO,
                    };
                    let (frac, disp_pos) = if dragging {
                        let p = dur.map(|d| d.mul_f64(drag_frac as f64)).unwrap_or(Duration::ZERO);
                        (Some(drag_frac), p)
                    } else {
                        match dur {
                            Some(d) if has_track && d.as_secs_f64() > 0.0 => (
                                Some((pos.as_secs_f64() / d.as_secs_f64()).clamp(0.0, 1.0) as f32),
                                pos,
                            ),
                            Some(_) => (Some(0.0), Duration::ZERO),
                            None => (None, Duration::ZERO),
                        }
                    };
                    let status = status_line(&path, waveform.as_ref(), disp_pos);

                    let mut buffer = surface.buffer_mut().unwrap();
                    let slice: &mut [u32] = &mut buffer;
                    draw_ui(
                        slice,
                        w,
                        h,
                        &UiState {
                            playing: has_track && !paused,
                            repeat,
                            frac,
                            waveform: waveform.as_ref(),
                            status: &status,
                        },
                    );
                    buffer.present().unwrap();
                }
                _ => {}
            },
            Event::AboutToWait => {
                // Play what a drag-and-drop delivered, through the same swap path as
                // an Explorer open (AppEvent::Open) so behavior is identical.
                if let Some(dropped) = pending_drop.take() {
                    let _ = proxy.send_event(AppEvent::Open(dropped));
                }
                // Detect the track's natural end: loop it when repeat is on, otherwise
                // reset to a stopped state. Only meaningful while actually playing.
                if has_track && !paused {
                    if let Some((_, player)) = &audio {
                        if player.empty() {
                            if repeat {
                                append_file(player, &path);
                            } else {
                                has_track = false;
                            }
                            window.request_redraw();
                        }
                    }
                }
                // Animate the playhead ~30fps while playing; stay idle otherwise.
                if has_track && !paused {
                    window.request_redraw();
                    elwt.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                        std::time::Instant::now() + Duration::from_millis(33),
                    ));
                } else {
                    elwt.set_control_flow(winit::event_loop::ControlFlow::Wait);
                }
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
