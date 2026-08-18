//! Cut, copy and paste of files, through the Windows clipboard.
//!
//! # Why the clipboard and not an internal buffer
//!
//! Because a file manager that can only paste into itself is a toy. Copying in
//! Neutron and pasting into Explorer, an Open dialog, an email, or a terminal —
//! and the reverse — is the whole point, and it costs nothing extra: the format
//! Explorer uses is `CF_HDROP`, which is public and unremarkable.
//!
//! # Why raw clipboard calls rather than `OleSetClipboard`
//!
//! `OleSetClipboard` takes an `IDataObject`, which is the right answer when the
//! data is expensive and should be rendered only if someone asks for it. A list
//! of paths is neither: it is a few hundred bytes, already in hand. Handing the
//! shell a live COM object would also tie the clipboard's contents to this
//! process staying alive, so closing Neutron would empty it — which is not what
//! copying a file means.
//!
//! # Threading
//!
//! **STA pool only.** `OpenClipboard` fails while another process holds the
//! clipboard, and the retry loop below can take a moment.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HANDLE, HGLOBAL, POINT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows::Win32::System::Memory::{
    GHND, GlobalAlloc, GlobalLock, GlobalUnlock,
};
use windows::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};
use windows::core::w;

use crate::fileops::Transfer;

/// `CF_HDROP`, the format Explorer both writes and reads for a file selection.
const CF_HDROP: u32 = 15;

/// Whether the source intended a copy or a move.
///
/// `CF_HDROP` says only *which* files; this says what to do with them. Explorer
/// writes it too, so a cut in Explorer arrives here as a move and vice versa.
/// Without it every paste would be a copy and `Ctrl+X` would silently duplicate.
fn preferred_drop_effect() -> u32 {
    // SAFETY: a static NUL-terminated name; registering a format twice returns
    // the same id.
    unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) }
}

const DROPEFFECT_COPY: u32 = 1;
const DROPEFFECT_MOVE: u32 = 2;

/// Puts `paths` on the clipboard.
///
/// **STA pool only.**
pub fn write(paths: &[PathBuf], how: Transfer) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }

    let drop = build_hdrop(paths);
    let effect = match how {
        Transfer::Copy => DROPEFFECT_COPY,
        Transfer::Move => DROPEFFECT_MOVE,
    };

    let _guard = Clipboard::open()?;

    // SAFETY: the clipboard is open and owned by this thread.
    unsafe { EmptyClipboard() }.map_err(|e| e.message())?;

    // Allocated after EmptyClipboard, because emptying frees everything the
    // clipboard currently holds — including, if the order were reversed, the
    // block just handed to it.
    put(CF_HDROP, &drop)?;
    put(preferred_drop_effect(), &effect.to_ne_bytes())?;
    Ok(())
}

/// Reads a file selection off the clipboard, if there is one.
///
/// **STA pool only.** Returns `None` when the clipboard holds something that is
/// not a file list — text, an image, nothing at all.
pub fn read() -> Option<(Vec<PathBuf>, Transfer)> {
    let _guard = Clipboard::open().ok()?;

    // SAFETY: the clipboard is open. The handle belongs to the clipboard and
    // must not be freed; it stays valid until the clipboard is closed.
    let handle = unsafe { GetClipboardData(CF_HDROP) }.ok()?;
    let hdrop = HDROP(handle.0);

    // 0xFFFF_FFFF asks for the count rather than a path.
    // SAFETY: `hdrop` came from the clipboard in CF_HDROP format.
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };

    let mut paths = Vec::with_capacity(count as usize);
    for i in 0..count {
        // SAFETY: `i` is below the count just reported.
        let len = unsafe { DragQueryFileW(hdrop, i, None) };
        if len == 0 {
            continue;
        }
        // One extra for the NUL the call writes.
        let mut buf = vec![0u16; len as usize + 1];
        // SAFETY: the buffer is long enough for the length reported above.
        let written = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) };
        if written > 0 {
            paths.push(PathBuf::from(String::from_utf16_lossy(
                &buf[..written as usize],
            )));
        }
    }

    if paths.is_empty() {
        return None;
    }
    Some((paths, effect_on_clipboard()))
}

