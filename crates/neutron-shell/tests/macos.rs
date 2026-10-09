#![cfg(target_os = "macos")]
use neutron_core::{Namespace, NodeId, entry::attr};
use neutron_shell::{fileops::{self, Disposal, Transfer}, fs::FsNamespace};
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = format!("neutron-macos-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

#[test]
fn enumeration_preserves_unicode_hidden_files_and_directory_symlinks() {
    let root = Scratch::new();
    std::fs::write(root.0.join("日本語.txt"), "hello").unwrap();
    std::fs::write(root.0.join(".hidden"), "secret").unwrap();
    std::fs::create_dir(root.0.join("folder")).unwrap();
    std::os::unix::fs::symlink(root.0.join("folder"), root.0.join("link")).unwrap();
    let list = FsNamespace.enumerate(&NodeId::path(&root.0)).unwrap();
    assert_eq!(list.len(), 4);
    let index = |name| (0..list.len()).find(|&i| list.name(i) == name).unwrap();
    assert_eq!(list.size(index("日本語.txt")), 5);
    assert!(list.is_hidden(index(".hidden")));
    assert!(list.kind(index("link")).is_container());
    assert_ne!(list.attrs(index("link")) & attr::REPARSE_POINT, 0);
}

#[test]
fn native_file_operations_copy_move_rename_and_preserve_symlinks() {
    let root = Scratch::new();
    let folder = fileops::create_folder(&root.0, 0).unwrap();
    let other = fileops::create_folder(&root.0, 0).unwrap();
    assert_ne!(folder, other);
    let source = root.0.join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("日本語.txt"), "data").unwrap();
    std::os::unix::fs::symlink("日本語.txt", source.join("link")).unwrap();
    let destination = root.0.join(folder);
    fileops::transfer(std::slice::from_ref(&source), &destination, Transfer::Copy, 0).unwrap();
    assert_eq!(std::fs::read(destination.join("source/日本語.txt")).unwrap(), b"data");
    assert!(destination.join("source/link").symlink_metadata().unwrap().file_type().is_symlink());
    fileops::rename(&destination.join("source"), "renamed", 0).unwrap();
    fileops::transfer(&[destination.join("renamed")], &root.0.join(other), Transfer::Move, 0).unwrap();
    assert!(!destination.join("renamed").exists());
    fileops::delete(std::slice::from_ref(&source), Disposal::Permanent, 0).unwrap();
    assert!(!source.exists());
}

#[test]
fn transfers_refuse_overwrites_and_symlink_aliases_into_the_source() {
    let root = Scratch::new();
    let source = root.0.join("source");
    let dest = root.0.join("dest");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("source"), "do not overwrite").unwrap();
    // A colliding copy gets a fresh name; a colliding move is refused.
    fileops::transfer(std::slice::from_ref(&source), &dest, Transfer::Copy, 0).unwrap();
    assert!(dest.join("source copy").is_dir());
    assert!(fileops::transfer(std::slice::from_ref(&source), &dest, Transfer::Move, 0).is_err());
    assert_eq!(std::fs::read(dest.join("source")).unwrap(), b"do not overwrite");
    std::os::unix::fs::symlink(&source, root.0.join("alias")).unwrap();
    assert!(fileops::transfer(std::slice::from_ref(&source), &root.0.join("alias"), Transfer::Copy, 0).is_err());
    assert!(fileops::rename(&source, "../escaped", 0).is_err());
    assert!(source.is_dir());
}

#[test]
fn watcher_reports_external_writes() {
    let root = Scratch::new();
    let file = root.0.join("file");
    std::fs::write(&file, "a").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let watcher = neutron_shell::watch::watch(&root.0, move || { let _ = tx.send(()); }).unwrap();
    // Let the worker establish its initial snapshot.
    std::thread::sleep(std::time::Duration::from_millis(300));
    // Rewriting a file in place does not touch the directory, so the kqueue
    // stays quiet and this is the slow metadata poll's job (every 5 s).
    std::fs::write(file, "different length").unwrap();
    rx.recv_timeout(std::time::Duration::from_secs(8)).unwrap();
    drop(watcher);
}

