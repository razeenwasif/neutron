//! Copying and moving files: with progress, a Stop that works in the middle
//! of a large file, and a choice about names that are already taken.
//!
//! # Why not one call to the system
//!
//! A single "copy these" call is all or nothing from the caller's side: no
//! count of what is done, no way to stop it, and whatever it does about a name
//! clash is decided for you. This plans the batch first — so a clash can be
//! put to the user before anything changes — and then works through it an item
//! at a time, reporting as it goes.
//!
//! On the Mac each item is copied by `copyfile(3)`, which is what the Finder
//! uses underneath: on APFS it clones rather than copies, it keeps extended
//! attributes, tags, ACLs and the quarantine flag, and its status call-back is
//! where both the progress and the Stop come from.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::fileops::Transfer;

/// What to do with an item whose name is taken in the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clash {
    /// Bring it in under Finder's "name copy" name.
    KeepBoth,
    /// Leave what is there, and do not bring this one in.
    Skip,
    /// Move what is there to the Trash, then bring this one in.
    Replace,
}

/// How far a transfer has got, shared between the worker and the UI.
#[derive(Default)]
pub struct Progress {
    bytes: AtomicU64,
    bytes_done: AtomicU64,
    items: AtomicUsize,
    items_done: AtomicUsize,
    stop: AtomicBool,
    current: Mutex<String>,
}

/// A copy of [`Progress`] at one moment.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// Bytes that have to be copied; moves within a volume count none.
    pub bytes: u64,
    pub bytes_done: u64,
    pub items: usize,
    pub items_done: usize,
    /// The item being worked on.
    pub current: String,
}

impl Progress {
    /// Asks the transfer to stop. A file being copied stops part-way, and
    /// what was made of it is removed.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            bytes: self.bytes.load(Ordering::Relaxed),
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            items: self.items.load(Ordering::Relaxed),
            items_done: self.items_done.load(Ordering::Relaxed),
            current: self.current.lock().map(|c| c.clone()).unwrap_or_default(),
        }
    }

    fn set_current(&self, name: &str) {
        if let Ok(mut current) = self.current.lock() {
            name.clone_into(&mut current);
        }
    }
}

/// What a transfer did.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub done: usize,
    /// Left alone: a clash the user chose to skip, or a move to where the item
    /// already is.
    pub skipped: usize,
    /// One line per item that could not be transferred.
    pub failed: Vec<String>,
    /// Stopped by the user before the end.
    pub stopped: bool,
    /// Each item transferred: where it was, and where it is now. What undoing
    /// the transfer needs.
    pub moved: Vec<(PathBuf, PathBuf)>,
}

/// One item of a planned batch.
#[derive(Debug, PartialEq)]
struct Job {
    source: PathBuf,
    target: PathBuf,
    /// Something is at `target`, and goes to the Trash first.
    replace: bool,
}

#[derive(Debug, Default)]
struct Plan {
    jobs: Vec<Job>,
    skipped: usize,
    failed: Vec<String>,
    /// Names taken in the destination, whatever was chosen about them.
    clashes: Vec<String>,
}

/// The names in `destination` that transferring `paths` there would land on.
///
/// Empty when there is nothing to ask. A copy into the folder the items are
/// already in is not a clash: that is a duplicate, and is named as one.
pub fn clashes(paths: &[PathBuf], destination: &Path, how: Transfer) -> Result<Vec<String>, String> {
    plan(paths, destination, how, None).map(|p| p.clashes)
}

