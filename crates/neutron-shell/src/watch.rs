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
    /// The worker's kqueue, shared for the same reason: so `Drop` can wake the
    /// `kevent` wait without racing the worker closing the descriptor.
    #[cfg(target_os = "macos")]
    queue: Arc<kqueue_impl::Queue>,
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
        // The same problem on macOS: the worker sleeps in `kevent` until the
        // directory changes or the metadata poll is due. A user event in the
        // same queue ends the wait now.
        #[cfg(target_os = "macos")]
        kqueue_impl::wake(&self.queue);
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

#[cfg(not(any(windows, target_os = "macos")))]
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

#[cfg(target_os = "macos")]
pub fn watch(dir: &Path, on_change: impl Fn() + Send + 'static) -> Option<Watcher> {
    kqueue_impl::watch(dir, on_change)
}

/// macOS: a kqueue on the directory itself.
///
/// `EVFILT_VNODE` on a directory fires when an entry is added, removed or
/// renamed — the directory's own contents change — so those are reported as
/// quickly as on Windows. What it cannot see is a file inside being rewritten
/// in place: that changes the file, not the directory. A slow metadata poll
/// ([`METADATA_POLL`]) covers that case, so a growing download or a saved
/// document still updates its size and date, just not instantly.
///
/// FSEvents would see both, but delivers through a run loop or dispatch queue
/// and coalesces with its own latency; for the one or few folders on screen,
/// a kqueue per folder is simpler and faster.
#[cfg(target_os = "macos")]
mod kqueue_impl {
    use super::*;
    use std::os::fd::RawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::time::Instant;

    /// How often in-place changes to the entries are looked for.
    const METADATA_POLL: Duration = Duration::from_secs(5);
    /// The `EVFILT_USER` identifier `Drop` triggers.
    const WAKE: usize = 1;

    /// The kqueue and the watched directory's descriptor, closed together
    /// when the last of the watcher and its worker lets go.
    pub(in super) struct Queue {
        kq: RawFd,
        dir: RawFd,
    }

    impl Drop for Queue {
        fn drop(&mut self) {
            // SAFETY: both were opened by `watch` and are closed only here.
            unsafe {
                libc::close(self.kq);
                libc::close(self.dir);
            }
        }
    }

    fn change(ident: usize, filter: i16, flags: u16, fflags: u32) -> libc::kevent {
        libc::kevent { ident, filter, flags, fflags, data: 0, udata: std::ptr::null_mut() }
    }

