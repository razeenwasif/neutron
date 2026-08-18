//! Application shell: layout, navigation, and input routing.
//!
//! # Action routing
//!
//! Drawing functions never mutate application state. They take `&self`, push
//! [`Action`]s onto a list, and the frame applies them afterwards in
//! [`NeutronApp::apply`].
//!
//! This is not ceremony — egui panels take a `&mut Ui` closure, so a draw
//! function that also mutated `self` would need a mutable borrow across the
//! closure and would not compile. Collecting actions keeps every handler
//! readable and makes the set of things the UI can do an explicit enum rather
//! than scattered field writes.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use neutron_core::places::Place;
use neutron_core::{Axis, EntryList, GroupId, NodeId, SelectMode, SortColumn};
use neutron_ui::file_list::{self, FileListAction, FileListState};
use neutron_ui::theme::{self, GUTTER, Palette, ThemeMode};

use crate::header;
use crate::commands;
use crate::icon_service::IconService;
use crate::index_client::{IndexClient, IndexState};
use crate::loader::{LoadResult, Loader, TabId};
use crate::panes::{self, PaneAction, TabAction, TabDrag};
use crate::finder::{self, Finder, FinderAction, Mode};
use crate::sidebar;
use crate::startup::StartupTimer;
use crate::workspace::{Status, Workspace};

/// How long a type-ahead prefix stays active between keystrokes.
const TYPE_AHEAD_TIMEOUT: Duration = Duration::from_millis(900);

/// Key under which the workspace layout is stored between sessions.
const STORAGE_KEY: &str = "neutron.workspace";

enum Action {
    Navigate(NodeId),
    Back,
    Forward,
    Up,
    Refresh,
    Activate(usize),
    Select { idx: usize, mode: SelectMode },
    SortBy(SortColumn),
    ClearSelection,
    SelectAll,
    ToggleHidden,
    ToggleView,
    ToggleTheme,
    /// Narrow the focused tab's listing. Empty clears the filter.
    SetFilter(String),
    /// Put the caret in the focused pane's filter field.
    FocusFilter,
    /// Delete the selection. `permanent` skips the Recycle Bin.
    Delete { permanent: bool },
    /// How many tiles the grid fitted per row, reported back after layout.
    SetGridColumns { group: GroupId, columns: usize },
    /// Native shell context menu. `idx` is the row clicked, or `None` for the
    /// empty space below the rows.
    ContextMenu { idx: Option<usize>, pos: egui::Pos2 },
    /// Files dropped from another application onto a pane.
    DropFiles { group: GroupId, paths: Vec<PathBuf>, copy: bool },

    // --- finder overlay ---
    ToggleFinder(Mode),
    CloseFinder,
    SetFinderNeedle(String),
    MoveFinderCursor(isize),
    /// Activate the row at this index in the finder.
    ActivateFinderRow(usize),
    StartIndexer,
    /// Begin the Google Drive consent flow.
    ConnectDrive,
    MoveCursor { delta: isize, extend: bool },
    /// One tile left or right. Ignored outside the grid.
    MoveWithinRow { delta: isize, extend: bool },
    MoveTo { pos: usize, extend: bool },
    TypeAhead(char),

    // --- workspace ---
    FocusGroup(GroupId),
    FocusNextPane,
    SetRatio { target: GroupId, ratio: f32 },
    Split(Axis),
    NewTab(GroupId),
    SelectTab { group: GroupId, index: usize },
    CloseTab { group: GroupId, index: usize },
    CloseActiveTab,
    FocusTabIndex(usize),
    BeginTabDrag(TabDrag),
    DropTab(GroupId),
    EndTabDrag,
}

/// Incremental result from the sidebar scan.
enum PlacesUpdate {
    KnownFolders(Vec<Place>),
    Cloud(Place),
    Wsl(Vec<Place>),
    Shell(Vec<Place>),
    Drive(Place),
    /// The volume scan finished — including any devices that timed out.
    Done,
}

pub struct NeutronApp {
    theme_mode: ThemeMode,
    startup: StartupTimer,
    /// Native window handle, for the DWM title-bar theme.
    hwnd: Option<isize>,

    loader: Loader,
    workspace: Workspace,
    /// Shell COM apartments, shared by every M3 service.
    sta: neutron_shell::sta::StaPool,
    icons: IconService,

    known_folders: Vec<Place>,
    cloud: Vec<Place>,
    wsl: Vec<Place>,
    shell_roots: Vec<Place>,
    drives: Vec<Place>,
    places_rx: crossbeam_channel::Receiver<PlacesUpdate>,
    scanning_drives: bool,

    type_ahead: String,
    type_ahead_at: Option<Instant>,
    /// Tab currently being dragged, if any.
    tab_drag: Option<TabDrag>,
    /// Last title pushed to the OS, so it is only sent when it changes.
    last_title: String,
    /// This frame's atlas texture, resolved once before drawing rather than
    /// per row — uploading is `&mut`, and rows are drawn through `&self`.
    icon_texture: Option<egui::TextureId>,

    /// Kept so worker jobs can wake the event loop.
    ctx: egui::Context,
    index: IndexClient,
    finder: Finder,
    /// Tabs a finished shell operation wants re-listed. A channel rather than a
    /// direct call because the operation completes on an apartment thread,
    /// which may not touch application state.
    refresh_tx: crossbeam_channel::Sender<TabId>,
    refresh_rx: crossbeam_channel::Receiver<TabId>,
    /// Answers to "is this file actually a folder to the shell?" — asked on an
    /// apartment thread, because only the shell knows and asking blocks.
    open_as_tx: crossbeam_channel::Sender<(TabId, PathBuf, bool)>,
    open_as_rx: crossbeam_channel::Receiver<(TabId, PathBuf, bool)>,

    /// Whether Drive is configured, signed in, or unavailable. Cached because
    /// answering it reads the credential store, which is not a per-frame cost.
    drive_state: neutron_cloud::google::DriveState,
    drive_tx: crossbeam_channel::Sender<neutron_cloud::google::DriveState>,
    drive_rx: crossbeam_channel::Receiver<neutron_cloud::google::DriveState>,
}

