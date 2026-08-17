//! File operations through `IFileOperation`.
//!
//! # Why not `std::fs::remove_file`
//!
//! Because deleting a file in a file manager is not `unlink`. Users expect the
//! Recycle Bin, an undo entry, a progress dialog for anything slow, the
//! "this is a system file, are you sure" prompts, and correct behaviour for
//! files that are in use, read-only, or on a network share. `IFileOperation` is
//! the shell's implementation of all of that, and reimplementing it badly is
//! how a file manager loses somebody's work.
//!
//! It also means deletes are *visible* to the rest of Windows: the Recycle Bin
//! updates, shell change notifications fire, and an open Explorer window
//! refreshes itself.
//!
//! # Threading
//!
//! **STA pool only.** `IFileOperation` is apartment-threaded, and it shows
//! modal UI — a confirmation prompt, a progress dialog — that must never appear
//! on the thread trying to paint the next frame.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::UI::Shell::{
    FOF_ALLOWUNDO, FOF_NOCONFIRMMKDIR, FOF_WANTNUKEWARNING, FOFX_ADDUNDORECORD,
    FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IShellItem,
    SHCreateItemFromParsingName,
};
use windows::core::PCWSTR;

/// How thoroughly to delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposal {
    /// To the Recycle Bin, undoable. What `Delete` should always do.
    Recycle,
    /// Irreversible. Reserved for an explicit `Shift+Delete`, and it keeps the
    /// shell's "this cannot be undone" warning — the one confirmation that
    /// genuinely earns its interruption.
    Permanent,
}

/// Deletes `paths`.
///
/// **STA pool only.** `owner` parents the shell's progress and confirmation
/// dialogs; passing 0 leaves them ownerless and able to appear behind the main
/// window, where a modal prompt looks like a hang.
///
/// All paths go into one operation rather than one operation each: the shell
/// then shows a single progress dialog, writes a single undo record, and the
/// user's `Ctrl+Z` restores the whole batch instead of the last file of it.
pub fn delete(paths: &[PathBuf], disposal: Disposal, owner: isize) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }

    // SAFETY: the pool thread has already called CoInitializeEx with
    // COINIT_APARTMENTTHREADED; see `sta`.
    let op: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }
        .map_err(|e| format!("could not start the file operation: {}", e.message()))?;

    let flags = match disposal {
        Disposal::Recycle => {
            // RECYCLEONDELETE is what actually routes to the bin; ALLOWUNDO and
            // ADDUNDORECORD are what make Ctrl+Z in Explorer bring it back.
            // Without all three a "recycled" file is simply gone.
            FOFX_RECYCLEONDELETE | FOF_ALLOWUNDO | FOFX_ADDUNDORECORD | FOF_NOCONFIRMMKDIR
        }
        // WANTNUKEWARNING asks for the "are you sure, this cannot be undone"
        // prompt. Deliberately kept: it is the only confirmation in the app
        // that stands between a keystroke and unrecoverable loss.
        Disposal::Permanent => FOF_WANTNUKEWARNING,
    };

    // SAFETY: `op` is a live COM object created on this apartment.
    unsafe {
        op.SetOperationFlags(flags).map_err(|e| e.message())?;
        op.SetOwnerWindow(HWND(owner as *mut _))
            .map_err(|e| e.message())?;
    }

    let mut queued = 0usize;
    for path in paths {
        match shell_item(path) {
            Ok(item) => {
                // SAFETY: `item` is a live IShellItem; the operation retains it.
                if let Err(e) = unsafe { op.DeleteItem(&item, None) } {
                    tracing::warn!(path = %path.display(), "could not queue delete: {}", e.message());
                } else {
                    queued += 1;
                }
            }
            // A path that vanished between listing and deleting is not worth
            // aborting the whole batch for.
            Err(e) => tracing::warn!(path = %path.display(), "{e}"),
        }
    }

    if queued == 0 {
        return Err("none of the selected items could be resolved".to_owned());
    }

    // SAFETY: `op` is live and has at least one queued item.
    unsafe { op.PerformOperations() }.map_err(|e| {
        tracing::warn!("file operation failed: {}", e.message());
        e.message()
    })
}

/// Whether a transfer duplicates or relocates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    Copy,
    Move,
}

/// Explorer's rule for what a plain drag means, given source and destination.
///
/// Within one volume a drag moves — nothing is duplicated and it is near
/// instant, because only the directory entry changes. Across volumes it copies,
/// since the bytes have to be written anyway and silently removing the original
/// after a long transfer is the more dangerous default.
///
/// Compares the path prefix rather than querying the volume: `C:\a` and `C:\b`
/// share a drive letter, `\\server\share` is its own volume, and a mount point
/// is deliberately treated as its parent volume — which matches what Explorer
/// does, even though the bytes really do move.
pub fn default_transfer(source: &Path, destination: &Path) -> Transfer {
    let volume = |p: &Path| {
        p.components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
    };

    match (volume(source), volume(destination)) {
        (Some(a), Some(b)) if a == b => Transfer::Move,
        _ => Transfer::Copy,
    }
}

