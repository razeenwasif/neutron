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

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use neutron_core::places::Place;
use neutron_core::namespace::CloudProviderId;
use neutron_core::{Axis, EntryList, GroupId, NodeId, SelectMode, SortColumn};
use neutron_ui::file_list::{self, FileListAction};
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
    /// A rubber band covered these display positions.
    Marquee { from: usize, to: usize, additive: bool },
    SelectAll,
    ToggleHidden,
    ToggleView,
    ToggleTheme,
    /// Narrow the focused tab's listing. Empty clears the filter.
    SetFilter(String),
    /// Put the caret in the focused pane's filter field.
    FocusFilter,
    /// Drag the selection out to another application.
    DragOut,
    /// Start renaming the focused tab's cursor row.
    BeginRename,
    /// The rename box's contents changed.
    RenameTyped(String),
    /// Apply the typed name, or do nothing if it is unchanged or invalid.
    RenameCommit,
    /// Abandon the rename.
    RenameCancel,
    /// Make a folder in the focused tab, then rename it.
    NewFolder,
    /// Rebuild the search index from the volumes, which needs administrator
    /// rights.
    RefreshIndex,
    /// Show or hide the preview pane.
    TogglePreview,
    /// Unpack the selected archive into a folder beside it.
    ExtractSelection,
    /// Zip the selection into a new archive beside it.
    CompressSelection,
    /// Stop the archive job that is running.
    CancelArchiveJob,
    /// Stop the plugin command that is running.
    CancelPluginCommand,

    // --- saved servers ---
    /// Show the "Connect to a server" dialog.
    OpenConnect,
    CloseConnect,
    SetConnectField(crate::connect::Field, String),
    SetConnectProtocol(neutron_remote::Protocol),
    /// Save the dialog's server and open it.
    SubmitConnect,
    /// Re-read `sites.json`.
    ReloadSites,
    /// Stop the download that is running.
    CancelDownload,
    /// Clear the status line.
    DismissNotice,
    /// Open a terminal at the focused tab's folder.
    OpenTerminal,
    /// Open another Neutron window on the focused tab's folder.
    NewWindow,
    /// Re-read the plugins folder.
    ReloadPlugins,
    /// Put the selection on the clipboard. `cut` marks it for a move.
    Clip { cut: bool },
    /// Paste whatever is on the clipboard into the focused tab's folder.
    Paste,
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
    TogglePin,
    /// Open the address bar on the focused pane, or close it if it is open.
    ToggleAddress,
    /// Open a file from the dashboard's recent rail.
    OpenRecent(PathBuf),
    /// Drop one from the recent list.
    ForgetRecent(PathBuf),
    SetAddress(String),
    SubmitAddress,
    CancelAddress,
    /// Ask first, then run the boxed action. See [`crate::confirm`] for the
    /// short list of things that earn one.
    Ask(Box<Action>),
    /// Right-clicked a sidebar row: open Neutron's own menu for that place.
    SidebarMenu(NodeId, egui::Pos2),
    SignOutDrive,
    ForgetServer,
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

/// Sends a failure from a worker to the one status line.
///
/// A free function rather than a method: it is called from inside the closures
/// handed to the apartment pool, which may not touch application state at all.
/// A disconnected channel means the window has gone, so the log is the only
/// place left for it — which is where it goes.
/// Menu id for Neutron's own context-menu rows.
///
/// Three id spaces share this menu and have to stay distinguishable by value
/// alone: the shell's `1..=0x7FFF`, Neutron's here, and the plugins' from
/// `plugins::MENU_ID_BASE` upward. That is what stops one of ours being handed
/// to `IContextMenu::InvokeCommand`, where it would invoke an unrelated verb.
const MENU_ID_PIN: u32 = 0x0800_0001;
const MENU_ID_NEW_TAB: u32 = 0x0800_0002;
const _: () = assert!(MENU_ID_PIN > 0x7FFF);
const _: () = assert!(MENU_ID_NEW_TAB > 0x7FFF);
const _: () = assert!(MENU_ID_PIN < crate::plugins::MENU_ID_BASE);
const _: () = assert!(MENU_ID_NEW_TAB < crate::plugins::MENU_ID_BASE);

fn report(to: &crossbeam_channel::Sender<(crate::notice::Level, String)>, text: String) {
    tracing::warn!("{text}");
    let _ = to.send((crate::notice::Level::Error, text));
}

/// The same, for a receipt rather than a failure. Clears itself after a few
/// seconds; see `notice.rs` for why the two are not the same thing.
fn report_info(to: &crossbeam_channel::Sender<(crate::notice::Level, String)>, text: String) {
    tracing::info!("{text}");
    let _ = to.send((crate::notice::Level::Info, text));
}

/// A remote file being fetched so it can be opened.
struct DownloadJob {
    /// "Fetching notes.txt", for the status bar.
    label: String,
    /// Bytes written so far, updated by the worker.
    bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    done: crossbeam_channel::Receiver<Result<PathBuf, String>>,
}

/// Where a piece of work on `sites.json` came from.
///
/// Only the connect dialog needs the answer routed back to it: the typed
/// password exists nowhere but that dialog, so a failed save has to leave it
/// open rather than closing it and losing what was typed.
enum SitesOrigin {
    /// Work that has already said what it did. Only the sidebar is waiting.
    Background,
    /// **Reload saved servers**, which owes the user a receipt saying how many
    /// there turned out to be — a reload that says nothing is one you run
    /// twice because you cannot tell whether it ran.
    Requested,
    /// The dialog's save, and whether it worked.
    Dialog(Result<(), String>),
}

/// What a worker did to `sites.json`, and what the UI should do about it.
struct SitesReply {
    /// The file as it now reads. Re-read on the worker rather than trusting a
    /// copy held here: the loader records SSH host keys into that same file,
    /// so the disk is the truth.
    sites: Result<neutron_remote::Sites, String>,
    origin: SitesOrigin,
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
    /// Folders pinned to Quick access. Read once at start-up and kept here;
    /// the file is only rewritten when one is added or removed.
    pins: crate::pins::Pins,
    /// What the open menu's Neutron-owned rows act on, decided when the menu
    /// was built rather than when a row is chosen: by then the selection may
    /// have moved.
    pin_target: Option<PathBuf>,
    tab_target: Option<NodeId>,
    /// Where the maximise button is, shared with the window procedure so
    /// Windows can offer Snap Layouts over it.
    snap: std::sync::Arc<neutron_shell::snaplayout::MaximiseButton>,
    /// A question waiting for an answer, and the action to run on yes.
    ///
    /// One at a time: two modals stacked on each other is a state nobody can
    /// reason about, and the second question is always about something the
    /// first one may be about to change.
    confirm: Option<(crate::confirm::Confirm, Box<Action>)>,
    /// The pane whose address bar is open, and what has been typed into it.
    ///
    /// One at a time, keyed by pane: two panes each showing a path field over
    /// their breadcrumb would be two answers to "where am I" on screen at once.
    address: Option<(GroupId, String)>,
    /// Files opened lately, for the dashboard.
    recent: crate::recent::Recent,
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
    /// Somewhere a worker decided the pane should go, once it had done the
    /// filesystem work needed to know that. Navigating before the work is
    /// finished would list a folder that does not exist yet.
    goto_tx: crossbeam_channel::Sender<(TabId, NodeId)>,
    goto_rx: crossbeam_channel::Receiver<(TabId, NodeId)>,
    /// `sites.json` as a worker last read it. Every change to that file is a
    /// write to `%APPDATA%` and a call into Credential Manager, neither of
    /// which belongs on the thread that paints.
    sites_tx: crossbeam_channel::Sender<SitesReply>,
    sites_rx: crossbeam_channel::Receiver<SitesReply>,

    /// The one line that says what just happened. See [`crate::notice`].
    notices: crate::notice::Notices,
    /// Failures reported from workers, which may not touch application state.
    ///
    /// Almost every file operation runs on an apartment thread, so almost every
    /// failure arrives from one. Without this they could only be logged, which
    /// is where they all used to go.
    notice_tx: crossbeam_channel::Sender<(crate::notice::Level, String)>,
    notice_rx: crossbeam_channel::Receiver<(crate::notice::Level, String)>,

    /// Saved servers, for the sidebar and the dialog. The live connections
    /// live on the loader thread; this is only the list.
    sites: neutron_remote::Sites,
    /// Why `sites.json` would not load, if it would not.
    sites_error: Option<String>,
    /// The "Connect to a server" dialog.
    connect: crate::connect::ConnectDialog,
    /// A remote file being fetched so it can be opened.
    download: Option<DownloadJob>,

    /// Installed plugins, and the command running from one if any.
    plugins: crate::plugins::PluginRuntime,
    /// The palette's list: built-in commands plus whatever the plugins declared.
    ///
    /// Built once rather than per keystroke, and rebuilt when the plugin set is
    /// re-read. The built-in half is `&'static` data being copied into owned
    /// strings, which is the price of the two sources sharing one list.
    palette: Vec<commands::Entry>,

    /// The archive job that is running, if any, and what it last reported.
    archive_job: Option<crate::archive_ops::ArchiveJob>,

    /// Directory watchers, one per distinct folder on screen.
    ///
    /// Keyed by path rather than by tab: a split showing the same folder twice
    /// needs one watcher, not two, and a tab that navigates away should not
    /// stop a watch another pane still wants.
    watchers: std::collections::HashMap<PathBuf, neutron_shell::watch::Watcher>,
    /// Set by a watcher thread when its folder changes. Drained on the UI
    /// thread, which is the only place a re-list may be started from.
    changed_tx: crossbeam_channel::Sender<PathBuf>,
    changed_rx: crossbeam_channel::Receiver<PathBuf>,

    /// The preview pane, and the worker that fills it.
    preview: crate::preview::PreviewState,
    previews: crate::preview::PreviewLoader,

    /// The rename in progress, if any.
    rename: Option<RenameEdit>,
    /// Counts renames, only so each gets its own text-field id.
    renames_started: u64,
    /// A folder just created, waiting for its listing to arrive so the new row
    /// can be found and put straight into a rename box.
    pending_rename: Option<(TabId, String)>,
    /// Folder names as they are created, from the apartment thread that made
    /// them.
    created_tx: crossbeam_channel::Sender<(TabId, String)>,
    created_rx: crossbeam_channel::Receiver<(TabId, String)>,

    /// Names marked by the last `Ctrl+X`, and the folder they live in.
    ///
    /// Held so those rows can be drawn faded, which is the only sign a cut
    /// happened — the files do not move until the paste. Names rather than full
    /// paths because that is what a row has, and the folder is compared once
    /// per pane instead of per row.
    cut_dir: Option<PathBuf>,
    cut_names: std::collections::HashSet<String>,
    /// Reports a clipboard write that did not land, so the rows faded in
    /// anticipation get un-faded rather than lying about a cut that never
    /// reached the clipboard.
    clip_fail_tx: crossbeam_channel::Sender<()>,
    clip_fail_rx: crossbeam_channel::Receiver<()>,
    /// `Ctrl+V` presses, seen by a window subclass because egui drops them.
    /// See `neutron_shell::pastekey` for why that is necessary.
    paste_rx: crossbeam_channel::Receiver<()>,

    /// The open context menu, if any.
    ///
    /// While this is `Some`, an apartment thread is parked inside
    /// `neutron_shell::menu::open` holding the live `IContextMenu`, waiting on
    /// `menu_reply` for the id the user picks.
    menu: Option<neutron_ui::MenuState>,
    /// Where to send the chosen command id. Dropping it without sending — which
    /// is what closing the window does — releases the parked thread, because a
    /// disconnected channel makes its `recv` return an error it treats as
    /// "dismissed".
    menu_reply: Option<crossbeam_channel::Sender<Option<u32>>>,
    menu_tx: crossbeam_channel::Sender<PendingMenu>,
    menu_rx: crossbeam_channel::Receiver<PendingMenu>,

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