impl NeutronApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        mut startup: StartupTimer,
        start_path: Option<String>,
    ) -> Self {
        startup.mark_gpu_ready();

        let theme_mode = ThemeMode::default();
        theme::apply(&cc.egui_ctx, theme_mode);

        let hwnd = window_handle(cc);
        if let Some(h) = hwnd {
            // Honour the OS "reduce transparency" setting rather than forcing
            // glass on users who have explicitly asked for none.
            if neutron_shell::backdrop::transparency_enabled() {
                neutron_shell::backdrop::apply_glass(h, theme_mode.is_dark());
            } else {
                neutron_shell::backdrop::disable_glass(h);
            }
        }

        // Sidebar discovery runs on a worker. Enumerating volumes touches
        // removable drives, and on a machine with an empty card reader that
        // blocks indefinitely — the first version of this called it inline and
        // the window never appeared at all.
        let places_rx = spawn_places_discovery(cc.egui_ctx.clone());

        let mut workspace = Self::initial_workspace(cc, start_path);
        let mut loader = Loader::spawn(cc.egui_ctx.clone());
        let sta = neutron_shell::sta::StaPool::spawn();
        let icons = IconService::new(sta.clone(), cc.egui_ctx.clone());
        let (refresh_tx, refresh_rx) = crossbeam_channel::unbounded();
        let (open_as_tx, open_as_rx) = crossbeam_channel::unbounded();
        let (drive_tx, drive_rx) = crossbeam_channel::unbounded();
        let drive_state = neutron_cloud::google::GoogleDrive::new().state();

        // Attaches to a helper left running by an earlier session, so search is
        // available with no prompt at all in the common case.
        let index = IndexClient::spawn(cc.egui_ctx.clone());
        index.try_attach();

        // Every restored tab needs its listing loaded, not just the visible
        // one: switching to a background tab should show its contents, not a
        // spinner and a fresh round-trip.
        let pending: Vec<(TabId, NodeId, _, bool)> = workspace
            .tabs
            .values()
            .map(|t| (t.id, t.location().clone(), t.view.sort, t.view.show_hidden))
            .collect();
        for (id, location, sort, show_hidden) in pending {
            let generation = loader.load(id, location, sort, show_hidden);
            if let Some(tab) = workspace.tabs.get_mut(&id) {
                tab.pending = Some(generation);
            }
        }

        Self {
            theme_mode,
            startup,
            hwnd,
            loader,
            workspace,
            sta,
            icons,
            known_folders: Vec::new(),
            cloud: Vec::new(),
            wsl: Vec::new(),
            shell_roots: Vec::new(),
            drives: Vec::new(),
            places_rx,
            scanning_drives: true,
            type_ahead: String::new(),
            type_ahead_at: None,
            tab_drag: None,
            last_title: String::new(),
            icon_texture: None,
            ctx: cc.egui_ctx.clone(),
            index,
            finder: Finder::default(),
            refresh_tx,
            refresh_rx,
            open_as_tx,
            open_as_rx,
            drive_state,
            drive_tx,
            drive_rx,
        }
    }

    /// An explicit path wins; otherwise restore the previous session; only then
    /// fall back to the home directory.
    fn initial_workspace(
        cc: &eframe::CreationContext<'_>,
        start_path: Option<String>,
    ) -> Workspace {
        if let Some(path) = start_path {
            return Workspace::new(NodeId::Path(PathBuf::from(path)));
        }

        let restored = cc
            .storage
            .and_then(|s| s.get_string(STORAGE_KEY))
            .and_then(|json| serde_json::from_str(&json).ok())
            .and_then(|saved| Workspace::restore(&saved));

        restored.unwrap_or_else(|| {
            Workspace::new(NodeId::Path(
                neutron_shell::places::home().unwrap_or_else(|| PathBuf::from(r"C:\")),
            ))
        })
    }

    fn palette(&self) -> Palette {
        Palette::for_mode(self.theme_mode)
    }

    fn set_theme(&mut self, ctx: &egui::Context, mode: ThemeMode) {
        self.theme_mode = mode;
        theme::apply(ctx, mode);
        if let Some(h) = self.hwnd {
            if neutron_shell::backdrop::transparency_enabled() {
                neutron_shell::backdrop::apply_glass(h, mode.is_dark());
            }
        }
    }

    /// Issues a load for `tab` without touching its history.
    fn reload(&mut self, tab: TabId, location: NodeId) {
        let Some(t) = self.workspace.tabs.get_mut(&tab) else {
            return;
        };
        t.selection.clear();
        t.status = Status::Loading;
        // The filter describes the folder being left, not the one being
        // entered. Carrying it across a navigation shows an arbitrarily empty
        // pane with no visible cause, which reads as a broken listing.
        t.view.filter.clear();
        let (sort, show_hidden) = (t.view.sort, t.view.show_hidden);

        let generation = self.loader.load(tab, location, sort, show_hidden);
        if let Some(t) = self.workspace.tabs.get_mut(&tab) {
            t.pending = Some(generation);
        }
    }

    fn navigate(&mut self, tab: TabId, id: NodeId) {
        let Some(t) = self.workspace.tabs.get_mut(&tab) else {
            return;
        };
        if t.location() == &id {
            return;
        }
        t.history.push(id.clone());
        self.reload(tab, id);
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        let active = self.workspace.active_tab_id();

        match action {
            Action::Navigate(id) => {
                if let Some(tab) = active {
                    self.navigate(tab, id);
                }
            }
            Action::Back => {
                if let Some(tab) = active {
                    let target = self
                        .workspace
                        .tabs
                        .get_mut(&tab)
                        .and_then(|t| t.history.back().cloned());
                    if let Some(id) = target {
                        self.reload(tab, id);
                    }
                }
            }
            Action::Forward => {
                if let Some(tab) = active {
                    let target = self
                        .workspace
                        .tabs
                        .get_mut(&tab)
                        .and_then(|t| t.history.forward().cloned());
                    if let Some(id) = target {
                        self.reload(tab, id);
                    }
                }
            }
            Action::Up => {
                if let Some(tab) = active {
                    let parent = self
                        .workspace
                        .tabs
                        .get(&tab)
                        .and_then(|t| t.location().parent());
                    if let Some(p) = parent {
                        self.navigate(tab, p);
                    }
                }
            }
            Action::Refresh => {
                if let Some(tab) = active {
                    if let Some(loc) = self.workspace.tabs.get(&tab).map(|t| t.location().clone()) {
                        self.reload(tab, loc);
                    }
                }
            }

            Action::Activate(idx) => self.activate(idx),

            Action::Select { idx, mode } => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.apply(&t.list, idx, mode);
                }
            }
            Action::ClearSelection => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.clear();
                }
            }
            Action::SelectAll => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.select_all(&t.list);
                }
            }

            Action::SortBy(column) => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    // Clicking the active column flips direction; a different
                    // column starts ascending, as in Explorer.
                    if t.view.sort.column == column {
                        t.view.sort.order = t.view.sort.order.flipped();
                    } else {
                        t.view.sort.column = column;
                        t.view.sort.order = neutron_core::SortOrder::Ascending;
                    }
                    // Re-sort in place. The selection holds storage indices, so
                    // it survives untouched — no reload needed.
                    neutron_core::sort::sort(&mut t.list, t.view.sort);
                    t.view.scroll_to = t.selection.cursor();
                }
            }

            Action::ToggleHidden => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.view.show_hidden = !t.view.show_hidden;
                    t.refilter();
                }
            }

            Action::ToggleView => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.view.view = t.view.view.toggled();
                    // Bring the cursor back into view: a row that was on screen
                    // in the list is on a completely different line of a grid.
                    t.view.scroll_to = t.selection.cursor();
                }
            }

            Action::SetFilter(text) => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    if t.view.filter != text {
                        t.view.filter = text;
                        // Re-filtered in place from the listing already in
                        // memory: no reload, no I/O, so this stays instant
                        // while typing even in a 100k-entry directory.
                        t.refilter();
                        // Entries the filter hid are still selected by storage
                        // index but no longer reachable; dropping the selection
                        // avoids a status bar counting rows nobody can see.
                        t.selection.clear();
                    }
                }
            }

            Action::Delete { permanent } => self.delete_selection(permanent),

            Action::SetGridColumns { group, columns } => {
                if let Some(tab) = self
                    .workspace
                    .groups
                    .get(&group)
                    .and_then(|g| g.active_tab())
                {
                    if let Some(t) = self.workspace.tabs.get_mut(&tab) {
                        t.view.columns = columns;
                    }
                }
            }

            Action::ContextMenu { idx, pos } => self.context_menu(ctx, idx, pos),

            Action::DropFiles { group, paths, copy } => self.drop_files(group, paths, copy),

            Action::ToggleFinder(mode) => {
                self.finder.toggle(mode);
                self.refresh_finder();
            }
            Action::CloseFinder => self.finder.close(),
            Action::SetFinderNeedle(text) => {
                self.finder.needle = text;
                // Reset rather than clamp: the previous highlight referred to a
                // different result set entirely.
                self.finder.cursor = 0;
                self.refresh_finder();
            }
            Action::MoveFinderCursor(delta) => self.finder.move_cursor(delta),
            Action::ActivateFinderRow(index) => self.activate_finder_row(index),
            Action::StartIndexer => self.index.start_helper(),

            Action::ConnectDrive => self.connect_drive(),

            Action::FocusFilter => {
                if let Some(group) = self.workspace.focused_group().map(|g| g.id) {
                    ctx.memory_mut(|m| m.request_focus(crate::header::filter_id(group)));
                }
            }

            Action::ToggleTheme => {
                let next = self.theme_mode.toggled();
                self.set_theme(ctx, next);
            }

            Action::MoveCursor { delta, extend } => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    // In a grid, Up and Down move by a whole row. The layout is
                    // the only thing that knows how wide a row is, so it
                    // records the column count during paint.
                    let step = if t.view.view == neutron_ui::file_list::ViewMode::Grid {
                        (t.view.columns.max(1)) as isize
                    } else {
                        1
                    };
                    t.selection.move_cursor(&t.list, delta * step, extend);
                    t.view.scroll_to = t.selection.cursor();
                }
            }
            Action::MoveWithinRow { delta, extend } => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    if t.view.view == neutron_ui::file_list::ViewMode::Grid {
                        t.selection.move_cursor(&t.list, delta, extend);
                        t.view.scroll_to = t.selection.cursor();
                    }
                }
            }

            Action::MoveTo { pos, extend } => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.move_to(&t.list, pos, extend);
                    t.view.scroll_to = t.selection.cursor();
                }
            }

            Action::TypeAhead(c) => self.type_ahead(c),

            // --- workspace ---
            Action::FocusGroup(g) => self.workspace.focused = g,
            Action::FocusNextPane => self.workspace.focus_next_pane(),
            Action::SetRatio { target, ratio } => {
                self.workspace.layout.set_ratio(target, ratio);
            }
            Action::Split(axis) => {
                tracing::debug!(?axis, panes = self.workspace.layout.len(), "split pane");
                if let Some(tab) = self.workspace.split(axis) {
                    if let Some(loc) = self.workspace.tabs.get(&tab).map(|t| t.location().clone()) {
                        self.reload(tab, loc);
                    }
                }
            }
            Action::NewTab(group) => {
                // Opens at the current pane's location rather than at home:
                // "new tab here" is what a file manager wants, and going home
                // would discard the context the user is working in.
                let location = self
                    .workspace
                    .groups
                    .get(&group)
                    .and_then(|g| g.active_tab())
                    .and_then(|t| self.workspace.tabs.get(&t))
                    .map(|t| t.location().clone())
                    .unwrap_or_else(|| {
                        NodeId::Path(
                            neutron_shell::places::home().unwrap_or_else(|| PathBuf::from(r"C:\")),
                        )
                    });
                let tab =
                    self.workspace
                        .open_tab(group, location.clone(), FileListState::default());
                self.workspace.focused = group;
                tracing::debug!(?group, ?tab, tabs = self.workspace.tabs.len(), "new tab");
                self.reload(tab, location);
            }
            Action::SelectTab { group, index } => {
                if let Some(g) = self.workspace.groups.get_mut(&group) {
                    if index < g.tabs.len() {
                        g.active = index;
                    }
                }
                self.workspace.focused = group;
            }
            Action::CloseTab { group, index } => {
                let tab = self
                    .workspace
                    .groups
                    .get(&group)
                    .and_then(|g| g.tabs.get(index).copied());
                if let Some(tab) = tab {
                    self.close_tab(ctx, tab);
                }
            }
            Action::CloseActiveTab => {
                if let Some(tab) = active {
                    self.close_tab(ctx, tab);
                }
            }
            Action::FocusTabIndex(i) => self.workspace.focus_tab_index(i),

            Action::BeginTabDrag(d) => self.tab_drag = Some(d),
            Action::EndTabDrag => self.tab_drag = None,
            Action::DropTab(target) => {
                if let Some(drag) = self.tab_drag.take() {
                    let tab = self
                        .workspace
                        .groups
                        .get(&drag.group)
                        .and_then(|g| g.tabs.get(drag.index).copied());
                    if let Some(tab) = tab {
                        self.workspace.move_tab(tab, target);
                    }
                }
            }
        }
    }

    fn close_tab(&mut self, ctx: &egui::Context, tab: TabId) {
        if !self.workspace.close_tab(tab, &mut self.loader) {
            // That was the last tab anywhere — closing it closes the window,
            // matching every other tabbed application.
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn activate(&mut self, idx: usize) {
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(t) = self.workspace.tabs.get(&tab) else {
            return;
        };
        if idx >= t.list.len() {
            return;
        }

        let kind = t.list.kind(idx);
        let name = t.list.name(idx).to_owned();

        // A shell listing tells us where each row leads, because its children
        // cannot be addressed by joining a name to the parent — "This PC"
        // contains `C:\`, and a Control Panel item has no path at all.
        if let Some((target, is_path)) = t.list.target(idx) {
            let target = target.to_owned();
            if !kind.is_container() && is_path {
                self.launch(PathBuf::from(target), neutron_shell::open::Verb::Open);
                return;
            }
            let node = if is_path {
                NodeId::Path(PathBuf::from(target))
            } else {
                NodeId::shell(target, name)
            };
            self.navigate(tab, node);
            return;
        }

        let Some(base) = t.location().as_path() else {
            return;
        };
        let full = base.join(&name);

        if kind.is_container() {
            self.navigate(tab, NodeId::Path(full));
            return;
        }

        // A zip is an ordinary file to the filesystem and a folder to the
        // shell. Only the shell knows which extensions have a namespace handler
        // registered, so it is asked rather than guessed at from a list of
        // extensions that would go stale.
        let probe = full.clone();
        let wake = self.ctx.clone();
        let open_as = self.open_as_tx.clone();
        let tab_id = tab;
        self.sta.submit(move || {
            let as_folder =
                neutron_shell::shell_ns::is_shell_container(&probe.to_string_lossy());
            let _ = open_as.send((tab_id, probe, as_folder));
            wake.request_repaint();
        });
    }

    /// Applies decisions made on an apartment thread about how to open a file.
    fn drain_open_as(&mut self) {
        let decisions: Vec<_> = self.open_as_rx.try_iter().collect();
        for (tab, path, as_folder) in decisions {
            if as_folder {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.navigate(tab, NodeId::shell(path.to_string_lossy().into_owned(), name));
            } else {
                self.launch(path, neutron_shell::open::Verb::Open);
            }
        }
    }

    /// Sends the focused tab's selection to the Recycle Bin.
    ///
    /// The listing is refreshed when the operation finishes rather than
    /// optimistically: the shell may have been refused, the user may have
    /// cancelled at the confirmation prompt, and some of a batch can fail while
    /// the rest succeeds. Re-reading the directory is the only account of what
    /// actually happened that is guaranteed to be true.
    fn delete_selection(&mut self, permanent: bool) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(tab) = self.workspace.tabs.get(&tab_id) else {
            return;
        };
        let Some(dir) = tab.location().as_path() else {
            return;
        };

        let paths: Vec<PathBuf> = tab
            .selection
            .iter()
            .filter(|&i| i < tab.list.len())
            .map(|i| dir.join(tab.list.name(i)))
            .collect();
        if paths.is_empty() {
            return;
        }

        let disposal = if permanent {
            neutron_shell::fileops::Disposal::Permanent
        } else {
            neutron_shell::fileops::Disposal::Recycle
        };
        let owner = self.hwnd.unwrap_or(0);
        let ctx = self.ctx.clone();
        let done = self.refresh_tx.clone();

        tracing::info!(count = paths.len(), ?disposal, "deleting");
        self.sta.submit(move || {
            if let Err(e) = neutron_shell::fileops::delete(&paths, disposal, owner) {
                tracing::warn!("delete failed: {e}");
            }
            // Refresh regardless of the outcome, for the reason above.
            let _ = done.send(tab_id);
            ctx.request_repaint();
        });
    }

    /// Copies or moves files dropped from another application into `group`.
    fn drop_files(&mut self, group: GroupId, paths: Vec<PathBuf>, copy: bool) {
        let Some(tab_id) = self
            .workspace
            .groups
            .get(&group)
            .and_then(|g| g.active_tab())
        else {
            return;
        };
        let Some(dir) = self
            .workspace
            .tabs
            .get(&tab_id)
            .and_then(|t| t.location().as_path())
            .map(|p| p.to_path_buf())
        else {
            return;
        };

        use neutron_shell::fileops::Transfer;
        // Ctrl forces a copy, matching Explorer. Without it the source and
        // destination volumes decide.
        let how = if copy {
            Transfer::Copy
        } else {
            neutron_shell::fileops::default_transfer(&paths[0], &dir)
        };

        let owner = self.hwnd.unwrap_or(0);
        let wake = self.ctx.clone();
        let done = self.refresh_tx.clone();

        tracing::info!(count = paths.len(), ?how, dest = %dir.display(), "drop");
        self.sta.submit(move || {
            if let Err(e) = neutron_shell::fileops::transfer(&paths, &dir, how, owner) {
                tracing::warn!("drop failed: {e}");
            }
            let _ = done.send(tab_id);
            wake.request_repaint();
        });
    }

    /// Shows the native shell context menu for the selection.
    ///
    /// Right-clicking a row that is not selected selects it first, as every
    /// file manager does — acting on a hidden selection is how people delete
    /// the wrong thing.
    fn context_menu(&mut self, ctx: &egui::Context, idx: Option<usize>, pos: egui::Pos2) {
        if let Some(idx) = idx {
            let outside = self
                .workspace
                .active_tab()
                .is_some_and(|t| !t.selection.is_selected(idx));
            if outside {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.apply(&t.list, idx, SelectMode::Replace);
                }
            }
        }

        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(tab) = self.workspace.tabs.get(&tab_id) else {
            return;
        };
        let Some(dir) = tab.location().as_path() else {
            return;
        };

        // With nothing selected the menu would be for the folder itself. That
        // needs a different IContextMenu (the background menu, with New and
        // Paste); until it exists, showing the folder's own item menu would be
        // actively misleading.
        let paths: Vec<PathBuf> = tab
            .selection
            .iter()
            .filter(|&i| i < tab.list.len())
            .map(|i| dir.join(tab.list.name(i)))
            .collect();
        if paths.is_empty() {
            return;
        }

        // egui reports points in logical units with the window's origin at
        // zero; the shell places menus in physical screen pixels.
        let scale = ctx.pixels_per_point();
        let origin = ctx
            .input(|i| i.viewport().outer_rect)
            .map(|r| r.min)
            .unwrap_or_default();
        let x = ((origin.x + pos.x) * scale).round() as i32;
        let y = ((origin.y + pos.y) * scale).round() as i32;

        let ctx_wake = self.ctx.clone();
        let done = self.refresh_tx.clone();

        self.sta.submit(move || {
            // Blocks this apartment for as long as the menu is open. The UI
            // thread keeps painting — that is the entire reason it is here.
            if let Err(e) = neutron_shell::menu::show(&paths, x, y) {
                tracing::warn!("context menu: {e}");
            }
            // The command may have renamed, deleted, or created something.
            // There is no way to know which, so re-list unconditionally.
            let _ = done.send(tab_id);
            ctx_wake.request_repaint();
        });
    }

    /// Opens `path` with the shell, on an apartment thread.
    ///
    /// Fire and forget. There is nothing useful to wait for — the launched
    /// process is not ours — and waiting is exactly what must not happen: the
    /// association lookup reads the registry, may load a shell extension, and
    /// on a network or cloud path can block for many seconds.
    fn launch(&self, path: PathBuf, verb: neutron_shell::open::Verb) {
        let owner = self.hwnd.unwrap_or(0);
        self.sta.submit(move || {
            if let Err(e) = neutron_shell::open::shell_execute(&path, verb, owner) {
                // Nothing is shown to the user yet. A toast for this belongs
                // with the rest of the error surface, which does not exist —
                // the status bar only reports the current listing.
                tracing::warn!(path = %path.display(), "{e}");
            }
        });
    }

    /// Jumps to the next entry whose name starts with the accumulated prefix.
    ///
    /// Repeating a single character cycles through matches for that letter,
    /// which is what a file list is expected to do; typing distinct characters
    /// within the timeout extends the prefix instead.
    fn type_ahead(&mut self, c: char) {
        let now = Instant::now();
        let expired = self
            .type_ahead_at
            .is_none_or(|t| now.duration_since(t) > TYPE_AHEAD_TIMEOUT);

        if expired {
            self.type_ahead.clear();
        }
        self.type_ahead.push(c.to_ascii_lowercase());
        self.type_ahead_at = Some(now);

        let first = self.type_ahead.chars().next().unwrap_or(c);
        let cycling = self.type_ahead.len() > 1 && self.type_ahead.chars().all(|ch| ch == first);
        let prefix: String = if cycling {
            first.to_string()
        } else {
            self.type_ahead.clone()
        };

        let Some(t) = self.workspace.active_tab_mut() else {
            return;
        };
        let visible = t.list.order().len();
        if visible == 0 {
            return;
        }

        // Start just past the cursor so repeats advance rather than sticking.
        let start = match t.selection.cursor().and_then(|c| t.list.rank(c)) {
            Some(pos) if cycling || expired => pos + 1,
            Some(pos) => pos,
            None => 0,
        };

        for step in 0..visible {
            let pos = (start + step) % visible;
            let idx = t.list.at(pos);
            if t.list.name(idx).to_lowercase().starts_with(&prefix) {
                t.selection.apply(&t.list, idx, SelectMode::Replace);
                t.view.scroll_to = t.selection.cursor();
                return;
            }
        }
    }
}

