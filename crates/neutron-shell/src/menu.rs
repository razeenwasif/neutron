//! Native shell context menus via `IContextMenu`.
//!
//! This is the entry to every extension the user has installed — 7-Zip, Git,
//! TortoiseSVN, "Scan with Defender", "Open in Terminal". Reimplementing a menu
//! of our own would mean shipping a file manager where none of the user's tools
//! work.
//!
//! # The whole menu runs on a worker apartment
//!
//! `TrackPopupMenuEx` runs a *modal message loop* on the calling thread until
//! the user picks something or dismisses it. On the UI thread that would freeze
//! rendering for as long as the menu is open — and far worse, a slow
//! third-party handler enumerating a network share during `QueryContextMenu`
//! would hang the window with no menu on screen at all. That is exactly the
//! Explorer failure this project exists to avoid.
//!
//! So everything here happens on an apartment thread, which is free to block.
//! The UI keeps painting at full rate behind the menu.
//!
//! # Why a hidden window rather than subclassing the main one
//!
//! Owner-drawn menu items — the ones with icons, which is most third-party
//! entries — require `WM_INITMENUPOPUP`, `WM_DRAWITEM` and `WM_MEASUREITEM` to
//! be forwarded to `IContextMenu2::HandleMenuMsg`. Those messages go to the
//! menu's owner window.
//!
//! The obvious approach is to subclass winit's `HWND` with `SetWindowSubclass`.
//! It is also wrong here: that window belongs to the UI thread, and
//! `TrackPopupMenuEx` must be called on the thread owning the window it is
//! given. Using it would drag the modal loop back onto the UI thread and undo
//! the point above.
//!
//! Instead each menu creates its own hidden window *on the apartment thread*
//! and owns it for the menu's lifetime. Forwarding happens in that window's own
//! procedure, winit's window is never touched, and the modal loop stays where it
//! belongs.

use std::cell::RefCell;
use std::path::Path;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    CMF_EXPLORE, CMF_NORMAL, CMINVOKECOMMANDINFOEX, IContextMenu, IContextMenu2, IContextMenu3,
    IShellFolder, SHBindToParent, SHParseDisplayName,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, GetMenuItemCount,
    HMENU, RegisterClassW, SW_HIDE, SetForegroundWindow, TPM_LEFTALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR,
    WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};
use windows::core::{Interface, PCWSTR, w};

/// Command-id range handed to the shell.
///
/// Starts at 1 rather than 0 because `TrackPopupMenuEx` with `TPM_RETURNCMD`
/// reports 0 for "dismissed without choosing", which would otherwise be
/// indistinguishable from the first menu item being picked.
const CMD_FIRST: u32 = 1;
const CMD_LAST: u32 = 0x7FFF;

thread_local! {
    /// The menu currently being tracked on this thread, so the window procedure
    /// can forward owner-draw messages to it.
    ///
    /// Thread-local rather than a global: each apartment can be showing its own
    /// menu, and a shared slot would let one thread's messages reach another's
    /// handler. Cleared before the tracking call returns.
    static ACTIVE: RefCell<Option<IContextMenu>> = const { RefCell::new(None) };
}

/// Shows the shell context menu for `paths` at screen position `(x, y)`.
///
/// **STA pool only**, and it blocks until the menu closes — which is the entire
/// design; see the module docs.
///
/// Every path must live in the same directory. That is what the caller has: a
/// selection within one listing.
pub fn show(paths: &[std::path::PathBuf], x: i32, y: i32) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }

    // Absolute PIDLs, kept alive for the whole operation — the child pointers
    // taken below point *into* these, so freeing one early leaves the shell
    // reading freed memory.
    let absolute: Vec<Pidl> = paths
        .iter()
        .filter_map(|p| Pidl::parse(p).ok())
        .collect();
    if absolute.is_empty() {
        return Err("none of the selected items could be resolved".to_owned());
    }

    let (folder, _first_child) = bind_parent(&absolute[0])?;

    // Child (single-level) PIDLs, which is what GetUIObjectOf takes. Valid
    // because every item shares a parent.
    let children: Vec<*const ITEMIDLIST> = absolute.iter().map(|p| p.last_id()).collect();

    // SAFETY: `folder` is live, and every child pointer addresses memory owned
    // by an entry of `absolute`, which outlives this call.
    let menu: IContextMenu = unsafe { folder.GetUIObjectOf(window_placeholder(), &children, None) }
        .map_err(|e| format!("no context menu for this item: {}", e.message()))?;

    let window = HiddenWindow::new()?;
    let popup = Popup::new()?;

    // CMF_EXPLORE asks for the menu Explorer's file pane shows, rather than the
    // shorter desktop-style one.
    //
    // Returns a raw HRESULT rather than a Result: on success its low word is
    // the number of items added, so a plain `?` would reject every populated
    // menu as an error.
    // SAFETY: `popup` is a live empty menu; the id range is ours to hand out.
    let hr = unsafe {
        menu.QueryContextMenu(popup.0, 0, CMD_FIRST, CMD_LAST, CMF_NORMAL | CMF_EXPLORE)
    };
    if hr.is_err() {
        return Err(format!("could not build the menu: {}", hr.message()));
    }

    // SAFETY: `popup` is live.
    if unsafe { GetMenuItemCount(Some(popup.0)) } <= 0 {
        return Err("the shell offered no commands for this item".to_owned());
    }

    let chosen = window.track(&menu, &popup, x, y);

    if let Some(id) = chosen {
        invoke(&menu, id, window.0)?;
    }
    Ok(())
}

