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
//! # The shell builds the menu; Neutron draws it
//!
//! [`open`] does *not* show a Win32 popup. It asks the shell to populate an
//! `HMENU` exactly as Explorer would, reads that menu out into
//! [`neutron_core::MenuItem`] values, and hands them to a caller-supplied
//! closure which is expected to render them and return the chosen command id.
//!
//! The reason is purely visual: a system-drawn menu is drawn in the *system's*
//! colours, and a stark grey Win32 rectangle landing on top of a translucent
//! glass panel undoes the whole design. Every command still comes from the
//! shell and is still invoked through `IContextMenu`, so third-party entries
//! work as before.
//!
//! What is lost is what only the system renderer can do: per-item icons
//! supplied as `HBITMAP`, and fully owner-drawn items that carry no menu
//! string at all. See [`read_menu`] for how the latter are handled.
//!
//! # Why the closure, rather than returning the items
//!
//! `IContextMenu` is apartment-affine and must stay alive between building the
//! menu and invoking the choice. Returning the items and taking the id in a
//! second call would mean parking a live COM pointer somewhere and hoping the
//! follow-up landed on the same pool thread. Instead the apartment thread
//! blocks inside the closure — the caller sends the items to the UI and waits
//! for a reply — which keeps the interface on its own thread and lets RAII
//! release everything in order.
//!
//! Blocking one apartment thread while a menu is open is exactly what the
//! previous `TrackPopupMenuEx` implementation did, so this costs nothing new.
//!
//! # Why a hidden window
//!
//! Menu handlers expect an owner window: `WM_INITMENUPOPUP` is how a handler
//! is told to populate a submenu lazily, and `InvokeCommand` wants a parent for
//! any dialog the command shows. Subclassing winit's `HWND` would put that on
//! the UI thread, which is the one place none of this may run. So each menu
//! creates its own hidden window on the apartment thread and owns it for the
//! menu's lifetime.

use std::cell::RefCell;
use std::path::Path;

use windows::Win32::UI::WindowsAndMessaging::WM_INITMENUPOPUP;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    CMF_EXPLORE, CMF_NORMAL, CMINVOKECOMMANDINFOEX, GCS_VERBW, IContextMenu, IContextMenu2,
    IContextMenu3, IShellFolder, SHBindToObject, SHBindToParent, SHParseDisplayName,
};

use neutron_core::menu::{MenuItem, parse_label, tidy};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, GetMenuItemCount,
    GetMenuItemInfoW, GetMenuStringW, HMENU, MENUITEMINFOW, MF_BYPOSITION, MFS_CHECKED,
    MFS_DEFAULT, MFS_DISABLED, MFS_GRAYED, MFT_SEPARATOR, MIIM_FTYPE, MIIM_ID, MIIM_STATE,
    MIIM_SUBMENU, RegisterClassW, SW_HIDE, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
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

/// Builds the shell context menu for `paths`, hands it to `choose` as plain
/// data, and invokes whatever `choose` returns.
///
/// **STA pool only.** It blocks for as long as `choose` takes, which is the
/// whole time the menu is on screen; see the module docs.
///
/// `choose` returns the id of the picked [`MenuItem`], or `None` if the user
/// dismissed the menu.
///
/// Every path must live in the same directory. That is what the caller has: a
/// selection within one listing.
pub fn open<F>(paths: &[std::path::PathBuf], choose: F) -> Result<(), String>
where
    F: FnOnce(Vec<MenuItem>) -> Option<u32>,
{
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

    present(menu, CMF_NORMAL | CMF_EXPLORE, choose)
}

/// Builds the context menu for a folder's *background* — the one with New,
/// Paste, Sort by and Properties, which Explorer shows on empty space.
///
/// **STA pool only**, same contract as [`open`].
///
/// A different COM object entirely from the item menu: `GetUIObjectOf` asks a
/// folder about the things *inside* it, while `CreateViewObject` asks the
/// folder about itself. Showing the folder's own item menu here instead — which
/// is what happens if you take the obvious shortcut of selecting nothing and
/// reusing [`open`] — offers Cut, Copy and Delete for the folder you are
/// standing in, which is actively dangerous.
pub fn open_background<F>(folder: &Path, choose: F) -> Result<(), String>
where
    F: FnOnce(Vec<MenuItem>) -> Option<u32>,
{
    let pidl = Pidl::parse(folder)?;

    // SAFETY: `pidl` is a valid absolute PIDL; a null parent folder means "bind
    // from the desktop", which is how an absolute PIDL is resolved.
    let shell_folder: IShellFolder = unsafe { SHBindToObject(None, pidl.0, None) }
        .map_err(|e| format!("could not open this folder: {}", e.message()))?;

    // SAFETY: `shell_folder` is live; a null owner window is allowed, and the
    // real one is created below.
    let menu: IContextMenu = unsafe { shell_folder.CreateViewObject(window_placeholder()) }
        .map_err(|e| format!("no background menu for this folder: {}", e.message()))?;

    // No CMF_EXPLORE: the background menu has no tree-pane variant, and asking
    // for one gets the same menu with an extra separator.
    present(menu, CMF_NORMAL, choose)
}