impl eframe::App for NeutronApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Ok(json) = serde_json::to_string(&self.workspace.persist()) {
            storage.set_string(STORAGE_KEY, json);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let frame_started = Instant::now();
        let ctx = ui.ctx().clone();
        let p = self.palette();
        let mut actions: Vec<Action> = Vec::new();

        // The ground the cards sit on: 3 slow-drifting colour fields behind the translucent glass panels.
        // Painted onto a background layer so it stays beneath every panel regardless of declaration order.
        let screen = ctx.content_rect();
        let bg = ui
            .painter()
            .clone()
            .with_layer_id(egui::LayerId::background());
        neutron_ui::ambient::paint(&bg, screen, &p, ui.ctx().input(|i| i.time));
        ctx.request_repaint_after(Duration::from_millis(33));

        self.drain_places();
        self.drain_loads(&ctx);
        self.drain_refreshes();
        self.drain_open_as();
        while let Ok(state) = self.drive_rx.try_recv() {
            self.drive_state = state;
        }
        // Before drawing: installs icons that arrived since the last frame, so
        // rows painted below pick them up immediately rather than a frame late.
        self.icons.pump();
        self.icon_texture = self.icons.texture(&ctx);
        self.index.pump();
        self.sync_finder_results();
        self.keyboard(&ctx, &mut actions);

