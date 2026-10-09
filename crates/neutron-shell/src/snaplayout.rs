//! Snap Layouts for a window that draws its own title bar.
//!
//! # What is missing without this
//!
//! Hovering the maximise button of a *system* caption pops Windows 11's snap
//! layout chooser. The shell decides to show it by hit-testing the window: it
//! asks `WM_NCHITTEST` where the cursor is, and offers the flyout when the
//! answer is `HTMAXBUTTON`. An undecorated window answers `HTCLIENT`
//! everywhere, so the feature simply never appears — the window is not opting
//! out, it is failing to say where its maximise button is.
//!
//! Dragging to a screen edge still snaps without any of this, because that goes
//! through the OS drag that `ViewportCommand::StartDrag` starts, and so does
//! `Win`+arrow. The flyout is the part that has to be claimed.
//!
//! # The catch, and why this module owns the click as well
//!
//! Answering `HTMAXBUTTON` has a consequence: the cursor is now over a
//! *non-client* area as far as Windows is concerned, so the button stops
//! receiving the ordinary mouse messages egui reads. Claiming the hit test
//! without also handling `WM_NCLBUTTONUP` would leave a button that summons the
//! flyout and no longer maximises anything.
//!
//! Hover is the same problem in miniature: egui never sees the pointer enter,
//! so the button would not light up. The hover flag here is what
//! `chrome.rs` draws from.
//!
//! Everything is read from and written to shared atomics. The window procedure
//! runs on the UI thread, between frames, and must not block or allocate.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Where the maximise button is, and whether the pointer is on it.
///
/// The rectangle is in **physical pixels relative to the client area**, which
/// is what `WM_NCHITTEST` can be compared against after one `ScreenToClient`.
/// The UI writes it every frame; the window procedure only ever reads it.
#[derive(Debug, Default)]
pub struct MaximiseButton {
    pub left: AtomicI32,
    pub top: AtomicI32,
    pub right: AtomicI32,
    pub bottom: AtomicI32,
    /// Set while the pointer is over the button. Read by the title bar so it
    /// can highlight, since egui will not see the pointer there.
    pub hovered: AtomicBool,
}

impl MaximiseButton {
    /// Records where the button is, in physical pixels from the client origin.
    pub fn set(&self, left: i32, top: i32, right: i32, bottom: i32) {
        self.left.store(left, Ordering::Relaxed);
        self.top.store(top, Ordering::Relaxed);
        self.right.store(right, Ordering::Relaxed);
        self.bottom.store(bottom, Ordering::Relaxed);
    }

    pub fn is_hovered(&self) -> bool {
        self.hovered.load(Ordering::Relaxed)
    }

    #[cfg(any(windows, test))]
    fn contains(&self, x: i32, y: i32) -> bool {
        let (l, t) = (
            self.left.load(Ordering::Relaxed),
            self.top.load(Ordering::Relaxed),
        );
        let (r, b) = (
            self.right.load(Ordering::Relaxed),
            self.bottom.load(Ordering::Relaxed),
        );
        // An unset rectangle is empty, so this is false until the first frame
        // has told us where the button is.
        r > l && b > t && x >= l && x < r && y >= t && y < b
    }
}

#[cfg(windows)]
mod imp {
    use super::MaximiseButton;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::UI::WindowsAndMessaging::{
        HTMAXBUTTON, IsZoomed, SW_MAXIMIZE, SW_RESTORE, ShowWindow, WM_NCHITTEST,
        WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE,
    };

    /// Distinguishes our subclass from anyone else's on the same window.
    const SUBCLASS_ID: usize = 0x4E45_5553;

    pub fn watch(hwnd: isize, button: Arc<MaximiseButton>) -> Result<(), String> {
        // Leaked deliberately, as the paste watcher's sender is: the subclass
        // outlives this call, holds the only reference, and the window dies
        // with the process.
        let boxed = Box::into_raw(Box::new(button)) as usize;

        // SAFETY: `hwnd` is this thread's window; `boxed` stays valid forever.
        unsafe { SetWindowSubclass(HWND(hwnd as *mut _), Some(proc), SUBCLASS_ID, boxed) }
            .ok()
            .map_err(|e| format!("could not claim the maximise button: {}", e.message()))
    }

