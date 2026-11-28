use super::*;

/// Uploads `sources`, refusing up front any whose name the server folder
/// already has — an upload replaces, and there is no bin behind it.
pub(super) fn upload_new(
    site: &neutron_remote::Site,
    dir: &str,
    sources: Vec<PathBuf>,
    taken: &[String],
    progress: &crate::remote_ops::Progress,
) -> Result<String, String> {
    let names: Vec<String> = sources
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
    let clashes = crate::remote_ops::collisions(names.iter().map(String::as_str), &taken);
    if !clashes.is_empty() {
        return Err(format!(
            "{} already on the server, so nothing was sent: {}",
            if clashes.len() == 1 { "this name is" } else { "these names are" },
            clashes.join(", ")
        ));
    }
    crate::remote_ops::run(site, dir, &crate::remote_ops::Op::Upload { sources }, progress)
}

/// The first SHA-256 certificate fingerprint in `text`, in the colon-separated
/// form an untrusted-certificate refusal names it in.
pub(super) fn certificate_in(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | '"' | '\''))
        .map(|word| word.trim_end_matches(['.', ';']))
        .find(|word| {
            let pairs: Vec<&str> = word.split(':').collect();
            pairs.len() == 32 && pairs.iter().all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
        })
        .map(str::to_owned)
}

/// What the app does with something the list reported.
///
/// Shared by the list, the grid and the column view's current column, which
/// all speak in the list's terms.
pub(super) fn list_action(a: FileListAction) -> Action {
    match a {
        FileListAction::Activate(idx) => Action::Activate(idx),
        FileListAction::Select { idx, mode } => Action::Select { idx, mode },
        FileListAction::SortBy(col) => Action::SortBy(col),
        FileListAction::ClearSelection => Action::ClearSelection,
        FileListAction::Marquee { from, to, additive } => Action::Marquee { from, to, additive },
        FileListAction::ContextMenu { idx, pos } => Action::ContextMenu { idx, pos },
        FileListAction::DragOut => Action::DragOut,
        FileListAction::RenameTyped(text) => Action::RenameTyped(text),
        FileListAction::RenameCommit(text) => Action::RenameCommit(text),
        FileListAction::RenameCancel => Action::RenameCancel,
    }
}

/// Adapts [`IconService`] to the file list's [`file_list::IconSource`].
///
/// Exists because `neutron-ui` must not depend on `neutron-shell` — that is
/// what keeps it building and testing on Linux — so the list knows only how to
/// ask for a UV rect, and this is where "ask" becomes "look up a shell icon".
///
/// Built per pane rather than per row: the directory is needed to root the
/// per-path keys (an executable's icon is inside the executable), and it is the
/// same for every row in the pane.
pub(super) struct RowIcons<'a> {
    pub(super) service: &'a IconService,
    pub(super) thumbs: &'a crate::thumbs::Thumbnails,
    pub(super) texture: Option<egui::TextureId>,
    pub(super) dir: Option<PathBuf>,
    pub(super) remote: Option<(neutron_remote::Site, String)>,
}

impl file_list::IconSource for RowIcons<'_> {
    fn texture(&self) -> egui::TextureId {
        // Only constructed when the texture exists; see the call site.
        self.texture.unwrap_or_default()
    }

    fn uv_for(&self, name: &str, kind: neutron_core::EntryKind) -> Option<egui::Rect> {
        let key = crate::icon_service::key_for(self.dir.as_deref(), name, kind.is_container());
        // The slot lookup runs unconditionally — it is what registers the
        // request. Only the *answer* is withheld until there is a texture to
        // sample it from.
        let slot = self.service.slot(&key)?;
        self.texture?;
        Some(self.service.uv(slot))
    }

    fn preview(&self, name: &str, kind: neutron_core::EntryKind, modified: i64) -> Option<neutron_ui::thumb::Thumb> {
        if let Some(dir) = self.dir.as_deref() {
            return self.thumbs.get(&dir.join(name), modified, crate::thumbs::Grade::Small);
        }
        if let Some((site, dir)) = &self.remote
            && site.protocol == neutron_remote::Protocol::Sftp
            && neutron_ui::thumb::wants_preview(name, kind)
        {
            if let Ok(full) = neutron_remote::path::join(dir, name) {
                return self.thumbs.get_remote(site, &full, modified, crate::thumbs::Grade::Small);
            }
        }
        None
    }
}