        self.status_bar(ui, &p);
        self.sidebar(ui, &p, &mut actions);
        self.panes(ui, &p, &mut actions);

        // Drawn last so it covers everything, and after input routing so the
        // overlay's own keys win while it is open.
        self.finder_overlay(ui, &p, &mut actions);

        // Queued *after* drawing so a `DropTab` pushed by a tab strip this
        // frame is applied first. Pushed before it, this would clear the drag
        // and turn every drop into a no-op.
        if self.tab_drag.is_some() && ctx.input(|i| i.pointer.any_released()) {
            actions.push(Action::EndTabDrag);
        }

        for action in actions {
            self.apply(&ctx, action);
        }

        self.startup.mark_frame(frame_started.elapsed());
    }
}

// --- background results ----------------------------------------------------

impl NeutronApp {
    fn drain_places(&mut self) {
        // Drain everything queued: several drives can resolve between frames.
        while let Ok(update) = self.places_rx.try_recv() {
            match update {
                PlacesUpdate::KnownFolders(folders) => self.known_folders = folders,
                PlacesUpdate::Cloud(place) => self.cloud.push(place),
                PlacesUpdate::Wsl(distros) => self.wsl = distros,
                PlacesUpdate::Shell(roots) => self.shell_roots = roots,
                PlacesUpdate::Drive(place) => self.drives.push(place),
                PlacesUpdate::Done => self.scanning_drives = false,
            }
        }
    }

    /// Re-lists tabs whose contents a finished shell operation may have changed.
    fn drain_refreshes(&mut self) {
        // Collected first because `reload` borrows `self` mutably.
        let tabs: Vec<TabId> = self.refresh_rx.try_iter().collect();
        for tab in tabs {
            if let Some(loc) = self.workspace.tabs.get(&tab).map(|t| t.location().clone()) {
                self.reload(tab, loc);
            }
        }
    }

    fn drain_loads(&mut self, ctx: &egui::Context) {
        while let Some((tab_id, generation, result)) = self.loader.poll() {
            let Some(tab) = self.workspace.tabs.get_mut(&tab_id) else {
                continue;
            };
            // Final staleness guard: a response can still be in the channel
            // from a navigation the user has already left.
            if tab.pending != Some(generation) {
                continue;
            }
            tab.pending = None;

            match result {
                LoadResult::Ready(loaded) => {
                    tab.status = Status::Loaded {
                        entries: loaded.list.len(),
                        elapsed: loaded.enumerate_time + loaded.sort_time,
                    };
                    tab.list = loaded.list;
                    tab.selection.clear();
                }
                LoadResult::Failed { id, error } => {
                    tab.status = Status::Error(format!("{id}: {error}"));
                    tab.list = EntryList::new();
                    tab.selection.clear();
                }
            }
        }

        // Title tracks the focused tab, so the taskbar entry is identifiable
        // when several Neutron windows are open.
        //
        // Only sent when it actually changes. Issuing a viewport command every
        // frame keeps the event loop permanently awake — it held the app at
        // ~9% of a core while sitting completely idle.
        if let Some(t) = self.workspace.active_tab() {
            let title = format!("{} — Neutron", t.title());
            if title != self.last_title {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
                self.last_title = title;
            }
        }
    }
}