/// What the source asked for, defaulting to a copy.
///
/// A copy is the safe default when the format is absent: pasting a copy of
/// something that was meant to move leaves a duplicate, while the reverse
/// deletes the original.
fn effect_on_clipboard() -> Transfer {
    // SAFETY: the clipboard is open — this is only called from `read`.
    let Ok(handle) = (unsafe { GetClipboardData(preferred_drop_effect()) }) else {
        return Transfer::Copy;
    };

    let global = HGLOBAL(handle.0);
    // SAFETY: a clipboard handle for a registered format is a global block.
    let ptr = unsafe { GlobalLock(global) };
    if ptr.is_null() {
        return Transfer::Copy;
    }
    // SAFETY: locked above; the format is a single DWORD.
    let value = unsafe { std::ptr::read_unaligned(ptr as *const u32) };
    // SAFETY: balances the lock.
    let _ = unsafe { GlobalUnlock(global) };

    // Tested as a flag, not compared: Explorer sets DROPEFFECT_MOVE alongside
    // DROPEFFECT_COPY to say "either is acceptable, but I would prefer a move".
    if value & DROPEFFECT_MOVE != 0 {
        Transfer::Move
    } else {
        Transfer::Copy
    }
}

/// Lays out the `DROPFILES` block: the header, then every path as wide
/// characters, each NUL-terminated, with a second NUL closing the list.
fn build_hdrop(paths: &[PathBuf]) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;

    let mut names: Vec<u16> = Vec::new();
    for path in paths {
        names.extend(path.as_os_str().encode_wide());
        names.push(0);
    }
    // The list terminator, which is what tells the reader where it ends.
    names.push(0);

    let header = DROPFILES {
        pFiles: std::mem::size_of::<DROPFILES>() as u32,
        pt: POINT { x: 0, y: 0 },
        fNC: false.into(),
        // The names above are UTF-16. Saying otherwise makes every reader
        // interpret them as ANSI and see a one-character path.
        fWide: true.into(),
    };

    let mut bytes = Vec::with_capacity(std::mem::size_of::<DROPFILES>() + names.len() * 2);
    // SAFETY: DROPFILES is a plain repr(C) struct with no padding to leak.
    bytes.extend_from_slice(unsafe {
        std::slice::from_raw_parts(
            &header as *const DROPFILES as *const u8,
            std::mem::size_of::<DROPFILES>(),
        )
    });
    for unit in names {
        bytes.extend_from_slice(&unit.to_ne_bytes());
    }
    bytes
}

/// Copies `data` into a global block and hands it to the clipboard.
///
/// On success the clipboard owns the block; freeing it here would leave the
/// clipboard pointing at released memory.
fn put(format: u32, data: &[u8]) -> Result<(), String> {
    // GHND is GMEM_MOVEABLE | GMEM_ZEROINIT. Moveable is required: the
    // clipboard takes ownership of the handle and may relocate the block.
    // SAFETY: a plain allocation of a known size.
    let global = unsafe { GlobalAlloc(GHND, data.len()) }.map_err(|e| e.message())?;

    // SAFETY: freshly allocated and not yet locked.
    let ptr = unsafe { GlobalLock(global) };
    if ptr.is_null() {
        return Err("could not lock clipboard memory".to_owned());
    }
    // SAFETY: the block is at least `data.len()` bytes, which is what is written.
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len()) };
    // SAFETY: balances the lock above.
    let _ = unsafe { GlobalUnlock(global) };

    // SAFETY: the clipboard is open and the handle is a valid global block.
    unsafe { SetClipboardData(format, Some(HANDLE(global.0))) }
        .map(|_| ())
        .map_err(|e| e.message())
}

