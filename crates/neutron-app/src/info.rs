//! Get Info, Neutron's own: a small window per item with what it is, how big,
//! where, when, who may use it, and its tags.
//!
//! The facts are read once, on a worker, when the window opens; a folder's
//! size is added up on a thread of its own, as View › Calculate All Sizes
//! does, and shows as "Calculating…" until it is known. For what this does
//! not cover — "Open with" for every file of a kind, comments, locking — the
//! window offers the Finder's.

use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, Sender};
use egui::RichText;
use neutron_ui::theme::{self, Palette};

/// What Get Info shows about an item.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub kind: String,
    pub is_dir: bool,
    /// A file's size; a folder's comes separately.
    pub size: Option<u64>,
    pub created: i64,
    pub modified: i64,
    pub added: i64,
    pub opened: i64,
    pub owner: String,
    pub group: String,
    /// What each may do, as `ls` writes it: `rwxr-xr-x`.
    pub mode: String,
    /// What you may do with it, in words.
    pub access: String,
    /// What an alias or a link stands for.
    pub original: Option<PathBuf>,
    pub tags: Vec<String>,
    pub error: Option<String>,
}

/// Reads an item's facts. Blocks on the disk: call it from a worker.
pub fn read(path: &Path) -> Facts {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = match path.symlink_metadata() {
        Ok(m) => m,
        Err(e) => return Facts { error: Some(e.to_string()), ..Default::default() },
    };
    let millis = |t: std::io::Result<std::time::SystemTime>| {
        t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let is_dir = meta.is_dir() && !crate::columns::is_package(path);
    let kind = if meta.file_type().is_symlink() {
        "Symbolic link".to_owned()
    } else if is_dir {
        "Folder".to_owned()
    } else {
        neutron_ui::file_list::kind_label(neutron_core::entry::EntryKind::File, &name)
    };
    let mode = meta.permissions().mode();
    let bits = |shift: u32| {
        let m = (mode >> shift) & 7;
        format!("{}{}{}", if m & 4 != 0 { 'r' } else { '-' }, if m & 2 != 0 { 'w' } else { '-' }, if m & 1 != 0 { 'x' } else { '-' })
    };
    let me = unsafe { libc::getuid() };
    let mine = meta.uid() == me;
    let can_write = if mine { mode & 0o200 != 0 } else { mode & 0o002 != 0 || mode & 0o020 != 0 };
    let access = match (mine, can_write) {
        (true, true) => "You can read and write",
        (true, false) => "You can only read",
        (false, true) => "You can read and write (shared)",
        (false, false) => "You can only read, or have no access",
    };
    Facts {
        kind,
        is_dir,
        size: (!is_dir).then_some(meta.len()),
        created: millis(meta.created()),
        modified: millis(meta.modified()),
        added: neutron_shell::fs::added(path),
        opened: neutron_shell::fs::last_opened(path),
        owner: user_name(meta.uid()),
        group: group_name(meta.gid()),
        mode: format!("{}{}{}", bits(6), bits(3), bits(0)),
        access: access.to_owned(),
        original: neutron_shell::fs::original(path).ok(),
        tags: neutron_shell::tags::read(path).into_iter().map(|t| t.name).collect(),
        error: None,
    }
}

fn user_name(uid: u32) -> String {
    // SAFETY: getpwuid returns a pointer into static storage or null; the
    // name is copied out before anything else can call it on this thread.
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return uid.to_string();
        }
        std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned()
    }
}

fn group_name(gid: u32) -> String {
    // SAFETY: as for `user_name`.
    unsafe {
        let gr = libc::getgrgid(gid);
        if gr.is_null() {
            return gid.to_string();
        }
        std::ffi::CStr::from_ptr((*gr).gr_name).to_string_lossy().into_owned()
    }
}

/// What an info window asks the app to do.
pub enum Ask {
    ShowInFolder(PathBuf),
    ShowOriginal(PathBuf),
    EditTags(PathBuf),
    FinderInfo(PathBuf),
}

struct Window {
    path: PathBuf,
    viewport: egui::ViewportId,
    facts: Option<Facts>,
    /// A folder's size: `None` while it is being added up.
    folder_size: Option<u64>,
    open: bool,
}

/// The open info windows.
pub struct Infos {
    windows: Vec<Window>,
    facts_tx: Sender<(PathBuf, Facts)>,
    facts_rx: Receiver<(PathBuf, Facts)>,
    size_tx: Sender<(PathBuf, u64)>,
    size_rx: Receiver<(PathBuf, u64)>,
}

impl Default for Infos {
    fn default() -> Self {
        let (facts_tx, facts_rx) = crossbeam_channel::unbounded();
        let (size_tx, size_rx) = crossbeam_channel::unbounded();
        Infos { windows: Vec::new(), facts_tx, facts_rx, size_tx, size_rx }
    }
}

