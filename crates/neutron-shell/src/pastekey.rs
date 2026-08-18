//! Notices `Ctrl+V` on the main window.
//!
//! # Why this exists at all
//!
//! egui never tells us. `egui-winit` intercepts the three clipboard chords
//! before they become key events: `Ctrl+C` and `Ctrl+X` become
//! `Event::Copy`/`Event::Cut` unconditionally, which is fine, but `Ctrl+V`
//! becomes `Event::Paste(text)` *only when the clipboard holds text* — and it
//! returns either way, so the `V` key press is dropped.
//!
//! Copying a file puts `CF_HDROP` on the clipboard and no text at all. Measured
//! on this machine: copying a file in Explorer leaves `Get-Clipboard -Format
//! Text` empty. So for the one case a file manager exists to handle, egui
//! delivers neither the event nor the key, and there is nothing to listen for.
//!
//! Hence a window subclass, which sees `WM_KEYDOWN` before winit translates it.
//!
//! # Why this is safe on the UI thread
//!
//! Because it does one thing: push to an unbounded channel and hand the message
//! straight on to the next procedure. No COM, no blocking, no allocation on the
//! hot path. The rule this crate is built around — nothing that can block runs
//! on the paint thread — is intact.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_V};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN;

/// Distinguishes our subclass from anyone else's on the same window.
const SUBCLASS_ID: usize = 0x4E45_5550;

/// Starts reporting `Ctrl+V` on `hwnd` through `tx`.
///
/// **UI thread only** — a window may only be subclassed from the thread that
/// owns it.
pub fn watch(hwnd: isize, tx: crossbeam_channel::Sender<()>) -> Result<(), String> {
    // Leaked deliberately: the subclass outlives this call and holds the only
    // reference. It is one sender for the life of the process, and the window
    // dies with the process, so there is nothing to reclaim.
    let boxed = Box::into_raw(Box::new(tx)) as usize;

    // SAFETY: `hwnd` is this thread's window; `boxed` stays valid forever.
    unsafe { SetWindowSubclass(HWND(hwnd as *mut _), Some(proc), SUBCLASS_ID, boxed) }
        .ok()
        .map_err(|e| format!("could not watch for paste: {}", e.message()))
}

/// Whether a key is currently held.
///
/// `GetKeyState` reports the state in the high bit, which makes the value
/// negative — the usual `& 0x8000` test on a signed 16-bit value.
fn held(key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
    // SAFETY: reads thread-local key state; no pointers involved.
    let state = unsafe { GetKeyState(key.0 as i32) };
    state < 0
}

unsafe extern "system" fn proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    if msg == WM_KEYDOWN && wparam.0 as u16 == VK_V.0 {
        // Bit 30 of lParam is the previous key state: set means this is the
        // keyboard repeating, and holding Ctrl+V should paste once, not once
        // per repeat interval.
        let repeat = lparam.0 & (1 << 30) != 0;

        // Alt excluded so Ctrl+Alt+V — which is AltGr+V on many layouts, and a
        // plain typed character — is not a paste.
        if !repeat && held(VK_CONTROL) && !held(VK_MENU) {
            // SAFETY: `data` is the pointer leaked by `watch`, which is never
            // freed and was created from this exact type.
            let tx = unsafe { &*(data as *const crossbeam_channel::Sender<()>) };
            let _ = tx.send(());
        }
    }

    // Passed on either way. Swallowing it would stop winit seeing the key, and
    // a text field in the window would lose its own paste.
    // SAFETY: the standard tail call for a subclass procedure.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}
