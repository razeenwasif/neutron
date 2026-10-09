//! The right-click menu on macOS, within the same contract as Windows: the
//! items go to the UI as plain data, the chosen id comes back, and the command
//! runs here on the worker. The app re-lists afterwards whatever was chosen.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use neutron_core::menu::MenuItem;
use serde_json::json;
use crate::{clipboard, fileops, open, terminal};

const OPEN: u32 = 1;
const QUICK_LOOK: u32 = 2;
const SHOW_IN_FINDER: u32 = 3;
const COPY_PATH: u32 = 4;
const COPY: u32 = 5;
const CUT: u32 = 6;
const DUPLICATE: u32 = 7;
const COMPRESS: u32 = 8;
const TRASH: u32 = 9;
/// Open With ▸ children are `OPEN_WITH + index` into the application list.
const OPEN_WITH: u32 = 100;
/// Finder lists every application that claims the type; past a dozen the
/// submenu is a scroll, not a choice.
const MAX_OPEN_WITH: usize = 12;

const NEW_FOLDER: u32 = 1;
const PASTE: u32 = 2;
const OPEN_IN_TERMINAL: u32 = 3;
const BACKGROUND_SHOW_IN_FINDER: u32 = 4;
const BACKGROUND_COPY_PATH: u32 = 5;

fn item(id: u32, label: &str) -> MenuItem {
    MenuItem { id, label: label.into(), enabled: true, ..Default::default() }
}

pub fn open<F>(paths: &[PathBuf], choose: F) -> Result<(), String>
where F: FnOnce(Vec<MenuItem>) -> Option<u32> {
    if paths.is_empty() { return Ok(()); }
    let apps = applications_for(&paths[0]);
    let mut items = vec![MenuItem { default: true, ..item(OPEN, "Open") }];
    if !apps.is_empty() {
        let children = apps.iter().enumerate().map(|(i, app)| {
            let label = if app.default { format!("{} (default)", app.name) } else { app.name.clone() };
            item(OPEN_WITH + i as u32, &label)
        });
        items.push(MenuItem { label: "Open With".into(), enabled: true, children: children.collect(), ..Default::default() });
    }
    items.extend([
        item(QUICK_LOOK, "Quick Look"), item(SHOW_IN_FINDER, "Show in Finder"), item(COPY_PATH, "Copy Path"),
        MenuItem::separator(),
        item(COPY, "Copy"), item(CUT, "Cut"), item(DUPLICATE, "Duplicate"),
        item(COMPRESS, &compress_label(paths)),
        MenuItem::separator(),
        item(TRASH, "Move to Trash"),
    ]);
    match choose(items) {
        Some(OPEN) => {
            for path in paths { open::shell_execute(path, open::Verb::Open, 0)?; }
            Ok(())
        },
        Some(id) if (OPEN_WITH..OPEN_WITH + apps.len() as u32).contains(&id) => {
            open_with(&apps[(id - OPEN_WITH) as usize].path, paths)
        },
        Some(QUICK_LOOK) => quick_look(paths),
        Some(SHOW_IN_FINDER) => reveal(paths),
        Some(COPY_PATH) => copy_text(&joined(paths)?),
        Some(COPY) => clipboard::write(paths, fileops::Transfer::Copy),
        Some(CUT) => clipboard::write(paths, fileops::Transfer::Move),
        Some(DUPLICATE) => duplicate(paths),
        Some(COMPRESS) => compress(paths).map(|_| ()),
        Some(TRASH) => fileops::delete(paths, fileops::Disposal::Recycle, 0),
        _ => Ok(()),
    }
}

pub fn open_background<F>(folder: &Path, choose: F) -> Result<(), String>
where F: FnOnce(Vec<MenuItem>) -> Option<u32> {
    // Offered only when it would work, rather than failing after the click.
    let pasteable = clipboard::read().is_some_and(|(paths, _)| clipboard::can_paste_into(&paths, folder));
    match choose(vec![
        item(NEW_FOLDER, "New Folder"),
        MenuItem { enabled: pasteable, ..item(PASTE, "Paste") },
        MenuItem::separator(),
        item(OPEN_IN_TERMINAL, "Open in Terminal"),
        item(BACKGROUND_SHOW_IN_FINDER, "Show in Finder"),
        item(BACKGROUND_COPY_PATH, "Copy Path"),
    ]) {
        Some(NEW_FOLDER) => fileops::create_folder(folder, 0).map(|_| ()),
        Some(PASTE) => {
            let (paths, how) = clipboard::read().ok_or("no files on the clipboard")?;
            fileops::transfer(&paths, folder, how, 0)
        },
        Some(OPEN_IN_TERMINAL) => terminal::open_at(folder),
        // The folder itself, in Finder by name: if Neutron is the default
        // folder viewer, a plain `open` would come straight back here.
        Some(BACKGROUND_SHOW_IN_FINDER) => run(Command::new("/usr/bin/open").args(["-a", "Finder", "--"]).arg(folder)),
        Some(BACKGROUND_COPY_PATH) => copy_text(super::macos::path_str(folder)?),
        _ => Ok(()),
    }
}