/// Reads a populated `IContextMenu` into plain data, shows it through `choose`,
/// and invokes the answer.
///
/// Shared by the item and background menus, which differ only in where the
/// interface came from.
fn present<F>(
    menu: IContextMenu,
    flags: u32,
    choose: F,
) -> Result<(), String>
where
    F: FnOnce(Vec<MenuItem>) -> Option<u32>,
{
    let window = HiddenWindow::new()?;
    let popup = Popup::new()?;

    // Returns a raw HRESULT rather than a Result: on success its low word is
    // the number of items added, so a plain `?` would reject every populated
    // menu as an error.
    // SAFETY: `popup` is a live empty menu; the id range is ours to hand out.
    let hr = unsafe { menu.QueryContextMenu(popup.0, 0, CMD_FIRST, CMD_LAST, flags) };
    if hr.is_err() {
        return Err(format!("could not build the menu: {}", hr.message()));
    }

    // Published so `init_popup` can reach the handler while submenus are read.
    ACTIVE.with(|a| *a.borrow_mut() = Some(menu.clone()));
    let items = tidy(read_menu(&menu, popup.0, 0));
    ACTIVE.with(|a| *a.borrow_mut() = None);

    if items.is_empty() {
        return Err("the shell offered no commands here".to_owned());
    }

    if let Some(id) = choose(items) {
        invoke(&menu, id, window.0)?;
    }
    Ok(())
}

/// How deep a submenu chain is followed.
///
/// Bounded because the tree is walked eagerly: each level costs a
/// `WM_INITMENUPOPUP` round trip into a third-party handler, and "Send to" or a
/// cloud provider's nested menus can be slow. Nothing in a real shell menu is
/// anywhere near this deep, so the limit only ever fires on a misbehaving
/// handler that hands back a cycle.
const MAX_DEPTH: u32 = 5;

/// Reads an `HMENU` the shell has populated into plain data.
///
/// # Items without a string
///
/// A handler may add a fully owner-drawn item, which carries no menu text at
/// all — the system would have asked it to paint the row itself. There is
/// nothing to read, so the item's verb is used as the label. That is a
/// programmatic name (`compress`, `pintohome`) rather than a display name, but
/// it is recognisable, and dropping the row would silently remove a command the
/// user has installed.
fn read_menu(menu: &IContextMenu, hmenu: HMENU, depth: u32) -> Vec<MenuItem> {
    // SAFETY: `hmenu` is a live menu.
    let count = unsafe { GetMenuItemCount(Some(hmenu)) };
    if count <= 0 {
        return Vec::new();
    }

    let mut items = Vec::with_capacity(count as usize);
    for index in 0..count as u32 {
        let mut info = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_SUBMENU,
            ..Default::default()
        };
        // SAFETY: `info` carries its own size and requests only fields that
        // need no output buffer.
        if unsafe { GetMenuItemInfoW(hmenu, index, true, &mut info) }.is_err() {
            continue;
        }

        if info.fType.0 & MFT_SEPARATOR.0 != 0 {
            items.push(MenuItem::separator());
            continue;
        }

        let raw = menu_string(hmenu, index)
            .or_else(|| verb_of(menu, info.wID))
            .unwrap_or_default();
        if raw.is_empty() {
            continue;
        }
        let (label, accel) = parse_label(&raw);

        // A submenu parent is not a command: its id is whatever the handler
        // happened to leave there, and invoking it does nothing useful.
        let has_sub = !info.hSubMenu.is_invalid();
        let children = if has_sub && depth < MAX_DEPTH {
            init_popup(info.hSubMenu, index);
            read_menu(menu, info.hSubMenu, depth + 1)
        } else {
            Vec::new()
        };

        items.push(MenuItem {
            id: if has_sub { 0 } else { info.wID },
            label,
            accel,
            enabled: info.fState.0 & (MFS_DISABLED.0 | MFS_GRAYED.0) == 0,
            default: info.fState.0 & MFS_DEFAULT.0 != 0,
            checked: info.fState.0 & MFS_CHECKED.0 != 0,
            separator: false,
            children,
        });
    }
    items
}

