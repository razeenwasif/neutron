//! Folders a drag can be dropped into: rows that are folders, sidebar places.
//!
//! While files are being dragged over a window, each one drawn says where it
//! is and where it leads. The app looks for the one under the pointer, lights
//! it, and on the drop puts the files inside it instead of in the folder the
//! pane shows. Nothing is recorded the rest of the time.

use std::path::{Path, PathBuf};

use egui::{Context, Id, Rect};

/// A place to drop into.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// Where it is drawn this frame, on screen.
    pub rect: Rect,
    /// Where it leads: a full path, or a name inside the folder the pane
    /// shows, which the app joins to that folder.
    pub path: PathBuf,
    /// How round its corners are drawn, so the highlight fits it.
    pub radius: f32,
}

fn id() -> Id {
    Id::new("neutron-drop-targets")
}

/// Whether files are being dragged over the window, or dropped on it, this
/// frame.
pub fn active(ctx: &Context) -> bool {
    ctx.input(|i| !i.raw.hovered_files.is_empty() || !i.raw.dropped_files.is_empty())
}

/// Offers `rect`, as drawn in `ui`, as a way into `path` — only while a drag is
/// over the window, and only the part of it that is visible.
pub fn offer(ui: &egui::Ui, rect: Rect, path: &Path, radius: f32) {
    if !active(ui.ctx()) {
        return;
    }
    let rect = rect.intersect(ui.clip_rect());
    if rect.is_positive() {
        ui.ctx().data_mut(|d| {
            d.get_temp_mut_or_default::<Vec<Target>>(id()).push(Target { rect, path: path.to_path_buf(), radius })
        });
    }
}

/// Everything offered since the last call, emptied for the next frame.
pub fn take(ctx: &Context) -> Vec<Target> {
    ctx.data_mut(|d| d.remove_temp::<Vec<Target>>(id())).unwrap_or_default()
}

/// Whether a name is a folder files can be put in, rather than a package the
/// Finder treats as one file.
pub fn accepts(name: &str, kind: neutron_core::entry::EntryKind) -> bool {
    kind.is_container() && !name.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("app"))
}
