//! Dragging files out of Neutron, into Explorer or anything else that accepts a
//! drop.
//!
//! # Why the shell's data object and not one of ours
//!
//! `SHCreateDataObject` hands back the same object Explorer would have offered
//! for the same selection. That matters because a drop target asks for whatever
//! format it prefers: `CF_HDROP` for most, `Shell IDList Array` for shell
//! extensions, `FileGroupDescriptor` and `FileContents` for a mail client that
//! wants the bytes rather than a path. Writing our own would mean answering all
//! of them, badly, and getting the virtual-item cases wrong.
//!
//! # Why this blocks a worker and not the UI
//!
//! `DoDragDrop` runs a modal loop until the drop finishes — it does not return
//! while the mouse is down. Called on the UI thread that would stop the window
//! painting for the whole drag, which is the exact failure this project exists
//! to avoid. It is documented as callable from any single-threaded apartment,
//! so it runs on the pool instead and the window keeps drawing behind it.
//!
//! That costs one thing, and it is not obvious. `DoDragDrop` decides whether
//! the button is still down by reading the *calling thread's* input state, and
//! input state is per-input-queue, not per-process. A pool thread has its own
//! queue, sees no button held, and ends the drag immediately — no error, no
//! drop, nothing at all. [`attach`] joins the two queues for the duration,
//! which is the documented way to share input state between threads.

use std::path::{Path, PathBuf};

use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DoDragDrop, IDropSource,
    IDropSource_Impl,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MK_RBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{ILFindLastID, SHCreateDataObject, SHParseDisplayName};
use windows::core::{BOOL, HRESULT, PCWSTR, implement};

/// `DRAGDROP_S_DROP` — the button was released over a target.
const S_DROP: HRESULT = HRESULT(0x00040100u32 as i32);
/// `DRAGDROP_S_CANCEL` — Escape, or a second button pressed.
const S_CANCEL: HRESULT = HRESULT(0x00040101u32 as i32);
/// `DRAGDROP_S_USEDEFAULTCURSORS` — let the system draw the drag cursors.
const S_USEDEFAULTCURSORS: HRESULT = HRESULT(0x00040102u32 as i32);

/// Decides when a drag ends, and lets the system draw it.
///
/// Deliberately the textbook implementation. A drag source that draws its own
/// cursors has to reproduce every state — copy, move, link, no-drop, plus the
/// target's own overrides — and getting one wrong is what makes a drag feel
/// broken rather than merely plain.
#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape_pressed: BOOL, key_state: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape_pressed.as_bool() {
            return S_CANCEL;
        }
        // The right button coming down mid-drag cancels, which is how Windows
        // has always let you back out without moving the mouse away.
        if key_state.0 & MK_RBUTTON.0 != 0 {
            return S_CANCEL;
        }
        if key_state.0 & MK_LBUTTON.0 == 0 {
            return S_DROP;
        }
        windows::Win32::Foundation::S_OK
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        S_USEDEFAULTCURSORS
    }
}

/// What the drop turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    /// Escape, or the right button — the user backed out.
    Cancelled,
    /// A target accepted it. Whether this folder changed is deliberately not
    /// claimed; see [`drag`].
    Completed,
}