/// An application offered under Open With.
struct App {
    name: String,
    path: PathBuf,
    default: bool,
}

/// The applications Launch Services says can open `path`, default first.
/// Asked of the first item only; Finder intersects the whole selection, which
/// costs a query per item for a difference that rarely matters.
fn applications_for(path: &Path) -> Vec<App> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let Some(path) = path.to_str() else { return Vec::new() };
    objc2::rc::autoreleasepool(|_| {
        let workspace = NSWorkspace::sharedWorkspace();
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        let default = workspace.URLForApplicationToOpenURL(&url).and_then(|u| u.path()).map(|p| p.to_string());
        let names = NSFileManager::defaultManager();
        let mut apps: Vec<App> = Vec::new();
        for app in workspace.URLsForApplicationsToOpenURL(&url).iter() {
            let Some(app) = app.path() else { continue };
            let app_path = app.to_string();
            // An application is its own only handler; "Open With ▸ itself"
            // is just Open.
            if app_path == path || apps.iter().any(|a| a.path.as_os_str() == app_path.as_str()) { continue; }
            apps.push(App {
                name: names.displayNameAtPath(&app).to_string(),
                default: default.as_deref() == Some(app_path.as_str()),
                path: PathBuf::from(app_path),
            });
        }
        // Stable: the rest stay in Launch Services' order.
        apps.sort_by_key(|a| !a.default);
        apps.truncate(MAX_OPEN_WITH);
        apps
    })
}

fn open_with(app: &Path, paths: &[PathBuf]) -> Result<(), String> {
    run(Command::new("/usr/bin/open").arg("-a").arg(app).arg("--").args(paths))
}

fn run(command: &mut Command) -> Result<(), String> {
    let output = command.stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if output.status.success() { Ok(()) }
    else { Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()) }
}

/// The Quick Look panel, as `qlmanage -p` shows it. Detached: the panel stays
/// up until the user closes it, and this worker has a menu to finish.
fn quick_look(paths: &[PathBuf]) -> Result<(), String> {
    let mut child = Command::new("/usr/bin/qlmanage").arg("-p").args(paths)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().map_err(|e| format!("could not start Quick Look: {e}"))?;
    // Reaped elsewhere so it does not linger as a zombie once closed.
    std::thread::spawn(move || { let _ = child.wait(); });
    Ok(())
}

fn reveal(paths: &[PathBuf]) -> Result<(), String> {
    let paths = super::macos::path_strs(paths)?;
    super::macos::call(json!({"action":"reveal", "paths":paths})).map(|_| ())
}

fn copy_text(text: &str) -> Result<(), String> {
    super::macos::call(json!({"action":"clipboard-text", "text":text})).map(|_| ())
}

/// POSIX paths, one per line, as Finder's Copy as Pathname writes them.
fn joined(paths: &[PathBuf]) -> Result<String, String> {
    Ok(super::macos::path_strs(paths)?.join("\n"))
}

/// Copies each item into its own folder. The helper names the copies the way
/// Finder does ("name copy", "name copy 2").
fn duplicate(paths: &[PathBuf]) -> Result<(), String> {
    for (folder, group) in by_parent(paths)? {
        fileops::transfer(&group, &folder, fileops::Transfer::Copy, 0)?;
    }
    Ok(())
}

fn by_parent(paths: &[PathBuf]) -> Result<Vec<(PathBuf, Vec<PathBuf>)>, String> {
    let mut groups: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for path in paths {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| format!("{} has no enclosing folder", path.display()))?;
        match groups.iter_mut().find(|(p, _)| p == parent) {
            Some((_, group)) => group.push(path.clone()),
            None => groups.push((parent.to_path_buf(), vec![path.clone()])),
        }
    }
    Ok(groups)
}

fn compress_label(paths: &[PathBuf]) -> String {
    match paths {
        [one] => format!("Compress \"{}\"", one.file_name().unwrap_or(one.as_os_str()).to_string_lossy()),
        many => format!("Compress {} Items", many.len()),
    }
}