/// Transfers `paths` into `destination`, settling clashes by `clash`.
///
/// Blocks until done or stopped; run it on a worker and watch `progress`.
/// Errs only when nothing could be attempted at all — a folder into itself, a
/// destination that is not there. Failures of single items are in the report.
pub fn run(
    paths: &[PathBuf],
    destination: &Path,
    how: Transfer,
    clash: Clash,
    progress: &Progress,
) -> Result<Report, String> {
    let plan = plan(paths, destination, how, Some(clash))?;
    let mut report = Report { skipped: plan.skipped, failed: plan.failed, ..Report::default() };

    // Sized first, so the bar measures bytes. Only what is copied counts: a
    // move within a volume is a rename, and takes no time whatever its size.
    let here = device(destination);
    let sized: Vec<(Job, bool, u64)> = plan
        .jobs
        .into_iter()
        .map(|job| {
            let copies = how == Transfer::Copy || device(&job.source) != here || here.is_none();
            let size = if copies { size_of(&job.source) } else { 0 };
            (job, copies, size)
        })
        .collect();
    progress.bytes.store(sized.iter().map(|(_, _, s)| s).sum(), Ordering::Relaxed);
    progress.items.store(sized.len(), Ordering::Relaxed);

    let mut base = 0u64;
    for (job, copies, size) in sized {
        if progress.stopped() {
            report.stopped = true;
            break;
        }
        let name = display_name(&job.source);
        progress.set_current(&name);

        if job.replace {
            if let Err(e) = crate::fileops::delete(std::slice::from_ref(&job.target), crate::fileops::Disposal::Recycle, 0) {
                report.failed.push(format!("{name}: what was there could not be moved to the Trash ({e})"));
                finish(progress, &mut base, size);
                continue;
            }
        }

        let outcome = if copies {
            copy_then(&job, how, progress, base)
        } else {
            match rename_exclusive(&job.source, &job.target) {
                // Across a mount point after all — a folder that is a volume
                // of its own. Copied, then the original removed.
                Err(e) if e.raw_os_error() == Some(EXDEV) => copy_then(&job, how, progress, base),
                other => other.map_err(|e| Failure::Error(e.to_string())),
            }
        };
        match outcome {
            Ok(()) => {
                report.done += 1;
                report.moved.push((job.source.clone(), job.target.clone()));
            }
            Err(Failure::Stopped) => {
                report.stopped = true;
                break;
            }
            Err(Failure::Error(why)) => report.failed.push(format!("{name}: {why}")),
        }
        finish(progress, &mut base, size);
    }
    Ok(report)
}

const EXDEV: i32 = 18;

/// Why [`relocate`] could not move something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelocateError {
    /// The system's privacy protection refused it — as it refuses moving
    /// anything out of the Trash for an app without Full Disk Access.
    NotPermitted(String),
    Other(String),
}

impl std::fmt::Display for RelocateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelocateError::NotPermitted(why) | RelocateError::Other(why) => f.write_str(why),
        }
    }
}