        // Read before the first paint, so a light-theme session does not open
        // dark and flip a frame later.
        let theme_mode = if saved_prefs(cc).light_theme {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
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

        // Pinned folders, for the sidebar. Same shape as the servers below:
        // one small JSON file read once, at construction, before there is a
        // frame to drop.
        let recent = match crate::recent::load() {
            Ok(recent) => recent,
            Err(e) => {
                tracing::warn!("recent.json: {e}");
                crate::recent::Recent::default()
            }
        };

        let pins = match crate::pins::load() {
            Ok(pins) => pins,
            Err(e) => {
                tracing::warn!("pins.json: {e}");
                crate::pins::Pins::default()
            }
        };

        // The saved servers, for the sidebar. Reading a small JSON file; no
        // connection is opened until one is chosen.
        let (sites, sites_error) = match neutron_remote::site::load() {
            Ok(sites) => (sites, None),
            Err(e) => {
                tracing::warn!("sites.json: {e}");
                (neutron_remote::Sites::default(), Some(e))
            }
        };

        // Reads manifests and nothing else — no plugin runs until a command
        // is chosen — so this is a directory listing on the startup path.
        let plugins = crate::plugins::PluginRuntime::load();
        let palette = commands::catalogue(&plugins.palette_entries());

        let mut workspace = Self::initial_workspace(cc, start_path);
        let mut loader = Loader::spawn(cc.egui_ctx.clone());
        let sta = neutron_shell::sta::StaPool::spawn();
        let icons = IconService::new(sta.clone(), cc.egui_ctx.clone());
        let (notice_tx, notice_rx) = crossbeam_channel::unbounded();
        let (refresh_tx, refresh_rx) = crossbeam_channel::unbounded();
        let (open_as_tx, open_as_rx) = crossbeam_channel::unbounded();
        let (goto_tx, goto_rx) = crossbeam_channel::unbounded();
        let (sites_tx, sites_rx) = crossbeam_channel::unbounded();
        let (drive_tx, drive_rx) = crossbeam_channel::unbounded();
        let (menu_tx, menu_rx) = crossbeam_channel::unbounded();
        let (clip_fail_tx, clip_fail_rx) = crossbeam_channel::unbounded();
        let (created_tx, created_rx) = crossbeam_channel::unbounded();
        let (changed_tx, changed_rx) = crossbeam_channel::unbounded();

        // `Ctrl+V` never reaches the application through egui when the
        // clipboard holds files rather than text, which is the only case that
        // matters here. A window subclass sees the key first.
        let (paste_tx, paste_rx) = crossbeam_channel::unbounded();
        let snap: std::sync::Arc<neutron_shell::snaplayout::MaximiseButton> =
            std::sync::Arc::new(Default::default());
        if let Some(h) = hwnd {
            if let Err(e) = neutron_shell::pastekey::watch(h, paste_tx) {
                tracing::warn!("{e}; Ctrl+V will not work");
            }
            // Not fatal: without it the window still maximises from its own
            // button, and only the hover flyout is missing.
            if let Err(e) = neutron_shell::snaplayout::watch(h, snap.clone()) {
                tracing::warn!("{e}; Snap Layouts will not appear");
            }
        }
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
            pins,
            pin_target: None,
            tab_target: None,
            snap,
            confirm: None,
            address: None,
            recent,
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
            goto_tx,
            goto_rx,
            sites_tx,
            sites_rx,
            drive_state,
            drive_tx,
            drive_rx,
            notices: crate::notice::Notices::default(),
            notice_tx,
            notice_rx,
            sites,
            sites_error,
            connect: crate::connect::ConnectDialog::default(),
            download: None,
            plugins,
            palette,
            archive_job: None,
            watchers: std::collections::HashMap::new(),
            changed_tx,
            changed_rx,
            preview: crate::preview::PreviewState {
                open: saved_preview_open(cc),
                ..Default::default()
            },
            previews: crate::preview::PreviewLoader::spawn(cc.egui_ctx.clone()),
            rename: None,
            renames_started: 0,
            pending_rename: None,
            created_tx,
            created_rx,
            cut_dir: None,
            cut_names: std::collections::HashSet::new(),
            clip_fail_tx,
            clip_fail_rx,
            paste_rx,
            menu: None,
            menu_reply: None,
            menu_tx,
            menu_rx,
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

        // A window with nothing restored opens on the dashboard: on launch
        // the question is where to go, not what is in one particular folder. A
        // path on the command line still wins, because that is someone saying
        // exactly where they want to be.
        restored.unwrap_or_else(|| Workspace::new(NodeId::Home))
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
        t.reselect = None;
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

    /// Re-reads the folder a tab is already showing, keeping what the user set
    /// up in it.
    ///
    /// Distinct from [`Self::reload`], which exists for *going somewhere else*
    /// and so throws the selection and the filter away. Refreshing is not going
    /// anywhere: it happens after a delete, on F5, and — once the watcher is
    /// wired up — whenever anything on disk changes. Clearing the selection
    /// every time would mean the listing rearranging itself under the user
    /// while they work.
    ///
    /// The selection is remembered by *name*, because re-reading rebuilds the
    /// entry list and every index into it. Names that are gone stay gone, which
    /// is what should happen to the file that was just deleted.
    ///
    /// The status is deliberately left alone: switching it to `Loading` would
    /// replace the count in the status bar with a spinner on every change,
    /// which is a lot of movement to announce that nothing the user did has
    /// finished yet.
    fn refresh_in_place(&mut self, tab: TabId) {
        let Some(t) = self.workspace.tabs.get_mut(&tab) else {
            return;
        };
        let remembered = t.remember_selection();
        t.reselect = Some(remembered);

        let location = t.location().clone();
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
                    self.refresh_in_place(tab);
                }
            }

            Action::Activate(idx) => self.activate(idx),