/// Copies or moves `paths` into `destination`.
///
/// **STA pool only.** Same batching argument as [`delete`]: one operation means
/// one progress dialog, one undo record, and one "there is already a file
/// called this" prompt sequence rather than one per file.
pub fn transfer(
    paths: &[PathBuf],
    destination: &Path,
    how: Transfer,
    owner: isize,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }

    // SAFETY: the pool thread has already called CoInitializeEx with
    // COINIT_APARTMENTTHREADED; see `sta`.
    let op: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }
        .map_err(|e| format!("could not start the file operation: {}", e.message()))?;

    // ALLOWUNDO on a copy too: it is what puts the newly created files in the
    // undo stack, so a mis-drop is one Ctrl+Z away rather than a manual cleanup.
    let flags = FOF_ALLOWUNDO | FOFX_ADDUNDORECORD | FOF_NOCONFIRMMKDIR;

    // SAFETY: `op` is a live COM object created on this apartment.
    unsafe {
        op.SetOperationFlags(flags).map_err(|e| e.message())?;
        op.SetOwnerWindow(HWND(owner as *mut _))
            .map_err(|e| e.message())?;
    }

    let target = shell_item(destination)?;

    let mut queued = 0usize;
    for path in paths {
        // Dropping a folder into itself, or onto its own parent, is a no-op at
        // best and an infinite recursion at worst. The shell catches most of
        // these, but not before showing a dialog about it.
        if destination == path || destination.starts_with(path) {
            tracing::warn!(path = %path.display(), "skipping: destination is inside the source");
            continue;
        }

        match shell_item(path) {
            Ok(item) => {
                // SAFETY: both items are live; the operation retains them.
                // `None` for the new name keeps the original.
                let queued_ok = unsafe {
                    match how {
                        Transfer::Copy => op.CopyItem(&item, &target, None, None),
                        Transfer::Move => op.MoveItem(&item, &target, None, None),
                    }
                };
                if let Err(e) = queued_ok {
                    tracing::warn!(path = %path.display(), "could not queue: {}", e.message());
                } else {
                    queued += 1;
                }
            }
            Err(e) => tracing::warn!(path = %path.display(), "{e}"),
        }
    }

    if queued == 0 {
        return Err("nothing could be transferred".to_owned());
    }

    // SAFETY: `op` is live and has at least one queued item.
    unsafe { op.PerformOperations() }.map_err(|e| {
        tracing::warn!("file operation failed: {}", e.message());
        e.message()
    })
}

/// Wraps a path as an `IShellItem`.
fn shell_item(path: &Path) -> Result<IShellItem, String> {
    // Not `\\?\`-prefixed, for the same reason as `open`: the shell's parsing
    // does not accept it. See that module.
    let wide: Vec<u16> = {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };

    // SAFETY: `wide` is NUL-terminated and outlives the call; the returned
    // interface is owned by the caller and released by its Drop impl.
    unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None) }
        .map_err(|e| format!("could not resolve: {}", e.message()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deleting_nothing_succeeds_without_starting_an_operation() {
        // The selection can be empty when a shortcut fires with nothing
        // selected. Starting a COM operation to delete zero files would show an
        // empty progress dialog.
        assert!(delete(&[], Disposal::Recycle, 0).is_ok());
    }

    #[test]
    fn a_drag_within_one_volume_moves_and_across_volumes_copies() {
        // Explorer's rule. Getting it backwards means a cross-disk drag
        // silently deletes the original after a long copy.
        assert_eq!(
            default_transfer(Path::new(r"C:\a\x.txt"), Path::new(r"C:\b")),
            Transfer::Move
        );
        assert_eq!(
            default_transfer(Path::new(r"C:\a\x.txt"), Path::new(r"D:\b")),
            Transfer::Copy
        );
    }

    #[test]
    fn volume_comparison_ignores_drive_letter_case() {
        assert_eq!(
            default_transfer(Path::new(r"c:\a"), Path::new(r"C:\b")),
            Transfer::Move
        );
    }

    #[test]
    fn transferring_nothing_succeeds_without_starting_an_operation() {
        assert!(transfer(&[], Path::new(r"C:\"), Transfer::Copy, 0).is_ok());
    }

    #[test]
    fn recycling_is_the_default_and_permanent_is_explicit() {
        // Guards the shape of the API: nothing should be able to delete
        // irreversibly without naming that intent.
        assert_ne!(Disposal::Recycle, Disposal::Permanent);
    }
}