// --- input -----------------------------------------------------------------

impl NeutronApp {
    fn keyboard(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        // Skip only while a *text field* has focus, or navigation keys would
        // fight with editing. Matters as soon as the rename box and search land.
        //
        // Not `memory().focused().is_some()`: that is true whenever any widget
        // holds focus, including a button the user merely clicked, which
        // silently disabled every shortcut in the application.
        if ctx.egui_wants_keyboard_input() {
            return;
        }

        let page = file_list::rows_per_page(ctx.content_rect().height()) as isize;
        let visible = self
            .workspace
            .active_tab()
            .map(|t| t.list.order().len())
            .unwrap_or(0);

        ctx.input(|i| {
            let shift = i.modifiers.shift;
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            let alt = i.modifiers.alt;

            for event in &i.events {
                // Text events rather than key events, so keyboard layout and
                // modifiers are already resolved by the OS.
                if let egui::Event::Text(text) = event {
                    if !ctrl && !alt {
                        if let Some(c) = text.chars().next() {
                            if !c.is_control() {
                                actions.push(Action::TypeAhead(c));
                            }
                        }
                    }
                }
            }

            // Built as a table rather than dispatched through a closure: a
            // closure capturing `actions` would hold the mutable borrow for the
            // whole block, blocking the conditional pushes below it.
            let mut bindings: Vec<(egui::Key, Action)> = Vec::new();

            if alt {
                bindings.push((egui::Key::ArrowLeft, Action::Back));
                bindings.push((egui::Key::ArrowRight, Action::Forward));
                bindings.push((egui::Key::ArrowUp, Action::Up));
            } else {
                let last = visible.saturating_sub(1);
                bindings.extend([
                    (
                        egui::Key::ArrowDown,
                        Action::MoveCursor { delta: 1, extend: shift },
                    ),
                    (
                        egui::Key::ArrowUp,
                        Action::MoveCursor { delta: -1, extend: shift },
                    ),
                    // Horizontal movement only means anything in a grid; in the
                    // list it is a no-op, which is what `MoveWithin` reports.
                    (
                        egui::Key::ArrowRight,
                        Action::MoveWithinRow { delta: 1, extend: shift },
                    ),
                    (
                        egui::Key::ArrowLeft,
                        Action::MoveWithinRow { delta: -1, extend: shift },
                    ),
                    (
                        egui::Key::PageDown,
                        Action::MoveCursor { delta: page, extend: shift },
                    ),
                    (
                        egui::Key::PageUp,
                        Action::MoveCursor { delta: -page, extend: shift },
                    ),
                    (egui::Key::Home, Action::MoveTo { pos: 0, extend: shift }),
                    (egui::Key::End, Action::MoveTo { pos: last, extend: shift }),
                ]);
                // Backspace goes up a level, as in Explorer. Only without Alt,
                // so it does not double up with Alt+← above.
                bindings.push((egui::Key::Backspace, Action::Up));
            }

            bindings.push((
                egui::Key::Delete,
                // Shift is the long-standing Windows convention for bypassing
                // the Recycle Bin, and the shell still confirms it.
                Action::Delete { permanent: shift },
            ));
            bindings.push((egui::Key::F5, Action::Refresh));
            bindings.push((egui::Key::Escape, Action::ClearSelection));
            bindings.push((egui::Key::F6, Action::FocusNextPane));

            if ctrl {
                bindings.extend([
                    (egui::Key::A, Action::SelectAll),
                    (egui::Key::H, Action::ToggleHidden),
                    // Shift+L rather than plain L, which type-ahead owns.
                    (egui::Key::L, Action::ToggleView),
                    // Filters the current listing. Index-wide search takes
                    // this binding at M4; until then narrowing what is already
                    // on screen is what Ctrl+F can honestly do.
                    (
                        // Shift escalates from filtering this listing to
                        // searching every volume.
                        egui::Key::F,
                        if shift {
                            Action::ToggleFinder(Mode::Everything)
                        } else {
                            Action::FocusFilter
                        },
                    ),
                    (
                        // Shift switches from finding a file to running a
                        // command, mirroring the editor convention.
                        egui::Key::P,
                        Action::ToggleFinder(if shift {
                            Mode::Commands
                        } else {
                            Mode::Files
                        }),
                    ),
                    (egui::Key::D, Action::ToggleTheme),
                    (egui::Key::R, Action::Refresh),
                    (egui::Key::W, Action::CloseActiveTab),
                    // Backslash splits, mirroring the common terminal binding.
                    (
                        egui::Key::Backslash,
                        Action::Split(if shift { Axis::Vertical } else { Axis::Horizontal }),
                    ),
                ]);

                if let Some(group) = self.workspace.focused_group().map(|g| g.id) {
                    bindings.push((egui::Key::T, Action::NewTab(group)));
                }

                // Ctrl+1..9 jumps to a tab within the focused pane.
                for (n, key) in [
                    egui::Key::Num1,
                    egui::Key::Num2,
                    egui::Key::Num3,
                    egui::Key::Num4,
                    egui::Key::Num5,
                    egui::Key::Num6,
                    egui::Key::Num7,
                    egui::Key::Num8,
                    egui::Key::Num9,
                ]
                .into_iter()
                .enumerate()
                {
                    bindings.push((key, Action::FocusTabIndex(n)));
                }
            }

            if i.key_pressed(egui::Key::Enter) {
                if let Some(idx) = self
                    .workspace
                    .active_tab()
                    .and_then(|t| t.selection.cursor())
                {
                    actions.push(Action::Activate(idx));
                }
            }

            for (key, action) in bindings {
                if i.key_pressed(key) {
                    actions.push(action);
                }
            }
        });
    }
}

// --- chrome ----------------------------------------------------------------

impl NeutronApp {
    /// Runs the Google Drive consent flow on a worker.
    ///
    /// Opens a browser and blocks until the user finishes, so it cannot be on
    /// the paint thread — and the STA pool is where every other blocking,
    /// user-facing operation already lives.
    fn connect_drive(&mut self) {
        if matches!(self.drive_state, neutron_cloud::google::DriveState::NotConfigured) {
            tracing::warn!("no Google client id configured; not starting the flow");
            return;
        }

        self.drive_state = neutron_cloud::google::DriveState::Error("Connecting…".to_owned());
        let wake = self.ctx.clone();
        let done = self.drive_tx.clone();

        self.sta.submit(move || {
            let drive = neutron_cloud::google::GoogleDrive::new();
            let state = match drive.sign_in() {
                Ok(()) => neutron_cloud::google::DriveState::SignedIn,
                Err(e) => {
                    tracing::warn!("Google Drive sign-in failed: {e}");
                    neutron_cloud::google::DriveState::Error(e.to_string())
                }
            };
            let _ = done.send(state);
            wake.request_repaint();
        });
    }