/// Zips the selection beside it, as Finder's Compress does: `name.zip` for
/// one item, `Archive.zip` for several, numbered rather than overwriting.
/// Returns the archive's path.
fn compress(paths: &[PathBuf]) -> Result<PathBuf, String> {
    let groups = by_parent(paths)?;
    let [(folder, items)] = groups.as_slice() else {
        return Err("items from different folders cannot be compressed together".into());
    };
    let stem = match items.as_slice() {
        [one] => one.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        _ => "Archive".to_owned(),
    };
    let archive = (1..10_000)
        .map(|n| folder.join(if n == 1 { format!("{stem}.zip") } else { format!("{stem} {n}.zip") }))
        .find(|p| p.symlink_metadata().is_err())
        .ok_or("could not choose a name for the archive")?;
    if let [one] = items.as_slice() {
        // ditto is what Archive Utility uses: it keeps extended attributes and
        // resource forks in the `__MACOSX` form Finder expects back.
        run(Command::new("/usr/bin/ditto").args(["-c", "-k", "--sequesterRsrc", "--keepParent"]).arg(one).arg(&archive))?;
    } else {
        // ditto takes a single source; zip takes many, relative to the folder
        // so the archive holds names rather than absolute paths. `-y` stores
        // symlinks as links instead of following them out of the selection.
        let names: Vec<&std::ffi::OsStr> = items.iter().filter_map(|p| p.file_name()).collect();
        let result = run(Command::new("/usr/bin/zip").args(["-q", "-r", "-y"]).arg(&archive).arg("--").args(names).current_dir(folder));
        if result.is_err() { let _ = std::fs::remove_file(&archive); }
        result?;
    }
    Ok(archive)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("neutron-menu-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    fn pick(label: &'static str) -> impl FnOnce(Vec<MenuItem>) -> Option<u32> {
        move |items| items.iter().find(|i| i.label.starts_with(label)).map(|i| i.id)
    }

    #[test]
    fn a_text_file_offers_open_with_and_the_finder_commands() {
        let root = Scratch::new("items");
        let file = root.0.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let mut seen = Vec::new();
        open(std::slice::from_ref(&file), |items| { seen = items; None }).unwrap();
        let labels: Vec<&str> = seen.iter().map(|i| i.label.as_str()).collect();
        for want in ["Open", "Open With", "Quick Look", "Show in Finder", "Copy Path", "Duplicate", "Compress \"a.txt\"", "Move to Trash"] {
            assert!(labels.contains(&want), "missing {want}: {labels:?}");
        }
        assert!(seen[0].default);
        let open_with = seen.iter().find(|i| i.label == "Open With").unwrap();
        assert!(open_with.children[0].label.ends_with("(default)"), "{:?}", open_with.children[0].label);
        assert!(open_with.children.iter().all(|c| c.id >= OPEN_WITH));
    }

    #[test]
    fn duplicate_makes_a_finder_style_copy_beside_the_original() {
        let root = Scratch::new("dup");
        let file = root.0.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        open(std::slice::from_ref(&file), pick("Duplicate")).unwrap();
        assert_eq!(std::fs::read(root.0.join("a copy.txt")).unwrap(), b"x");
    }

    #[test]
    fn compress_names_archives_like_finder_and_never_overwrites() {
        let root = Scratch::new("zip");
        let (a, b) = (root.0.join("a.txt"), root.0.join("b"));
        std::fs::write(&a, "x").unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::write(b.join("inner"), "y").unwrap();
        assert_eq!(compress(std::slice::from_ref(&a)).unwrap(), root.0.join("a.txt.zip"));
        assert_eq!(compress(std::slice::from_ref(&a)).unwrap(), root.0.join("a.txt 2.zip"));
        let archive = compress(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(archive, root.0.join("Archive.zip"));
        let listing = Command::new("/usr/bin/unzip").arg("-Z1").arg(&archive).output().unwrap();
        let listing = String::from_utf8_lossy(&listing.stdout);
        assert!(listing.lines().any(|l| l == "a.txt") && listing.lines().any(|l| l == "b/inner"), "{listing}");
    }

    #[test]
    fn the_background_menu_disables_paste_rather_than_failing_later() {
        let root = Scratch::new("bg");
        let mut seen = Vec::new();
        open_background(&root.0, |items| { seen = items; None }).unwrap();
        let labels: Vec<&str> = seen.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["New Folder", "Paste", "", "Open in Terminal", "Show in Finder", "Copy Path"]);
        open_background(&root.0, pick("New Folder")).unwrap();
        assert!(root.0.join("New Folder").is_dir());
    }
}