/// The interface's font files: Manrope where it has been bundled, otherwise
/// the system's own face on the Mac, otherwise egui's built-in one. With
/// Manrope, the system's face stands behind it for the symbols it lacks.
///
/// Read once at start-up from the app bundle's resources, or from the source
/// tree's `assets/fonts` during development.
pub(super) fn interface_faces() -> Option<theme::Faces> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(exe) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.push(exe.join("../Resources/fonts"));
    }
    dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts"));

    let system = if cfg!(target_os = "macos") {
        std::fs::read("/System/Library/Fonts/SFNS.ttf").ok()
    } else {
        None
    };

    for dir in dirs {
        if let Ok(regular) = std::fs::read(dir.join("Manrope-Medium.ttf")) {
            let strong = std::fs::read(dir.join("Manrope-Bold.ttf")).ok();
            return Some(theme::Faces { regular, strong, fallback: system });
        }
    }

    system.map(|regular| theme::Faces { regular, strong: None, fallback: None })
}

/// The user's Trash folder.
/// How long a drag rests on a folder before the folder opens under it, and
/// how much of that the folder spends blinking to say it is about to.
pub(super) const SPRING_DELAY: Duration = Duration::from_millis(900);
pub(super) const SPRING_BLINK: Duration = Duration::from_millis(320);

/// Where the pointer is in this window, in its own points. While files are
/// dragged over it the window hears of the drag, not of the pointer moving,
/// so the system is asked directly.
pub(super) fn drop_pointer(ctx: &egui::Context) -> Option<egui::Pos2> {
    if neutron_ui::drop::active(ctx) {
        let (inner, native, ppp) = ctx.input(|i| (i.viewport().inner_rect, i.viewport().native_pixels_per_point, i.pixels_per_point));
        if let (Some([x, y]), Some(inner)) = (neutron_shell::dragout::pointer(), inner) {
            // Screen points to egui's, which differ when the interface is zoomed.
            let scale = native.unwrap_or(ppp) / ppp;
            return Some(egui::pos2(x * scale, y * scale) - inner.min.to_vec2());
        }
    }
    ctx.pointer_hover_pos()
}

pub(super) fn trash_folder() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".Trash"))
}

/// The menu wording for pinning: the Mac calls the list the sidebar.
pub(super) fn pin_label(pinned: bool, name: Option<&str>) -> String {
    let (add, remove, list) = if cfg!(target_os = "macos") {
        ("Add", "Remove", "Sidebar")
    } else {
        ("Pin", "Unpin", "Quick access")
    };
    match (pinned, name) {
        (false, Some(name)) => format!("{add} {name} to {list}"),
        (true, Some(name)) => format!("{remove} {name} from {list}"),
        (false, None) => format!("{add} to {list}"),
        (true, None) => format!("{remove} from {list}"),
    }
}

/// A folder's path as a person writes it: under the home folder, `~/…`.
pub(super) fn refresh_tab_in(workspace: &mut Workspace, loader: &mut Loader, tab: TabId) {
    let Some(t) = workspace.tabs.get_mut(&tab) else {
        return;
    };
    if t.location().is_terminal() || matches!(t.location(), NodeId::Viewer { .. }) {
        return;
    }
    let remembered = t.remember_selection();
    t.reselect = Some(remembered);

    let location = t.location().clone();
    let (sort, show_hidden) = (t.view.sort, t.view.show_hidden);

    let generation = loader.load(tab, location, sort, show_hidden);
    if let Some(t) = workspace.tabs.get_mut(&tab) {
        t.pending = Some(generation);
    }
}

pub(crate) fn tidy_location(dir: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        if let Ok(rest) = dir.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() {
                "~".to_owned()
            } else {
                format!("~/{}", rest.display())
            };
        }
    }
    dir.display().to_string()
}

pub(super) fn pos(x: f32, y: f32) -> egui::Pos2 {
    egui::pos2(x, y)
}

/// Paints a side panel's pane surface under its contents.
///
/// The panel's frame paints nothing — a frame can only fill flat — so this
/// runs first inside it, on the rectangle the frame would have filled, and
/// everything the panel draws afterwards lands on top. Clipped to the window
/// rather than to the panel, so the shadow is not cut off at the panel's edge.
#[derive(Clone, Copy)]
pub(super) enum ChromeEdge {
    Left,
    Right,
    Bottom,
}