    unsafe extern "system" fn proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        data: usize,
    ) -> LRESULT {
        // SAFETY: `data` is the pointer leaked by `watch`, never freed, and
        // created from this exact type.
        let button = unsafe { &*(data as *const Arc<MaximiseButton>) };

        match msg {
            WM_NCHITTEST => {
                // Let the default run first: it is what reports the resize
                // borders and the caption, and those must keep working.
                // SAFETY: standard tail call, examined rather than returned.
                let hit = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };

                let mut point = POINT {
                    x: (lparam.0 & 0xFFFF) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
                };
                // SAFETY: `point` is a valid out-parameter for this window.
                let converted = unsafe { ScreenToClient(hwnd, &mut point) }.as_bool();

                if converted && button.contains(point.x, point.y) {
                    button.hovered.store(true, Ordering::Relaxed);
                    return LRESULT(HTMAXBUTTON as isize);
                }
                button.hovered.store(false, Ordering::Relaxed);
                hit
            }

            // Swallowed. The default would begin a caption drag or open the
            // system menu, and the button acts on release like every other
            // window button.
            WM_NCLBUTTONDOWN if wparam.0 as i32 == HTMAXBUTTON as i32 => LRESULT(0),

            WM_NCLBUTTONUP if wparam.0 as i32 == HTMAXBUTTON as i32 => {
                // SAFETY: plain window-state calls on this thread's window.
                unsafe {
                    let zoomed = IsZoomed(hwnd).as_bool();
                    let _ = ShowWindow(hwnd, if zoomed { SW_RESTORE } else { SW_MAXIMIZE });
                }
                LRESULT(0)
            }

            // The pointer left the button without the hit test firing again.
            WM_NCMOUSELEAVE => {
                button.hovered.store(false, Ordering::Relaxed);
                // SAFETY: standard tail call for a subclass procedure.
                unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
            }

            WM_NCMOUSEMOVE => {
                button
                    .hovered
                    .store(wparam.0 as i32 == HTMAXBUTTON as i32, Ordering::Relaxed);
                // SAFETY: standard tail call for a subclass procedure.
                unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
            }

            // SAFETY: standard tail call for a subclass procedure.
            _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::MaximiseButton;
    use std::sync::Arc;

    pub fn watch(_hwnd: isize, _button: Arc<MaximiseButton>) -> Result<(), String> {
        Ok(())
    }
}

/// Claims the maximise button's rectangle for Snap Layouts.
///
/// **UI thread only** — a window may only be subclassed from the thread that
/// owns it.
pub fn watch(hwnd: isize, button: Arc<MaximiseButton>) -> Result<(), String> {
    imp::watch(hwnd, button)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_button_contains_nothing() {
        // Before the first frame the rectangle is all zeroes, and a zero-sized
        // rect must not swallow the hit test at the window's top-left corner —
        // which is a resize grip.
        let b = MaximiseButton::default();
        assert!(!b.contains(0, 0));
        assert!(!b.contains(5, 5));
    }

    #[test]
    fn the_rectangle_is_half_open() {
        let b = MaximiseButton::default();
        b.set(100, 0, 144, 34);
        assert!(b.contains(100, 0), "the top-left corner is inside");
        assert!(b.contains(143, 33));
        // Exclusive on the far edges, so two buttons drawn edge to edge do not
        // both claim the pixel between them.
        assert!(!b.contains(144, 33), "the right edge belongs to the next button");
        assert!(!b.contains(143, 34));
        assert!(!b.contains(99, 10));
    }

    #[test]
    fn hover_round_trips() {
        let b = MaximiseButton::default();
        assert!(!b.is_hovered());
        b.hovered.store(true, Ordering::Relaxed);
        assert!(b.is_hovered());
    }
}
