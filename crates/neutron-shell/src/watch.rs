//! Noticing that a folder changed.
//!
//! A listing that only updates when asked is wrong most of the time: a download
//! finishes, a build writes its output, another application saves a file, and
//! the pane goes on showing what was there a minute ago. Explorer updates
//! itself, and a file manager that does not feels broken in a way that is hard
//! to name and easy to notice.
//!
//! # What this reports, and what it does not
//!
//! That *something* changed, and nothing else. `ReadDirectoryChangesW` will
//! describe each change — added, removed, renamed from, renamed to — and it is
//! tempting to apply them to the listing directly. Doing that correctly means
//! reproducing the sort position, the filter, the hidden-file rule and the
//! rename pairing for every event, and being wrong leaves a listing that
//! disagrees with the disk in a way nobody can see. Re-reading the directory is
//! 25 ms for 27,000 entries and is right by construction.
//!
//! # Rate limiting is not optional
//!
//! Extracting an archive or cloning a repository produces thousands of events
//! in a few seconds, and re-reading the directory for each one would be far
//! more work than the extraction. So the first change is reported at once —
//! responsiveness is the entire point — and then the watcher waits
//! [`QUIET_PERIOD`] before looking again. Everything that happens during that
//! window is still queued by the kernel and collapses into the next single
//! read.
//!
//! Reporting first and waiting second, rather than the other way around, is
//! what keeps a single file appearing instantly while a thousand files cost
//! four reports a second.
//!
//! # One thread per watched directory
//!
//! `ReadDirectoryChangesW` blocks, and the number of watched directories is the
//! number of visible panes — one, or a handful. A completion port would be the
//! right answer for hundreds; for four it is machinery with nothing to do.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The shortest gap between two reports.
///
/// Long enough that a burst collapses, short enough that a second change feels
/// like it was noticed rather than polled for.
const QUIET_PERIOD: Duration = Duration::from_millis(250);

/// A directory being watched. Stops when dropped.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    path: PathBuf,
    /// Shared with the worker so the handle outlives whichever drops first,
    /// and so this end can cancel the read the worker is blocked in.
    #[cfg(windows)]
    directory: Arc<windows_impl::Directory>,
}

impl Watcher {
    /// The directory this is watching, so a caller can tell whether it still
    /// wants the one it has.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);

        // Setting the flag is not enough on its own. The worker is blocked
        // inside `ReadDirectoryChangesW`, which returns when the directory
        // changes — so a watcher on a quiet folder would sit there until
        // something happened in it, holding a thread and a handle for a pane
        // the user navigated away from minutes ago. `CancelIoEx` is what
        // actually ends the wait.
        #[cfg(windows)]
        windows_impl::cancel(&self.directory);
    }
}

/// Starts watching `dir`, calling `on_change` from a background thread whenever
/// its contents settle after changing.
///
/// Returns `None` if the directory cannot be watched — a path that has gone
/// away, a filesystem with no notification support. That is not an error worth
/// surfacing: the listing simply stays manual, exactly as it was before.
#[cfg(windows)]
pub fn watch(dir: &Path, on_change: impl Fn() + Send + 'static) -> Option<Watcher> {
    windows_impl::watch(dir, on_change)
}

#[cfg(not(windows))]
pub fn watch(_dir: &Path, _on_change: impl Fn() + Send + 'static) -> Option<Watcher> {
    None
}

#[cfg(windows)]
mod windows_impl {
    use super::*;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::IO::CancelIoEx;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
        FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
        FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
    };
    use windows::core::PCWSTR;

    /// The open directory handle, shared between the watcher and its worker.
    ///
    /// Shared rather than moved so that cancelling from the watcher cannot race
    /// the worker closing it: the handle is valid for as long as either end
    /// still holds a reference.
    pub(super) struct Directory(HANDLE);

    impl Drop for Directory {
        fn drop(&mut self) {
            // SAFETY: opened by `open`, and this runs once when the last
            // reference goes.
            unsafe { let _ = CloseHandle(self.0); };
        }
    }

    // SAFETY: a handle is a kernel object identifier with no thread affinity,
    // and every use of it here is a call that takes it by value.
    unsafe impl Send for Directory {}
    unsafe impl Sync for Directory {}

    fn wide(path: &Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn open(dir: &Path) -> Option<Directory> {
        let path = wide(dir);

        // FILE_SHARE_DELETE alongside read and write: without it, holding this
        // handle would stop anyone renaming or deleting the folder being
        // watched — a file manager that locks the directory you are looking at.
        //
        // BACKUP_SEMANTICS is what makes CreateFileW open a directory at all.
        // SAFETY: `path` is NUL-terminated and outlives the call.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .ok()?;

        Some(Directory(handle))
    }

    /// Ends the read the worker is blocked in, so it can see its stop flag.
    pub(super) fn cancel(directory: &Directory) {
        // SAFETY: the handle is alive — the caller holds a reference to it —
        // and cancelling a handle with no pending I/O is a documented no-op
        // rather than an error worth reporting.
        unsafe { let _ = CancelIoEx(directory.0, None); };
    }

    pub(super) fn watch(dir: &Path, on_change: impl Fn() + Send + 'static) -> Option<Watcher> {
        let directory = Arc::new(open(dir)?);
        let stop = Arc::new(AtomicBool::new(false));

        let worker_stop = Arc::clone(&stop);
        let worker_directory = Arc::clone(&directory);
        std::thread::Builder::new()
            .name("neutron-watch".into())
            .spawn(move || run(&worker_directory, worker_stop, on_change))
            .ok()?;

        Some(Watcher {
            stop,
            path: dir.to_path_buf(),
            directory,
        })
    }

    fn run(directory: &Directory, stop: Arc<AtomicBool>, on_change: impl Fn()) {
        // The buffer the kernel fills with change records. Its contents are
        // never read: this reports *that* something changed. It still has to be
        // large enough that a burst does not overflow it — though an overflow
        // is reported as a zero-length read, which is handled below as
        // "something changed", which is exactly right.
        let mut buffer = vec![0u8; 16 * 1024];

        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }

            let mut returned = 0u32;
            // SAFETY: the handle is open for as long as `directory` is
            // borrowed, and the buffer outlives the call because this blocks
            // until it completes.
            let ok = unsafe {
                ReadDirectoryChangesW(
                    directory.0,
                    buffer.as_mut_ptr() as *mut _,
                    buffer.len() as u32,
                    // Not recursive: the pane shows one directory, and watching
                    // a whole tree turns a source checkout into a firehose of
                    // events about files that are not on screen.
                    false,
                    FILE_NOTIFY_CHANGE_FILE_NAME
                        | FILE_NOTIFY_CHANGE_DIR_NAME
                        | FILE_NOTIFY_CHANGE_ATTRIBUTES
                        | FILE_NOTIFY_CHANGE_SIZE
                        | FILE_NOTIFY_CHANGE_LAST_WRITE,
                    Some(&mut returned),
                    None,
                    None,
                )
            }
            .is_ok();

            // Checked after the read as well as before: the usual way out is
            // `CancelIoEx` from `Drop`, which makes the read fail *because*
            // this flag was just set.
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if !ok {
                // The directory went away, or its volume was removed. Nothing
                // to recover — the pane reports the failure when the user next
                // asks it to do something.
                return;
            }

            on_change();

            // Reported first, waited after. Everything that happens during this
            // window is still queued by the kernel and collapses into the next
            // single read, which is what keeps an unpacking archive from
            // costing one directory re-read per file.
            std::thread::sleep(QUIET_PERIOD);
        }
    }
}