/// Moves one item from `from` to exactly `to` — a name of its own choosing,
/// unlike [`run`] — never onto something already there. Across volumes it is
/// copied, then the original removed. For undoing: putting a file back where
/// it was, or back out of the Trash.
pub fn relocate(from: &Path, to: &Path) -> Result<(), RelocateError> {
    if to.symlink_metadata().is_ok() {
        return Err(RelocateError::Other(format!("something called {} is already there", display_name(to))));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| RelocateError::Other(e.to_string()))?;
    }
    match rename_exclusive(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(EXDEV) => {
            let job = Job { source: from.to_path_buf(), target: to.to_path_buf(), replace: false };
            copy_then(&job, Transfer::Move, &Progress::default(), 0).map_err(|failure| match failure {
                Failure::Stopped => RelocateError::Other("stopped".to_owned()),
                Failure::Error(why) => RelocateError::Other(why),
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(RelocateError::NotPermitted(e.to_string()))
        }
        Err(e) => Err(RelocateError::Other(e.to_string())),
    }
}

/// Marks one item done, and the bar at exactly where it should be after it —
/// whatever the call-backs did or did not report along the way.
fn finish(progress: &Progress, base: &mut u64, size: u64) {
    *base += size;
    progress.bytes_done.store(*base, Ordering::Relaxed);
    progress.items_done.fetch_add(1, Ordering::Relaxed);
}

enum Failure {
    Stopped,
    Error(String),
}

/// Copies the job's item, and for a move removes the original once the copy
/// is whole.
fn copy_then(job: &Job, how: Transfer, progress: &Progress, base: u64) -> Result<(), Failure> {
    let was_there = job.target.symlink_metadata().is_ok();
    match copy(&job.source, &job.target, progress, base) {
        Ok(()) => {
            if how == Transfer::Move {
                remove(&job.source).map_err(|e| {
                    Failure::Error(format!("copied, but the original could not be removed ({e})"))
                })?;
            }
            Ok(())
        }
        Err(failure) => {
            // Half a copy is worse than none: it looks like the file. Only
            // ever what this copy made — the target is created exclusively,
            // so if something was there first, it was not touched.
            if !was_there {
                let _ = remove(&job.target);
            }
            Err(failure)
        }
    }
}

fn plan(paths: &[PathBuf], destination: &Path, how: Transfer, clash: Option<Clash>) -> Result<Plan, String> {
    let into = destination
        .canonicalize()
        .map_err(|e| format!("{} is not available ({e})", display_name(destination)))?;
    let mut plan = Plan::default();
    let mut seen: HashSet<String> = HashSet::new();

    for source in paths {
        let real = source.canonicalize().unwrap_or_else(|_| source.clone());
        if into == real || into.starts_with(&real) {
            return Err(format!(
                "{} cannot be {} into itself",
                display_name(source),
                if how == Transfer::Move { "moved" } else { "copied" }
            ));
        }
        let Some(name) = source.file_name() else {
            plan.failed.push(format!("{}: not something that can be transferred", source.display()));
            continue;
        };
        let beside_itself = source.parent().and_then(|p| p.canonicalize().ok()).as_deref() == Some(into.as_path());
        if how == Transfer::Move && beside_itself {
            // Already where it is being moved to.
            plan.skipped += 1;
            continue;
        }

        let mut target = into.join(name);
        let mut replace = false;
        if taken(&target, &seen) {
            if beside_itself {
                // Pasted into its own folder: a duplicate, never a question.
                target = copy_name(source, &into, &seen);
            } else {
                plan.clashes.push(name.to_string_lossy().into_owned());
                match clash {
                    None => {}
                    Some(Clash::KeepBoth) => target = copy_name(source, &into, &seen),
                    Some(Clash::Skip) => {
                        plan.skipped += 1;
                        continue;
                    }
                    Some(Clash::Replace) => {
                        // Replacing a folder with something inside it would
                        // throw away the very thing being brought in.
                        let existing = target.canonicalize().unwrap_or_else(|_| target.clone());
                        if real.starts_with(&existing) {
                            plan.failed.push(format!(
                                "{}: cannot replace the folder it is inside",
                                display_name(source)
                            ));
                            continue;
                        }
                        replace = true;
                    }
                }
            }
        }
        seen.insert(fold(&target));
        plan.jobs.push(Job { source: source.clone(), target, replace });
    }
    Ok(plan)
}

/// Whether `target` is taken on disk or by an earlier item of the batch.
fn taken(target: &Path, seen: &HashSet<String>) -> bool {
    target.symlink_metadata().is_ok() || seen.contains(&fold(target))
}

/// A name as the default Mac volume compares it: without regard to case.
fn fold(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Finder's name for a copy: "a.txt", then "a copy.txt", "a copy 2.txt".
/// Copying "a copy.txt" continues the count rather than making "a copy copy".
fn copy_name(source: &Path, folder: &Path, seen: &HashSet<String>) -> PathBuf {
    let name = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let is_dir = source.symlink_metadata().is_ok_and(|m| m.is_dir());
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 && !is_dir => (&name[..dot], &name[dot..]),
        _ => (name.as_str(), ""),
    };
    let stem = strip_copy(stem);
    for n in 1..=10_000 {
        let candidate = if n == 1 { format!("{stem} copy{ext}") } else { format!("{stem} copy {n}{ext}") };
        let path = folder.join(&candidate);
        if !taken(&path, seen) {
            return path;
        }
    }
    // Ten thousand copies of one name: give up on the pattern, not the copy.
    folder.join(format!("{stem} copy {}{ext}", std::process::id()))
}

fn strip_copy(stem: &str) -> &str {
    if let Some(base) = stem.strip_suffix(" copy") {
        return base;
    }
    if let Some(at) = stem.rfind(" copy ") {
        let count = &stem[at + " copy ".len()..];
        if !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()) {
            return &stem[..at];
        }
    }
    stem
}