/// Runs the chosen command.
fn invoke(menu: &IContextMenu, id: u32, owner: HWND) -> Result<(), String> {
    // The shell identifies the command by its *offset* within the range it was
    // given, not by the raw menu id.
    let offset = id - CMD_FIRST;

    let info = CMINVOKECOMMANDINFOEX {
        cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        hwnd: owner,
        // A verb is passed as an integer stuffed into a string pointer — the
        // MAKEINTRESOURCE convention. It is not a string and must not be
        // dereferenced as one.
        lpVerb: windows::core::PCSTR(offset as usize as *const u8),
        lpVerbW: PCWSTR(offset as usize as *const u16),
        nShow: windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL.0,
        ..Default::default()
    };

    // SAFETY: `info` carries its own size and an owner window that is still
    // alive; the verb fields follow the documented MAKEINTRESOURCE form.
    unsafe { menu.InvokeCommand(&info as *const _ as *const _) }.map_err(|e| {
        tracing::warn!("context menu command failed: {}", e.message());
        e.message()
    })
}

// --- RAII wrappers ---------------------------------------------------------

/// An absolute PIDL from the shell allocator.
struct Pidl(*mut ITEMIDLIST);

impl Pidl {
    fn parse(path: &Path) -> Result<Self, String> {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated and outlives the call; on success
        // the out-param owns a shell allocation freed by this type's Drop.
        unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None) }
            .map_err(|e| e.message())?;

        if pidl.is_null() {
            return Err("empty PIDL".to_owned());
        }
        Ok(Pidl(pidl))
    }

    /// The final (child) item id within this absolute PIDL.
    ///
    /// Borrows into the allocation rather than copying, so the returned pointer
    /// is only valid while `self` lives.
    fn last_id(&self) -> *const ITEMIDLIST {
        // SAFETY: `self.0` is a valid absolute PIDL.
        unsafe { windows::Win32::UI::Shell::ILFindLastID(self.0) }
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        // SAFETY: allocated by the shell via SHParseDisplayName.
        unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
    }
}

/// Binds an absolute PIDL's parent folder.
fn bind_parent(pidl: &Pidl) -> Result<(IShellFolder, *mut ITEMIDLIST), String> {
    let mut child: *mut ITEMIDLIST = std::ptr::null_mut();

    // SAFETY: `pidl` is a valid absolute PIDL. The child pointer written back
    // borrows from it and must not be freed separately.
    let folder: IShellFolder = unsafe { SHBindToParent(pidl.0, Some(&mut child)) }
        .map_err(|e| format!("could not bind the parent folder: {}", e.message()))?;

    Ok((folder, child))
}

/// `GetUIObjectOf` wants an owner window for any UI a handler decides to show
/// while building the menu. There is none yet at that point — the hidden window
/// is created after — and passing null is explicitly allowed.
fn window_placeholder() -> HWND {
    HWND(std::ptr::null_mut())
}

/// An `HMENU` that is destroyed on drop.
struct Popup(HMENU);

impl Popup {
    fn new() -> Result<Self, String> {
        // SAFETY: no arguments; returns a menu this type owns.
        let menu = unsafe { CreatePopupMenu() }.map_err(|e| e.message())?;
        Ok(Popup(menu))
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        // SAFETY: created by CreatePopupMenu and not used afterwards.
        // Destroying the menu also destroys the submenus the shell added.
        unsafe { let _ = DestroyMenu(self.0); };
    }
}