/// The menu text for one row, or `None` when the item has none.
fn menu_string(hmenu: HMENU, index: u32) -> Option<String> {
    // SAFETY: a null buffer asks only for the length, which is the documented
    // way to size the real call.
    let len = unsafe { GetMenuStringW(hmenu, index, None, MF_BYPOSITION) };
    if len <= 0 {
        return None;
    }

    // One extra for the NUL the call always writes.
    let mut buf = vec![0u16; len as usize + 1];
    // SAFETY: `buf` is at least as long as the length reported above.
    let written = unsafe { GetMenuStringW(hmenu, index, Some(&mut buf), MF_BYPOSITION) };
    if written <= 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..written as usize]))
}

/// The handler's own name for a command, used when there is no menu text.
fn verb_of(menu: &IContextMenu, id: u32) -> Option<String> {
    if id < CMD_FIRST {
        return None;
    }
    // The shell identifies a command by its offset within the range we handed
    // out, not by the raw menu id.
    let offset = (id - CMD_FIRST) as usize;

    let mut buf = [0u16; 260];
    // GCS_VERBW writes UTF-16 through a byte-typed pointer — the parameter is
    // PSTR for the ANSI form and is reused as-is for the wide one.
    // SAFETY: the buffer is 260 wide characters and `cchmax` says so.
    let ok = unsafe {
        menu.GetCommandString(
            offset,
            GCS_VERBW,
            None,
            windows::core::PSTR(buf.as_mut_ptr() as *mut u8),
            buf.len() as u32,
        )
    }
    .is_ok();
    if !ok {
        return None;
    }

    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let verb = String::from_utf16_lossy(&buf[..end]);
    (!verb.is_empty()).then_some(verb)
}

/// Tells the handler to fill in a submenu, as the system would before drawing
/// it.
///
/// Menus like "Send to", "Open with" and most cloud-provider entries are empty
/// until this arrives — the handler populates them on demand so that a menu
/// that is never opened costs nothing. Without it those submenus read as empty.
fn init_popup(hsub: HMENU, index: u32) {
    let wparam = WPARAM(hsub.0 as usize);
    // Low word is the item's position in its parent; the high word flags a
    // window menu, which this is not.
    let lparam = LPARAM(index as isize);

    ACTIVE.with(|active| {
        let borrowed = active.borrow();
        let Some(menu) = borrowed.as_ref() else { return };

        if let Ok(cm3) = menu.cast::<IContextMenu3>() {
            let mut result = LRESULT(0);
            // SAFETY: forwarding the documented message with the parameters the
            // system would have sent.
            if unsafe { cm3.HandleMenuMsg2(WM_INITMENUPOPUP, wparam, lparam, Some(&mut result)) }
                .is_ok()
            {
                return;
            }
        }
        if let Ok(cm2) = menu.cast::<IContextMenu2>() {
            // SAFETY: as above.
            let _ = unsafe { cm2.HandleMenuMsg(WM_INITMENUPOPUP, wparam, lparam) };
        }
    });
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

/// The owner window's procedure.
///
/// It forwards nothing. Owner-draw messages — `WM_DRAWITEM`, `WM_MEASUREITEM`,
/// `WM_MENUCHAR` — exist so the *system* menu renderer can ask a handler to
/// paint its own rows, and Neutron never asks the system to render a menu.
/// `WM_INITMENUPOPUP` is the one message that still matters, and [`init_popup`]
/// sends it to the handler directly while the menu is being read, rather than
/// waiting for a message that will never arrive here.
///
/// The window exists for the other half of its job: somewhere for
/// `InvokeCommand` to parent the dialogs a command puts up.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: standard handling for a window with no messages of its own.
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
        // selection; a menu of zero items would be an empty box.
        let mut asked = false;
        let result = open(&[], |_| {
            asked = true;
            None
        });
        assert!(result.is_ok());
        assert!(!asked, "no selection should not reach the shell at all");
    }

    #[test]
    fn command_ids_never_collide_with_dismissal() {
        // TrackPopupMenuEx reports 0 for "dismissed". A command allocated id 0
        // would be silently swallowed every time it was picked.
        assert!(CMD_FIRST > 0);
        assert!(CMD_LAST > CMD_FIRST);
    }

    #[test]
    fn submenus_are_not_followed_forever() {
        // A handler that hands back a menu containing itself would otherwise
        // recurse until the stack ran out.
        assert!(MAX_DEPTH > 0 && MAX_DEPTH < 16);
    }
}