#[test]
fn watcher_reports_new_files_at_once_and_stops_when_dropped() {
    use std::time::{Duration, Instant};
    let root = Scratch::new();
    let (tx, rx) = std::sync::mpsc::channel();
    let watcher = neutron_shell::watch::watch(&root.0, move || { let _ = tx.send(()); }).unwrap();
    let start = Instant::now();
    std::fs::write(root.0.join("new"), "x").unwrap();
    rx.recv_timeout(Duration::from_millis(500)).expect("a new file is reported within 500ms");
    assert!(start.elapsed() < Duration::from_millis(500));
    // The worker owns the only sender; when its thread ends the closure is
    // dropped and the channel disconnects. A quiet folder must not keep it.
    std::thread::sleep(Duration::from_millis(400));
    while rx.try_recv().is_ok() {}
    drop(watcher);
    assert_eq!(rx.recv_timeout(Duration::from_millis(500)), Err(std::sync::mpsc::RecvTimeoutError::Disconnected));
}

#[test]
fn watcher_on_a_deleted_folder_reports_once_and_stops() {
    use std::time::Duration;
    let root = Scratch::new();
    let watched = root.0.join("doomed");
    std::fs::create_dir(&watched).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let _watcher = neutron_shell::watch::watch(&watched, move || { let _ = tx.send(()); }).unwrap();
    std::fs::remove_dir(&watched).unwrap();
    rx.recv_timeout(Duration::from_millis(500)).expect("the deletion is reported");
    // The worker exits on its own, still holding a watcher handle, instead of
    // spinning on a directory that is not there.
    assert_eq!(rx.recv_timeout(Duration::from_millis(1000)), Err(std::sync::mpsc::RecvTimeoutError::Disconnected));
}

#[test]
fn startup_volume_reports_real_capacity() {
    let volumes = neutron_shell::places::drives();
    let root = volumes.iter().find(|v| v.id.as_path() == Some(std::path::Path::new("/"))).unwrap();
    let capacity = root.capacity.expect("the startup disk reports capacity");
    assert!(capacity.total > 0);
    assert!(capacity.free <= capacity.total);
}

#[test]
fn copying_into_the_same_folder_makes_finder_style_copies() {
    let root = Scratch::new();
    let file = root.0.join("a.txt");
    std::fs::write(&file, "data").unwrap();
    let dir = root.0.join("dir.d");
    std::fs::create_dir(&dir).unwrap();
    fileops::transfer(&[file.clone(), dir.clone()], &root.0, Transfer::Copy, 0).unwrap();
    assert_eq!(std::fs::read(root.0.join("a copy.txt")).unwrap(), b"data");
    assert!(root.0.join("dir.d copy").is_dir());
    fileops::transfer(std::slice::from_ref(&file), &root.0, Transfer::Copy, 0).unwrap();
    assert!(root.0.join("a copy 2.txt").is_file());
    // Copying a copy continues the sequence instead of nesting "copy copy".
    fileops::transfer(&[root.0.join("a copy.txt")], &root.0, Transfer::Copy, 0).unwrap();
    assert!(root.0.join("a copy 3.txt").is_file());
    assert_eq!(std::fs::read(&file).unwrap(), b"data");
}

#[test]
fn moving_into_the_current_folder_is_a_no_op_but_never_overwrites_elsewhere() {
    let root = Scratch::new();
    let file = root.0.join("a.txt");
    std::fs::write(&file, "mine").unwrap();
    fileops::transfer(std::slice::from_ref(&file), &root.0, Transfer::Move, 0).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"mine");
    let dest = root.0.join("dest");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("a.txt"), "theirs").unwrap();
    let error = fileops::transfer(std::slice::from_ref(&file), &dest, Transfer::Move, 0).unwrap_err();
    assert!(error.contains("a.txt"), "{error}");
    assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"theirs");
    assert!(file.exists());
}