    /// Recomputes the finder's rows for the current mode and needle.
    ///
    /// Commands are matched here and now — a few dozen entries, so a round trip
    /// to the indexer would cost more than the match. The two file modes send a
    /// request and fill in when it answers.
    fn refresh_finder(&mut self) {
        if !self.finder.open {
            return;
        }

        match self.finder.mode {
            crate::finder::Mode::Commands => {
                let matched = crate::finder::match_commands(&self.finder.needle);
                let status = format!("{} of {} commands", matched.len(), commands::ALL.len());
                self.finder
                    .set_rows(matched.into_iter().map(|(_, row)| row).collect(), status);
            }

            crate::finder::Mode::Files => {
                let Some(scope) = self
                    .workspace
                    .active_tab()
                    .and_then(|t| t.location().as_path())
                    .map(|p| p.display().to_string())
                else {
                    self.finder
                        .set_rows(Vec::new(), "This pane has no folder to search".into());
                    return;
                };

                match self.index.state() {
                    IndexState::Ready(_) => self.index.find(&self.finder.needle, &scope),
                    // A helper from an older build answers requests it knows
                    // and refuses the rest, so say which rather than claiming
                    // there is no index at all.
                    IndexState::Rejected(why) => {
                        let why = why.clone();
                        self.finder.set_rows(Vec::new(), why);
                    }
                    IndexState::Starting => {
                        self.finder.set_rows(Vec::new(), "Indexing…".into());
                    }
                    IndexState::Unavailable(why) => {
                        let why = why.clone();
                        self.finder.set_rows(Vec::new(), why);
                    }
                    IndexState::Idle => self.finder.set_rows(
                        Vec::new(),
                        "Needs the search index — press Enter to set it up".into(),
                    ),
                }
            }

            crate::finder::Mode::Everything => {
                match self.index.state() {
                    IndexState::Ready(_) => self.index.search(&self.finder.needle),
                    IndexState::Rejected(why) | IndexState::Unavailable(why) => {
                        let why = why.clone();
                        self.finder.set_rows(Vec::new(), why);
                    }
                    IndexState::Starting => {
                        self.finder
                            .set_rows(Vec::new(), "Indexing every volume — this happens once".into());
                    }
                    IndexState::Idle => self.finder.set_rows(
                        Vec::new(),
                        "Press Enter to start the indexer (requires administrator)".into(),
                    ),
                }
            }
        }
    }

    /// Copies whatever the indexer last returned into the finder's rows.
    ///
    /// Called every frame rather than on arrival, because the client owns the
    /// staleness check — it drops answers to queries the user has moved past,
    /// and this must not reintroduce them.
    fn sync_finder_results(&mut self) {
        if !self.finder.open || self.finder.mode == crate::finder::Mode::Commands {
            return;
        }

        match self.index.results() {
            Some(results) => {
                let rows: Vec<crate::finder::Row> = results
                    .hits
                    .iter()
                    .map(|hit| crate::finder::Row {
                        primary: hit.name.clone(),
                        secondary: hit.parent.clone(),
                        matched: hit.matched.clone(),
                        is_dir: hit.is_dir,
                    })
                    .collect();

                let status = format!(
                    "{}{} results in {:.2} ms",
                    results.total,
                    if results.truncated { "+" } else { "" },
                    results.elapsed_micros as f64 / 1000.0,
                );
                if rows != self.finder.rows {
                    self.finder.set_rows(rows, status);
                } else {
                    self.finder.status = status;
                }
            }
            None if self.finder.needle.is_empty() => {}
            None => {}
        }
    }

    /// Opens the selected finder row, or runs the selected command.
    fn activate_finder_row(&mut self, index: usize) {
        let mode = self.finder.mode;
        let Some(row) = self.finder.rows.get(index).cloned() else {
            return;
        };

        if mode == crate::finder::Mode::Commands {
            // Matched by title: the row carries no id, and the catalogue is the
            // only place ids live.
            if let Some(command) = commands::ALL.iter().find(|c| c.title == row.primary) {
                let id = command.id;
                self.finder.close();
                let ctx = self.ctx.clone();
                self.run_command(&ctx, id);
            }
            return;
        }

        self.finder.close();

        // A folder result opens the folder itself; a file result opens the
        // folder containing it, since there is nowhere else to put the user.
        let target = if row.is_dir {
            PathBuf::from(&row.secondary).join(&row.primary)
        } else {
            PathBuf::from(&row.secondary)
        };
        if let Some(tab) = self.workspace.active_tab_id() {
            self.navigate(tab, NodeId::Path(target));
        }
    }

    /// Runs a palette command by turning it back into an ordinary action.
    ///
    /// Deliberately routed through the same actions the keyboard uses, so a
    /// command and its shortcut can never drift apart.
    fn run_command(&mut self, ctx: &egui::Context, id: commands::CommandId) {
        use commands::CommandId as C;

        let known_folder = |name: &str| -> Option<NodeId> {
            self.known_folders
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.id.clone())
        };

        let action = match id {
            C::NewTab => self
                .workspace
                .focused_group()
                .map(|g| g.id)
                .map(Action::NewTab),
            C::CloseTab => Some(Action::CloseActiveTab),
            C::SplitRight => Some(Action::Split(Axis::Horizontal)),
            C::SplitDown => Some(Action::Split(Axis::Vertical)),
            C::FocusNextPane => Some(Action::FocusNextPane),

            C::GoUp => Some(Action::Up),
            C::GoBack => Some(Action::Back),
            C::GoForward => Some(Action::Forward),
            C::Refresh => Some(Action::Refresh),

            C::SelectAll => Some(Action::SelectAll),
            C::ClearSelection => Some(Action::ClearSelection),
            C::DeleteSelection => Some(Action::Delete { permanent: false }),

            C::ToggleHidden => Some(Action::ToggleHidden),
            C::ToggleView => Some(Action::ToggleView),
            C::ToggleTheme => Some(Action::ToggleTheme),

            C::FindInFolder => Some(Action::ToggleFinder(crate::finder::Mode::Files)),
            C::SearchEverything => Some(Action::ToggleFinder(crate::finder::Mode::Everything)),
            C::StopIndexer => {
                self.index.stop_server();
                None
            }

            C::GoHome => known_folder("Home").map(Action::Navigate),
            C::GoDesktop => known_folder("Desktop").map(Action::Navigate),
            C::GoDownloads => known_folder("Downloads").map(Action::Navigate),
            C::GoDocuments => known_folder("Documents").map(Action::Navigate),
        };