    pub(in super) fn wake(queue: &Queue) {
        let trigger = change(WAKE, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER);
        // SAFETY: `kq` is open while `queue` is borrowed; one change, no events.
        unsafe { libc::kevent(queue.kq, &trigger, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    }

    pub(in super) fn watch(dir: &Path, on_change: impl Fn() + Send + 'static) -> Option<Watcher> {
        let c_path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        // O_EVTONLY: opened only to be notified about. Unlike an ordinary
        // descriptor it does not stop the volume being ejected — holding one
        // per visible pane would otherwise make every USB stick "in use".
        // SAFETY: `c_path` is NUL-terminated and outlives the call.
        let dir_fd = unsafe {
            libc::open(c_path.as_ptr(), libc::O_EVTONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)
        };
        if dir_fd < 0 {
            return None;
        }
        // SAFETY: no arguments; the result is checked.
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            // SAFETY: opened above and not shared.
            unsafe { libc::close(dir_fd) };
            return None;
        }
        let queue = Arc::new(Queue { kq, dir: dir_fd });

        let changes = [
            // EV_CLEAR: one event stands for everything since the last read,
            // which is the collapse the quiet period relies on.
            change(dir_fd as usize, libc::EVFILT_VNODE, libc::EV_ADD | libc::EV_CLEAR,
                libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_EXTEND
                    | libc::NOTE_ATTRIB | libc::NOTE_REVOKE),
            change(WAKE, libc::EVFILT_USER, libc::EV_ADD | libc::EV_CLEAR, 0),
        ];
        // SAFETY: `kq` is open and `changes` is a valid array of its length.
        let registered = unsafe {
            libc::kevent(kq, changes.as_ptr(), changes.len() as i32, std::ptr::null_mut(), 0, std::ptr::null())
        };
        if registered < 0 {
            return None;
        }

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_queue = Arc::clone(&queue);
        let worker_path = dir.to_path_buf();
        std::thread::Builder::new()
            .name("neutron-watch".into())
            .spawn(move || run(&worker_queue, &worker_stop, &worker_path, on_change))
            .ok()?;

        Some(Watcher { stop, path: dir.to_path_buf(), queue })
    }

    enum Wake {
        /// The deadline passed with nothing to report.
        Timeout,
        /// The directory changed. `gone` when it was deleted, renamed away
        /// or its volume unmounted — there is nothing more to watch.
        Changed { gone: bool },
        /// The watcher was dropped, or the queue failed.
        Stop,
    }

    /// Waits until `deadline` for something to happen.
    fn wait(queue: &Queue, deadline: Instant) -> Wake {
        let mut events = [change(0, 0, 0, 0); 4];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let timeout = libc::timespec {
                tv_sec: left.as_secs() as libc::time_t,
                tv_nsec: left.subsec_nanos() as libc::c_long,
            };
            // SAFETY: `kq` is open while `queue` is borrowed, and `events` is
            // writable for the length passed.
            let n = unsafe {
                libc::kevent(queue.kq, std::ptr::null(), 0, events.as_mut_ptr(), events.len() as i32, &timeout)
            };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Wake::Stop;
            }
            if n == 0 {
                return Wake::Timeout;
            }
            let mut result = Wake::Timeout;
            for event in &events[..n as usize] {
                if event.filter == libc::EVFILT_USER || event.flags & libc::EV_ERROR != 0 {
                    return Wake::Stop;
                }
                let gone = event.fflags & (libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE) != 0;
                result = match result {
                    Wake::Changed { gone: was } => Wake::Changed { gone: was || gone },
                    _ => Wake::Changed { gone },
                };
            }
            return result;
        }
    }

    fn run(queue: &Queue, stop: &AtomicBool, path: &Path, on_change: impl Fn()) {
        let mut baseline = snapshot(path);
        let mut next_poll = Instant::now() + METADATA_POLL;
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let mut gone = match wait(queue, next_poll) {
                Wake::Stop => return,
                Wake::Changed { gone } => gone,
                Wake::Timeout => {
                    next_poll = Instant::now() + METADATA_POLL;
                    let next = snapshot(path);
                    if next == baseline {
                        continue;
                    }
                    // A directory that can no longer be read (unmounted
                    // without a revoke, permissions withdrawn) is reported
                    // once and then left alone, rather than re-reported on
                    // every poll.
                    next.is_none()
                }
            };
            // Reported first, waited after, exactly as on Windows: the first
            // change is instant, and a burst collapses into one report per
            // quiet period because EV_CLEAR folds the queued events into one.
            loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                // Taken before the report, so the app's re-read can only be
                // newer than the baseline; at worst the poll reports once more.
                baseline = snapshot(path);
                on_change();
                if gone || baseline.is_none() {
                    return;
                }
                match settle(queue) {
                    None => return,
                    Some(None) => break,
                    Some(Some(again)) => gone = again,
                }
            }
            next_poll = Instant::now() + METADATA_POLL;
        }
    }

    /// Waits out the quiet period. `None` to stop, `Some(None)` if nothing
    /// happened meanwhile, and `Some(Some(gone))` if something did.
    fn settle(queue: &Queue) -> Option<Option<bool>> {
        let deadline = Instant::now() + QUIET_PERIOD;
        let mut changed: Option<bool> = None;
        loop {
            match wait(queue, deadline) {
                Wake::Stop => return None,
                Wake::Timeout => return Some(changed),
                Wake::Changed { gone } => changed = Some(changed.unwrap_or(false) || gone),
            }
        }
    }

    /// A fingerprint of every entry's name, size, date and writability. Only
    /// the slow poll needs it; adds, removes and renames come from the queue.
    fn snapshot(dir: &Path) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            if let Ok(meta) = entry.path().symlink_metadata() {
                entries.push((entry.file_name(), meta.len(), meta.modified().ok(), meta.permissions().readonly()));
            }
        }
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        entries.hash(&mut hash);
        Some(hash.finish())
    }
}