#[test]
fn batches_continue_past_failures_and_name_every_one() {
    let root = Scratch::new();
    let dest = root.0.join("dest");
    std::fs::create_dir(&dest).unwrap();
    let (gone, good) = (root.0.join("gone.txt"), root.0.join("good.txt"));
    std::fs::write(&good, "ok").unwrap();
    let error = fileops::transfer(&[gone.clone(), good.clone()], &dest, Transfer::Copy, 0).unwrap_err();
    assert!(error.starts_with("Could not copy 1 of 2 items: gone.txt: "), "{error}");
    assert!(dest.join("good.txt").is_file());
    let error = fileops::delete(&[gone, good.clone()], Disposal::Permanent, 0).unwrap_err();
    assert!(error.starts_with("Could not delete 1 of 2 items: gone.txt: "), "{error}");
    assert!(!good.exists());
}

#[test]
fn paste_checks_ignore_sources_that_no_longer_exist() {
    use neutron_shell::clipboard::can_paste_into;
    let root = Scratch::new();
    let (folder, gone) = (root.0.join("folder"), root.0.join("gone"));
    std::fs::create_dir_all(folder.join("inner")).unwrap();
    assert!(can_paste_into(&[gone.clone(), folder.clone()], &root.0));
    assert!(!can_paste_into(&[gone.clone(), folder.clone()], &folder.join("inner")));
    assert!(!can_paste_into(std::slice::from_ref(&gone), &root.0));
    assert!(!can_paste_into(&[], &root.0));
}

#[test]
fn non_utf8_paths_are_refused_rather_than_panicking() {
    use std::os::unix::ffi::OsStrExt;
    let root = Scratch::new();
    let odd = root.0.join(std::ffi::OsStr::from_bytes(b"bad\xff.txt"));
    let error = fileops::transfer(std::slice::from_ref(&odd), &root.0, Transfer::Copy, 0).unwrap_err();
    assert!(error.contains("cannot pass to macOS"), "{error}");
    assert!(fileops::delete(std::slice::from_ref(&odd), Disposal::Permanent, 0).is_err());
    assert!(fileops::rename(&odd, "x", 0).is_err());
    assert!(fileops::create_folder(&odd, 0).is_err());
    assert!(neutron_shell::clipboard::write(&[odd], Transfer::Copy).is_err());
}

#[test]
fn bsd_hidden_flag_marks_entries_hidden() {
    let root = Scratch::new();
    std::fs::write(root.0.join("flagged"), "").unwrap();
    std::fs::write(root.0.join("plain"), "").unwrap();
    assert!(std::process::Command::new("chflags").arg("hidden").arg(root.0.join("flagged")).status().unwrap().success());
    let list = FsNamespace.enumerate(&NodeId::path(&root.0)).unwrap();
    let index = |name| (0..list.len()).find(|&i| list.name(i) == name).unwrap();
    assert!(list.is_hidden(index("flagged")));
    assert!(!list.is_hidden(index("plain")));
}

#[test]
fn sidebar_drives_skip_hidden_mounts() {
    for drive in neutron_shell::places::drives() {
        assert!(!drive.name.starts_with('.'), "{}", drive.name);
    }
}

#[test]
fn packages_are_listed_as_files_and_ordinary_folders_are_not() {
    use neutron_core::EntryKind;
    let root = Scratch::new();
    std::fs::create_dir_all(root.0.join("x.app/Contents")).unwrap();
    std::fs::write(root.0.join("x.app/Contents/Info.plist"), "<plist/>").unwrap();
    std::fs::create_dir(root.0.join("plain")).unwrap();
    // An extension that is not a package type stays a folder.
    std::fs::create_dir(root.0.join("v1.2")).unwrap();
    let list = FsNamespace.enumerate(&NodeId::path(&root.0)).unwrap();
    let index = |name| (0..list.len()).find(|&i| list.name(i) == name).unwrap();
    assert_eq!(list.kind(index("x.app")), EntryKind::File);
    assert_eq!(list.attrs(index("x.app")) & attr::DIRECTORY, 0);
    assert_eq!(list.kind(index("plain")), EntryKind::Directory);
    assert_eq!(list.kind(index("v1.2")), EntryKind::Directory);
}