/// Drags `paths` out of the application, blocking until the drop completes.
///
/// **STA pool only.** Every path must live in the same directory, which is what
/// a selection within one listing gives.
pub fn drag(paths: &[PathBuf], owner: isize) -> Result<Dropped, String> {
    if paths.is_empty() {
        return Ok(Dropped::Cancelled);
    }

    // Kept alive for the whole call: the child pointers below borrow from these.
    let absolute: Vec<Pidl> = paths.iter().filter_map(|p| Pidl::parse(p).ok()).collect();
    if absolute.is_empty() {
        return Err("none of the selected items could be resolved".to_owned());
    }
    let children: Vec<*const ITEMIDLIST> = absolute.iter().map(|p| p.last_id()).collect();

    let folder = Pidl::parse(
        paths[0]
            .parent()
            .ok_or_else(|| "the selection has no parent folder".to_owned())?,
    )?;

    // SAFETY: `folder` is a live absolute PIDL and every child borrows from an
    // entry of `absolute`, which outlives this call.
    let data: IDataObject = unsafe { SHCreateDataObject(Some(folder.0), Some(&children), None) }
        .map_err(|e| format!("could not prepare the drag: {}", e.message()))?;

    let source: IDropSource = DropSource.into();
    let mut effect = DROPEFFECT(0);

    // Held for the whole modal loop; see the module docs.
    let _input = InputAttachment::to(owner);

    // LINK offered as well as copy and move: dragging with Alt held makes a
    // shortcut, and a target that only accepts links — the Start menu, a
    // toolbar — would otherwise refuse the drop with no explanation.
    // SAFETY: both interfaces are live for the duration of the modal loop.
    let hr = unsafe {
        DoDragDrop(
            &data,
            &source,
            DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK,
            &mut effect,
        )
    };

    if hr == S_CANCEL {
        return Ok(Dropped::Cancelled);
    }
    if hr.is_err() {
        return Err(format!("the drag failed: {}", hr.message()));
    }

    // `effect` is deliberately ignored. It looks like it says whether the files
    // moved, and for a plain target it does — but Explorer performs an
    // *optimized move*: it relocates the files itself and then reports
    // DROPEFFECT_NONE, precisely so the source does not also try to delete
    // originals that are already gone. Trusting the effect meant a file dragged
    // into Explorer vanished from disk while still being listed here.
    //
    // The truth is in the "Performed DropEffect" format on the data object,
    // which is another registered format and another STGMEDIUM to unpack for an
    // answer that only decides whether to re-read one directory. Re-reading it
    // either way is cheaper than the drag that just happened, and it is what
    // the context menu already does for the same reason: no way to know what a
    // command did, so look again.
    let _ = effect;
    Ok(Dropped::Completed)
}

/// Shares the window thread's input state with this one, and gives it back on
/// drop.
///
/// Undone as soon as the drag finishes. While attached the two threads share an
/// input queue, so one blocking on input would block the other — tolerable for
/// the length of a drag, and not something to leave switched on.
struct InputAttachment {
    window_thread: u32,
    attached: bool,
}

impl InputAttachment {
    fn to(owner: isize) -> Self {
        if owner == 0 {
            return InputAttachment { window_thread: 0, attached: false };
        }
        // SAFETY: `owner` is the application's own window; a null process
        // out-param is allowed.
        let window_thread = unsafe { GetWindowThreadProcessId(HWND(owner as *mut _), None) };
        // SAFETY: both ids name live threads in this process.
        let attached = window_thread != 0
            && unsafe { AttachThreadInput(window_thread, GetCurrentThreadId(), true) }.as_bool();

        if !attached {
            tracing::warn!("could not share input state; the drag may not start");
        }
        InputAttachment { window_thread, attached }
    }
}

impl Drop for InputAttachment {
    fn drop(&mut self) {
        if self.attached {
            // SAFETY: balances the attach above.
            unsafe {
                let _ = AttachThreadInput(self.window_thread, GetCurrentThreadId(), false);
            };
        }
    }
}

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
        // SAFETY: `wide` is NUL-terminated and outlives the call; the out-param
        // owns a shell allocation freed by this type's Drop.
        unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None) }
            .map_err(|e| e.message())?;
        if pidl.is_null() {
            return Err("empty PIDL".to_owned());
        }
        Ok(Pidl(pidl))
    }

    /// The final (child) item id, borrowing into this allocation.
    fn last_id(&self) -> *const ITEMIDLIST {
        // SAFETY: `self.0` is a valid absolute PIDL.
        unsafe { ILFindLastID(self.0) }
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        // SAFETY: allocated by the shell via SHParseDisplayName.
        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(self.0 as *const _)) };
    }
}