/// The open clipboard, closed on drop.
///
/// Every early return between opening and closing would otherwise leave the
/// clipboard locked against every other process on the machine — a failure that
/// looks like the whole desktop breaking rather than like a bug in Neutron.
struct Clipboard;

impl Clipboard {
    fn open() -> Result<Self, String> {
        // Only one process may hold the clipboard at a time, and Explorer,
        // password managers and clipboard-history tools all grab it briefly.
        // A single attempt fails often enough to be noticed as "sometimes
        // Ctrl+C does nothing".
        for attempt in 0..10 {
            // SAFETY: a null owner is allowed and leaves the clipboard
            // unowned, which is what we want — the data outlives this process.
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Clipboard);
            }
            std::thread::sleep(std::time::Duration::from_millis(10 * (attempt + 1)));
        }
        Err("the clipboard is in use by another program".to_owned())
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // SAFETY: opened by `open`, and this runs exactly once per open.
        unsafe { let _ = CloseClipboard(); };
    }
}

/// Whether `paths` can be pasted into `destination`.
///
/// Pasting a folder into itself or into its own subtree is the one case the
/// shell handles by putting up a dialog rather than by declining, so it is
/// worth catching first.
pub fn can_paste_into(paths: &[PathBuf], destination: &Path) -> bool {
    !paths.is_empty()
        && paths
            .iter()
            .any(|p| destination != p && !destination.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_points_past_itself_to_the_names() {
        let bytes = build_hdrop(&[PathBuf::from(r"C:\a.txt")]);
        let offset = u32::from_ne_bytes(bytes[0..4].try_into().unwrap()) as usize;
        assert_eq!(offset, std::mem::size_of::<DROPFILES>());
        assert!(bytes.len() > offset);
    }

    #[test]
    fn the_list_ends_with_two_nuls() {
        // One closes the last path, the second closes the list. A reader that
        // does not find the second walks off the end of the block.
        let bytes = build_hdrop(&[PathBuf::from(r"C:\a.txt")]);
        let tail = &bytes[bytes.len() - 4..];
        assert_eq!(tail, [0, 0, 0, 0]);
    }

    #[test]
    fn several_paths_are_packed_one_after_another() {
        let one = build_hdrop(&[PathBuf::from(r"C:\a")]);
        let two = build_hdrop(&[PathBuf::from(r"C:\a"), PathBuf::from(r"C:\b")]);
        // The second path plus its NUL: four characters, two bytes each.
        assert_eq!(two.len(), one.len() + 5 * 2);
    }

    #[test]
    fn pasting_a_folder_into_itself_is_refused() {
        let src = PathBuf::from(r"C:\work");
        assert!(!can_paste_into(&[src.clone()], Path::new(r"C:\work")));
    }

    #[test]
    fn pasting_a_folder_into_its_own_subtree_is_refused() {
        // The shell answers this with a dialog rather than a refusal, which is
        // a worse experience than simply not offering it.
        let src = PathBuf::from(r"C:\work");
        assert!(!can_paste_into(&[src], Path::new(r"C:\work\nested\deeper")));
    }

    #[test]
    fn pasting_elsewhere_is_allowed() {
        let src = PathBuf::from(r"C:\work");
        assert!(can_paste_into(&[src], Path::new(r"D:\backup")));
    }

    #[test]
    fn a_mixed_selection_is_allowed_if_anything_can_move() {
        // One item being the destination itself must not veto the rest.
        let paths = vec![PathBuf::from(r"C:\work"), PathBuf::from(r"C:\notes.txt")];
        assert!(can_paste_into(&paths, Path::new(r"C:\work")));
    }

    #[test]
    fn an_empty_clipboard_pastes_nowhere() {
        assert!(!can_paste_into(&[], Path::new(r"C:\anywhere")));
    }
}