            Action::Select { idx, mode } => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.apply(&t.list, idx, mode);
                }
            }
            Action::Marquee { from, to, additive } => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.select_span(&t.list, from, to, additive);
                }
            }

            Action::DragOut => {
                if !self.refuse_without_a_path("drag files out of") {
                    self.drag_out();
                }
            }
            Action::BeginRename => {
                if !self.refuse_without_a_path("rename anything in") {
                    self.begin_rename(ctx);
                }
            }
            Action::RenameTyped(text) => {
                if let Some(r) = self.rename.as_mut() {
                    r.text = text;
                }
            }
            Action::RenameCommit => self.commit_rename(ctx),
            Action::RenameCancel => {
                self.close_rename(ctx);
            }
            Action::NewFolder => {
                if !self.refuse_without_a_path("create folders in") {
                    self.new_folder();
                }
            }
            Action::RefreshIndex => self.index.refresh_index(),
            Action::ExtractSelection => {
                if !self.refuse_without_a_path("extract archives in") {
                    self.extract_selection();
                }
            }
            Action::CompressSelection => {
                if !self.refuse_without_a_path("compress anything in") {
                    self.compress_selection();
                }
            }
            Action::CancelArchiveJob => {
                if let Some(job) = self.archive_job.as_ref() {
                    job.cancel();
                }
            }
            Action::CancelPluginCommand => self.plugins.cancel(),

            Action::OpenConnect => self.connect.open(),
            Action::CloseConnect => self.connect.close(),
            Action::SetConnectField(field, text) => self.connect.set(field, text),
            Action::SetConnectProtocol(protocol) => {
                self.connect.protocol = protocol;
                self.connect.error = None;
            }
            Action::SubmitConnect => self.submit_connect(),
            Action::ReloadSites => self.refresh_sites(SitesOrigin::Requested),
            Action::DismissNotice => self.notices.dismiss(),
            Action::OpenTerminal => self.open_terminal(),
            Action::NewWindow => self.new_window(),
            Action::CancelDownload => {
                if let Some(job) = self.download.as_ref() {
                    job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Action::ReloadPlugins => {
                self.plugins.reload();
                self.palette = commands::catalogue(&self.plugins.palette_entries());
                self.refresh_finder();
            }
            Action::TogglePreview => {
                self.preview.open = !self.preview.open;
                if !self.preview.open {
                    // Dropped rather than kept for next time: the texture is
                    // the largest thing the pane owns, and a hidden pane
                    // holding a few megabytes of an image nobody is looking at
                    // is the kind of thing that is never noticed and never
                    // freed.
                    self.previews.clear();
                    self.preview.showing = None;
                    self.preview.content = crate::preview::Preview::Nothing;
                    self.preview.texture = None;
                }
            }

            Action::Clip { cut } => {
                if !self.refuse_without_a_path(if cut { "cut from" } else { "copy from" }) {
                    self.clip_selection(cut);
                }
            }
            Action::Paste => {
                if !self.refuse_without_a_path("paste into") {
                    self.paste();
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
                // Every tab, not just the focused one. "Show hidden files" is
                // an answer to "what do I want to see", not a property of one
                // folder — and with no per-tab indicator, having it on in one
                // tab and off in the next reads as tabs showing different
                // contents for no reason.
                let show = self
                    .workspace
                    .active_tab()
                    .is_some_and(|t| !t.view.show_hidden);
                for tab in self.workspace.tabs.values_mut() {
                    tab.view.show_hidden = show;
                    tab.refilter();
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

            Action::Delete { permanent } => {
                if !self.refuse_without_a_path("delete from") {
                    self.delete_selection(permanent);
                }
            }

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

            Action::ContextMenu { idx, pos } => self.context_menu(idx, pos),

            Action::DropFiles { group, paths, copy } => {
                if !self.refuse_without_a_path("copy files into") {
                    self.drop_files(group, paths, copy);
                }
            }

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
            Action::TogglePin => self.toggle_pin(),
            Action::ToggleAddress => self.toggle_address(ctx),
            Action::OpenRecent(path) => self.open_local(path),
            Action::ForgetRecent(path) => self.forget_recent(path),
            Action::SetAddress(text) => {
                if let Some((_, current)) = self.address.as_mut() {
                    *current = text;
                }
            }
            Action::SubmitAddress => self.submit_address(),
            Action::CancelAddress => self.address = None,
            Action::Ask(then) => self.ask(*then),
            Action::SidebarMenu(id, at) => self.sidebar_menu(id, at),
            Action::SignOutDrive => self.sign_out_drive(),
            Action::ForgetServer => self.forget_server(),

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
                let view = self.workspace.view_for_new_tab();
                let tab = self.workspace.open_tab(group, location.clone(), view);
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

        // A cloud listing's target is the provider's own object id. It has to
        // be read *before* the generic target branch below, which treats a
        // non-path target as a shell parsing name — so a Drive folder id went
        // to `SHParseDisplayName`, which is why no folder inside Drive would
        // open and why activating a Drive file navigated to nothing. Present
        // since M7; there was never a cloud branch here at all.
        if let NodeId::Cloud { provider, .. } = t.location() {
            let provider = *provider;
            // Every cloud row carries one. A listing without targets is a bug
            // in the provider, not something to guess a path from.
            let Some((target, _)) = t.list.target(idx) else {
                self.notices
                    .error(format!("{name} has no id Neutron can open"));
                return;
            };
            let target = target.to_owned();
            if kind.is_container() {
                // The name travels with the id: it is what titles the tab, and
                // asking the provider for it later would be an API call on the
                // paint thread.
                self.navigate(tab, NodeId::cloud(provider, target, name));
            } else {
                match provider {
                    CloudProviderId::GoogleDrive => self.start_drive_download(target, name),
                    // OneDrive syncs to a real directory, so it is a `Path`
                    // node and never reaches here. Named rather than
                    // wildcarded so that changing it is a compile error, not a
                    // OneDrive file quietly fetched through the Drive API.
                    CloudProviderId::OneDrive => self
                        .notices
                        .error(format!("Neutron cannot open {name} from OneDrive this way")),
                }
            }
            return;
        }

        // A shell listing tells us where each row leads, because its children
        // cannot be addressed by joining a name to the parent — "This PC"
        // contains `C:\`, and a Control Panel item has no path at all.
        if let Some((target, is_path)) = t.list.target(idx) {
            let target = target.to_owned();
            if !kind.is_container() && is_path {
                self.open_local(PathBuf::from(target));
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

        // A server's rows are addressed by joining a name to a POSIX path,
        // and the name has already been through `safe_name` on the way into
        // the listing — but this is where it becomes a path, so it is checked
        // again here rather than trusted.
        if let Some((site, dir)) = t.location().as_remote() {
            let (site, dir) = (site.to_owned(), dir.to_owned());
            let container = kind.is_container();
            let Ok(full) = neutron_remote::path::join(&dir, &name) else {
                self.notices.error(format!("{name} is not a name this can open"));
                return;
            };
            if container {
                self.navigate(tab, NodeId::remote(site.as_str(), full.as_str()));
            } else {
                self.start_download(&site, full, name);
            }
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

    /// Applies whatever a worker made of `sites.json`.
    fn drain_sites(&mut self) {
        while let Ok(reply) = self.sites_rx.try_recv() {
            match reply.sites {
                Ok(sites) => {
                    self.sites = sites;
                    self.sites_error = None;
                }
                Err(e) => {
                    self.sites_error = Some(e.clone());
                    self.notices.error(format!("sites.json: {e}"));
                }
            }
            // The loader holds the connections, so it has to hear about this
            // too — a site whose host or port changed must not keep answering
            // from the session opened against the old one.
            self.loader.reload_sites();

            match reply.origin {
                SitesOrigin::Background => {}
                SitesOrigin::Requested => {
                    let count = self.sites.sites.len();
                    self.notices.info(match count {
                        0 => "No servers saved".to_owned(),
                        1 => "1 server saved".to_owned(),
                        n => format!("{n} servers saved"),
                    });
                }
                SitesOrigin::Dialog(Ok(())) => self.connect.close(),
                // Kept open: the password is only in the dialog, and closing it
                // would make the user type it again to find out what went
                // wrong.
                SitesOrigin::Dialog(Err(reason)) => self.connect.error = Some(reason),
            }
        }
    }

    /// Applies navigations a worker asked for once its work was done.
    fn drain_goto(&mut self) {
        let wanted: Vec<_> = self.goto_rx.try_iter().collect();
        for (tab, id) in wanted {
            self.navigate(tab, id);
        }
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
                self.open_local(path);
            }
        }
    }

    /// Hands the selection to the system as a drag.
    ///
    /// Returns at once. The drag itself blocks an apartment thread until the
    /// drop finishes, which is exactly why it is not on this one — the window
    /// keeps painting for the whole gesture, so the ground goes on drifting and
    /// hover still responds behind the drag image.
    fn drag_out(&mut self) {
        let Some((tab_id, _, paths)) = self.selection_paths() else {
            return;
        };

        let owner = self.hwnd.unwrap_or(0);
        let wake = self.ctx.clone();
        let done = self.refresh_tx.clone();

        tracing::info!(count = paths.len(), "dragging out");
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            match neutron_shell::dragout::drag(&paths, owner) {
                // Re-listed after any completed drop, not only after a move.
                // The drag cannot report reliably whether the files left —
                // Explorer moves them itself and then says it did nothing —
                // so the listing is the only account that is true.
                Ok(neutron_shell::dragout::Dropped::Completed) => {
                    let _ = done.send(tab_id);
                }
                Ok(neutron_shell::dragout::Dropped::Cancelled) => {}
                Err(e) => report(&notices, format!("Drag failed: {e}")),
            }
            wake.request_repaint();
        });
    }

    /// Opens the rename box on the focused tab's cursor row.
    fn begin_rename(&mut self, ctx: &egui::Context) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(tab) = self.workspace.tabs.get(&tab_id) else {
            return;
        };
        // The cursor rather than the selection: renaming is a single-file
        // operation, and with several selected the cursor is the one the user
        // last touched.
        let Some(idx) = tab.selection.cursor().filter(|&i| i < tab.list.len()) else {
            return;
        };

        let name = tab.list.name(idx).to_owned();
        self.open_rename(ctx, tab_id, idx, name);
    }

    /// Puts a row into a rename box and focuses it.
    fn open_rename(&mut self, ctx: &egui::Context, tab: TabId, idx: usize, name: String) {
        self.renames_started += 1;
        let generation = self.renames_started;
        self.rename = Some(RenameEdit {
            tab,
            generation,
            idx,
            original: name.clone(),
            text: name,
        });

        // Focused from here rather than from the widget: the box has not been
        // drawn yet, and without focus the first keystroke would go to the key
        // table instead — where it would be a navigation shortcut.
        ctx.memory_mut(|m| m.request_focus(file_list::rename_field_id(generation)));
    }

    /// Ends the rename and gives the keyboard back.
    ///
    /// Without surrendering focus the field keeps it after the box is gone, and
    /// `egui_wants_keyboard_input` stays true — which switches off every
    /// shortcut in the application until something else is clicked.
    fn close_rename(&mut self, ctx: &egui::Context) -> Option<RenameEdit> {
        let edit = self.rename.take()?;
        ctx.memory_mut(|m| m.surrender_focus(file_list::rename_field_id(edit.generation)));
        Some(edit)
    }

    /// Applies the typed name.
    fn commit_rename(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.close_rename(ctx) else {
            return;
        };

        let text = edit.text.trim().to_owned();
        // An unchanged name is not a failure; it is what pressing Enter without
        // typing means, and it should not put a file operation on a worker.
        if text == edit.original || text.is_empty() {
            return;
        }
        if let Some(reason) = neutron_core::naming::rejection(&text) {
            self.notices.error(format!("Cannot rename to {text:?}: {reason}"));
            return;
        }

        let Some(dir) = self
            .workspace
            .tabs
            .get(&edit.tab)
            .and_then(|t| t.location().as_path())
            .map(|p| p.to_path_buf())
        else {
            return;
        };
        let from = dir.join(&edit.original);

        // The commonest rename mistake, answered here rather than by the shell.
        // `IFileOperation` reports a collision as `0x80070057: The parameter is
        // incorrect` in a modal dialog, which tells the user nothing about what
        // they did or how to fix it.
        //
        // Only when the *name* differs by more than case: `README.md` to
        // `readme.md` is a real rename that Windows reports as already
        // existing, because on this filesystem it is the same file.
        // `to_lowercase` rather than `eq_ignore_ascii_case`: a name differing
        // only in the case of a non-ASCII letter is the same rename, and
        // refusing it would be a bug nobody could work around.
        let same_file_different_case = text.to_lowercase() == edit.original.to_lowercase();

        let owner = self.hwnd.unwrap_or(0);
        let wake = self.ctx.clone();
        let done = self.refresh_tx.clone();
        let tab = edit.tab;

        tracing::info!(from = %from.display(), to = %text, "rename");
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            // The collision check runs here, not before the submit. It is a
            // `stat`, and a stat is the blocking call the paint thread must
            // never make — on a sleeping drive or a disconnected share it is the
            // stall this architecture exists to refuse.
            if !same_file_different_case && dir.join(&text).exists() {
                report(
                    &notices,
                    format!("{text} is already here — pick another name"),
                );
            } else {
                if let Err(e) = neutron_shell::fileops::rename(&from, &text, owner) {
                    report(&notices, format!("Could not rename to {text:?}: {e}"));
                }
                let _ = done.send(tab);
            }
            wake.request_repaint();
        });
    }

    /// Creates a folder in the focused tab and opens its rename box.
    fn new_folder(&mut self) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
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

        let owner = self.hwnd.unwrap_or(0);
        let wake = self.ctx.clone();
        let done = self.refresh_tx.clone();
        let created = self.created_tx.clone();

        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            match neutron_shell::fileops::create_folder(&dir, owner) {
                // The name is reported so the row can be found once the listing
                // is re-read: a new folder appears with its name selected, and
                // that cannot be done without knowing which row it is.
                Ok(name) => {
                    let _ = created.send((tab_id, name));
                }
                Err(e) => report(&notices, format!("Could not create a folder here: {e}")),
            }
            let _ = done.send(tab_id);
            wake.request_repaint();
        });
    }

    /// Puts a newly created folder into its rename box, once its row exists.
    fn open_pending_rename(&mut self, ctx: &egui::Context) {
        let Some((tab_id, name)) = self.pending_rename.clone() else {
            return;
        };
        let Some(tab) = self.workspace.tabs.get(&tab_id) else {
            self.pending_rename = None;
            return;
        };
        // The listing may not have caught up yet. Left pending, it is retried
        // next frame; the entry appears as soon as the reload lands.
        let Some(idx) = (0..tab.list.len()).find(|&i| tab.list.name(i) == name) else {
            return;
        };

        self.pending_rename = None;
        self.open_rename(ctx, tab_id, idx, name);
    }

    /// The focused tab, the folder it shows, and the paths of its selection.
    ///
    /// Every command that acts on files needs exactly this triple, and each one
    /// used to rebuild it — including the `i < len` guard, which is what stops
    /// a selection left over from a previous listing indexing into the new one.
    fn selection_paths(&self) -> Option<(TabId, PathBuf, Vec<PathBuf>)> {
        let tab_id = self.workspace.active_tab_id()?;
        let tab = self.workspace.tabs.get(&tab_id)?;
        let dir = tab.location().as_path()?.to_path_buf();

        // Sorted into the order the rows appear in, because the selection is a
        // set and iterating it directly gives a different order every run. That
        // order is visible: it decides the order files go into a zip, onto the
        // clipboard, and into a plugin's request — and a plugin writing a file
        // per selected item would produce a differently-ordered file each time
        // from the same selection.
        let mut selected: Vec<usize> = tab.selection.iter().filter(|&i| i < tab.list.len()).collect();
        selected.sort_by_key(|&i| tab.list.rank(i).unwrap_or(usize::MAX));

        let paths: Vec<PathBuf> = selected
            .into_iter()
            .map(|i| dir.join(tab.list.name(i)))
            .collect();

        (!paths.is_empty()).then_some((tab_id, dir, paths))
    }

    /// Puts the selection on the clipboard.
    ///
    /// Nothing is copied or moved here — that happens on paste, possibly in
    /// another application, possibly never. All this does is publish the list.
    fn clip_selection(&mut self, cut: bool) {
        let Some((_, dir, paths)) = self.selection_paths() else {
            return;
        };

        use neutron_shell::fileops::Transfer;
        let how = if cut { Transfer::Move } else { Transfer::Copy };

        // Marked before the worker runs, not after: the fade is feedback for a
        // keystroke and has to land on the next frame, and a failed clipboard
        // write clears it again below.
        if cut {
            self.cut_dir = Some(dir);
            self.cut_names = paths
                .iter()
                .filter_map(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect();
        } else {
            self.clear_cut_marks();
        }

        let wake = self.ctx.clone();
        let failed = self.clip_fail_tx.clone();

        tracing::info!(count = paths.len(), ?how, "clipboard");
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            if let Err(e) = neutron_shell::clipboard::write(&paths, how) {
                report(&notices, format!("Could not put that on the clipboard: {e}"));
                let _ = failed.send(());
            }
            wake.request_repaint();
        });
    }

    /// Turns the clipboard chords into actions.
    ///
    /// `Ctrl+C` and `Ctrl+X` arrive as `Event::Copy` and `Event::Cut`, because
    /// egui-winit converts them before they become key presses — so there is no
    /// `Key::C` to bind and these have to be read from the event stream.
    ///
    /// `Ctrl+V` does not arrive at all: egui-winit turns it into
    /// `Event::Paste(text)` and drops it when the clipboard holds no text,
    /// which is exactly what a copied *file* looks like. It comes in through
    /// the window subclass instead.
    fn clipboard_keys(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        // A menu or a text field is using the keyboard; its own copy and paste
        // win.
        if self.menu.is_some() || ctx.egui_wants_keyboard_input() {
            // Still drained, so a press held while a field had focus does not
            // fire the moment focus leaves.
            while self.paste_rx.try_recv().is_ok() {}
            return;
        }

        ctx.input(|i| {
            for event in &i.events {
                match event {
                    egui::Event::Copy => actions.push(Action::Clip { cut: false }),
                    egui::Event::Cut => actions.push(Action::Clip { cut: true }),
                    _ => {}
                }
            }
        });

        // Collapsed to one: several presses between frames still mean one
        // paste, and a repeated file operation is not a harmless duplicate.
        let mut pasted = false;
        while self.paste_rx.try_recv().is_ok() {
            pasted = true;
        }
        if pasted {
            actions.push(Action::Paste);
        }
    }

    fn clear_cut_marks(&mut self) {
        self.cut_dir = None;
        self.cut_names.clear();
    }

    /// Pastes the clipboard into the focused tab's folder.
    ///
    /// Reading the clipboard is itself a blocking call — another process may
    /// hold it — so the whole operation, decision included, happens on the
    /// worker. The UI thread never learns what was on the clipboard, only that
    /// the listing should be re-read.
    fn paste(&mut self) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
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

        // A cut is spent by the paste that consumes it, whatever the outcome:
        // the files have either moved or the paste failed, and in both cases
        // leaving rows faded would be a lie about a clipboard we no longer own.
        self.clear_cut_marks();

        let owner = self.hwnd.unwrap_or(0);
        let wake = self.ctx.clone();
        let done = self.refresh_tx.clone();

        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            let Some((paths, how)) = neutron_shell::clipboard::read() else {
                tracing::debug!("nothing on the clipboard to paste");
                return;
            };
            if !neutron_shell::clipboard::can_paste_into(&paths, &dir) {
                report(
                    &notices,
                    "Cannot paste a folder into itself".to_owned(),
                );
                return;
            }

            tracing::info!(count = paths.len(), ?how, dest = %dir.display(), "paste");
            if let Err(e) = neutron_shell::fileops::transfer(&paths, &dir, how, owner) {
                report(&notices, format!("Paste failed: {e}"));
            }
            let _ = done.send(tab_id);
            wake.request_repaint();
        });
    }

    /// Sends the focused tab's selection to the Recycle Bin.
    ///
    /// The listing is refreshed when the operation finishes rather than
    /// optimistically: the shell may have been refused, the user may have
    /// cancelled at the confirmation prompt, and some of a batch can fail while
    /// the rest succeeds. Re-reading the directory is the only account of what
    /// actually happened that is guaranteed to be true.
    fn delete_selection(&mut self, permanent: bool) {
        let Some((tab_id, _, paths)) = self.selection_paths() else {
            return;
        };

        let disposal = if permanent {
            neutron_shell::fileops::Disposal::Permanent
        } else {
            neutron_shell::fileops::Disposal::Recycle
        };
        let owner = self.hwnd.unwrap_or(0);
        let ctx = self.ctx.clone();
        let done = self.refresh_tx.clone();

        tracing::info!(count = paths.len(), ?disposal, "deleting");
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            if let Err(e) = neutron_shell::fileops::delete(&paths, disposal, owner) {
                report(&notices, format!("Delete failed: {e}"));
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
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            if let Err(e) = neutron_shell::fileops::transfer(&paths, &dir, how, owner) {
                report(&notices, format!("Could not copy those files here: {e}"));
            }
            let _ = done.send(tab_id);
            wake.request_repaint();
        });
    }

    /// Asks the shell for the context menu of the selection.
    ///
    /// Right-clicking a row that is not selected selects it first, as every
    /// file manager does — acting on a hidden selection is how people delete
    /// the wrong thing.
    ///
    /// Returns immediately; the menu appears once the apartment thread has
    /// built it, which is a frame or two later on a folder with slow handlers
    /// installed.
    fn context_menu(&mut self, idx: Option<usize>, pos: egui::Pos2) {
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

        // Nothing selected means the click was on empty space, and the menu is
        // the folder's own — New, Paste, Sort by. A different COM object, so it
        // is fetched differently below.
        let background = self.selection_paths();
        let (tab_id, dir, paths) = match background {
            Some(found) => found,
            None => {
                let Some(tab_id) = self.workspace.active_tab_id() else {
                    return;
                };
                let Some(dir) = self
                    .workspace
                    .tabs
                    .get(&tab_id)
                    .and_then(|t| t.location().as_path())
                    .map(|p| p.to_path_buf())
                else {
                    // The folder's own menu comes from a shell view object, and
                    // there is no path to bind one to here. Silence read as a
                    // right-click the application had dropped.
                    self.refuse_without_a_path("show a menu for");
                    return;
                };
                (tab_id, dir, Vec::new())
            }
        };

        // A menu already up is replaced, not stacked. Dropping the reply
        // sender releases whichever apartment thread is parked on the old one.
        self.menu = None;
        self.menu_reply = None;

        let ctx_wake = self.ctx.clone();
        let done = self.refresh_tx.clone();
        let offer = self.menu_tx.clone();

        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            // Blocks this apartment for as long as the menu is open. The UI
            // thread keeps painting — that is the entire reason it is here.
            let present = |items: Vec<neutron_core::MenuItem>| {
                // The shell's menu, now plain data. Hand it to the UI and wait
                // for the answer; the COM object stays on this thread.
                let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                if offer.send(PendingMenu { items, pos, reply: reply_tx }).is_err() {
                    return None;
                }
                ctx_wake.request_repaint();
                // A disconnected channel means the UI went away — a closed
                // window, or a second right-click replacing this menu. Either
                // way there is nothing to invoke.
                reply_rx.recv().unwrap_or(None)
            };

            let result = if paths.is_empty() {
                neutron_shell::menu::open_background(&dir, present)
            } else {
                neutron_shell::menu::open(&paths, present)
            };
            if let Err(e) = result {
                report(&notices, format!("The context menu could not be built: {e}"));
            }
            // The command may have renamed, deleted, or created something.
            // There is no way to know which, so re-list unconditionally.
            let _ = done.send(tab_id);
            ctx_wake.request_repaint();
        });
    }

    /// Draws the open context menu and answers the thread waiting on it.
    fn context_menu_overlay(&mut self, ctx: &egui::Context, p: &neutron_ui::Palette) {
        while let Ok(pending) = self.menu_rx.try_recv() {
            // Plugin rows are appended here rather than on the apartment
            // thread: this is the side that knows what is installed, and it is
            // also where the ids handed out have to be remembered.
            let mut items = pending.items;

            // Neutron's own rows come before the plugins', and after the
            // shell's: they are part of the application rather than an
            // extension of it.
            self.pin_target = self.menu_pin_row().map(|(row, target)| {
                items.push(neutron_core::MenuItem::separator());
                items.push(row);
                target
            });

            let has_selection = self.selection_paths().is_some();
            let rows = self.plugins.menu_rows(has_selection);
            if !rows.is_empty() {
                items.push(neutron_core::MenuItem::separator());
                items.extend(rows);
            }

            self.menu = Some(neutron_ui::MenuState::new(items, pending.pos));
            // Replaces any previous reply sender, which releases the thread
            // parked on it as dismissed.
            self.menu_reply = Some(pending.reply);
        }

        let Some(state) = self.menu.as_mut() else {
            return;
        };

        let answer = match neutron_ui::context_menu::show(ctx, p, state) {
            neutron_ui::MenuOutcome::Open => return,
            neutron_ui::MenuOutcome::Chosen(id) => Some(id),
            neutron_ui::MenuOutcome::Dismissed => None,
        };

        self.menu = None;

        // A plugin row's id means nothing to the shell — handing it to
        // `InvokeCommand` would invoke an unrelated verb. So the apartment
        // thread is told the menu was dismissed, and the command runs here.
        // Ours, like a plugin's, mean nothing to the shell.
        let pin = (answer == Some(MENU_ID_PIN))
            .then(|| self.pin_target.take())
            .flatten();
        let new_tab = (answer == Some(MENU_ID_NEW_TAB))
            .then(|| self.tab_target.take())
            .flatten();

        let plugin = answer.and_then(|id| self.plugins.menu_choice(id));
        let for_shell = match (&pin, &new_tab, &plugin) {
            (None, None, None) => answer,
            _ => None,
        };

        if let Some(reply) = self.menu_reply.take() {
            let _ = reply.send(for_shell);
        }

        if let Some(dir) = pin {
            self.apply_pin(dir);
        }

        if let Some(id) = new_tab {
            self.open_in_new_tab(id);
        }

        if let Some(reference) = plugin {
            let ctx = ctx.clone();
            self.run_plugin(&ctx, &reference);
        }
    }

    /// Drops a file from the recent list.
    fn forget_recent(&mut self, path: PathBuf) {
        if !self.recent.forget(&path) {
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        self.notices.info(format!("Removed {name} from Recent files"));

        let recent = self.recent.clone();
        self.sta.submit(move || {
            if let Err(e) = crate::recent::save(&recent) {
                tracing::warn!("could not save recent files: {e}");
            }
        });
    }

    /// Opens a local file and remembers it.
    ///
    /// The two call sites are a double-click and the shell's answer about what
    /// a row leads to. Deliberately not used for a plugin's `open` responses —
    /// one command can ask for eight, and a recent list flooded by one plugin
    /// run is no longer a record of what *you* were doing — nor for a file
    /// fetched from a server, which is a copy under `%TEMP%` that will not be
    /// there later.
    fn open_local(&mut self, path: PathBuf) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        if self.recent.record(&path, now) {
            let recent = self.recent.clone();
            let notices = self.notice_tx.clone();
            self.sta.submit(move || {
                if let Err(e) = crate::recent::save(&recent) {
                    // A warning, not a notice: failing to remember a file is
                    // not a failure to open it, and the open is what the user
                    // asked for.
                    tracing::warn!("could not save recent files: {e}");
                    let _ = &notices;
                }
            });
        }
        self.launch(path, neutron_shell::open::Verb::Open);
    }

    /// Opens `path` with the shell, on an apartment thread.
    ///
    /// Fire and forget. There is nothing useful to wait for — the launched
    /// process is not ours — and waiting is exactly what must not happen: the
    /// association lookup reads the registry, may load a shell extension, and
    /// on a network or cloud path can block for many seconds.
    fn launch(&self, path: PathBuf, verb: neutron_shell::open::Verb) {
        // Recording happens in `open_local`, not here: `launch` is also how a
        // fetched *copy* of a remote file is opened, and a path under %TEMP%
        // is not somewhere anyone can go back to.
        let owner = self.hwnd.unwrap_or(0);
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            if let Err(e) = neutron_shell::open::shell_execute(&path, verb, owner) {
                report(
                    &notices,
                    format!("Could not open {}: {e}", path.display()),
                );
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
        let mut snapshot = self.workspace.persist();
        snapshot.preview = self.preview.open;
        // The theme belongs to the window, not to the pane tree, so the
        // workspace cannot fill it in itself.
        snapshot.prefs.light_theme = self.theme_mode == ThemeMode::Light;
        if let Ok(json) = serde_json::to_string(&snapshot) {
            storage.set_string(STORAGE_KEY, json);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let frame_started = Instant::now();
        let ctx = ui.ctx().clone();
        let p = self.palette();
        let mut actions: Vec<Action> = Vec::new();

        // The ground the cards sit on. Painted onto a background layer so it
        // stays beneath every panel regardless of declaration order.
        //
        // No repaint is requested afterwards. The ground is still, so once a
        // frame is drawn there is nothing that would make the next one differ:
        // egui repaints on input and on an explicit request, and every worker
        // that finishes already makes one of its own.
        let screen = ctx.content_rect();
        let bg = ui
            .painter()
            .clone()
            .with_layer_id(egui::LayerId::background());
        neutron_ui::ambient::paint(&bg, screen, &p);

        self.drain_places();
        self.drain_loads(&ctx);
        self.drain_refreshes();
        self.drain_open_as();
        self.drain_goto();
        self.drain_sites();
        while let Ok(state) = self.drive_rx.try_recv() {
            self.drive_state = state;
        }
        if self.clip_fail_rx.try_recv().is_ok() {
            self.clear_cut_marks();
        }
        self.clipboard_keys(&ctx, &mut actions);
        while let Ok((tab, name)) = self.created_rx.try_recv() {
            self.pending_rename = Some((tab, name));
        }
        self.open_pending_rename(&ctx);
        // Before drawing: installs icons that arrived since the last frame, so
        // rows painted below pick them up immediately rather than a frame late.
        self.pump_archive_job();
        self.pump_plugin(&ctx);
        self.pump_download();
        self.drain_notices(&ctx);
        self.sync_watchers(&ctx);
        self.drain_changes();
        self.pump_preview(&ctx);
        self.icons.pump();
        self.icon_texture = self.icons.texture(&ctx);
        self.index.pump();
        self.sync_finder_results();

        // Before `keyboard`, not after: the menu *consumes* the arrows and
        // Escape it uses, and `keyboard` only reads them. Drawn after the panes
        // regardless — it lives on the foreground layer, so painting order is
        // not what puts it on top.
        self.context_menu_overlay(&ctx, &p);

        self.keyboard(&ctx, &mut actions);

        // Before every other panel: it claims the top strip, and the rest
        // lay out inside what is left.
        self.title_bar(ui, &p);

        self.status_bar(ui, &p, &mut actions);
        self.sidebar(ui, &p, &mut actions);
        self.action_rail(ui, &p, &mut actions);
        self.preview_pane(ui, &p);
        self.panes(ui, &p, &mut actions);

        // After every panel: the eight grip regions sit on top of whatever
        // is under the window edge, and an undecorated window has no resize
        // border of its own.
        crate::chrome::resize_edges(ui, screen);

        // Drawn last so it covers everything, and after input routing so the
        // overlay's own keys win while it is open.
        self.finder_overlay(ui, &p, &mut actions);
        self.connect_overlay(ui, &p, &mut actions);

        // Last of all: a question is answered before anything underneath it is
        // touched, and its scrim has to cover the overlays too.
        self.confirm_overlay(ui, &p);


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

/// Whether the preview pane was showing when the last session was saved.
///
/// Read separately from the workspace restore because the pane belongs to the
/// window rather than to the pane tree, and because a `--start-path` launch
/// skips the workspace restore entirely but should still remember it.
fn saved_preview_open(cc: &eframe::CreationContext<'_>) -> bool {
    saved_session(cc).is_some_and(|saved| saved.preview)
}

/// The saved settings, or the defaults on a first run.
///
/// Read separately from the workspace restore because the theme has to be
/// applied before the first paint, which happens before the pane tree is built.
fn saved_prefs(cc: &eframe::CreationContext<'_>) -> crate::workspace::PersistedPrefs {
    saved_session(cc).map(|saved| saved.prefs).unwrap_or_default()
}

fn saved_session(
    cc: &eframe::CreationContext<'_>,
) -> Option<crate::workspace::PersistedWorkspace> {
    cc.storage
        .and_then(|s| s.get_string(STORAGE_KEY))
        .and_then(|json| serde_json::from_str::<crate::workspace::PersistedWorkspace>(&json).ok())
}

/// A rename in progress.
///
/// The typed text lives here rather than in the list widget because that widget
/// is drawn from a clone of the view state — anything it wrote would be thrown
/// away when the frame ended. It edits a copy and reports each keystroke back.
struct RenameEdit {
    tab: TabId,
    /// Which rename this is. Gives the text field a fresh id each time, so it
    /// cannot inherit the previous rename's caret position.
    generation: u64,
    /// Storage index, so a re-sort under the box does not move the rename to a
    /// different file.
    idx: usize,
    /// What the file is called now, to tell an unchanged rename from a real one.
    original: String,
    text: String,
}

/// A shell menu waiting to be shown, from the apartment thread that built it.
struct PendingMenu {
    items: Vec<neutron_core::MenuItem>,
    /// Where to put it, in egui's coordinates — which is where the click was
    /// reported, so no conversion to physical screen pixels is needed any more.
    pos: egui::Pos2,
    reply: crossbeam_channel::Sender<Option<u32>>,
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
        // Collected first because refreshing borrows `self` mutably.
        let tabs: Vec<TabId> = self.refresh_rx.try_iter().collect();
        for tab in tabs {
            self.refresh_in_place(tab);
        }
    }

    fn drain_loads(&mut self, ctx: &egui::Context) {
        // Set when a remote folder arrives, and applied after the loop — `tab`
        // borrows `self.workspace` mutably for the whole body.
        let mut cleared_remote_note = false;

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
                    match tab.reselect.take() {
                        Some(wanted) => tab.restore_selection(&wanted),
                        None => tab.selection.clear(),
                    }
                    // A folder that opened is its own proof. Leaving
                    // "Connecting to …" up beside a listing that plainly
                    // arrived is a status line telling a small lie.
                    if matches!(tab.location(), NodeId::Remote { .. }) {
                        cleared_remote_note = true;
                    }
                }
                LoadResult::Failed { id, error } => {
                    tab.status = Status::Error(format!("{id}: {error}"));
                    tab.list = EntryList::new();
                    tab.reselect = None;
                    tab.selection.clear();
                }
            }
        }

        if cleared_remote_note {
            // A folder that opened is its own proof, so "Connecting…" goes.
            // An error stays: it has still not been acknowledged.
            self.notices.dismiss_info();
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

        // A context menu is modal to the pane behind it. Without this, Delete
        // while the menu is open would bin the selection *and* run whichever
        // command the cursor was on.
        if self.menu.is_some() {
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
            // Alt+P, which is Explorer's binding for the same pane. Not
            // Ctrl+P: that is the file finder, and it is used far more often.
            if i.modifiers.alt && !ctrl && i.key_pressed(egui::Key::P) {
                actions.push(Action::TogglePreview);
            }

            bindings.push((egui::Key::F2, Action::BeginRename));
            bindings.push((egui::Key::F5, Action::Refresh));
            bindings.push((egui::Key::Escape, Action::ClearSelection));
            bindings.push((egui::Key::F6, Action::FocusNextPane));

            if ctrl {
                bindings.extend([
                    (egui::Key::A, Action::SelectAll),
                    (egui::Key::H, Action::ToggleHidden),
                    (
                        // Ctrl+L is the address bar, as it is in every browser
                        // and in Explorer. Shift escalates to the view toggle,
                        // which had the plain binding first only because there
                        // was nothing else asking for it.
                        egui::Key::L,
                        if shift {
                            Action::ToggleView
                        } else {
                            Action::ToggleAddress
                        },
                    ),
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
                    (
                        // One binding, chosen by Shift, rather than two entries
                        // for the same key: both were in the table at once and
                        // Ctrl+Shift+D ran the pin *and* flipped the theme.
                        egui::Key::D,
                        if shift {
                            Action::TogglePin
                        } else {
                            Action::ToggleTheme
                        },
                    ),
                    (egui::Key::R, Action::Refresh),
                    (egui::Key::W, Action::CloseActiveTab),
                    // Backslash splits, mirroring the common terminal binding.
                    (
                        egui::Key::Backslash,
                        Action::Split(if shift { Axis::Vertical } else { Axis::Horizontal }),
                    ),
                ]);

                if shift {
                    bindings.push((egui::Key::N, Action::NewFolder));
                } else {
                    // Ctrl+N is a new window everywhere else; Ctrl+Shift+N is
                    // a new folder in every file manager. Both conventions are
                    // strong enough that neither can be given to the other.
                    bindings.push((egui::Key::N, Action::NewWindow));
                }

                // Ctrl+` for the terminal, as in the editor this is most often
                // sitting beside.
                bindings.push((egui::Key::Backtick, Action::OpenTerminal));

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
                    // At error level: this is the only record of why a sign-in
                    // the user actively started did not work, and it must show
                    // under the default log filter.
                    tracing::error!("Google Drive sign-in failed: {e}");
                    neutron_cloud::google::DriveState::Error(e.to_string())
                }
            };
            let _ = done.send(state);
            wake.request_repaint();
        });
    }



    /// Pinned folders as sidebar places.
    ///
    /// Built per frame rather than kept: it is a handful of paths, and a cached
    /// copy is one more thing that can disagree with `self.pins` after a
    /// toggle.
    fn pinned_places(&self) -> Vec<Place> {
        self.pins
            .folders
            .iter()
            .map(|path| Place {
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned()),
                id: NodeId::Path(path.clone()),
                kind: neutron_core::places::PlaceKind::KnownFolder,
                capacity: None,
            })
            .collect()
    }

    /// What Quick access holds: the known folders, then anything pinned.
    ///
    /// Known folders first because they are always the same four and the eye
    /// learns where they are; pins after, newest first, because that order is
    /// the point of them.
    fn quick_access(&self) -> Vec<Place> {
        let mut all = self.known_folders.clone();
        // A pin that is already a known folder is not listed twice. Pinning
        // Downloads is a reasonable thing to try, and two identical tiles side
        // by side looks like a bug rather than a preference.
        all.extend(
            self.pinned_places()
                .into_iter()
                .filter(|pinned| !self.known_folders.iter().any(|k| k.id == pinned.id)),
        );
        all
    }




    /// Opens `location` in a new tab of the focused pane.
    ///
    /// The sidebar's menu route. `Action::NewTab` deliberately opens at the
    /// pane's *current* location — "new tab here" is what Ctrl+T means — so it
    /// is the wrong thing to reuse when the point is to go somewhere else.
    fn open_in_new_tab(&mut self, location: NodeId) {
        let Some(group) = self.workspace.focused_group().map(|g| g.id) else {
            return;
        };
        let view = self.workspace.view_for_new_tab();
        let tab = self.workspace.open_tab(group, location.clone(), view);
        self.workspace.focused = group;
        self.reload(tab, location);
    }



    /// Opens the address bar on the focused pane, or closes it.
    ///
    /// Opens holding where the pane already is, selected, so `Ctrl+L` followed
    /// by typing replaces the path rather than appending to it — which is what
    /// every browser and Explorer do, and what muscle memory expects.
    fn toggle_address(&mut self, ctx: &egui::Context) {
        if self.address.is_some() {
            self.address = None;
            return;
        }
        let Some(group) = self.workspace.focused_group().map(|g| g.id) else {
            return;
        };
        let Some(text) = self
            .workspace
            .active_tab()
            .and_then(|t| crate::address::text_for(t.location()))
        else {
            // The dashboard and a Drive folder have no path to put in the
            // field, and an empty address bar is a worse answer than none.
            self.notices
                .info("There is no path here to type over");
            return;
        };

        let id = crate::header::address_id(group);
        ctx.memory_mut(|m| m.request_focus(id));
        self.address = Some((group, text));
    }

    /// Goes where the address bar says.
    fn submit_address(&mut self) {
        let Some((_, text)) = self.address.take() else {
            return;
        };
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };
        let current = self
            .workspace
            .tabs
            .get(&tab)
            .map(|t| t.location().clone())
            .unwrap_or(NodeId::Home);

        match crate::address::resolve(&text, &current, neutron_shell::places::home().as_deref()) {
            Some(target) => self.navigate(tab, target),
            // Nothing resolvable. Named rather than silent: the commonest cause
            // is a variable that is not set, and the path the user typed is not
            // the path an error would otherwise report.
            None => self
                .notices
                .error(format!("Cannot work out where {text:?} is")),
        }
    }

    /// Runs an action that has already been decided on.
    ///
    /// The context is the one held on `self`, so a confirmed action can be run
    /// from the dialog's own frame rather than being queued for the next one.
    fn apply_now(&mut self, action: Action) {
        let ctx = self.ctx.clone();
        self.apply(&ctx, action);
    }

    /// Puts a question in front of an action, choosing its wording from what
    /// the action is about to do.
    ///
    /// The wording is built here rather than at each call site so the count and
    /// the place named in the button are the ones the action will actually act
    /// on — a dialog that says "3 files" while the code deletes 4 is worse than
    /// no dialog.
    fn ask(&mut self, then: Action) {
        let Some(question) = self.question_for(&then) else {
            // Nothing about this needs asking. Doing it rather than inventing a
            // dialog keeps the list of things that get one short, which is what
            // makes people read them.
            self.apply_now(then);
            return;
        };
        self.confirm = Some((question, Box::new(then)));
    }

    /// The wording for `action`, or `None` if it does not warrant asking.
    fn question_for(&self, action: &Action) -> Option<crate::confirm::Confirm> {
        match action {
            Action::Delete { .. } => {
                let place = self.workspace.active_tab()?.location();
                // Only on a server. A local delete goes to the Recycle Bin and
                // is undone from there; asking about it would train people to
                // dismiss the dialog that matters.
                let (site, _) = place.as_remote()?;
                let name = self
                    .sites
                    .get(site)
                    .map(|s| s.display().to_owned())
                    .unwrap_or_else(|| "that server".to_owned());

                let count = self.workspace.active_tab()?.selection.len();
                let what = match count {
                    1 => "1 item".to_owned(),
                    n => format!("{n} items"),
                };
                Some(crate::confirm::Confirm {
                    title: format!("Delete from {name}?"),
                    body: format!(
                        "{what} will be deleted from {name}. A server has no \
                         Recycle Bin, so this cannot be undone."
                    ),
                    proceed: format!("Delete {what}"),
                })
            }

            Action::ForgetServer => {
                let (site, _) = self.workspace.active_tab()?.location().as_remote()?;
                let name = self
                    .sites
                    .get(site)
                    .map(|s| s.display().to_owned())
                    .unwrap_or_else(|| "that server".to_owned());
                Some(crate::confirm::Confirm {
                    title: format!("Forget {name}?"),
                    body: "Its saved password is removed from Credential Manager. \
                           If you do not know the password, this cannot be undone."
                        .to_owned(),
                    proceed: "Forget it".to_owned(),
                })
            }

            _ => None,
        }
    }

    /// The confirmation dialog, drawn over everything.
    fn confirm_overlay(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let Some((question, _)) = self.confirm.as_ref() else {
            return;
        };
        match crate::confirm::show(ui, p, question) {
            crate::confirm::Answer::Waiting => {}
            crate::confirm::Answer::Cancelled => self.confirm = None,
            crate::confirm::Answer::Confirmed => {
                if let Some((_, then)) = self.confirm.take() {
                    self.apply_now(*then);
                }
            }
        }
    }

    /// Neutron's own menu for a sidebar row.
    ///
    /// Not the shell's: a sidebar row is a destination rather than a file, and
    /// half of them — the dashboard, a server, a Drive root — have no shell
    /// item behind them at all. So this menu is built here and answered here,
    /// and never reaches `IContextMenu`.
    ///
    /// It exists mostly for one thing. Pinned folders appear in this list, and
    /// until now the only way to remove one was to navigate into it and use the
    /// keystroke — which is backwards, because the sidebar is exactly where a
    /// stale pin is noticed.
    fn sidebar_menu(&mut self, id: NodeId, at: egui::Pos2) {
        let mut items = Vec::new();

        items.push(neutron_core::MenuItem {
            id: MENU_ID_NEW_TAB,
            label: "Open in a new tab".to_owned(),
            enabled: true,
            default: true,
            ..Default::default()
        });
        self.tab_target = Some(id.clone());

        // Only a place with a path can be pinned, for the reason in `pins.rs`:
        // a pin has to survive a restart, and nothing else here can be reached
        // from a string on disk without its provider being connected first.
        self.pin_target = id.as_path().map(Path::to_path_buf);
        if let Some(dir) = self.pin_target.clone() {
            let pinned = self.pins.folders.contains(&dir);
            items.push(neutron_core::MenuItem::separator());
            items.push(neutron_core::MenuItem {
                id: MENU_ID_PIN,
                label: if pinned {
                    "Unpin from Quick access".to_owned()
                } else {
                    "Pin to Quick access".to_owned()
                },
                accel: "Ctrl+Shift+D".to_owned(),
                enabled: true,
                ..Default::default()
            });
        }

        // No reply sender: nothing on an apartment thread is waiting on this
        // one, unlike a menu the shell produced.
        self.menu = Some(neutron_ui::MenuState::new(items, at));
        self.menu_reply = None;
    }

    /// The pin row Neutron adds to the shell's menu, and the folder it acts on.
    ///
    /// The target is worked out when the menu is *built*, not when a row is
    /// chosen: by then the selection may have moved, and a pin that lands on a
    /// different folder than the one right-clicked is worse than no pin at all.
    ///
    /// Right-clicking a folder pins that folder; right-clicking anywhere else
    /// pins the one being shown, which is what `Ctrl+Shift+D` does.
    fn menu_pin_row(&self) -> Option<(neutron_core::MenuItem, PathBuf)> {
        let tab = self.workspace.active_tab()?;
        let dir = tab.location().as_path()?;

        let target = tab
            .selection
            .cursor()
            .filter(|&i| i < tab.list.len())
            .filter(|&i| tab.list.kind(i).is_container())
            .filter(|&i| tab.selection.is_selected(i))
            .map(|i| dir.join(tab.list.name(i)))
            .unwrap_or_else(|| dir.to_path_buf());

        let pinned = self.pins.folders.contains(&target);
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| target.to_string_lossy().into_owned());

        Some((
            neutron_core::MenuItem {
                id: MENU_ID_PIN,
                label: if pinned {
                    format!("Unpin {name} from Quick access")
                } else {
                    format!("Pin {name} to Quick access")
                },
                accel: "Ctrl+Shift+D".to_owned(),
                enabled: true,
                ..Default::default()
            },
            target,
        ))
    }

    /// Pins or unpins `dir`, and persists it.
    ///
    /// Shared by the keyboard, the palette and the context menu, so all three
    /// give the same answer to the same question.
    fn apply_pin(&mut self, dir: PathBuf) {
        let mut pins = self.pins.clone();
        let outcome = pins.toggle(&dir);
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.to_string_lossy().into_owned());

        if outcome == crate::pins::Toggled::Full {
            // Refused rather than dropping the oldest: that would lose a pin
            // the user made on purpose, and say nothing about it.
            self.notices.info(format!(
                "Quick access holds {} folders — unpin one first",
                crate::pins::MAX
            ));
            return;
        }

        // Applied here so the sidebar changes on this frame; the write below
        // only makes it outlast the session.
        self.pins = pins.clone();
        self.notices.info(match outcome {
            crate::pins::Toggled::Pinned => format!("Pinned {name} to Quick access"),
            _ => format!("Unpinned {name}"),
        });

        let notices = self.notice_tx.clone();
        let wake = self.ctx.clone();
        self.sta.submit(move || {
            if let Err(e) = crate::pins::save(&pins) {
                report(&notices, format!("Could not save pins: {e}"));
            }
            wake.request_repaint();
        });
    }

    /// Pins the focused folder to Quick access, or unpins it if it is already
    /// there.
    ///
    /// One command for both, because which one applies is a property of where
    /// you are standing rather than something to choose from a menu.
    fn toggle_pin(&mut self) {
        let Some(dir) = self
            .workspace
            .active_tab()
            .and_then(|t| t.location().as_path())
            .map(Path::to_path_buf)
        else {
            // A pin is a path on disk. Nothing without one can be restored
            // next launch, so the refusal names the place rather than failing
            // quietly — see `place.rs`.
            self.refuse_without_a_path("pin");
            return;
        };

        self.apply_pin(dir);
    }

    /// Forgets the Google Drive credential.
    ///
    /// The counterpart to [`Self::connect_drive`]. `sign_out` has existed in
    /// `neutron-cloud` since M7 with nothing calling it, so the only way to
    /// revoke a Drive connection was to delete the entry from Credential
    /// Manager by hand — a credential the application would store and would not
    /// let go of.
    fn sign_out_drive(&mut self) {
        use neutron_cloud::google::DriveState;
        if !matches!(self.drive_state, DriveState::SignedIn) {
            // The palette's entries are static, so this one is offered whether
            // or not Drive is connected. Saying so beats doing nothing.
            self.notices.info("Google Drive is not connected");
            return;
        }

        // Optimistic, and deliberately so: the sidebar changes on this frame.
        // A failure below replaces this with the reason.
        self.drive_state = DriveState::SignedOut;
        // The loader caches an access token that outlives the credential by
        // most of an hour. Told here rather than after the worker answers,
        // because a listing must not succeed once the user has disconnected.
        self.loader.sign_out_drive();

        let wake = self.ctx.clone();
        let done = self.drive_tx.clone();
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            let drive = neutron_cloud::google::GoogleDrive::new();
            if let Err(e) = drive.sign_out() {
                tracing::error!("Google Drive sign-out failed: {e}");
                report(&notices, format!("Could not disconnect Google Drive: {e}"));
                let _ = done.send(DriveState::Error(e.to_string()));
            } else {
                report(&notices, "Google Drive disconnected".to_owned());
                let _ = done.send(DriveState::SignedOut);
            }
            wake.request_repaint();
        });
    }

    /// Removes the focused tab's server from `sites.json`, password and all.
    ///
    /// The counterpart to **Connect to a server**. Until this existed a saved
    /// server could only be removed by editing the file by hand, and
    /// `forget_password` — which is what takes the password out of Credential
    /// Manager — had no callers at all, so the password outlived every other
    /// trace of the site.
    ///
    /// Scoped to the focused tab rather than offering a list, because
    /// navigating to a server is already the unambiguous way to say which one,
    /// and a palette row cannot ask a follow-up question.
    fn forget_server(&mut self) {
        let Some(id) = self
            .workspace
            .active_tab()
            .and_then(|t| t.location().as_remote())
            .map(|(site, _)| site.to_owned())
        else {
            self.notices
                .info("Open a server first — this forgets the one you are looking at");
            return;
        };

        let Some(site) = self.sites.get(&id).cloned() else {
            self.notices.error(format!("{id} is no longer saved"));
            return;
        };
        let name = site.display().to_owned();

        let sites_tx = self.sites_tx.clone();
        let notices = self.notice_tx.clone();
        let wake = self.ctx.clone();
        self.sta.submit(move || {
            // The password goes first. If the file write fails afterwards the
            // site is still listed and can be re-entered; reversed, a failed
            // clear would leave a credential belonging to nothing, which
            // nothing would ever offer to remove again.
            if let Err(e) = neutron_remote::forget_password(&site) {
                // Not fatal to the removal — a site saved without a password
                // has nothing to clear, and Credential Manager reports that the
                // same way as a real failure.
                tracing::warn!("could not clear the password for {}: {e}", site.id);
            }

            // Re-read before writing, for the reason `connect::save` does it:
            // the loader records SSH host keys into this same file, and writing
            // a copy held in the UI over the top would silently un-learn one.
            match neutron_remote::site::load() {
                Ok(mut on_disk) => {
                    on_disk.sites.retain(|s| s.id != site.id);
                    match neutron_remote::site::save(&on_disk) {
                        Ok(_) => report_info(
                            &notices,
                            format!("Forgot {name} and its saved password"),
                        ),
                        Err(e) => {
                            report(&notices, format!("Could not update sites.json: {e}"))
                        }
                    }
                }
                Err(e) => report(&notices, format!("Could not read sites.json: {e}")),
            }

            // Sent whatever happened, so what is on screen is what is on disk
            // rather than the removal that may not have taken.
            let _ = sites_tx.send(SitesReply {
                sites: neutron_remote::site::load(),
                origin: SitesOrigin::Background,
            });
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
                let matched = crate::finder::match_commands(&self.palette, &self.finder.needle);
                let status = format!("{} of {} commands", matched.len(), self.palette.len());
                self.finder.set_command_rows(matched, status);
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
                        "Press Enter to start searching".into(),
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

                // A cached index is missing everything created since it was
                // written, and a search tool that quietly omits results is
                // worse than one that admits it might. Named where the count
                // is, because that is the number being qualified.
                let cached = match self.index.state() {
                    IndexState::Ready(status) if !status.fresh => format!(
                        "  ·  index is {} — \"Rebuild the search index\" to update it",
                        crate::index_client::describe_age(status.cached_age_secs),
                    ),
                    _ => String::new(),
                };

                let status = format!(
                    "{}{} results in {:.2} ms{cached}",
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
            // By position, through the invocation the row was built with. A
            // lookup by title would run the wrong command as soon as two
            // plugins offered the same one.
            let Some(invoke) = self.finder.invoke_at(index).cloned() else {
                return;
            };
            self.finder.close();
            let ctx = self.ctx.clone();
            match invoke {
                commands::Invoke::Builtin(id) => self.run_command(&ctx, id),
                commands::Invoke::Plugin(reference) => self.run_plugin(&ctx, &reference),
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
            C::Copy => Some(Action::Clip { cut: false }),
            C::Cut => Some(Action::Clip { cut: true }),
            C::Paste => Some(Action::Paste),
            C::Rename => Some(Action::BeginRename),
            C::NewFolder => Some(Action::NewFolder),
            C::Extract => Some(Action::ExtractSelection),
            C::Compress => Some(Action::CompressSelection),
            C::DeleteSelection => Some(Action::Delete { permanent: false }),

            C::ToggleHidden => Some(Action::ToggleHidden),
            C::ToggleView => Some(Action::ToggleView),
            C::TogglePreview => Some(Action::TogglePreview),
            C::ToggleTheme => Some(Action::ToggleTheme),

            C::FindInFolder => Some(Action::ToggleFinder(crate::finder::Mode::Files)),
            C::SearchEverything => Some(Action::ToggleFinder(crate::finder::Mode::Everything)),
            C::RefreshIndex => Some(Action::RefreshIndex),
            C::StopIndexer => {
                self.index.stop_server();
                None
            }

            C::OpenTerminal => Some(Action::OpenTerminal),
            C::NewWindow => Some(Action::NewWindow),

            C::ConnectServer => Some(Action::OpenConnect),
            C::ReloadSites => Some(Action::ReloadSites),
            // Asks first: it takes the password out of Credential Manager,
            // and the user may not know it any more.
            C::ForgetServer => Some(Action::Ask(Box::new(Action::ForgetServer))),
            C::TogglePin => Some(Action::TogglePin),

            C::SignOutDrive => Some(Action::SignOutDrive),

            C::ReloadPlugins => Some(Action::ReloadPlugins),
            C::OpenPluginsFolder => {
                self.open_plugins_folder();
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
            // The shortcuts that opened the overlay, so they still work once it
            // is open — pressing Ctrl+Shift+F again closes it, and the other
            // two switch mode. Without these they did nothing at all and the
            // keystroke's letter was typed into the needle instead, which is
            // how `neutron` became `neutronneutron-index` while testing.
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            if ctrl {
                if i.modifiers.shift && i.key_pressed(egui::Key::F) {
                    actions.push(Action::ToggleFinder(crate::finder::Mode::Everything));
                }
                if i.modifiers.shift && i.key_pressed(egui::Key::P) {
                    actions.push(Action::ToggleFinder(crate::finder::Mode::Commands));
                }
                if !i.modifiers.shift && i.key_pressed(egui::Key::P) {
                    actions.push(Action::ToggleFinder(crate::finder::Mode::Files));
                }
            }

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

    fn status_bar(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
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

                        self.archive_status(ui, p, actions);
                        self.plugin_status(ui, p, actions);
                        self.download_status(ui, p, actions);
                        self.notice_bar(ui, p, actions);
                    });
                });
            });
    }

    /// The running archive job, or what the last one did.
    ///
    /// Lives in the status bar rather than in a dialog. Extracting is not modal
    /// — there is no reason the user cannot carry on browsing while it runs —
    /// and a window that steals focus to say "43%" is the thing every other
    /// archiver gets wrong.
    fn archive_status(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        if let Some(job) = self.archive_job.as_ref() {
            if ui
                .add(egui::Button::new(egui::RichText::new("Stop").size(11.0)).small())
                .on_hover_text("Stop this archive job. What has been written already stays.")
                .clicked()
            {
                actions.push(Action::CancelArchiveJob);
            }

            let progress = job.state.progress;
            let text = if progress.total > 0 {
                format!(
                    "{} — {:.0}%",
                    job.state.label,
                    (progress.done as f64 / progress.total as f64) * 100.0
                )
            } else {
                // A tar is a stream and a zip being written has no known total,
                // so there is no percentage to give. The byte count still moves,
                // which is what says it has not hung.
                format!(
                    "{} — {}",
                    job.state.label,
                    neutron_ui::format::size(Some(progress.done))
                )
            };
            ui.colored_label(p.accent, text);
        }
    }

    /// Unpacks the selected archive into a folder beside it.
    fn extract_selection(&mut self) {
        let Some((_, dir, paths)) = self.selection_paths() else {
            return;
        };
        // One at a time, and the first selected archive is the one meant. A
        // selection of several archives is far more likely to be a stray
        // Ctrl+A than a request to unpack all of them at once.
        let Some(archive) = paths
            .iter()
            .find(|p| neutron_archive::format_of(p).is_some())
            .cloned()
        else {
            self.notices.error("That is not an archive Neutron can open");
            return;
        };
        let Some(format) = neutron_archive::format_of(&archive) else {
            return;
        };

        let _ = dir;
        self.start_archive_job(crate::archive_ops::extract(
            archive,
            format,
            self.ctx.clone(),
        ));
    }

    /// Zips the selection into a new archive beside it.
    fn compress_selection(&mut self) {
        let Some((_, dir, paths)) = self.selection_paths() else {
            return;
        };

        self.start_archive_job(crate::archive_ops::compress(paths, dir, self.ctx.clone()));
    }

    fn start_archive_job(&mut self, job: crate::archive_ops::ArchiveJob) {
        if self.archive_job.is_some() {
            // Refused rather than queued: archive work is disk-bound, so two at
            // once makes both slower, and the status bar has room for one line.
            self.notices
                .info("Already busy with an archive — wait for it to finish");
            return;
        }
        self.notices.dismiss_info();
        self.archive_job = Some(job);
    }

    /// Applies whatever the archive worker has reported.
    fn pump_archive_job(&mut self) {
        let Some(job) = self.archive_job.as_mut() else {
            return;
        };
        let Some(finished) = job.poll() else {
            return;
        };

        self.archive_job = None;
        let (level, text) = match finished {
            crate::archive_ops::Finished::Done { label, summary } => {
                if !summary.refused.is_empty() {
                    // Logged in full, summarised on screen: the status bar has
                    // one line and a hostile archive can refuse hundreds.
                    for refusal in &summary.refused {
                        tracing::warn!("archive: {refusal}");
                    }
                }
                (
                    crate::notice::Level::Info,
                    format!("{label} — {}", crate::archive_ops::describe(&summary)),
                )
            }
            crate::archive_ops::Finished::Failed { label, error } => (
                crate::notice::Level::Error,
                format!("{label} failed: {error}"),
            ),
        };
        self.notices.raise_reported(level, text);

        // The folder the result landed in is watched, so the new archive or
        // folder appears on its own. Nothing to refresh here.
    }

    /// `%APPDATA%\\Neutron\\plugins`, created if it is not there yet.
    ///
    /// Created rather than reported missing, because the only reason to ask for
    /// it is to put a plugin in it, and "the folder does not exist" is not a
    /// useful answer to that.
    /// Creating it is a write, so it happens on a worker and the pane follows
    /// once the folder is there. `%APPDATA%` is an ordinary local directory for
    /// most people and a redirected network share for some, and this code
    /// cannot tell which it is looking at.
    fn open_plugins_folder(&mut self) {
        let Some(dir) = neutron_plugin::discover::plugins_dir() else {
            self.notices
                .error("There is no %APPDATA% to keep plugins in");
            return;
        };
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };

        let goto = self.goto_tx.clone();
        let notices = self.notice_tx.clone();
        let wake = self.ctx.clone();
        self.sta.submit(move || {
            match std::fs::create_dir_all(&dir) {
                Ok(()) => {
                    let _ = goto.send((tab, NodeId::Path(dir)));
                }
                Err(e) => report(&notices, format!("Could not open the plugins folder: {e}")),
            }
            wake.request_repaint();
        });
    }

    /// Runs a plugin command against the focused tab.
    ///
    /// The folder and selection are read here, at the moment the command is
    /// chosen, rather than being carried in the action — a palette left open
    /// while another pane is clicked would otherwise act on the wrong one.
    fn run_plugin(&mut self, ctx: &egui::Context, reference: &crate::plugins::Ref) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(folder) = self
            .workspace
            .tabs
            .get(&tab_id)
            .and_then(|t| t.location().as_path())
            .map(Path::to_path_buf)
        else {
            self.refuse_without_a_path("run plugins in");
            return;
        };

        let selection = self
            .selection_paths()
            .map(|(_, _, paths)| paths)
            .unwrap_or_default();

        self.plugins
            .start(reference, tab_id, folder, selection, ctx);
    }

    /// Collects failures reported from workers, and clears a notice that has
    /// run its course.
    fn drain_notices(&mut self, ctx: &egui::Context) {
        while let Ok((level, text)) = self.notice_rx.try_recv() {
            match level {
                crate::notice::Level::Error => {
                    // Already logged where it was raised; raising it here would
                    // log it twice.
                    self.notices.raise_reported(level, text);
                }
                crate::notice::Level::Info => self.notices.info(text),
            }
        }

        // The plugin runtime keeps its own pending message rather than holding
        // a reference to the surface, so it is collected here.
        if let Some((level, text)) = self.plugins.note.take() {
            self.notices.raise_reported(level, text);
        }

        // Without this an information notice would sit there until something
        // else happened to cause a repaint.
        if let Some(remaining) = self.notices.tick() {
            ctx.request_repaint_after(remaining);
        }
    }

    /// Applies whatever the plugin worker reported.
    fn pump_plugin(&mut self, ctx: &egui::Context) {
        let Some((tab, outcome)) = self.plugins.poll() else {
            return;
        };

        if let Some(error) = &outcome.error {
            tracing::warn!("plugin: {error}");
        }

        // Navigation before the refresh, since navigating re-lists anyway.
        if let Some(path) = outcome.navigate {
            self.navigate(tab, NodeId::Path(path));
        } else if outcome.refresh {
            self.refresh_in_place(tab);
        }

        for path in outcome.open {
            self.launch(path, neutron_shell::open::Verb::Open);
        }

        ctx.request_repaint();
    }

    /// Opens a terminal at the focused tab's folder.
    ///
    /// Runs on an apartment thread like every other launch: creating a process
    /// reads the registry and can block, and the frame must not.
    fn open_terminal(&mut self) {
        let Some(dir) = self.focused_folder("open a terminal in") else {
            return;
        };
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            if let Err(e) = neutron_shell::terminal::open_at(&dir) {
                report(&notices, format!("Could not open a terminal: {e}"));
            }
        });
    }

    /// Opens a second Neutron window on the focused tab's folder.
    fn new_window(&mut self) {
        let Some(dir) = self.focused_folder("open a window in") else {
            return;
        };
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            if let Err(e) = neutron_shell::terminal::new_window(&dir) {
                report(&notices, format!("Could not open a second window: {e}"));
            }
        });
    }

    /// The focused tab's folder, or a notice saying why there isn't one.
    ///
    /// "This PC", a server and a Drive folder are all real places with no path
    /// a process can be started in, and both callers need exactly that answer.
    fn focused_folder(&mut self, what: &str) -> Option<PathBuf> {
        let location = self.workspace.active_tab()?.location().clone();
        if let Some(path) = location.as_path() {
            return Some(path.to_path_buf());
        }
        let place = crate::place::pathless(&location, &self.sites)?;
        self.notices.info(crate::place::cannot(what, &place));
        None
    }

    /// Refuses an operation that needs a filesystem path, and says where the
    /// pane is pointing instead.
    ///
    /// Every operation guarded by this reaches for `location().as_path()` and
    /// finds nothing in a shell folder, a cloud folder or a server. Without
    /// this they are silent no-ops — the user presses Delete and nothing at all
    /// happens, which reads as a bug in the key handling rather than as a
    /// missing feature.
    ///
    /// This used to check only for a server, so a zip and a Drive folder — the
    /// other two pathless cases — went on failing silently, which is the exact
    /// thing `notice.rs` exists to end. See [`crate::place`] for why the
    /// sentence differs between them.
    fn refuse_without_a_path(&mut self, what: &str) -> bool {
        let Some(place) = self
            .workspace
            .active_tab()
            .and_then(|t| crate::place::pathless(t.location(), &self.sites))
        else {
            return false;
        };
        // Information, not an error: nothing failed. The user asked for
        // something Neutron does not do here, and a receipt that clears itself
        // is the right weight for that.
        self.notices.info(crate::place::cannot(what, &place));
        true
    }

    /// Saves the dialog's server, then opens it in the focused tab.
    fn submit_connect(&mut self) {
        let site = match self.connect.to_site(&self.sites) {
            Ok(site) => site,
            Err(reason) => {
                self.connect.error = Some(reason);
                return;
            }
        };

        // Writing the site is a write to `%APPDATA%` and a call into
        // Credential Manager, so it happens on a worker. The dialog stays open
        // until the reply arrives — it is the only place the password exists,
        // and closing it before the save is known to have worked would lose it.
        let password = self.connect.password().to_owned();
        let tab = self.workspace.active_tab_id();
        let start = site.start_path();
        let target = NodeId::remote(site.id.as_str(), start.as_str());

        self.notices.info(format!("Connecting to {}…", site.address()));

        let sites_tx = self.sites_tx.clone();
        let goto = self.goto_tx.clone();
        let wake = self.ctx.clone();
        self.sta.submit(move || {
            let result = crate::connect::save(&site, &password);
            let ok = result.is_ok();
            // Re-read rather than pushing the new site onto a list held in the
            // UI: the file is the truth, and this is also how the sidebar picks
            // up an edit made by hand.
            let sites = neutron_remote::site::load();
            let _ = sites_tx.send(SitesReply {
                sites,
                origin: SitesOrigin::Dialog(result),
            });
            if ok {
                if let Some(tab) = tab {
                    let _ = goto.send((tab, target));
                }
            }
            wake.request_repaint();
        });
    }

    /// Re-reads `sites.json` on a worker, and applies it when it arrives.
    ///
    /// Reading it here would be a read of `%APPDATA%`, which is an ordinary
    /// local directory for most people and a redirected network share for some
    /// — and this code cannot tell which it is looking at.
    fn refresh_sites(&mut self, origin: SitesOrigin) {
        let sites_tx = self.sites_tx.clone();
        let wake = self.ctx.clone();
        self.sta.submit(move || {
            let _ = sites_tx.send(SitesReply {
                sites: neutron_remote::site::load(),
                origin,
            });
            wake.request_repaint();
        });
    }

    /// Fetches a remote file into the cache, then opens it with the shell.
    ///
    /// A copy, deliberately: editing it does not write back. Saying so is
    /// better than pretending to a two-way sync that this protocol layer does
    /// not do — see the note in the status bar when it lands.
    fn start_download(&mut self, site_id: &str, remote_path: String, name: String) {
        if self.download.is_some() {
            self.notices.info("Already fetching a file — wait for it to finish");
            return;
        }

        let Some(site) = self.sites.get(site_id).cloned() else {
            self.notices.error(format!("No saved server called {site_id:?}"));
            return;
        };
        let Some(destination) = crate::remote::cache_path(site_id, &remote_path) else {
            self.notices.error(format!("{name} cannot be saved under that name"));
            return;
        };

        let bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::bounded(1);

        let worker_bytes = std::sync::Arc::clone(&bytes);
        let worker_cancel = std::sync::Arc::clone(&cancel);
        let wake = self.ctx.clone();
        let target = destination.clone();

        let spawned = std::thread::Builder::new()
            .name("neutron-download".into())
            .spawn(move || {
                let outcome = crate::remote::download(
                    &site,
                    &remote_path,
                    &target,
                    &mut |done| {
                        worker_bytes.store(done, std::sync::atomic::Ordering::Relaxed);
                        // Repainting per chunk would cost more than the
                        // transfer on a fast link; the status bar is already
                        // repainting for the drifting ground.
                        if worker_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            neutron_remote::Continue::Stop
                        } else {
                            neutron_remote::Continue::Yes
                        }
                    },
                );
                let _ = tx.send(outcome.map(|_| target));
                wake.request_repaint();
            });

        match spawned {
            Ok(_) => {
                self.notices.dismiss_info();
                self.download = Some(DownloadJob {
                    label: format!("Fetching {name}"),
                    bytes,
                    cancel,
                    done: rx,
                });
            }
            Err(e) => self.notices.error(format!("Could not start the download: {e}")),
        }
    }

    /// Fetches a Drive object into the cache, then opens it with the shell.
    ///
    /// Shares [`DownloadJob`] with the server download — same progress, same
    /// Stop, same "opened a local copy" receipt — because from the user's side
    /// it is the same thing happening.
    ///
    /// The metadata request comes first and on the worker. Which of the three
    /// ways to fetch an object applies depends on its MIME type, and the
    /// listing deliberately does not keep that: it is one field the row never
    /// renders, and one request before a transfer costs nothing next to the
    /// transfer.
    fn start_drive_download(&mut self, file_id: String, name: String) {
        if self.download.is_some() {
            self.notices.info("Already fetching a file — wait for it to finish");
            return;
        }

        let Some(cache) = crate::cloud::cache_dir() else {
            self.notices.error("There is no temp directory to fetch into");
            return;
        };

        let bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::bounded(1);

        let worker_bytes = std::sync::Arc::clone(&bytes);
        let worker_cancel = std::sync::Arc::clone(&cancel);
        let wake = self.ctx.clone();

        let spawned = std::thread::Builder::new()
            .name("neutron-drive-download".into())
            .spawn(move || {
                let outcome = crate::cloud::fetch(
                    &file_id,
                    &cache,
                    &mut |done| {
                        worker_bytes.store(done, std::sync::atomic::Ordering::Relaxed);
                        if worker_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            neutron_cloud::drive::Continue::Stop
                        } else {
                            neutron_cloud::drive::Continue::Yes
                        }
                    },
                );
                let _ = tx.send(outcome);
                wake.request_repaint();
            });

        match spawned {
            Ok(_) => {
                self.notices.dismiss_info();
                self.download = Some(DownloadJob {
                    label: format!("Fetching {name}"),
                    bytes,
                    cancel,
                    done: rx,
                });
            }
            Err(e) => self.notices.error(format!("Could not start the download: {e}")),
        }
    }

    /// Applies a finished download.
    fn pump_download(&mut self) {
        let Some(job) = self.download.as_ref() else {
            return;
        };
        let Ok(outcome) = job.done.try_recv() else {
            return;
        };
        let label = job.label.clone();
        self.download = None;

        match outcome {
            Ok(path) => {
                // Named as a copy, because it is one: editing it does not write
                // back to the server, and a user who assumes otherwise loses
                // work.
                self.notices.info(format!("{label} — opened a local copy"));
                self.launch(path, neutron_shell::open::Verb::Open);
            }
            Err(reason) => self.notices.error(format!("{label} — {reason}")),
        }
    }

    /// The running download, or what the last remote operation said.
    fn download_status(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        if let Some(job) = self.download.as_ref() {
            if ui
                .add(egui::Button::new(egui::RichText::new("Stop").size(11.0)).small())
                .on_hover_text("Stop this download. The partial file is removed.")
                .clicked()
            {
                actions.push(Action::CancelDownload);
            }
            // Bytes rather than a percentage: neither protocol reliably reports
            // a size up front, and a percentage that jumps or stalls at 99 is
            // worse than a number that is plainly still moving.
            let done = job.bytes.load(std::sync::atomic::Ordering::Relaxed);
            ui.colored_label(
                p.accent,
                format!("{} — {}", job.label, neutron_ui::format::size(Some(done))),
            );
        }
    }

    /// The connect dialog, and the text fields it cannot draw itself.
    fn connect_overlay(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        if !self.connect.open {
            return;
        }

        if let Some(action) = crate::connect::show(ui, p, &self.connect) {
            actions.push(match action {
                crate::connect::ConnectAction::Close => Action::CloseConnect,
                crate::connect::ConnectAction::Submit => Action::SubmitConnect,
                crate::connect::ConnectAction::SetProtocol(protocol) => {
                    Action::SetConnectProtocol(protocol)
                }
            });
        }

        let screen = ui.ctx().content_rect();

        // Where the caret should be when nothing in the dialog has it: the
        // first field still empty, so opening the dialog starts at the top and
        // clicking a protocol chip — which takes focus away, being an ordinary
        // clickable — returns it to where there is still work to do rather than
        // dropping it into the file list behind.
        let field_id = |field| egui::Id::new(("neutron-connect-field", field));
        let focused = ui.memory(|m| m.focused());
        let ours = crate::connect::FIELDS
            .iter()
            .any(|&f| focused == Some(field_id(f)));
        let wants_caret = crate::connect::FIELDS
            .iter()
            .copied()
            .find(|&f| self.connect.value(f).is_empty())
            .unwrap_or(crate::connect::FIELDS[0]);

        for &field in crate::connect::FIELDS {
            let rect = crate::connect::field_rect(screen, field);
            let id = field_id(field);

            let mut text = self.connect.value(field).to_owned();
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layer_id(egui::LayerId::new(
                        egui::Order::Foreground,
                        "neutron-connect".into(),
                    )),
            );

            let output = egui::TextEdit::singleline(&mut text)
                .id(id)
                .password(crate::connect::is_secret(field))
                .background_color(egui::Color32::TRANSPARENT)
                .desired_width(rect.width())
                .font(egui::FontId::proportional(13.0))
                .text_color(p.text)
                .hint_text(
                    egui::RichText::new(crate::connect::hint(field, self.connect.protocol))
                        .color(p.text_faint)
                        .size(13.0),
                )
                .show(&mut child);

            if !ours && field == wants_caret {
                child.memory_mut(|m| m.request_focus(id));
            }

            if text != self.connect.value(field) {
                actions.push(Action::SetConnectField(field, text));
            }

            let _ = output;
        }

        // Handled here rather than in the main key table, which is skipped
        // while a text field has focus — which is the whole time this is open.
        ui.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                actions.push(Action::CloseConnect);
            }
            if i.key_pressed(egui::Key::Enter) {
                actions.push(Action::SubmitConnect);
            }
        });
    }

    /// The one line that says what just happened.
    ///
    /// Drawn last in the right-to-left row, so it takes whatever width is left
    /// after the running-job indicators rather than pushing them off the edge.
    /// Clicking it dismisses it — which is the only way to clear an error, and
    /// is why an error is drawn as something you can click.
    fn notice_bar(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        let Some(notice) = self.notices.current() else {
            return;
        };

        let error = notice.level == crate::notice::Level::Error;
        let text = if error {
            // The glyph survives being read out of the corner of an eye, which
            // a sentence in a slightly different colour does not.
            format!("⚠ {}", notice.text)
        } else {
            notice.text.clone()
        };

        let response = ui
            .add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .color(if error { p.danger } else { p.text_muted }),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_text(if error {
                "Click to dismiss. The full error is in the log."
            } else {
                "Click to dismiss"
            });

        if response.clicked() {
            actions.push(Action::DismissNotice);
        }
    }

    /// The running plugin command, or what the last one said.
    fn plugin_status(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        if let Some(label) = self.plugins.running_label() {
            if ui
                .add(egui::Button::new(egui::RichText::new("Stop").size(11.0)).small())
                .on_hover_text("Stop this plugin command")
                .clicked()
            {
                actions.push(Action::CancelPluginCommand);
            }
            // No percentage: the protocol has no progress, and inventing one
            // would be a lie. A named command and a Stop is the honest amount.
            ui.colored_label(p.accent, format!("{label}…"));
        }
    }

    /// Starts and stops watchers so they match the folders tabs are showing.
    ///
    /// Run every frame and diffed rather than driven by navigation events,
    /// because there are a dozen ways a pane's folder can change — navigate,
    /// back, a tab closing, a split — and one of them will always be missed.
    /// The set of visible folders is the truth; this makes the watchers agree
    /// with it.
    fn sync_watchers(&mut self, ctx: &egui::Context) {
        use std::collections::HashSet;

        // Every tab's folder, not only the visible ones. A background tab
        // that is kept current is right the moment it is switched to; watching
        // only the active tab would leave it showing whatever was true when it
        // was last on screen. The cost is a blocked thread per open tab, and
        // tabs are counted in ones.
        let wanted: HashSet<PathBuf> = self
            .workspace
            .tabs
            .values()
            .filter_map(|t| t.location().as_path().map(Path::to_path_buf))
            .collect();

        // Dropping a watcher cancels its blocking read and joins nothing — the
        // thread notices and exits on its own.
        self.watchers.retain(|path, _| wanted.contains(path));

        for path in wanted {
            if self.watchers.contains_key(&path) {
                continue;
            }
            let tx = self.changed_tx.clone();
            let wake = ctx.clone();
            let reported = path.clone();

            // A folder that cannot be watched is not an error: the pane just
            // keeps updating only when asked, exactly as it did before.
            if let Some(watcher) = neutron_shell::watch::watch(&path, move || {
                let _ = tx.send(reported.clone());
                wake.request_repaint();
            }) {
                tracing::debug!(path = %path.display(), "watching");
                self.watchers.insert(path, watcher);
            }
        }
    }

    /// Re-lists any tab showing a folder that changed on disk.
    fn drain_changes(&mut self) {
        let changed: std::collections::HashSet<PathBuf> = self.changed_rx.try_iter().collect();
        if changed.is_empty() {
            return;
        }

        // Every tab on that folder, not only the visible one: a background tab
        // that is re-listed now is correct the moment it is switched to,
        // instead of showing a stale listing until something else happens.
        let stale: Vec<TabId> = self
            .workspace
            .tabs
            .values()
            .filter(|t| {
                t.location()
                    .as_path()
                    .is_some_and(|p| changed.contains(p))
            })
            .map(|t| t.id)
            .collect();

        for tab in stale {
            self.refresh_in_place(tab);
        }
    }

    /// Keeps the preview in step with the selection, and uploads a decoded
    /// image once it arrives.
    ///
    /// Called before drawing, so the pane paints this frame's answer rather
    /// than last frame's.
    fn pump_preview(&mut self, ctx: &egui::Context) {
        if !self.preview.open {
            return;
        }

        // What the pane should be showing: the cursor row of the focused tab.
        // The cursor rather than the selection, because with several files
        // selected there is no single thing to preview, and the cursor is the
        // one the user last touched.
        let wanted = self.workspace.active_tab().and_then(|tab| {
            let idx = tab.selection.cursor().filter(|&i| i < tab.list.len())?;
            let name = tab.list.name(idx);
            let source = match tab.location() {
                // Nothing is ever selected on the dashboard, so this is
                // unreachable in practice — listed rather than wildcarded so a
                // future location has to decide for itself.
                NodeId::Home => return None,
                NodeId::Path(dir) => crate::preview::Source::Local(dir.join(name)),
                // A server's rows are addressed by joining a name to a POSIX
                // path. The name went through `safe_name` on the way into the
                // listing, and joining is checked again here.
                NodeId::Remote { site, path } => {
                    let full = neutron_remote::path::join(path, name).ok()?;
                    let saved = self.sites.get(site)?;
                    crate::preview::Source::Remote {
                        site: Box::new(saved.clone()),
                        path: full,
                        name: name.to_owned(),
                    }
                }
                // Addressed by id, like everything else in Drive. A shortcut
                // resolves to its target, so previewing one shows what it
                // points at — which is what opening it would do.
                NodeId::Cloud { .. } => {
                    let (id, _) = tab.list.target(idx)?;
                    crate::preview::Source::Drive { id: id.to_owned() }
                }
                NodeId::Shell { .. } => crate::preview::Source::Shell,
            };
            Some((source, tab.list.size(idx), tab.list.kind(idx).is_container()))
        });

        match &wanted {
            Some((source, size, is_dir)) => self.previews.request(
                source.clone(),
                *size,
                *is_dir,
                self.preview.showing.as_ref(),
            ),
            None => {
                if self.preview.showing.is_some() {
                    self.previews.clear();
                    self.preview.showing = None;
                    self.preview.content = crate::preview::Preview::Nothing;
                    self.preview.texture = None;
                }
            }
        }

        self.previews.tick();

        if let Some((source, content)) = self.previews.poll() {
            self.preview.texture = match &content {
                crate::preview::Preview::Image {
                    rgba,
                    width,
                    height,
                    ..
                } => {
                    // Uploaded here because only the UI thread may touch the
                    // graphics context. The worker hands over plain bytes,
                    // already downscaled, so this copies a few megabytes at
                    // most and does it once per file rather than per frame.
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [*width, *height],
                        rgba,
                    );
                    Some(ctx.load_texture("neutron-preview", image, egui::TextureOptions::LINEAR))
                }
                _ => None,
            };
            self.preview.showing = Some(source);
            self.preview.content = content;
        } else if self.previews.busy() && self.preview.showing != wanted.as_ref().map(|(s, _, _)| s.clone()) {
            // Something is on its way for a different file. Say so rather than
            // leaving the previous file's contents under the new name.
            self.preview.content = crate::preview::Preview::Loading;
            self.preview.texture = None;
        }
    }

    fn preview_pane(&self, ui: &mut egui::Ui, p: &Palette) {
        if !self.preview.open {
            return;
        }

        let subject = self.workspace.active_tab().and_then(|tab| {
            let idx = tab.selection.cursor().filter(|&i| i < tab.list.len())?;
            Some(crate::preview::Subject {
                name: tab.list.name(idx),
                kind: neutron_ui::file_list::kind_label(tab.list.kind(idx), tab.list.name(idx)),
                size: Some(tab.list.size(idx)),
                modified: tab.list.modified(idx),
                is_dir: tab.list.kind(idx).is_container(),
            })
        });

        egui::Panel::right("preview")
            .default_size(320.0)
            .size_range(egui::Rangef::new(220.0, 640.0))
            .frame(
                theme::card(p)
                    .outer_margin(egui::Margin {
                        left: 0,
                        right: GUTTER as i8,
                        top: GUTTER as i8,
                        bottom: GUTTER as i8,
                    })
                    .inner_margin(egui::Margin::symmetric(12, 12)),
            )
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                crate::preview::show(ui, p, &self.preview, subject);
            });
    }


    /// The action rail: a fixed strip of verbs between the sidebar and the
    /// panes. See [`crate::rail`] for why it exists in a keyboard-first
    /// application.
    fn action_rail(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        let tab = self.workspace.active_tab();
        let state = crate::rail::RailState {
            has_selection: tab.is_some_and(|t| !t.selection.is_empty()),
            has_cursor: tab.is_some_and(|t| {
                t.selection.cursor().is_some_and(|i| i < t.list.len())
            }),
        };

        egui::Panel::left("action-rail")
            .resizable(false)
            .exact_size(crate::rail::WIDTH)
            .frame(
                // No stroke: the wash inside already separates the strip
                // from the sidebar, and a border around a 64px column beside a
                // bordered sidebar is two lines doing one job.
                theme::card(p)
                    .stroke(egui::Stroke::NONE)
                    .outer_margin(egui::Margin {
                        left: GUTTER as i8,
                        right: 0,
                        top: GUTTER as i8,
                        bottom: GUTTER as i8,
                    })
                    .inner_margin(egui::Margin::symmetric(0, 10)),
            )
            .show(ui, |ui| {
                if let Some(chosen) = crate::rail::show(ui, p, &state) {
                    use crate::rail::RailAction as R;
                    actions.push(match chosen {
                        R::Copy => Action::Clip { cut: false },
                        R::Cut => Action::Clip { cut: true },
                        R::Paste => Action::Paste,
                        R::Delete => Action::Delete { permanent: false },
                        R::Rename => Action::BeginRename,
                        R::NewFolder => Action::NewFolder,
                        R::Extract => Action::ExtractSelection,
                        R::Compress => Action::CompressSelection,
                    });
                }
            });
    }

    /// The window's own title bar, above every other panel.
    ///
    /// Transparent: it sits on the ground, in the gutter the cards already
    /// leave around themselves, so the bar costs no vertical space that the
    /// layout was not spending anyway.
    fn title_bar(&self, ui: &mut egui::Ui, p: &Palette) {
        let title = self
            .workspace
            .active_tab()
            .map(|t| format!("{} — Neutron", t.location().display_name()))
            .unwrap_or_else(|| "Neutron".to_owned());

        egui::Panel::top("title-bar")
            .resizable(false)
            .exact_size(crate::chrome::HEIGHT)
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::TRANSPARENT)
                    .inner_margin(egui::Margin {
                        left: GUTTER as i8,
                        right: 0,
                        top: 0,
                        bottom: 0,
                    }),
            )
            .show(ui, |ui| crate::chrome::show(ui, p, &title, &self.snap));
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
                let dashboard = neutron_core::places::Place {
                    name: "Dashboard".to_owned(),
                    id: NodeId::Home,
                    kind: neutron_core::places::PlaceKind::KnownFolder,
                    capacity: None,
                };
                match sidebar::row(ui, p, &dashboard, current) {
                        sidebar::RowHit::Open => actions.push(Action::Navigate(NodeId::Home)),
                        sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(dashboard.id.clone(), at)),
                        sidebar::RowHit::None => {}
                }

                sidebar::section(ui, p, "Quick access");
                for place in &self.known_folders {
                    match sidebar::row(ui, p, place, current) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                            sidebar::RowHit::None => {}
                    }
                }

                for place in self.pinned_places() {
                    match sidebar::row(ui, p, &place, current) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                            sidebar::RowHit::None => {}
                    }
                }

                if !self.shell_roots.is_empty() {
                    sidebar::section(ui, p, "System");
                    for place in &self.shell_roots {
                        match sidebar::row(ui, p, place, current) {
                                sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                                sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                                sidebar::RowHit::None => {}
                        }
                    }
                }

                sidebar::section(ui, p, "Cloud");
                for place in &self.cloud {
                    match sidebar::row(ui, p, place, current) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                            sidebar::RowHit::None => {}
                    }
                }
                use neutron_cloud::google::DriveState;
                match &self.drive_state {
                    DriveState::SignedIn => {
                        let place = neutron_core::places::Place {
                            name: "Google Drive".to_owned(),
                            id: NodeId::cloud(
                                CloudProviderId::GoogleDrive,
                                neutron_cloud::drive::ROOT_ID,
                                "Google Drive",
                            ),
                            kind: neutron_core::places::PlaceKind::Cloud,
                            capacity: None,
                        };
                        match sidebar::row(ui, p, &place, current) {
                                sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                                sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                                sidebar::RowHit::None => {}
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
                    // Retryable, not a dead row. A failed sign-in is usually
                    // something the user can act on — a declined consent, a
                    // dropped connection — and making them restart the
                    // application to try again is a poor answer to it.
                    DriveState::Error(why) => {
                        if sidebar::action_row_with_hint(
                            ui,
                            p,
                            "Retry Google Drive",
                            neutron_ui::icons::Glyph::Cloud,
                            why,
                        ) {
                            actions.push(Action::ConnectDrive);
                        }
                    }
                }

                // Always shown, unlike Linux below: the section is where a
                // server gets added, so hiding it when there are none would
                // hide the only way to make one.
                sidebar::section(ui, p, "Servers");
                for site in &self.sites.sites {
                    let place = neutron_core::places::Place {
                        name: site.display().to_owned(),
                        id: NodeId::remote(site.id.as_str(), site.start_path().as_str()),
                        kind: neutron_core::places::PlaceKind::Server,
                        capacity: None,
                    };
                    match sidebar::row(ui, p, &place, current) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                            sidebar::RowHit::None => {}
                    }
                }
                if let Some(reason) = &self.sites_error {
                    sidebar::placeholder_row(
                        ui,
                        p,
                        "sites.json could not be read",
                        neutron_ui::icons::Glyph::Server,
                        reason,
                    );
                }
                if sidebar::action_row(ui, p, "Add a server…", neutron_ui::icons::Glyph::Server) {
                    actions.push(Action::OpenConnect);
                }

                // Only shown when WSL is actually installed. An empty "Linux"
                // heading on a machine without it is a permanent reminder of a
                // feature the user does not have.
                if !self.wsl.is_empty() {
                    sidebar::section(ui, p, "Linux");
                    for place in &self.wsl {
                        match sidebar::row(ui, p, place, current) {
                                sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                                sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                                sidebar::RowHit::None => {}
                        }
                    }
                }

                sidebar::section(ui, p, "Drives");
                for drive in &self.drives {
                    match sidebar::row(ui, p, drive, current) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(drive.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(drive.id.clone(), at)),
                            sidebar::RowHit::None => {}
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
            // Only in the pane it was opened on. A split showing two panes would
            // otherwise draw the same field over both breadcrumbs, bound to one
            // text id, and neither would behave.
            address: self
                .address
                .as_ref()
                .filter(|(g, _)| *g == group_id)
                .map(|(_, text)| text.as_str()),
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
                header::HeaderAction::SetAddress(text) => Action::SetAddress(text),
                header::HeaderAction::SubmitAddress => Action::SubmitAddress,
                header::HeaderAction::CancelAddress => Action::CancelAddress,
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

        // The dashboard is not a listing, so it does not go through the list
        // widget at all. It draws from what the sidebar scan already found.
        if matches!(tab.location(), NodeId::Home) {
            if let Some(hit) = crate::dashboard::show(
                ui,
                p,
                &self.drives,
                &self.quick_access(),
                &self.recent.files,
            ) {
                actions.push(match hit {
                    crate::dashboard::Chosen::Go(id) => Action::Navigate(id),
                    crate::dashboard::Chosen::Open(path) => Action::OpenRecent(path),
                    crate::dashboard::Chosen::Forget(path) => Action::ForgetRecent(path),
                });
            }
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

        // Only when the cut happened in the folder this pane is showing:
        // otherwise a file called `notes.txt` cut from somewhere else would
        // fade an unrelated `notes.txt` here.
        let cut = (self.cut_dir.as_deref() == tab.location().as_path())
            .then_some(&self.cut_names)
            .filter(|names| !names.is_empty());

        let drawn = file_list::show(
            ui,
            &mut view,
            &tab.list,
            &file_list::Marks {
                selection: &tab.selection,
                cut,
                // Only in the pane that owns it. A split showing the same
                // folder twice would otherwise draw two boxes, both bound to
                // one text field id, and neither would behave.
                rename: self
                    .rename
                    .as_ref()
                    .filter(|r| Some(r.tab) == group.active_tab())
                    .map(|r| (r.idx, r.text.as_str(), r.generation)),
            },
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
                FileListAction::Marquee { from, to, additive } => {
                    actions.push(Action::Marquee { from, to, additive })
                }
                FileListAction::ContextMenu { idx, pos } => {
                    actions.push(Action::ContextMenu { idx, pos })
                }
                FileListAction::DragOut => actions.push(Action::DragOut),
                FileListAction::RenameTyped(text) => actions.push(Action::RenameTyped(text)),
                FileListAction::RenameCommit => actions.push(Action::RenameCommit),
                FileListAction::RenameCancel => actions.push(Action::RenameCancel),
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