        if let Some(action) = action {
            self.apply(ctx, action);
        }
    }

    /// The finder overlay, plus its text field and key handling.
    ///
    /// The field lives here rather than in `finder.rs` because the string it
    /// edits belongs to the app: drawing modules take `&self` like every other
    /// one, and a `TextEdit` needs `&mut String`.
    fn finder_overlay(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        if !self.finder.open {
            return;
        }

        if let Some(action) = finder::show(ui, p, &self.finder) {
            actions.push(match action {
                FinderAction::Close => Action::CloseFinder,
                FinderAction::Activate(i) => Action::ActivateFinderRow(i),
            });
        }

        let screen = ui.ctx().content_rect();
        let field = finder::field_rect(screen, self.finder.rows.len());
        let id = egui::Id::new("neutron-finder-field");

        let mut text = self.finder.needle.clone();
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(field)
                .layer_id(egui::LayerId::new(egui::Order::Foreground, "finder".into())),
        );

        let output = egui::TextEdit::singleline(&mut text)
            .id(id)
            .background_color(egui::Color32::TRANSPARENT)
            .desired_width(field.width())
            .font(egui::FontId::proportional(16.0))
            .text_color(p.text)
            .hint_text(
                egui::RichText::new(self.finder.mode.placeholder())
                    .color(p.text_faint)
                    .size(16.0),
            )
            .show(&mut child);

        // The overlay opens on a keystroke, so the caret has to arrive without
        // a click. Requested every frame while open rather than once: egui
        // surrenders focus when the widget is rebuilt, which it is each frame.
        if !output.response.has_focus() {
            child.memory_mut(|m| m.request_focus(id));
        }

        if text != self.finder.needle {
            actions.push(Action::SetFinderNeedle(text));
        }

        // Handled here, not in the main key table: that table is skipped while
        // a text field has focus, which is the whole time the overlay is open.
        child.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                actions.push(Action::CloseFinder);
            }
            if i.key_pressed(egui::Key::ArrowDown) {
                actions.push(Action::MoveFinderCursor(1));
            }
            if i.key_pressed(egui::Key::ArrowUp) {
                actions.push(Action::MoveFinderCursor(-1));
            }
            if i.key_pressed(egui::Key::Enter) {
                // With no index and nothing to show, Enter is the one control
                // that can set search up — otherwise it would do nothing at all
                // on the screen that explains why there is nothing.
                if self.finder.rows.is_empty()
                    && matches!(self.index.state(), IndexState::Idle)
                {
                    actions.push(Action::StartIndexer);
                } else {
                    actions.push(Action::ActivateFinderRow(self.finder.cursor));
                }
            }
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui, p: &Palette) {
        let tab = self.workspace.active_tab();

        egui::Panel::bottom("status_bar")
            .exact_size(30.0)
            .frame(toolbar_frame(p))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let Some(t) = tab else { return };

                    match &t.status {
                        Status::Loading => {
                            ui.add(egui::Spinner::new().size(12.0));
                            ui.colored_label(p.text_muted, "Loading…");
                        }
                        Status::Error(e) => {
                            ui.colored_label(p.danger, format!("⚠ {e}"));
                        }
                        Status::Loaded { entries, .. } => {
                            let shown = t.list.order().len();
                            ui.colored_label(
                                p.text_muted,
                                if shown == *entries {
                                    format!("{shown} items")
                                } else {
                                    // Say so explicitly, or a filtered view
                                    // looks like a folder that lost files.
                                    format!("{shown} of {entries} items")
                                },
                            );
                        }
                    }

                    if !t.selection.is_empty() {
                        ui.colored_label(p.text_faint, "•");
                        let n = t.selection.len();
                        let bytes = t.selection.total_size(&t.list);
                        let size = neutron_ui::format::size(Some(bytes));
                        ui.colored_label(
                            p.accent,
                            if bytes > 0 {
                                format!("{n} selected ({size})")
                            } else {
                                format!("{n} selected")
                            },
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Status::Loaded { elapsed, .. } = &t.status {
                            ui.colored_label(
                                p.text_faint,
                                format!("{:.0}ms", elapsed.as_secs_f64() * 1000.0),
                            )
                            .on_hover_text("Enumerate + sort time");
                        }
                        // Frame pacing, shown only when it breaches the budget.
                        // A permanent counter would be clutter; one that appears
                        // when p99 exceeds 8ms is a warning.
                        if self.icons.resolved() > 0 {
                            ui.colored_label(
                                p.text_faint,
                                format!("{} icons", self.icons.resolved()),
                            )
                            .on_hover_text(
                                "Distinct system icons resolved and cached in the atlas",
                            );
                        }
                        if let Some((p50, p99)) = self.startup.frame_percentiles() {
                            if p99 > 8.0 {
                                ui.colored_label(p.warning, format!("{p99:.0}ms p99"))
                                    .on_hover_text(format!(
                                        "Frame build p50 {p50:.1}ms / p99 {p99:.1}ms — over the 8ms budget"
                                    ));
                            }
                        }
                    });
                });
            });
    }

    fn sidebar(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        let current = self.workspace.active_tab().map(|t| t.location().clone());

        egui::Panel::left("sidebar")
            .default_size(232.0)
            .size_range(egui::Rangef::new(180.0, 420.0))
            .frame(
                // A card like the panes, with the gutter carried on its outer
                // margin so it lines up with them on every edge.
                theme::card(p)
                    .outer_margin(egui::Margin {
                        left: GUTTER as i8,
                        right: 0,
                        top: GUTTER as i8,
                        bottom: GUTTER as i8,
                    })
                    .inner_margin(egui::Margin::symmetric(10, 12)),
            )
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());

                // The panes paint their own card and so get the glass edges
                // from `draw_card`; the sidebar is an `egui::Frame`, which
                // draws only fill and stroke. Without this it was the one
                // panel on screen that was translucent but not glass.
                theme::glass_highlight(
                    ui.painter(),
                    ui.max_rect().expand2(egui::vec2(10.0, 12.0)),
                    egui::CornerRadius::same(theme::RADIUS_CARD),
                );

                sidebar::brand(ui, p);
                ui.add_space(6.0);

                // The footer is carved out of the bottom of the card before the
                // list is drawn, so the storage panel stays pinned there rather
                // than scrolling away with the destinations above it.
                let body = ui.available_rect_before_wrap();
                let footer_top = body.bottom() - sidebar::footer_height();
                let scroll_area = egui::Rect::from_min_max(
                    body.min,
                    egui::pos2(body.right(), footer_top - 8.0),
                );

                let mut list = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(scroll_area)
                        .id_salt("sidebar-list"),
                );
                list.set_clip_rect(scroll_area);
                self.sidebar_places(&mut list, p, current.as_ref(), actions);

                let footer = egui::Rect::from_min_max(
                    egui::pos2(body.left(), footer_top),
                    body.max,
                );
                if sidebar::footer(ui, p, footer, &self.drives, self.theme_mode) {
                    actions.push(Action::ToggleTheme);
                }
            });
    }

    /// The scrolling groups of destinations.
    fn sidebar_places(
        &self,
        ui: &mut egui::Ui,
        p: &Palette,
        current: Option<&NodeId>,
        actions: &mut Vec<Action>,
    ) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("sidebar_scroll")
            .show(ui, |ui| {
                sidebar::section(ui, p, "Quick access");
                for place in &self.known_folders {
                    if sidebar::row(ui, p, place, current) {
                        actions.push(Action::Navigate(place.id.clone()));
                    }
                }

                if !self.shell_roots.is_empty() {
                    sidebar::section(ui, p, "System");
                    for place in &self.shell_roots {
                        if sidebar::row(ui, p, place, current) {
                            actions.push(Action::Navigate(place.id.clone()));
                        }
                    }
                }

                sidebar::section(ui, p, "Cloud");
                for place in &self.cloud {
                    if sidebar::row(ui, p, place, current) {
                        actions.push(Action::Navigate(place.id.clone()));
                    }
                }
                use neutron_cloud::google::DriveState;
                match &self.drive_state {
                    DriveState::SignedIn => {
                        let place = neutron_core::places::Place {
                            name: "Google Drive".to_owned(),
                            id: NodeId::Cloud {
                                provider: neutron_core::namespace::CloudProviderId::GoogleDrive,
                                id: neutron_cloud::drive::ROOT_ID.into(),
                            },
                            kind: neutron_core::places::PlaceKind::Cloud,
                            capacity: None,
                        };
                        if sidebar::row(ui, p, &place, current) {
                            actions.push(Action::Navigate(place.id.clone()));
                        }
                    }
                    DriveState::SignedOut => {
                        if sidebar::action_row(
                            ui,
                            p,
                            "Connect Google Drive",
                            neutron_ui::icons::Glyph::Cloud,
                        ) {
                            actions.push(Action::ConnectDrive);
                        }
                    }
                    DriveState::NotConfigured => sidebar::placeholder_row(
                        ui,
                        p,
                        "Google Drive",
                        neutron_ui::icons::Glyph::Cloud,
                        "Set NEUTRON_GOOGLE_CLIENT_ID to enable Drive",
                    ),
                    DriveState::Error(why) => sidebar::placeholder_row(
                        ui,
                        p,
                        "Google Drive",
                        neutron_ui::icons::Glyph::Cloud,
                        why,
                    ),
                }

                // Only shown when WSL is actually installed. An empty "Linux"
                // heading on a machine without it is a permanent reminder of a
                // feature the user does not have.
                if !self.wsl.is_empty() {
                    sidebar::section(ui, p, "Linux");
                    for place in &self.wsl {
                        if sidebar::row(ui, p, place, current) {
                            actions.push(Action::Navigate(place.id.clone()));
                        }
                    }
                }

                sidebar::section(ui, p, "Drives");
                for drive in &self.drives {
                    if sidebar::row(ui, p, drive, current) {
                        actions.push(Action::Navigate(drive.id.clone()));
                    }
                }
                // Below the drives found so far, not above them: the slow
                // devices are the ones still outstanding.
                if self.scanning_drives {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.add(egui::Spinner::new().size(10.0));
                        ui.colored_label(p.text_faint, "Scanning…");
                    });
                }
                ui.add_space(8.0);
            });
    }

    /// The pane tree fills whatever the chrome panels left over.
    fn panes(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
            .show(ui, |ui| {
                // Half a gutter here plus half on each leaf gives a full
                // gutter at the window edge and between adjacent cards alike.
                let rect = ui.available_rect_before_wrap().shrink(GUTTER / 2.0);
                let mut pane_actions = Vec::new();
                let mut group_actions = Vec::new();

                panes::show(
                    ui,
                    &self.workspace.layout,
                    rect,
                    p,
                    self.workspace.focused,
                    &mut pane_actions,
                    &mut |ui, group_id, is_focused| {
                        self.draw_group(ui, p, group_id, is_focused, &mut group_actions);
                    },
                );

                actions.append(&mut group_actions);
                for a in pane_actions {
                    actions.push(match a {
                        PaneAction::Focus(g) => Action::FocusGroup(g),
                        PaneAction::SetRatio { target, ratio } => Action::SetRatio { target, ratio },
                    });
                }
            });
    }

    /// One pane: tab strip, then header, then listing.
    fn draw_group(
        &self,
        ui: &mut egui::Ui,
        p: &Palette,
        group_id: GroupId,
        is_focused: bool,
        actions: &mut Vec<Action>,
    ) {
        let Some(group) = self.workspace.groups.get(&group_id) else {
            return;
        };

        let titles: Vec<String> = group
            .tabs
            .iter()
            .filter_map(|t| self.workspace.tabs.get(t))
            .map(|t| t.title())
            .collect();

        if let Some(a) = panes::tab_strip(
            ui,
            group_id,
            &titles,
            group.active,
            is_focused,
            self.tab_drag,
            p,
        ) {
            actions.push(match a {
                TabAction::Select(index) => Action::SelectTab {
                    group: group_id,
                    index,
                },
                TabAction::Close(index) => Action::CloseTab {
                    group: group_id,
                    index,
                },
                TabAction::New => Action::NewTab(group_id),
                TabAction::BeginDrag(index) => Action::BeginTabDrag(TabDrag {
                    group: group_id,
                    index,
                }),
                TabAction::DropHere => Action::DropTab(group_id),
            });
        }

        let Some(tab) = group.active_tab().and_then(|t| self.workspace.tabs.get(&t)) else {
            return;
        };

        // Header first, so navigation stays available even on a location that
        // failed to open — otherwise a bad path is a dead end with no way back.
        let (shown, total) = tab.counts();
        let head = header::Header {
            location: tab.location(),
            can_back: tab.history.can_go_back(),
            can_forward: tab.history.can_go_forward(),
            has_parent: tab.location().parent().is_some(),
            show_hidden: tab.view.show_hidden,
            grid: tab.view.view == neutron_ui::file_list::ViewMode::Grid,
            filter: &tab.view.filter,
            shown,
            total,
        };
        if let Some(a) = header::show(ui, p, group_id, &head) {
            // Every header control acts on its own pane, so using one focuses
            // it — otherwise Back would apply to a different pane than the one
            // whose arrow was clicked.
            if group_id != self.workspace.focused {
                actions.push(Action::FocusGroup(group_id));
            }
            actions.push(match a {
                header::HeaderAction::Back => Action::Back,
                header::HeaderAction::Forward => Action::Forward,
                header::HeaderAction::Up => Action::Up,
                header::HeaderAction::Navigate(id) => Action::Navigate(id),
                header::HeaderAction::SetFilter(text) => Action::SetFilter(text),
                header::HeaderAction::ToggleHidden => Action::ToggleHidden,
                header::HeaderAction::ToggleView => Action::ToggleView,
                header::HeaderAction::Split(axis) => Action::Split(axis),
            });
        }

        // Files dragged in from another application. winit owns the window's
        // IDropTarget, so this arrives through egui as plain paths rather than
        // through an IDropTarget of our own — enough to act on, though it means
        // no drop-effect feedback in the source application.
        //
        // Handled here rather than centrally because the pane under the pointer
        // is the destination, and only that pane can answer which one it is.
        let pane_rect = ui.min_rect().union(ui.max_rect());
        let pointer_here = ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|p| pane_rect.contains(p));

        if pointer_here {
            let (hovering, dropped, ctrl) = ui.ctx().input(|i| {
                (
                    !i.raw.hovered_files.is_empty(),
                    i.raw
                        .dropped_files
                        .iter()
                        .map(|f| f.path().to_path_buf())
                        .collect::<Vec<_>>(),
                    i.modifiers.ctrl,
                )
            });

            if hovering {
                // The only place the accent outlines a whole pane, which is
                // what makes "this one" unmistakable while dragging.
                ui.painter().rect_stroke(
                    pane_rect.shrink(2.0),
                    theme::RADIUS_CARD as f32,
                    egui::Stroke::new(2.0, p.accent),
                    egui::StrokeKind::Inside,
                );
            }

            if !dropped.is_empty() {
                actions.push(Action::DropFiles {
                    group: group_id,
                    paths: dropped,
                    copy: ctrl,
                });
            }
        }

        if let Status::Error(e) = &tab.status {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.colored_label(p.danger, "Could not open this location");
                ui.colored_label(p.text_muted, e.as_str());
            });
            return;
        }

        // The list widget needs `&mut FileListState` to consume `scroll_to` and
        // record the scroll offset, but this method holds only `&self`. It is
        // drawn from a clone; the scroll offset is re-derived next frame and
        // `scroll_to` is re-issued by whichever action moved the cursor, so
        // nothing durable is lost.
        let mut view = tab.view.clone();
        // Always supplied, even before any icon exists. Gating this on the
        // texture being present deadlocks the whole service: rows only ask for
        // icons through the source, so no source means no requests, which means
        // no icons, which means no texture — forever. `uv_for` returns `None`
        // until a slot is genuinely ready, and a ready slot implies the atlas
        // is non-empty and was uploaded by `pump` earlier this frame.
        let source = RowIcons {
            service: &self.icons,
            texture: self.icon_texture,
            dir: tab.location().as_path().map(|p| p.to_path_buf()),
        };

        let drawn = file_list::show(
            ui,
            &mut view,
            &tab.list,
            &tab.selection,
            p,
            Some(&source as &dyn file_list::IconSource),
        );

        // The grid works out its own column count from the pane width, and
        // that number is what Up and Down have to step by. It is computed on a
        // *clone* of the view state — this method holds only `&self` — so
        // without reporting it back the count stayed zero and the arrow keys
        // moved one tile at a time instead of one row.
        if view.columns != tab.view.columns {
            actions.push(Action::SetGridColumns {
                group: group_id,
                columns: view.columns,
            });
        }

        if let Some(a) = drawn {
            // Interacting with a pane also focuses it, so keystrokes go where
            // the user just clicked.
            if group_id != self.workspace.focused {
                actions.push(Action::FocusGroup(group_id));
            }
            match a {
                FileListAction::Activate(idx) => actions.push(Action::Activate(idx)),
                FileListAction::Select { idx, mode } => actions.push(Action::Select { idx, mode }),
                FileListAction::SortBy(col) => actions.push(Action::SortBy(col)),
                FileListAction::ClearSelection => actions.push(Action::ClearSelection),
                FileListAction::ContextMenu { idx, pos } => {
                    actions.push(Action::ContextMenu { idx, pos })
                }
            }
        }
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
struct RowIcons<'a> {
    service: &'a IconService,
    texture: Option<egui::TextureId>,
    dir: Option<PathBuf>,
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
}