impl Infos {
    /// Opens a window on each of `paths` in `viewport`, or brings forward the
    /// one already open.
    pub fn open(&mut self, paths: Vec<PathBuf>, viewport: egui::ViewportId, ctx: &egui::Context) {
        for path in paths {
            if let Some(w) = self.windows.iter_mut().find(|w| w.path == path) {
                w.open = true;
                w.viewport = viewport;
                ctx.move_to_top(egui::LayerId::new(egui::Order::Middle, window_id(&path)));
                continue;
            }
            let facts_tx = self.facts_tx.clone();
            let size_tx = self.size_tx.clone();
            let wake = ctx.clone();
            let owned = path.clone();
            let _ = std::thread::Builder::new().name("get-info".into()).spawn(move || {
                let facts = read(&owned);
                let is_dir = facts.is_dir;
                let _ = facts_tx.send((owned.clone(), facts));
                wake.request_repaint();
                if is_dir {
                    let size = crate::folder_sizes::measure(&owned, || true).unwrap_or(0);
                    let _ = size_tx.send((owned, size));
                    wake.request_repaint();
                }
            });
            self.windows.push(Window { path, viewport, facts: None, folder_size: None, open: true });
        }
    }

    /// Takes in facts and sizes read since the last frame.
    pub fn pump(&mut self) {
        for (path, facts) in self.facts_rx.try_iter() {
            if let Some(w) = self.windows.iter_mut().find(|w| w.path == path) {
                w.facts = Some(facts);
            }
        }
        for (path, size) in self.size_rx.try_iter() {
            if let Some(w) = self.windows.iter_mut().find(|w| w.path == path) {
                w.folder_size = Some(size);
            }
        }
        self.windows.retain(|w| w.open);
    }

    /// Draws the windows belonging to `viewport`.
    pub fn show(&mut self, ctx: &egui::Context, p: &Palette, viewport: egui::ViewportId) -> Option<Ask> {
        let mut ask = None;
        for (n, w) in self.windows.iter_mut().filter(|w| w.viewport == viewport).enumerate() {
            let name = w.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| w.path.display().to_string());
            let offset = 24.0 * n as f32;
            egui::Window::new(format!("{name} Info"))
                .id(window_id(&w.path))
                .open(&mut w.open)
                .resizable(false)
                .collapsible(true)
                .default_width(320.0)
                .default_pos(ctx.content_rect().center() - egui::vec2(160.0 - offset, 220.0 - offset))
                .show(ctx, |ui| {
                    ui.label(RichText::new(&name).font(theme::strong(ui.ctx(), 15.0)).color(p.text));
                    let Some(f) = &w.facts else {
                        ui.label(RichText::new("Reading…").color(p.text_faint));
                        return;
                    };
                    if let Some(e) = &f.error {
                        ui.label(RichText::new(e).color(p.danger));
                        return;
                    }
                    ui.label(RichText::new(&f.kind).size(12.0).color(p.text_muted));
                    ui.add_space(8.0);
                    let row = |ui: &mut egui::Ui, label: &str, value: String| {
                        ui.label(RichText::new(label).size(12.0).color(p.text_faint));
                        ui.label(RichText::new(value).size(12.0).color(p.text));
                        ui.end_row();
                    };
                    let when = |t: i64| if t > 0 { neutron_ui::format::timestamp(t) } else { "—".to_owned() };
                    egui::Grid::new(("info-general", &w.path)).num_columns(2).spacing([12.0, 5.0]).show(ui, |ui| {
                        let size = match (f.size, w.folder_size) {
                            (Some(s), _) | (None, Some(s)) => format!("{} ({} bytes)", neutron_ui::format::size(Some(s)), group_digits(s)),
                            (None, None) => "Calculating…".to_owned(),
                        };
                        row(ui, "Size", size);
                        row(ui, "Where", w.path.parent().map(|p| p.display().to_string()).unwrap_or_default());
                        row(ui, "Created", when(f.created));
                        row(ui, "Modified", when(f.modified));
                        row(ui, "Added", when(f.added));
                        row(ui, "Last opened", when(f.opened));
                        if let Some(original) = &f.original {
                            row(ui, "Original", original.display().to_string());
                        }
                        row(ui, "Tags", if f.tags.is_empty() { "None".to_owned() } else { f.tags.join(", ") });
                        row(ui, "Owner", format!("{} ({})", f.owner, f.group));
                        row(ui, "Permissions", format!("{} · {}", f.mode, f.access));
                    });
                    ui.add_space(10.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Show in Folder").clicked() {
                            ask = Some(Ask::ShowInFolder(w.path.clone()));
                        }
                        if f.original.is_some() && ui.button("Show Original").clicked() {
                            ask = Some(Ask::ShowOriginal(w.path.clone()));
                        }
                        if ui.button("Edit Tags…").clicked() {
                            ask = Some(Ask::EditTags(w.path.clone()));
                        }
                        if ui.button("More in the Finder…").on_hover_text("The Finder's Info window: Open With, comments, locking").clicked() {
                            ask = Some(Ask::FinderInfo(w.path.clone()));
                        }
                    });
                });
        }
        ask
    }
}

fn window_id(path: &Path) -> egui::Id {
    egui::Id::new(("neutron-info", path))
}

/// `1234567` as `1,234,567`.
fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_and_a_folder_are_described() {
        let dir = std::env::temp_dir().join(format!("neutron-info-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        let file = read(&dir.join("a.txt"));
        assert_eq!(file.size, Some(5));
        assert!(!file.is_dir && file.error.is_none());
        assert_eq!(file.mode.len(), 9);
        assert!(file.access.starts_with("You can read and write"), "{}", file.access);
        assert!(!file.owner.is_empty());
        let folder = read(&dir);
        assert!(folder.is_dir && folder.size.is_none());
        assert_eq!(folder.kind, "Folder");
        assert!(read(&dir.join("missing")).error.is_some());
        assert_eq!(group_digits(1234567), "1,234,567");
        assert_eq!(group_digits(12), "12");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