pub(super) fn paint_chrome_panel(ui: &egui::Ui, inner: egui::Margin, edge: ChromeEdge, p: &Palette) {
    let rect = ui.max_rect() + inner;
    let screen = ui.ctx().content_rect();
    let corners = egui::CornerRadius {
        nw: if rect.left() <= screen.left() + 1.5 && rect.top() <= screen.top() + 1.5 {
            theme::RADIUS_WINDOW
        } else {
            0
        },
        ne: if rect.right() >= screen.right() - 1.5 && rect.top() <= screen.top() + 1.5 {
            theme::RADIUS_WINDOW
        } else {
            0
        },
        sw: if rect.left() <= screen.left() + 1.5 && rect.bottom() >= screen.bottom() - 1.5 {
            theme::RADIUS_WINDOW
        } else {
            0
        },
        se: if rect.right() >= screen.right() - 1.5 && rect.bottom() >= screen.bottom() - 1.5 {
            theme::RADIUS_WINDOW
        } else {
            0
        },
    };
    let _ = edge;
    let painter = ui.painter().with_clip_rect(screen);
    painter.rect_filled(rect, corners, p.sidebar);
}


// --- small pieces ----------------------------------------------------------

/// Scans for sidebar destinations on a background thread.
///
/// Results are **streamed**, not batched. Known folders resolve in
/// milliseconds; a single empty card reader can take many seconds to time out.
/// Sending one combined result at the end would hold the fast, common entries
/// hostage to the slowest device in the machine — which is exactly what the
/// first version did, leaving the sidebar empty for the better part of a minute.
pub(super) fn spawn_places_discovery(ctx: egui::Context) -> crossbeam_channel::Receiver<PlacesUpdate> {
    let (tx, rx) = crossbeam_channel::unbounded();

    std::thread::Builder::new()
        .name("neutron-places".into())
        .spawn(move || {
            if tx
                .send(PlacesUpdate::KnownFolders(
                    neutron_shell::places::known_folders(),
                ))
                .is_err()
            {
                return;
            }
            ctx.request_repaint();

            for place in neutron_shell::places::cloud_folders() {
                let _ = tx.send(PlacesUpdate::Cloud(place));
            }
            ctx.request_repaint();

            // The shell roots are constant CLSIDs, so this is free — no COM,
            // no disk. Sent first so This PC is present on the very first frame
            // the sidebar draws.
            let roots: Vec<Place> = neutron_shell::shell_ns::WELL_KNOWN
                .iter()
                .map(|(parsing, display)| Place {
                    name: (*display).to_owned(),
                    id: NodeId::shell(*parsing, *display),
                    kind: neutron_core::places::PlaceKind::Shell,
                    capacity: None,
                })
                .collect();
            if !roots.is_empty() {
                let _ = tx.send(PlacesUpdate::Shell(roots));
                ctx.request_repaint();
            }

            // Registry-only, so it lands almost immediately — well before the
            // volume scan, which is why it is sequenced here rather than last.
            let distros = neutron_shell::places::wsl_distributions();
            if !distros.is_empty() {
                let _ = tx.send(PlacesUpdate::Wsl(distros));
                ctx.request_repaint();
            }

            let scan = |tx: &crossbeam_channel::Sender<PlacesUpdate>| {
                neutron_shell::places::drives_streaming(|place| {
                    let path = place.id.as_path().map(Path::to_path_buf);
                    if tx.send(PlacesUpdate::Drive(place)).is_ok() {
                        ctx.request_repaint();
                    }
                    if let Some(path) = path.filter(|p| neutron_shell::places::ejectable(p)) {
                        let _ = tx.send(PlacesUpdate::Ejectable(path));
                    }
                });
                let _ = tx.send(PlacesUpdate::Done);
                ctx.request_repaint();
            };
            scan(&tx);

            // Then watch for disks coming and going. A look at the names in
            // /Volumes every two seconds costs nothing, and catches a stick
            // plugged in, an image mounted, or a disk ejected from the Finder
            // — none of which says anything to this process otherwise.
            if !cfg!(target_os = "macos") {
                return;
            }
            let names = || -> Vec<std::ffi::OsString> {
                let mut names: Vec<_> = std::fs::read_dir("/Volumes")
                    .map(|entries| entries.flatten().map(|e| e.file_name()).collect())
                    .unwrap_or_default();
                names.sort();
                names
            };
            let mut seen = names();
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let now = names();
                if now != seen {
                    seen = now;
                    if tx.send(PlacesUpdate::DrivesReset).is_err() {
                        return;
                    }
                    scan(&tx);
                }
            }
        })
        .expect("failed to spawn places thread");

    rx
}

/// Extracts the Win32 window handle for the DWM title-bar theme.
pub(super) fn window_handle(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}