/// A hidden owner window for the menu, on this apartment thread.
struct HiddenWindow(HWND);

impl HiddenWindow {
    fn new() -> Result<Self, String> {
        register_class();

        // SAFETY: the class is registered above; all other parameters are the
        // documented defaults for a message-only tool window.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS_NAME,
                PCWSTR::null(),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(|e| format!("could not create the menu owner window: {}", e.message()))?;

        Ok(HiddenWindow(hwnd))
    }

    /// Shows the menu and returns the chosen command id, or `None` if
    /// dismissed.
    fn track(&self, menu: &IContextMenu, popup: &Popup, x: i32, y: i32) -> Option<u32> {
        // Published so the window procedure can forward owner-draw messages.
        // Set for exactly the duration of the modal loop.
        ACTIVE.with(|a| *a.borrow_mut() = Some(menu.clone()));

        // Without this the menu does not dismiss when the user clicks
        // elsewhere — the documented quirk of tracking a popup from a window
        // that is not in the foreground.
        // SAFETY: `self.0` is a live window owned by this thread.
        unsafe { let _ = SetForegroundWindow(self.0); };

        // SAFETY: live menu and window; TPM_RETURNCMD makes this return the
        // command id rather than posting WM_COMMAND.
        let chosen = unsafe {
            TrackPopupMenuEx(
                popup.0,
                (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN).0,
                x,
                y,
                self.0,
                None,
            )
        };

        ACTIVE.with(|a| *a.borrow_mut() = None);

        // 0 means dismissed. See CMD_FIRST for why no command uses that id.
        (chosen.0 != 0).then_some(chosen.0 as u32)
    }
}

impl Drop for HiddenWindow {
    fn drop(&mut self) {
        // SAFETY: created on this thread and not used afterwards.
        unsafe { let _ = DestroyWindow(self.0); };
    }
}

const CLASS_NAME: PCWSTR = w!("NeutronShellMenuOwner");

/// Registers the owner window class once per process.
fn register_class() {
    use std::sync::Once;
    static ONCE: Once = Once::new();

    ONCE.call_once(|| {
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // SAFETY: `class` is fully initialised and its name outlives the
        // process. A duplicate registration would fail harmlessly, but `Once`
        // means it cannot happen.
        unsafe { RegisterClassW(&class) };
        let _ = SW_HIDE;
    });
}

/// Forwards the messages owner-drawn menu items depend on.
///
/// Without this, third-party entries render as blank strips or with no icon,
/// and cascading submenus from some handlers never populate at all.
///
/// `IContextMenu3` is tried first because it handles `WM_MENUCHAR` (keyboard
/// accelerators within the menu) which `IContextMenu2` does not.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
        let handled = ACTIVE.with(|active| {
            let borrowed = active.borrow();
            let menu = borrowed.as_ref()?;

            if let Ok(cm3) = menu.cast::<IContextMenu3>() {
                let mut result = LRESULT(0);
                // SAFETY: forwarding the message the shell asked to see, with
                // the parameters it was given.
                if unsafe { cm3.HandleMenuMsg2(msg, wparam, lparam, Some(&mut result)) }.is_ok() {
                    return Some(result);
                }
            }
            if let Ok(cm2) = menu.cast::<IContextMenu2>() {
                // SAFETY: as above.
                if unsafe { cm2.HandleMenuMsg(msg, wparam, lparam) }.is_ok() {
                    return Some(LRESULT(0));
                }
            }
            None
        });

        if let Some(result) = handled {
            return result;
        }
    }

    // SAFETY: standard fallback for messages this window does not handle.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Screen position for a menu, from a click.
pub fn at(x: f32, y: f32) -> POINT {
    POINT {
        x: x as i32,
        y: y as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn showing_a_menu_for_nothing_is_a_no_op() {
        // Right-clicking empty space below the rows produces an empty
        // selection; a menu of zero items would be an empty grey box.
        assert!(show(&[], 0, 0).is_ok());
    }

    #[test]
    fn command_ids_never_collide_with_dismissal() {
        // TrackPopupMenuEx reports 0 for "dismissed". A command allocated id 0
        // would be silently swallowed every time it was picked.
        assert!(CMD_FIRST > 0);
        assert!(CMD_LAST > CMD_FIRST);
    }
}