/// Chrome bars sit directly on the ground with no fill of their own.
///
/// Filled toolbar strips spanning the full window width are the most dated
/// element a desktop app can have. Padding and the cards below provide all the
/// separation needed.
fn toolbar_frame(_p: &Palette) -> egui::Frame {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(GUTTER as i8 + 4, 4))
}

// --- small pieces ----------------------------------------------------------

/// Scans for sidebar destinations on a background thread.
///
/// Results are **streamed**, not batched. Known folders resolve in
/// milliseconds; a single empty card reader can take many seconds to time out.
/// Sending one combined result at the end would hold the fast, common entries
/// hostage to the slowest device in the machine — which is exactly what the
/// first version did, leaving the sidebar empty for the better part of a minute.
fn spawn_places_discovery(ctx: egui::Context) -> crossbeam_channel::Receiver<PlacesUpdate> {
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

            neutron_shell::places::drives_streaming(|place| {
                if tx.send(PlacesUpdate::Drive(place)).is_ok() {
                    ctx.request_repaint();
                }
            });

            let _ = tx.send(PlacesUpdate::Done);
            ctx.request_repaint();
        })
        .expect("failed to spawn places thread");

    rx
}

/// Extracts the Win32 window handle for the DWM title-bar theme.
fn window_handle(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}
