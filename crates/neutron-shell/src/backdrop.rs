//! Window backdrop: real compositor-blurred glass.
//!
//! The blur is done by DWM, not by us. Windows 11 22H2 added
//! `DWMWA_SYSTEMBACKDROP_TYPE`, which tells the compositor to sample and blur
//! whatever is behind the window and composite it beneath our (translucent)
//! client area. That is the only way to get *true* glassmorphism: the blur
//! covers other applications, not just our own wallpaper-coloured fill.
//!
//! The alternative — painting a blurred screenshot ourselves — cannot see other
//! windows, goes stale the moment anything behind moves, and costs a full-screen
//! capture per frame.
//!
//! Acrylic (`TRANSIENTWINDOW`) is chosen over Mica (`MAINWINDOW`) because Mica
//! samples only the desktop wallpaper and ignores intervening windows, which
//! reads as a flat tint rather than glass.
//!
//! Everything here degrades silently: on Windows 10 the attributes are simply
//! rejected and the window renders with its opaque fallback fill. That is why
//! failures are logged at debug level rather than surfaced.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
};

/// Enables acrylic glass and matches the title bar to the app theme.
///
/// `hwnd` is the raw window handle from `raw-window-handle`. Safe to call
/// repeatedly — the theme toggle re-applies it so the title bar follows.
pub fn apply_glass(hwnd: isize, dark: bool) {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);

    set_dark_titlebar(hwnd, dark);
    set_backdrop(hwnd, DWMSBT_TRANSIENTWINDOW);
}

/// Turns the effect off — for the "reduce transparency" accessibility setting,
/// or when a user simply dislikes it.
pub fn disable_glass(hwnd: isize) {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    set_backdrop(hwnd, DWMSBT_NONE);
}

fn set_backdrop(hwnd: HWND, kind: DWM_SYSTEMBACKDROP_TYPE) {
    let value = kind.0;
    // SAFETY: `hwnd` is a live top-level window; the pointer and size describe
    // the i32 the attribute expects.
    let hr = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &value as *const i32 as *const core::ffi::c_void,
            size_of::<i32>() as u32,
        )
    };
    if let Err(e) = hr {
        // Expected on Windows 10 and pre-22H2 Windows 11.
        tracing::debug!("system backdrop unavailable, falling back to opaque: {e}");
    }
}

/// Switches the non-client area (title bar, borders) to dark mode.
///
/// Without this a dark glass window keeps a bright white title bar, which looks
/// broken — the one piece of chrome we do not draw ourselves.
fn set_dark_titlebar(hwnd: HWND, dark: bool) {
    let value: i32 = i32::from(dark);
    // SAFETY: as above.
    let hr = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &value as *const i32 as *const core::ffi::c_void,
            size_of::<i32>() as u32,
        )
    };
    if let Err(e) = hr {
        tracing::debug!("immersive dark mode unavailable: {e}");
    }
}

/// Whether the user has asked the OS to reduce transparency.
///
/// Honoured because glass is exactly the effect this setting exists to
/// suppress: users enable it for motion sensitivity, for legibility, or to save
/// power on a laptop. Ignoring it would be a straightforward accessibility bug.
pub fn transparency_enabled() -> bool {
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, KEY_READ, REG_VALUE_TYPE, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };
    use windows::core::w;

    let mut key = Default::default();
    // SAFETY: constant key path; `key` receives an owned handle closed below.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            Some(0),
            KEY_READ,
            &mut key,
        )
    };
    if opened.is_err() {
        return true;
    }

    let mut value: u32 = 1;
    let mut size = size_of::<u32>() as u32;
    let mut kind = REG_VALUE_TYPE::default();
    // SAFETY: `value` and `size` are valid for the duration of the call.
    let read = unsafe {
        RegQueryValueExW(
            key,
            w!("EnableTransparency"),
            None,
            Some(&mut kind),
            Some(&mut value as *mut u32 as *mut u8),
            Some(&mut size),
        )
    };
    // SAFETY: `key` was opened successfully and is not used afterwards.
    let _ = unsafe { RegCloseKey(key) };

    // Absent value means the default, which is transparency on.
    if read.is_err() { true } else { value != 0 }
}