fn display_name(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

/// Bytes in a file or a whole tree, not following links.
fn size_of(path: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![path.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(meta) = next.symlink_metadata() else { continue };
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&next) {
                pending.extend(entries.flatten().map(|e| e.path()));
            }
        } else {
            total += meta.len();
        }
    }
    total
}

#[cfg(unix)]
fn device(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    path.symlink_metadata().ok().map(|m| m.dev())
}

#[cfg(not(unix))]
fn device(_path: &Path) -> Option<u64> {
    None
}

fn remove(path: &Path) -> std::io::Result<()> {
    if path.symlink_metadata()?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// A rename that refuses to replace anything at `to`. The plain one would,
/// silently, if a file appeared there after the batch was planned.
#[cfg(target_os = "macos")]
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both are valid NUL-terminated paths for the duration of the call.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.symlink_metadata().is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "the name is taken"));
    }
    std::fs::rename(from, to)
}

// --- copying ----------------------------------------------------------------

#[cfg(target_os = "macos")]
mod mac {
    use super::{Failure, Progress};
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::atomic::Ordering;

    /// What the call-back needs: where to report, and what it has counted.
    struct Context<'a> {
        progress: &'a Progress,
        /// Bytes done before the file now being copied.
        base: u64,
        /// Size of the file now being copied.
        file: u64,
        /// The first failure, named, for the report.
        error: Option<String>,
    }

    extern "C" fn status(
        what: c_int,
        stage: c_int,
        state: libc::copyfile_state_t,
        source: *const c_char,
        _target: *const c_char,
        context: *mut c_void,
    ) -> c_int {
        // SAFETY: `context` is the `Context` set on this state by `copy`,
        // which outlives the `copyfile` call that invokes this.
        let ctx = unsafe { &mut *(context as *mut Context) };
        if ctx.progress.stopped() {
            return libc::COPYFILE_QUIT;
        }
        let name = || {
            if source.is_null() {
                String::new()
            } else {
                // SAFETY: copyfile passes NUL-terminated paths.
                let path = unsafe { CStr::from_ptr(source) };
                Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()))
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            }
        };
        match (what, stage) {
            (libc::COPYFILE_RECURSE_FILE, libc::COPYFILE_START) => {
                ctx.file = if source.is_null() {
                    0
                } else {
                    // SAFETY: as above.
                    let path = unsafe { CStr::from_ptr(source) };
                    Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()))
                        .symlink_metadata()
                        .map(|m| m.len())
                        .unwrap_or(0)
                };
            }
            (libc::COPYFILE_RECURSE_FILE, libc::COPYFILE_FINISH) => {
                ctx.base += ctx.file;
                ctx.file = 0;
                ctx.progress.bytes_done.store(ctx.base, Ordering::Relaxed);
            }
            (libc::COPYFILE_COPY_DATA, libc::COPYFILE_PROGRESS) => {
                let mut copied: libc::off_t = 0;
                // SAFETY: COPIED writes one off_t through the pointer.
                unsafe {
                    libc::copyfile_state_get(
                        state,
                        libc::COPYFILE_STATE_COPIED as u32,
                        &mut copied as *mut libc::off_t as *mut c_void,
                    );
                }
                ctx.progress.bytes_done.store(ctx.base + copied.max(0) as u64, Ordering::Relaxed);
            }
            (_, libc::COPYFILE_ERR) => {
                if ctx.error.is_none() {
                    let why = std::io::Error::last_os_error();
                    let name = name();
                    ctx.error = Some(if name.is_empty() { why.to_string() } else { format!("{name}: {why}") });
                }
                return libc::COPYFILE_QUIT;
            }
            _ => {}
        }
        libc::COPYFILE_CONTINUE
    }

    pub(super) fn copy(source: &Path, target: &Path, progress: &Progress, base: u64) -> Result<(), Failure> {
        let unpassable = || Failure::Error("its name cannot be passed to macOS".into());
        let from = CString::new(source.as_os_str().as_bytes()).map_err(|_| unpassable())?;
        let to = CString::new(target.as_os_str().as_bytes()).map_err(|_| unpassable())?;
        // A single file is not walked, so the call-back never hears it start;
        // its size is known here instead.
        let file = source.symlink_metadata().map(|m| if m.is_dir() { 0 } else { m.len() }).unwrap_or(0);
        let mut ctx = Context { progress, base, file, error: None };

        // SAFETY: the state is allocated, configured, used and freed here; the
        // call-back and the context pointer it carries are valid throughout.
        let (rc, errno) = unsafe {
            let state = libc::copyfile_state_alloc();
            if state.is_null() {
                return Err(Failure::Error("macOS could not start the copy".into()));
            }
            let callback: extern "C" fn(c_int, c_int, libc::copyfile_state_t, *const c_char, *const c_char, *mut c_void) -> c_int = status;
            libc::copyfile_state_set(state, libc::COPYFILE_STATE_STATUS_CB as u32, callback as *const c_void);
            libc::copyfile_state_set(
                state,
                libc::COPYFILE_STATE_STATUS_CTX as u32,
                &mut ctx as *mut Context as *const c_void,
            );
            // Everything about the file, the whole tree, links as links,
            // never onto something already there, and cloned where the volume
            // can — which on APFS makes a copy instant and free.
            let flags = libc::COPYFILE_METADATA
                | libc::COPYFILE_DATA
                | libc::COPYFILE_RECURSIVE
                | libc::COPYFILE_NOFOLLOW
                | libc::COPYFILE_EXCL
                | libc::COPYFILE_CLONE;
            let rc = libc::copyfile(from.as_ptr(), to.as_ptr(), state, flags);
            let errno = std::io::Error::last_os_error();
            libc::copyfile_state_free(state);
            (rc, errno)
        };
        if rc == 0 {
            return Ok(());
        }
        if progress.stopped() {
            return Err(Failure::Stopped);
        }
        Err(Failure::Error(ctx.error.unwrap_or_else(|| errno.to_string())))
    }
}

#[cfg(target_os = "macos")]
use mac::copy;

/// Elsewhere: a plain recursive copy, with progress by file.
#[cfg(not(target_os = "macos"))]
fn copy(source: &Path, target: &Path, progress: &Progress, base: u64) -> Result<(), Failure> {
    fn walk(source: &Path, target: &Path, progress: &Progress, done: &mut u64) -> Result<(), Failure> {
        if progress.stopped() {
            return Err(Failure::Stopped);
        }
        let meta = source.symlink_metadata().map_err(|e| Failure::Error(e.to_string()))?;
        if meta.is_dir() {
            std::fs::create_dir(target).map_err(|e| Failure::Error(e.to_string()))?;
            for entry in std::fs::read_dir(source).map_err(|e| Failure::Error(e.to_string()))?.flatten() {
                walk(&entry.path(), &target.join(entry.file_name()), progress, done)?;
            }
        } else {
            if target.symlink_metadata().is_ok() {
                return Err(Failure::Error("the name is taken".into()));
            }
            std::fs::copy(source, target).map_err(|e| Failure::Error(e.to_string()))?;
            *done += meta.len();
            progress.bytes_done.store(*done, Ordering::Relaxed);
        }
        Ok(())
    }
    let mut done = base;
    walk(source, target, progress, &mut done)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("neutron-transfer-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("from")).unwrap();
            std::fs::create_dir_all(dir.join("to")).unwrap();
            Self(dir)
        }
        fn from(&self, name: &str) -> PathBuf {
            self.0.join("from").join(name)
        }
        fn to(&self, name: &str) -> PathBuf {
            self.0.join("to").join(name)
        }
        fn write(path: &Path, text: &str) {
            std::fs::write(path, text).unwrap();
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn copies_are_named_as_the_finder_names_them() {
        assert_eq!(strip_copy("a copy"), "a");
        assert_eq!(strip_copy("a copy 12"), "a");
        assert_eq!(strip_copy("a copy x"), "a copy x");
        assert_eq!(strip_copy("copy"), "copy");
        let s = Scratch::new("names");
        Scratch::write(&s.to("a.txt"), "");
        Scratch::write(&s.to("a copy.txt"), "");
        let seen = HashSet::new();
        assert_eq!(copy_name(&s.to("a.txt"), &s.0.join("to"), &seen).file_name().unwrap(), "a copy 2.txt");
        assert_eq!(copy_name(&s.to("a copy.txt"), &s.0.join("to"), &seen).file_name().unwrap(), "a copy 2.txt");
    }

    #[test]
    fn a_clash_is_found_before_anything_changes() {
        let s = Scratch::new("clash");
        Scratch::write(&s.from("a.txt"), "new");
        Scratch::write(&s.from("b.txt"), "new");
        Scratch::write(&s.to("A.TXT"), "old");
        let found = clashes(&[s.from("a.txt"), s.from("b.txt")], &s.0.join("to"), Transfer::Copy).unwrap();
        assert_eq!(found, vec!["a.txt".to_owned()], "case is not a difference on a Mac volume");
        assert_eq!(std::fs::read_to_string(s.to("A.TXT")).unwrap(), "old");
    }

    #[test]
    fn keep_both_and_skip_do_what_they_say() {
        let s = Scratch::new("choices");
        Scratch::write(&s.from("a.txt"), "new");
        Scratch::write(&s.to("a.txt"), "old");
        let progress = Progress::default();

        let report = run(&[s.from("a.txt")], &s.0.join("to"), Transfer::Copy, Clash::Skip, &progress).unwrap();
        assert_eq!((report.done, report.skipped), (0, 1));
        assert_eq!(std::fs::read_to_string(s.to("a.txt")).unwrap(), "old");

        let report = run(&[s.from("a.txt")], &s.0.join("to"), Transfer::Copy, Clash::KeepBoth, &progress).unwrap();
        assert_eq!(report.done, 1);
        assert_eq!(report.moved, vec![(s.from("a.txt"), s.0.join("to").canonicalize().unwrap().join("a copy.txt"))]);
        assert_eq!(std::fs::read_to_string(s.to("a copy.txt")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(s.to("a.txt")).unwrap(), "old");
    }

    #[test]
    fn a_tree_is_copied_whole_and_counted() {
        let s = Scratch::new("tree");
        std::fs::create_dir_all(s.from("dir/inner")).unwrap();
        Scratch::write(&s.from("dir/one.txt"), "12345");
        Scratch::write(&s.from("dir/inner/two.txt"), "123");
        let progress = Progress::default();
        let report = run(&[s.from("dir")], &s.0.join("to"), Transfer::Copy, Clash::KeepBoth, &progress).unwrap();
        assert_eq!((report.done, report.failed.len(), report.stopped), (1, 0, false));
        assert_eq!(std::fs::read_to_string(s.to("dir/inner/two.txt")).unwrap(), "123");
        let seen = progress.snapshot();
        assert_eq!((seen.bytes, seen.bytes_done, seen.items_done), (8, 8, 1));
        assert!(s.from("dir/one.txt").exists(), "a copy took the original");
    }

    #[test]
    fn a_move_takes_the_original_and_one_to_where_it_is_does_nothing() {
        let s = Scratch::new("move");
        Scratch::write(&s.from("a.txt"), "x");
        let progress = Progress::default();
        let report = run(&[s.from("a.txt")], &s.0.join("to"), Transfer::Move, Clash::KeepBoth, &progress).unwrap();
        assert_eq!(report.done, 1);
        assert!(!s.from("a.txt").exists() && s.to("a.txt").exists());

        let report = run(&[s.to("a.txt")], &s.0.join("to"), Transfer::Move, Clash::KeepBoth, &progress).unwrap();
        assert_eq!((report.done, report.skipped), (0, 1));
    }

    #[test]
    fn pasting_beside_itself_duplicates_without_asking() {
        let s = Scratch::new("beside");
        Scratch::write(&s.to("a.txt"), "x");
        assert!(clashes(&[s.to("a.txt")], &s.0.join("to"), Transfer::Copy).unwrap().is_empty());
        let report = run(&[s.to("a.txt")], &s.0.join("to"), Transfer::Copy, Clash::Skip, &Progress::default()).unwrap();
        assert_eq!(report.done, 1);
        assert!(s.to("a copy.txt").exists());
    }

    #[test]
    fn a_folder_cannot_go_into_itself() {
        let s = Scratch::new("itself");
        std::fs::create_dir_all(s.from("dir/inner")).unwrap();
        let err = run(&[s.from("dir")], &s.from("dir/inner"), Transfer::Copy, Clash::KeepBoth, &Progress::default());
        assert!(err.is_err());
    }

    /// Needs a second volume, where a copy is a real copy rather than a clone:
    /// `NEUTRON_TRANSFER_VOLUME=<mounted folder> cargo test -p neutron-shell
    /// part_way -- --ignored`.
    #[test]
    #[ignore]
    fn a_copy_to_another_volume_reports_progress_and_stops_part_way() {
        let Some(volume) = std::env::var_os("NEUTRON_TRANSFER_VOLUME").map(PathBuf::from) else {
            return;
        };
        let s = Scratch::new("volume");
        let big = s.from("big.bin");
        std::fs::write(&big, vec![7u8; 200 << 20]).unwrap();
        let _ = std::fs::remove_file(volume.join("big.bin"));

        let progress = Progress::default();
        let mut seen_part_way = false;
        let report = std::thread::scope(|scope| {
            let worker = scope.spawn(|| run(std::slice::from_ref(&big), &volume, Transfer::Copy, Clash::KeepBoth, &progress));
            loop {
                let now = progress.snapshot();
                if now.bytes > 0 && now.bytes_done > now.bytes / 3 {
                    seen_part_way = now.bytes_done < now.bytes;
                    progress.stop();
                    break;
                }
                if worker.is_finished() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            worker.join().unwrap().unwrap()
        });
        assert!(seen_part_way, "no progress was reported during the copy");
        assert!(report.stopped, "{report:?}");
        assert!(!volume.join("big.bin").exists(), "the partial copy was left behind");
    }

    #[test]
    fn relocating_moves_to_the_exact_name_and_never_over_anything() {
        let s = Scratch::new("relocate");
        Scratch::write(&s.from("a.txt"), "x");
        Scratch::write(&s.to("taken.txt"), "keep");
        assert!(relocate(&s.from("a.txt"), &s.to("taken.txt")).is_err());
        assert_eq!(std::fs::read_to_string(s.to("taken.txt")).unwrap(), "keep");
        relocate(&s.from("a.txt"), &s.to("deeper/renamed.txt")).unwrap();
        assert!(!s.from("a.txt").exists());
        assert_eq!(std::fs::read_to_string(s.to("deeper/renamed.txt")).unwrap(), "x");
    }

    #[test]
    fn stopping_before_the_start_leaves_nothing_behind() {
        let s = Scratch::new("stop");
        Scratch::write(&s.from("a.txt"), "x");
        let progress = Progress::default();
        progress.stop();
        let report = run(&[s.from("a.txt")], &s.0.join("to"), Transfer::Copy, Clash::KeepBoth, &progress).unwrap();
        assert!(report.stopped);
        assert!(!s.to("a.txt").exists());
    }
}
