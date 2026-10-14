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
    RunCommand(commands::CommandId),
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
    /// Step to the next view: list, grid, columns.
    ToggleView,
    /// Show the focused tab in this view.
    SetView(file_list::ViewMode),
    /// A click in a column other than the current one: go to `dir` and select
    /// `entry`. See `neutron_ui::columns::ColumnAction::Pick`.
    ColumnPick {
        dir: PathBuf,
        entry: Option<(String, neutron_core::EntryKind)>,
        double: bool,
    },
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
    RenameCommit(String),
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
    /// Stop the copy or move with this id.
    StopTransfer(u64),
    /// Give the chosen items a Finder tag, or take it away.
    ToggleTag { name: &'static str, on: bool },
    /// Go to the folder `.0` is in, with it selected.
    ShowInFolder(PathBuf),
    /// Reverse the last change to files.
    Undo,
    /// Make again the change last undone.
    Redo,
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
    /// Stop the running server write.
    CancelRemoteJob,
    /// Delete the selection from a server, once confirmed. See `ask`.
    DeleteOnServer,
    /// Open the connect dialog on this saved server.
    EditSite(String),
    /// Show a file in the system's Quick Look panel.
    QuickLook(PathBuf),
    /// Open Quick Look on the selection, or close it.
    QuickLookSelection,
    /// Eject the volume mounted here.
    Eject(PathBuf),
    /// Eject the volume the current folder is on.
    EjectCurrent,
    /// The Finder's AirDrop window.
    OpenAirDrop,
    /// Empty the Trash, after asking.
    EmptyTrash,
    /// System Settings, at Privacy & Security › Full Disk Access.
    OpenFullDiskAccess,
    OpenInNewTab(NodeId),
    OpenTerminalAt(PathBuf),
    /// The Finder's Get Info windows for these items.
    GetInfo(Vec<PathBuf>),
    /// Get Info for the selection.
    GetInfoSelection,
    /// Finder aliases to these items, beside them.
    MakeAlias(Vec<PathBuf>),
    /// Take the selected entries of the archive being browsed out into the
    /// folder the archive is in.
    ExtractEntries,
    /// Unpack the whole archive being browsed beside itself.
    ExtractWholeArchive,
    /// Create google.json for the user to fill in, or pick it up once they have.
    SetUpDrive,
    /// The selection's paths as text on the clipboard, one per line, or the
    /// folder's own with nothing selected.
    CopyPath,
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
    /// One tile left or right in the grid; out of or into a folder in the
    /// column view. Ignored in the list.
    MoveWithinRow { delta: isize, extend: bool },
    MoveTo { pos: usize, extend: bool },
    TypeAhead(char),

    // --- workspace ---
    FocusGroup(GroupId),
    FocusNextPane,
    SetRatio { target: GroupId, ratio: f32 },
    Split(Axis),
    NewTab(GroupId),
    /// A tab dragged within its own strip.
    ReorderTab { group: GroupId, from: usize, to: usize },
    SelectTab { group: GroupId, index: usize },
    CloseTab { group: GroupId, index: usize },
    CloseActiveTab,
    FocusTabIndex(usize),
    /// The next tab in the focused pane, or the previous one, wrapping.
    CycleTab(isize),
    /// Go to one of the known folders, by name.
    GoKnown(&'static str),
    /// Copy the selection beside itself, under Finder's "copy" names.
    Duplicate,
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
const MENU_ID_EJECT: u32 = 0x0800_0010;
/// Neutron's rows in a file's menu are numbered up from here.
const MENU_ID_OURS: u32 = 0x0800_0100;
const MENU_ID_EMPTY_TRASH: u32 = 0x0800_0011;
const MENU_ID_EDIT_SITE: u32 = 0x0800_0003;
const _: () = assert!(MENU_ID_EDIT_SITE > 0x7FFF);
const _: () = assert!(MENU_ID_EDIT_SITE < crate::plugins::MENU_ID_BASE);
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

/// The storage indices of `tab`'s selection that are on screen, in row order.
///
/// Only rows the filter shows: an entry the listing hides is never acted on,
/// whatever state the selection was left in. And in row order, because the
/// Where row `i` of `tab` is on disk: its own path when the listing says, as
/// a list of tagged files does, or its name in `dir`.
fn row_path(tab: &crate::workspace::Tab, dir: &Path, i: usize) -> PathBuf {
    match tab.list.target(i) {
        Some((target, true)) => PathBuf::from(target),
        _ => dir.join(tab.list.name(i)),
    }
}

/// selection is a set and its own order changes from run to run.
fn visible_selection(tab: &crate::workspace::Tab) -> Vec<usize> {
    let mut selected: Vec<(usize, usize)> = tab
        .selection
        .iter()
        .filter_map(|i| tab.list.rank(i).map(|rank| (rank, i)))
        .collect();
    selected.sort_unstable();
    selected.into_iter().map(|(_, i)| i).collect()
}

/// What a pending question is about: the tab it was asked in, the place that
/// tab showed, and the names selected at the time.
///
/// Compared again when the answer arrives. An action like Delete reads the
/// selection when it runs, so if the selection moved while the dialog was up
/// the files deleted would not be the ones the dialog counted.
#[derive(Debug, PartialEq, Eq)]
struct Subject {
    tab: TabId,
    location: NodeId,
    names: Vec<String>,
}

/// A write to a server, running on its own connection.
struct RemoteJob {
    /// "Uploading 3 items to files.example.com", for the status bar.
    label: String,
    /// The tab to re-read when it finishes.
    tab: TabId,
    progress: std::sync::Arc<crate::remote_ops::Progress>,
    done: crossbeam_channel::Receiver<Result<String, String>>,
    /// A folder just created, to open in its rename box once it is listed.
    rename_after: Option<String>,
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
    /// A drive just sent can be ejected.
    Ejectable(PathBuf),
    /// A disk came or went: the drives are about to be sent again.
    DrivesReset,
    /// The volume scan finished — including any devices that timed out.
    Done,
}

pub struct NeutronApp {
    theme_mode: ThemeMode,
    startup: StartupTimer,
    /// Native window handle, for the DWM title-bar theme.
    hwnd: Option<isize>,

    loader: Loader,
    /// Listings for the column view's other columns. See `crate::columns`.
    columns: crate::columns::ColumnCache,
    /// The home folder, read once: the column view starts its columns there
    /// for anything inside it, and asks every frame.
    home: Option<PathBuf>,
    workspace: Workspace,
    /// Shell COM apartments, shared by every M3 service.
    sta: neutron_shell::sta::StaPool,
    icons: IconService,
    /// Previews of what is inside files, for tiles, rows and the info column.
    thumbs: crate::thumbs::Thumbnails,

    known_folders: Vec<Place>,
    /// Folders pinned to Quick access. Read once at start-up and kept here;
    /// the file is only rewritten when one is added or removed.
    pins: crate::pins::Pins,
    /// What the open menu's Neutron-owned rows act on, decided when the menu
    /// was built rather than when a row is chosen: by then the selection may
    /// have moved.
    pin_target: Option<PathBuf>,
    tab_target: Option<NodeId>,
    /// The saved server a sidebar menu was opened on, for its "Edit…" row.
    edit_site_target: Option<String>,
    /// Where the maximise button is, shared with the window procedure so
    /// Windows can offer Snap Layouts over it.
    snap: std::sync::Arc<neutron_shell::snaplayout::MaximiseButton>,
    /// A question waiting for an answer, and the action to run on yes.
    ///
    /// One at a time: two modals stacked on each other is a state nobody can
    /// reason about, and the second question is always about something the
    /// first one may be about to change.
    confirm: Option<(crate::confirm::Confirm, Box<Action>, Option<Subject>)>,
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
    /// What the Quick Look panel was last given: the tab, its cursor, and how
    /// many items. When this no longer matches, the panel is told to follow.
    quick_look_shown: Option<QuickLookKey>,
    /// Volumes that can be ejected, by mount point.
    ejectable: std::collections::HashSet<PathBuf>,
    /// What Neutron's own items in an open menu do, by id: rows it adds to
    /// the shell's menu and to the sidebar's. The shell never sees these ids.
    menu_actions: Vec<(u32, Action)>,
    /// What `menu_rows` built, for the overlay to take; it runs through
    /// `&self`, so it cannot set `menu_actions` itself.
    pending_menu_actions: std::cell::RefCell<Vec<(u32, Action)>>,
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
    /// The write to a server in progress, if any. See `crate::remote_ops`.
    remote_job: Option<RemoteJob>,

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
    /// Copies and moves under way. Several at once: unlike archive work, a
    /// copy to a USB stick should not hold up a move within the same disk.
    transfers: Vec<crate::transfers::Job>,
    next_transfer: u64,
    /// Checked transfers, back from the worker that looked for clashes.
    planned_tx: crossbeam_channel::Sender<crate::transfers::Planned>,
    planned_rx: crossbeam_channel::Receiver<crate::transfers::Planned>,
    /// Clashes waiting for an answer, first asked first.
    clash_questions: std::collections::VecDeque<crate::transfers::Question>,
    /// What can be undone and redone.
    history: crate::undo::History,
    /// The window being drawn: the first is the root viewport.
    viewport: egui::ViewportId,
    /// Origin of the window being drawn, as `WindowState::origin`.
    origin: Option<egui::Pos2>,
    /// The other windows, each with its state; see `WindowState`.
    windows: Vec<WindowState>,
    /// Counts windows opened, to name each viewport.
    windows_opened: u64,
    mail: Mail,
    /// The window that opened Quick Look, which its arrow keys move.
    look_window: Option<egui::ViewportId>,
    /// The window a context menu being built was asked for in.
    menu_window: Option<egui::ViewportId>,
    /// Changes made and reversals finished, reported from workers.
    undo_tx: crossbeam_channel::Sender<UndoEvent>,
    undo_rx: crossbeam_channel::Receiver<UndoEvent>,

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
    /// Folders and files the system asked Neutron to open. See `main.rs`.
    opened: crossbeam_channel::Receiver<Vec<PathBuf>>,

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
    /// A browser sign-in is waiting on a worker. See `connect_drive`.
    drive_signing_in: bool,
    drive_rx: crossbeam_channel::Receiver<neutron_cloud::google::DriveState>,
}

impl NeutronApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        mut startup: StartupTimer,
        start_path: Option<String>,
        opened: crossbeam_channel::Receiver<Vec<PathBuf>>,
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
        theme::install_fonts(&cc.egui_ctx, interface_faces());

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
        let thumbs = crate::thumbs::Thumbnails::new(cc.egui_ctx.clone());
        {
            // Quick Look's panel and the context menu's worker both need to
            // wake the event loop, and winit sees neither.
            let ctx = cc.egui_ctx.clone();
            neutron_shell::quicklook::set_waker(move || ctx.request_repaint());
            let ctx = cc.egui_ctx.clone();
            neutron_shell::menubar::set_waker(move || ctx.request_repaint());
            if let Err(e) = neutron_shell::menubar::install(crate::menubar::menus()) {
                tracing::warn!(error = %e, "the menu bar could not be built");
            }
        }
        let (planned_tx, planned_rx) = crossbeam_channel::unbounded();
        let (undo_tx, undo_rx) = crossbeam_channel::unbounded();
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
        #[cfg(target_os = "macos")]
        if let Err(e) = neutron_shell::pastekey::watch(0, paste_tx.clone()) {
            tracing::warn!("{e}; Command+V will not work");
        }
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
        // Read on a worker. Whether Drive is signed in is a credential-store
        // lookup — on the Mac that is the Keychain, through a helper process,
        // and it can put up a prompt — and `main.rs` promises nothing of the
        // kind happens on this thread. Shown as not set up until the answer
        // arrives, which hides the row rather than flashing a sign-in button.
        let drive_state = neutron_cloud::google::DriveState::NotConfigured;
        {
            let done = drive_tx.clone();
            let wake = cc.egui_ctx.clone();
            sta.submit(move || {
                let _ = done.send(neutron_cloud::google::GoogleDrive::new().state());
                wake.request_repaint();
            });
        }

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
            columns: crate::columns::ColumnCache::spawn(cc.egui_ctx.clone()),
            home: neutron_shell::places::home(),
            workspace,
            sta,
            icons,
            thumbs,
            known_folders: Vec::new(),
            pins,
            pin_target: None,
            tab_target: None,
            edit_site_target: None,
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
            quick_look_shown: None,
            ejectable: std::collections::HashSet::new(),
            menu_actions: Vec::new(),
            pending_menu_actions: std::cell::RefCell::new(Vec::new()),
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
            drive_signing_in: false,
            drive_tx,
            drive_rx,
            notices: crate::notice::Notices::default(),
            notice_tx,
            notice_rx,
            sites,
            sites_error,
            connect: crate::connect::ConnectDialog::default(),
            download: None,
            remote_job: None,
            plugins,
            palette,
            archive_job: None,
            transfers: Vec::new(),
            next_transfer: 0,
            planned_tx,
            planned_rx,
            clash_questions: std::collections::VecDeque::new(),
            history: crate::undo::History::default(),
            viewport: egui::ViewportId::ROOT,
            origin: None,
            windows: Vec::new(),
            windows_opened: 0,
            mail: Mail::default(),
            look_window: None,
            menu_window: None,
            undo_tx,
            undo_rx,
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
            opened,
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
        // An index into the folder being left. Applied to the one being
        // entered, it would scroll to whichever entry happens to share it.
        t.view.scroll_to = None;
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
            Action::RunCommand(id) => self.run_command(ctx, id),
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
            Action::Up if self.in_columns() => self.column_step(-1),
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
                // The other columns are not the tab's listing, so a refresh
                // of the tab alone would leave them as they were.
                self.columns.clear();
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
            Action::BeginRename if self.on_server() => self.begin_rename(ctx),
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
            Action::RenameCommit(text) => {
                if let Some(edit) = self.rename.as_mut() { edit.text = text; }
                self.commit_rename(ctx);
            },
            Action::RenameCancel => {
                self.close_rename(ctx);
            }
            Action::NewFolder if self.on_server() => self.remote_new_folder(),
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
            Action::ToggleTag { name, on } => {
                let Some(paths) = self.chosen_paths() else { return };
                let notices = self.notice_tx.clone();
                let done = self.refresh_tx.clone();
                let tab = self.workspace.active_tab_id();
                self.sta.submit(move || {
                    if let Err(e) = neutron_shell::tags::toggle(&paths, name, on) {
                        report(&notices, format!("The tag could not be changed: {e}"));
                    }
                    if let Some(tab) = tab {
                        let _ = done.send(tab);
                    }
                });
            }
            Action::ShowInFolder(path) => {
                let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else { return };
                let Some(tab) = self.workspace.active_tab_id() else { return };
                let name = name.to_string_lossy().into_owned();
                self.navigate(tab, NodeId::path(folder));
                if let Some(t) = self.workspace.tabs.get_mut(&tab) {
                    t.reselect = Some(crate::workspace::Reselect { names: vec![name.clone()], cursor: Some(name) });
                }
            }
            Action::Undo | Action::Redo => {
                let undo = matches!(action, Action::Undo);
                if self.history.busy {
                    self.notices.info("Still putting the last change back");
                    return;
                }
                let Some(record) = (if undo { self.history.take_undo() } else { self.history.take_redo() }) else {
                    self.notices.info(if undo { "Nothing to undo" } else { "Nothing to redo" });
                    return;
                };
                self.history.busy = true;
                let events = self.undo_tx.clone();
                let wake = self.ctx.clone();
                self.sta.submit(move || {
                    let done = crate::undo::reverse(&record.step);
                    let step = done.inverse.unwrap_or(crate::undo::Step::Created(Vec::new()));
                    let record = crate::undo::Record { label: record.label, step };
                    // What macOS would not let Neutron take out of the Trash,
                    // shown in the Finder, which can.
                    if !done.blocked.is_empty() {
                        let _ = neutron_shell::open::reveal(&done.blocked);
                    }
                    let _ = events.send(UndoEvent::Reversed { undo, record, failed: done.failed, blocked: done.blocked });
                    wake.request_repaint();
                });
            }
            Action::StopTransfer(id) => {
                if let Some(job) = self.transfers.iter().find(|j| j.id == id) {
                    job.stop();
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
            Action::CancelRemoteJob => {
                if let Some(job) = self.remote_job.as_ref() {
                    job.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
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

            Action::Clip { cut: false } if self.in_archive() => self.copy_out_of_archive(),
            Action::Clip { cut: false } if self.in_tag_view() => {
                if let Some(paths) = self.chosen_paths() {
                    let notices = self.notice_tx.clone();
                    self.sta.submit(move || {
                        if let Err(e) = neutron_shell::clipboard::write(&paths, neutron_shell::fileops::Transfer::Copy) {
                            report(&notices, format!("Could not copy: {e}"));
                        }
                    });
                }
            }
            Action::Clip { cut } => {
                if !self.refuse_without_a_path(if cut { "cut from" } else { "copy from" }) {
                    self.clip_selection(cut);
                }
            }
            Action::Paste if self.on_server() => self.remote_paste(),
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
            Action::SetView(mode) => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.view.view = mode;
                    t.view.scroll_to = t.selection.cursor();
                }
            }
            Action::ColumnPick { dir, entry, double } => self.column_pick(dir, entry, double),

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

            // On a server the delete is permanent and asked about first; the
            // question names the server and the count. See `question_for`.
            Action::Delete { .. } if self.on_server() => self.ask(Action::DeleteOnServer),
            Action::DeleteOnServer => self.remote_delete(),
            Action::EditSite(id) => self.edit_site(&id),
            Action::QuickLook(path) => {
                if let Err(e) = neutron_shell::quicklook::toggle(std::slice::from_ref(&path), 0) {
                    self.notices.error(e);
                }
            }
            Action::Eject(volume) => {
                let name = volume.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let notices = self.notice_tx.clone();
                let wake = self.ctx.clone();
                self.notices.info(format!("Ejecting {name}…"));
                std::thread::spawn(move || {
                    match neutron_shell::places::eject(&volume) {
                        Ok(()) => report_info(&notices, format!("Ejected {name}")),
                        Err(why) => report(&notices, format!("{name} could not be ejected: {why}")),
                    }
                    wake.request_repaint();
                });
            }
            Action::EjectCurrent => {
                let here = self.workspace.active_tab().and_then(|t| t.location().as_path().map(Path::to_path_buf));
                let volume = here.and_then(|here| {
                    self.ejectable.iter().filter(|v| here.starts_with(v)).max_by_key(|v| v.as_os_str().len()).cloned()
                });
                match volume {
                    Some(volume) => self.apply(ctx, Action::Eject(volume)),
                    None => self.notices.info("This folder is not on a disk that can be ejected"),
                }
            }
            Action::OpenAirDrop => {
                let notices = self.notice_tx.clone();
                self.sta.submit(move || {
                    let opened = std::process::Command::new("/usr/bin/open")
                        .arg("/System/Library/CoreServices/Finder.app/Contents/Applications/AirDrop.app")
                        .status();
                    if !opened.is_ok_and(|s| s.success()) {
                        report(&notices, "AirDrop could not be opened".into());
                    }
                });
            }
            Action::OpenInNewTab(id) => self.open_in_new_tab(id),
            Action::ExtractEntries => {
                // Out to the cache, then copied beside the archive by an
                // ordinary transfer — so a name already taken there is asked
                // about, and a large entry shows its progress.
                let Some((archive, entries)) = self.archive_selection() else { return };
                let Some(folder) = archive.parent().map(Path::to_path_buf) else { return };
                let Some(tab) = self.workspace.active_tab_id() else { return };
                let planned = self.planned_tx.clone();
                let notices = self.notice_tx.clone();
                let wake = self.ctx.clone();
                self.sta.submit(move || {
                    match crate::archives::materialise(&archive, &entries) {
                        Ok(paths) => {
                            let request = crate::transfers::Request {
                                tab,
                                paths,
                                destination: folder,
                                how: neutron_shell::fileops::Transfer::Copy,
                            };
                            let _ = planned.send(crate::transfers::plan(request));
                        }
                        Err(e) => report(&notices, format!("Could not extract that: {e}")),
                    }
                    wake.request_repaint();
                });
            }
            Action::ExtractWholeArchive => {
                if let Some((archive, _)) = self.workspace.active_tab().and_then(|t| t.location().as_archive()) {
                    let archive = archive.to_path_buf();
                    if let Some(format) = neutron_archive::format_of(&archive) {
                        let ctx = self.ctx.clone();
                        self.start_archive_job(move || crate::archive_ops::extract(archive, format, ctx));
                    }
                }
            }
            Action::SetUpDrive => {
                let notices = self.notice_tx.clone();
                let done = self.drive_tx.clone();
                let wake = self.ctx.clone();
                self.sta.submit(move || {
                    use neutron_cloud::google::{DriveState, GoogleDrive};
                    // Filled in since the last look: Drive is ready to sign in.
                    let state = GoogleDrive::new().state();
                    if state != DriveState::NotConfigured {
                        let _ = done.send(state);
                        wake.request_repaint();
                        return;
                    }
                    let Some(file) = neutron_cloud::config::config_path() else {
                        report(&notices, "There is no settings folder to keep google.json in".into());
                        return;
                    };
                    if !file.exists() {
                        let template = "{\n  \"client_id\": \"\",\n  \"client_secret\": \"\"\n}\n";
                        let made = file.parent().map_or(Ok(()), std::fs::create_dir_all)
                            .and_then(|()| std::fs::write(&file, template));
                        if let Err(e) = made {
                            report(&notices, format!("google.json could not be created: {e}"));
                            return;
                        }
                    }
                    let _ = neutron_shell::open::reveal(std::slice::from_ref(&file));
                    report_info(
                        &notices,
                        "Put your Google client's client_id and client_secret in google.json, \
                         then choose Set up Google Drive again"
                            .into(),
                    );
                    wake.request_repaint();
                });
            }
            Action::CopyPath => {
                let paths = match self.selection_paths() {
                    Some((_, _, paths)) => paths,
                    None => self
                        .workspace
                        .active_tab()
                        .and_then(|t| t.location().as_path().map(|p| vec![p.to_path_buf()]))
                        .unwrap_or_default(),
                };
                if !paths.is_empty() {
                    let text = paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
                    let notices = self.notice_tx.clone();
                    let count = paths.len();
                    self.sta.submit(move || match neutron_shell::clipboard::write_text(&text) {
                        Ok(()) => report_info(&notices, if count == 1 { "Copied the path".into() } else { format!("Copied {count} paths") }),
                        Err(e) => report(&notices, format!("The path could not be copied: {e}")),
                    });
                }
            }
            Action::OpenTerminalAt(dir) => {
                let notices = self.notice_tx.clone();
                self.sta.submit(move || {
                    if let Err(e) = neutron_shell::terminal::open_at(&dir) {
                        report(&notices, format!("Could not open a terminal: {e}"));
                    }
                });
            }
            Action::GetInfoSelection => {
                if let Some(paths) = self.chosen_paths() {
                    self.apply(ctx, Action::GetInfo(paths));
                }
            }
            Action::GetInfo(paths) => {
                // The Finder's own window: permissions, tags, comments and
                // "Open with" for all files of the kind are all there, and a
                // copy of it here would be a worse one.
                self.finder_script(
                    paths,
                    &["open information window of f"],
                    "Get Info could not be opened",
                    true,
                );
            }
            Action::MakeAlias(paths) => {
                self.finder_script(
                    paths,
                    &["make new alias file at (container of f) to f"],
                    "The alias could not be made",
                    false,
                );
            }
            Action::OpenFullDiskAccess => {
                self.sta.submit(|| {
                    let _ = std::process::Command::new("/usr/bin/open")
                        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
                        .status();
                });
            }
            Action::EmptyTrash => {
                let notices = self.notice_tx.clone();
                let done = self.refresh_tx.clone();
                let tab = self.workspace.active_tab_id();
                self.sta.submit(move || {
                    // The Finder owns the Trash, including its other volumes'
                    // parts; asking it is the only complete way to empty it.
                    let output = std::process::Command::new("/usr/bin/osascript")
                        .args(["-e", "tell application \"Finder\" to empty trash"])
                        .output();
                    match output {
                        Ok(o) if o.status.success() => report_info(&notices, "Emptied the Trash".into()),
                        Ok(o) => report(&notices, format!(
                            "The Trash could not be emptied: {}",
                            String::from_utf8_lossy(&o.stderr).trim()
                        )),
                        Err(e) => report(&notices, format!("The Trash could not be emptied: {e}")),
                    }
                    if let Some(tab) = tab {
                        let _ = done.send(tab);
                    }
                });
            }
            Action::QuickLookSelection if self.in_archive() && !neutron_shell::quicklook::is_open() => {
                // Taken out first; the panel opens when the files are there.
                if let Some((archive, entries)) = self.archive_selection() {
                    self.look_window = Some(self.viewport);
                    let notices = self.notice_tx.clone();
                    self.sta.submit(move || match crate::archives::materialise(&archive, &entries) {
                        Ok(paths) => neutron_shell::quicklook::request(paths),
                        Err(e) => report(&notices, format!("Quick Look could not open that: {e}")),
                    });
                }
            }
            Action::QuickLookSelection => {
                if neutron_shell::quicklook::is_open() {
                    let _ = neutron_shell::quicklook::toggle(&[], 0);
                    self.quick_look_shown = None;
                } else if let Some((paths, index, key)) = self.quick_look_subject() {
                    self.look_window = Some(self.viewport);
                    match neutron_shell::quicklook::toggle(&paths, index) {
                        Ok(()) => self.quick_look_shown = Some(key),
                        Err(e) => self.notices.error(e),
                    }
                }
            }
            Action::Delete { permanent } if self.in_tag_view() => self.delete_selection(permanent),
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

            Action::DropFiles { group, paths, .. } if self.on_server() => {
                if let Some(tab) = self.workspace.groups.get(&group).and_then(|g| g.active_tab()) {
                    self.remote_upload(tab, paths);
                }
            }
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
            Action::MoveWithinRow { delta, .. } if self.in_columns() => self.column_step(delta),
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
            Action::ReorderTab { group, from, to } => {
                self.workspace.reorder_tab(group, from, to);
                // The drag is over; without this the next release anywhere
                // would read as a drop.
                self.tab_drag = None;
            }
            Action::FocusNextPane => self.workspace.focus_next_pane(),
            Action::SetRatio { target, ratio } => {
                self.workspace.layout.set_divider_ratio(target, ratio);
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
            // The last tab of a window closes the window, as Command-W does in
            // the Finder — unless it is the only window, which stays open
            // rather than quitting the application on a tab shortcut.
            Action::CloseActiveTab
                if self.workspace.tabs.len() == 1
                    && (self.viewport != egui::ViewportId::ROOT || !self.windows.is_empty()) =>
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Action::CloseActiveTab => {
                if let Some(tab) = active {
                    self.close_tab(ctx, tab);
                }
            }
            Action::FocusTabIndex(i) => self.workspace.focus_tab_index(i),
            Action::CycleTab(delta) => self.workspace.cycle_tab(delta),
            Action::GoKnown(name) => {
                match self.known_folders.iter().find(|p| p.name == name).map(|p| p.id.clone()) {
                    Some(id) => self.apply(ctx, Action::Navigate(id)),
                    None => self.notices.info(format!("There is no {name} folder here")),
                }
            }
            Action::Duplicate => {
                if !self.refuse_without_a_path("duplicate anything in") {
                    if let Some((tab, dir, paths)) = self.selection_paths() {
                        let request = crate::transfers::Request {
                            tab,
                            paths,
                            destination: dir,
                            how: neutron_shell::fileops::Transfer::Copy,
                        };
                        // Beside itself, so never a clash: each is named as a copy.
                        self.start_transfer(request, neutron_shell::transfer::Clash::KeepBoth);
                    }
                }
            }

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

        // Inside an archive: a folder is gone into, a file is taken out into
        // the cache and opened from there, as a file on a server is.
        if let Some((archive, inner)) = t.location().as_archive() {
            let (archive, entry) = (archive.to_path_buf(), crate::archives::join(inner, &name));
            if kind.is_container() {
                self.navigate(tab, NodeId::archive(archive, entry));
            } else {
                let notices = self.notice_tx.clone();
                self.sta.submit(move || {
                    let opened = crate::archives::materialise(&archive, std::slice::from_ref(&entry))
                        .and_then(|paths| neutron_shell::open::shell_execute(&paths[0], neutron_shell::open::Verb::Open, 0));
                    if let Err(e) = opened {
                        report(&notices, format!("{name} could not be opened: {e}"));
                    }
                });
            }
            return;
        }

        let Some(base) = t.location().as_path() else {
            return;
        };
        let full = base.join(&name);
        // An archive opens as a folder: browsed in place, as in Explorer,
        // rather than unpacked beside itself. Extract is in its menu for that.
        if !kind.is_container() && neutron_archive::format_of(&full).is_some() {
            self.navigate(tab, NodeId::archive(full, ""));
            return;
        }
        #[cfg(target_os = "macos")]
        if full.extension().is_some_and(|ext| ext == "app") && kind.is_container() {
            self.open_local(full);
            return;
        }

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
        let owned = &self.workspace.tabs;
        let wanted = take_mine(&mut self.mail.goto, |(tab, _)| owned.contains_key(tab));
        for (tab, id) in wanted {
            self.navigate(tab, id);
        }
    }

    /// Applies decisions made on an apartment thread about how to open a file.
    fn drain_open_as(&mut self) {
        let owned = &self.workspace.tabs;
        let decisions = take_mine(&mut self.mail.open_as, |(tab, ..)| owned.contains_key(tab));
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

        // On the Mac a drag is started on the UI thread and does not block:
        // AppKit runs it from the event loop this thread already owns. On
        // Windows `DoDragDrop` is a modal loop of its own, hence the worker.
        #[cfg(target_os = "macos")]
        {
            let _ = tab_id;
            if let Err(e) = neutron_shell::dragout::drag(&paths, 0) {
                self.notices.error(format!("Drag failed: {e}"));
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
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
        // A folder on disk or on a server: the two places a name can be changed.
        let Some(place) = self
            .workspace
            .tabs
            .get(&tab)
            .map(|t| t.location().clone())
            .filter(|l| l.as_path().is_some() || l.as_remote().is_some())
        else {
            return;
        };
        if let Some(tab) = self.workspace.tabs.get_mut(&tab) {
            tab.selection.apply(&tab.list, idx, SelectMode::Replace);
            tab.view.scroll_to = Some(idx);
        }
        self.renames_started += 1;
        let generation = self.renames_started;
        self.rename = Some(RenameEdit {
            tab,
            generation,
            idx,
            place,
            original: name.clone(),
            text: name,
            refresh_pending: false,
        });

        // Focused from here rather than from the widget: the box has not been
        // drawn yet, and without focus the first keystroke would go to the key
        // table instead — where it would be a navigation shortcut.
        ctx.memory_mut(|m| m.request_focus(file_list::rename_field_id(generation)));
        ctx.request_repaint();
    }

    /// Ends the rename and gives the keyboard back.
    ///
    /// Without surrendering focus the field keeps it after the box is gone, and
    /// `egui_wants_keyboard_input` stays true — which switches off every
    /// shortcut in the application until something else is clicked.
    fn close_rename(&mut self, ctx: &egui::Context) -> Option<RenameEdit> {
        let edit = self.rename.take()?;
        ctx.memory_mut(|m| m.surrender_focus(file_list::rename_field_id(edit.generation)));
        if edit.refresh_pending {
            self.refresh_in_place(edit.tab);
        }
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

        // The folder the rename was started in, not the one the tab shows
        // now. Clicking away commits, and a click on Up or a sidebar row
        // navigates in the same frame — reading the tab here would rename a
        // file of the same name in the folder just arrived at.
        if edit.place.as_remote().is_some() {
            self.remote_rename(edit.tab, &edit.place, edit.original, text);
            return;
        }
        let Some(dir) = edit.place.as_path().map(Path::to_path_buf) else {
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
        let undo = self.undo_tx.clone();
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
                } else {
                    report_info(&notices, format!("Renamed to {text}"));
                    let _ = undo.send(UndoEvent::Did(crate::undo::Record {
                        label: "Rename".into(),
                        step: crate::undo::Step::Relocated(vec![(from.clone(), dir.join(&text))]),
                    }));
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
        let undo = self.undo_tx.clone();

        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            match neutron_shell::fileops::create_folder(&dir, owner) {
                // The name is reported so the row can be found once the listing
                // is re-read: a new folder appears with its name selected, and
                // that cannot be done without knowing which row it is.
                Ok(name) => {
                    let _ = undo.send(UndoEvent::Did(crate::undo::Record {
                        label: "New Folder".into(),
                        step: crate::undo::Step::Created(vec![dir.join(&name)]),
                    }));
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
        let paths: Vec<PathBuf> = visible_selection(tab)
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
            } else {
                report_info(&notices, if cut {
                    format!("Cut {} items — choose a destination and Paste", paths.len())
                } else { format!("Copied {} items to the clipboard", paths.len()) });
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
        if self.menu.is_some() || self.confirm.is_some() || !self.clash_questions.is_empty() || ctx.egui_wants_keyboard_input() {
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

    /// Starts checked transfers, queues questions about clashes, and reports
    /// transfers that have ended.
    fn pump_transfers(&mut self) {
        while let Ok(event) = self.undo_rx.try_recv() {
            match event {
                UndoEvent::Did(record) => self.history.record(record),
                UndoEvent::Reversed { undo, record, failed, blocked } => {
                    self.history.busy = false;
                    let label = record.label.clone();
                    if undo {
                        self.history.undone(record);
                    } else {
                        self.history.redone(record);
                    }
                    let verb = if undo { "Undid" } else { "Redid" };
                    if !blocked.is_empty() {
                        self.notices.error(format!(
                            "macOS lets only apps with Full Disk Access take things out of the Trash. {} selected \
                             in the Finder — choose File › Put Back there, or turn on Full Disk Access for Neutron \
                             in System Settings.",
                            if blocked.len() == 1 { "It is" } else { "They are" }
                        ));
                    } else if failed.is_empty() {
                        self.notices.info(format!("{verb} {label}"));
                    } else {
                        for line in &failed {
                            tracing::warn!("undo: {line}");
                        }
                        self.notices.error(format!("{verb} {label}, except: {}", failed.join("; ")));
                    }
                    if let Some(tab) = self.workspace.active_tab_id() {
                        let _ = self.refresh_tx.send(tab);
                    }
                }
            }
        }

        while let Ok(planned) = self.planned_rx.try_recv() {
            match planned {
                crate::transfers::Planned::Clear(request) => {
                    // Nothing is in the way, so how clashes are settled does
                    // not matter; Keep Both is the one that can never lose a
                    // file to a name that appears in the meantime.
                    self.start_transfer(request, neutron_shell::transfer::Clash::KeepBoth);
                }
                crate::transfers::Planned::Clashes(request, names) => {
                    self.clash_questions.push_back(crate::transfers::Question { request, names });
                }
                crate::transfers::Planned::Refused(why) => self.notices.error(why),
            }
        }

        let mut finished = Vec::new();
        self.transfers.retain(|job| match job.poll() {
            Some(outcome) => {
                finished.push((job.request.clone(), outcome));
                false
            }
            None => true,
        });
        for (request, outcome) in finished {
            if let Ok(report) = &outcome {
                let beside = request.paths.iter().all(|p| p.parent() == Some(request.destination.as_path()));
                let record = match request.how {
                    neutron_shell::fileops::Transfer::Move => crate::undo::Record {
                        label: "Move".into(),
                        step: crate::undo::Step::Relocated(report.moved.clone()),
                    },
                    neutron_shell::fileops::Transfer::Copy => crate::undo::Record {
                        label: if beside { "Duplicate".into() } else { "Copy".into() },
                        step: crate::undo::Step::Created(report.moved.iter().map(|(_, to)| to.clone()).collect()),
                    },
                };
                self.history.record(record);
            }
            let (error, text) = crate::transfers::summary(&request, &outcome);
            if error {
                self.notices.error(text);
            } else {
                self.notices.info(text);
            }
            // The folder watcher sees the change too; this makes it certain.
            let _ = self.refresh_tx.send(request.tab);
        }

        if !self.transfers.is_empty() {
            // The bar moves without input; nothing else would redraw it.
            self.ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    fn start_transfer(&mut self, request: crate::transfers::Request, clash: neutron_shell::transfer::Clash) {
        self.next_transfer += 1;
        self.notices.dismiss_info();
        let job = crate::transfers::Job::start(self.next_transfer, request, clash, self.ctx.clone());
        self.transfers.push(job);
    }

    /// The question about clashing names, when there is one.
    fn clash_overlay(&mut self, ui: &mut egui::Ui, p: &Palette) {
        // One question at a time, and never over the delete confirmation.
        if self.confirm.is_some() {
            return;
        }
        let Some(question) = self.clash_questions.front() else {
            return;
        };
        match crate::confirm::ask_clash(ui, p, &question.title(), &question.body()) {
            crate::confirm::ClashAnswer::Waiting => {}
            crate::confirm::ClashAnswer::Cancelled => {
                self.clash_questions.pop_front();
            }
            crate::confirm::ClashAnswer::Chose(clash) => {
                if let Some(question) = self.clash_questions.pop_front() {
                    self.start_transfer(question.request, clash);
                }
            }
        }
    }

    /// Each copy or move under way: what it is doing, a bar, and Stop.
    fn transfer_status(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        for job in &self.transfers {
            if ui
                .add(egui::Button::new(egui::RichText::new("Stop").size(11.0)).small())
                .on_hover_text("Stop. The item being copied is removed; what is already done stays done.")
                .clicked()
            {
                actions.push(Action::StopTransfer(job.id));
            }
            ui.label(egui::RichText::new(job.status()).color(p.text_soft()));
            let (rect, _) = ui.allocate_exact_size(egui::vec2(90.0, 6.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, theme::pill(6.0), p.inset);
            let fill = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * job.fraction(), rect.height()));
            if fill.width() >= 1.0 {
                ui.painter().rect_filled(fill, theme::pill(6.0), p.accent);
            }
        }
    }

    /// Sends any tab showing a disk that is no longer mounted to the
    /// dashboard. Run when a rescan of the drives finishes: a disk ejected,
    /// from here or anywhere else, leaves its tabs pointing at nothing.
    fn leave_missing_volumes(&mut self) {
        let mounted: Vec<PathBuf> = self.drives.iter().filter_map(|d| d.id.as_path().map(Path::to_path_buf)).collect();
        let stranded: Vec<TabId> = self
            .workspace
            .tabs
            .iter()
            .filter_map(|(id, tab)| {
                let path = tab.location().as_path()?;
                let on_volume = path.starts_with("/Volumes/") && path.components().count() >= 3;
                let gone = on_volume && !mounted.iter().any(|m| m != Path::new("/") && path.starts_with(m));
                gone.then_some(*id)
            })
            .collect();
        for tab in stranded {
            self.navigate(tab, NodeId::Home);
        }
    }

    /// What Quick Look should show: the selection when there are several, or
    /// the item under the cursor, with where in it the cursor is.
    fn quick_look_subject(&self) -> Option<(Vec<PathBuf>, usize, QuickLookKey)> {
        let tab = self.workspace.active_tab()?;
        let dir = tab.location().as_path().map(Path::to_path_buf).unwrap_or_default();
        // Somewhere with neither a folder nor rows that lead to files has
        // nothing for Quick Look.
        if tab.location().as_path().is_none() && !self.in_tag_view() {
            return None;
        }
        let cursor = tab.selection.cursor().filter(|&i| i < tab.list.len());
        let selected = visible_selection(tab);
        let (items, index) = if selected.len() > 1 {
            let at = cursor.and_then(|c| selected.iter().position(|&i| i == c)).unwrap_or(0);
            (selected, at)
        } else {
            (vec![cursor?], 0)
        };
        let paths: Vec<PathBuf> = items.iter().map(|&i| row_path(tab, &dir, i)).collect();
        let key = (tab.id, cursor, paths.len());
        Some((paths, index, key))
    }

    /// Keeps an open Quick Look panel on the cursor as it moves.
    fn quick_look_follow(&mut self) {
        if self.quick_look_shown.is_none() || !neutron_shell::quicklook::is_open() {
            self.quick_look_shown = None;
            return;
        }
        if let Some((paths, index, key)) = self.quick_look_subject() {
            if self.quick_look_shown != Some(key) {
                neutron_shell::quicklook::follow(&paths, index);
                self.quick_look_shown = Some(key);
            }
        }
    }

    /// Runs `lines` in the Finder for each of `paths`, bound to `f`.
    ///
    /// Paths are passed as arguments, never pasted into the script, so a name
    /// with a quote in it cannot change what the script does. The first use
    /// asks the user to let Neutron control the Finder, which is the system's
    /// own consent and the only way to these.
    fn finder_script(&self, paths: Vec<PathBuf>, lines: &[&str], failed: &'static str, bring_forward: bool) {
        if paths.is_empty() {
            return;
        }
        let mut script = vec![
            "on run argv".to_owned(),
            "tell application \"Finder\"".to_owned(),
            "repeat with p in argv".to_owned(),
            "set f to (POSIX file (p as text)) as alias".to_owned(),
        ];
        script.extend(lines.iter().map(|l| (*l).to_owned()));
        script.push("end repeat".to_owned());
        if bring_forward {
            script.push("activate".to_owned());
        }
        script.extend(["end tell".to_owned(), "end run".to_owned()]);
        let notices = self.notice_tx.clone();
        let done = self.refresh_tx.clone();
        let tab = self.workspace.active_tab_id();
        self.sta.submit(move || {
            let mut command = std::process::Command::new("/usr/bin/osascript");
            for line in &script {
                command.arg("-e").arg(line);
            }
            command.arg("--").args(&paths);
            match command.output() {
                Ok(o) if o.status.success() => {}
                Ok(o) => report(&notices, format!("{failed}: {}", String::from_utf8_lossy(&o.stderr).trim())),
                Err(e) => report(&notices, format!("{failed}: {e}")),
            }
            if let Some(tab) = tab {
                let _ = done.send(tab);
            }
        });
    }

    /// Neutron's rows for a file's menu, or for the folder's own when nothing
    /// is selected, as labels and what each does.
    fn menu_rows(&self) -> Vec<neutron_core::MenuItem> {
        use neutron_core::MenuItem;
        let mut rows: Vec<MenuItem> = Vec::new();
        let mut actions: Vec<(u32, Action)> = Vec::new();
        let mut next = MENU_ID_OURS;
        let mut row = |label: &str, action: Action, actions: &mut Vec<(u32, Action)>| -> MenuItem {
            next += 1;
            actions.push((next, action));
            MenuItem { id: next, label: label.to_owned(), enabled: true, ..Default::default() }
        };

        let Some(tab) = self.workspace.active_tab() else { return rows };
        let Some(dir) = tab.location().as_path() else { return rows };
        let selected = visible_selection(tab);

        if selected.is_empty() {
            let views = [
                ("List", file_list::ViewMode::List),
                ("Grid", file_list::ViewMode::Grid),
                ("Columns", file_list::ViewMode::Columns),
            ];
            let children = views
                .into_iter()
                .map(|(label, mode)| MenuItem { checked: tab.view.view == mode, ..row(label, Action::SetView(mode), &mut actions) })
                .collect();
            rows.push(MenuItem { label: "View as".into(), enabled: true, children, ..Default::default() });
            let sorts = [
                ("Name", neutron_core::SortColumn::Name),
                ("Kind", neutron_core::SortColumn::Kind),
                ("Date Modified", neutron_core::SortColumn::Modified),
                ("Date Created", neutron_core::SortColumn::Created),
                ("Size", neutron_core::SortColumn::Size),
            ];
            let children = sorts
                .into_iter()
                .map(|(label, column)| MenuItem {
                    checked: tab.view.sort.column == column,
                    ..row(label, Action::SortBy(column), &mut actions)
                })
                .collect();
            rows.push(MenuItem { label: "Sort By".into(), enabled: true, children, ..Default::default() });
            let hidden = if tab.view.show_hidden { "Hide Hidden Files" } else { "Show Hidden Files" };
            rows.push(row(hidden, Action::ToggleHidden, &mut actions));
        } else {
            let paths: Vec<PathBuf> = selected.iter().map(|&i| dir.join(tab.list.name(i))).collect();
            if let [only] = selected.as_slice() {
                let path = dir.join(tab.list.name(*only));
                rows.push(row("Rename", Action::BeginRename, &mut actions));
                if crate::columns::is_package(&path) {
                    rows.push(row("Show Package Contents", Action::Navigate(NodeId::path(&path)), &mut actions));
                } else if tab.list.kind(*only).is_container() {
                    rows.push(row("Open in New Tab", Action::OpenInNewTab(NodeId::path(&path)), &mut actions));
                    rows.push(row("Open in Terminal", Action::OpenTerminalAt(path.clone()), &mut actions));
                }
            }
            if paths.iter().any(|p| neutron_archive::format_of(p).is_some()) {
                rows.push(row("Extract", Action::ExtractSelection, &mut actions));
            }
            if cfg!(target_os = "macos") {
                rows.push(MenuItem { accel: neutron_ui::format::shortcut("Cmd+I"), ..row("Get Info", Action::GetInfo(paths.clone()), &mut actions) });
                rows.push(row("Make Alias", Action::MakeAlias(paths), &mut actions));
                // Ticked where every selected item has the tag.
                let bits: Vec<u8> = selected.iter().map(|&i| tab.list.tag_colors(i)).collect();
                let children = neutron_shell::tags::COLORS
                    .iter()
                    .map(|&(name, color)| {
                        let all = bits.iter().all(|b| b & (1 << (color - 1)) != 0);
                        MenuItem { checked: all, ..row(name, Action::ToggleTag { name, on: !all }, &mut actions) }
                    })
                    .collect();
                rows.push(MenuItem { label: "Tags".into(), enabled: true, children, ..Default::default() });
            }
        }
        // Remembered for the answer; see `context_menu_overlay`.
        self.pending_menu_actions.replace(actions);
        rows
    }

    /// The menu for a row inside an archive, or for the archive folder itself.
    fn archive_menu(&mut self, on_row: bool, at: egui::Pos2) {
        use neutron_core::MenuItem;
        self.menu_actions.clear();
        let mut items = Vec::new();
        let mut next = MENU_ID_OURS;
        let mut add = |label: &str, action: Action, actions: &mut Vec<(u32, Action)>, default: bool| {
            next += 1;
            actions.push((next, action));
            MenuItem { id: next, label: label.to_owned(), enabled: true, default, ..Default::default() }
        };
        if on_row {
            if let Some(cursor) = self.workspace.active_tab().and_then(|t| t.selection.cursor()) {
                items.push(add("Open", Action::Activate(cursor), &mut self.menu_actions, true));
            }
            items.push(add("Quick Look", Action::QuickLookSelection, &mut self.menu_actions, false));
            items.push(MenuItem::separator());
            items.push(add("Copy", Action::Clip { cut: false }, &mut self.menu_actions, false));
            items.push(add("Extract to the Archive's Folder", Action::ExtractEntries, &mut self.menu_actions, false));
        } else {
            items.push(add("Extract All", Action::ExtractWholeArchive, &mut self.menu_actions, false));
        }
        self.menu = Some(neutron_ui::MenuState::new(items, at));
        self.menu_reply = None;
    }

    /// Whether the focused tab is showing the files with a tag.
    fn in_tag_view(&self) -> bool {
        self.workspace.active_tab().is_some_and(|t| matches!(t.location(), NodeId::Tag { .. }))
    }

    /// The real paths of what is selected: joined to the folder, or — in a
    /// list of tagged files, which come from everywhere — each row's own.
    fn chosen_paths(&self) -> Option<Vec<PathBuf>> {
        if let Some((_, _, paths)) = self.selection_paths() {
            return Some(paths);
        }
        let tab = self.workspace.active_tab()?;
        if !matches!(tab.location(), NodeId::Tag { .. }) {
            return None;
        }
        let paths: Vec<PathBuf> = visible_selection(tab)
            .into_iter()
            .filter_map(|i| match tab.list.target(i) {
                Some((target, true)) => Some(PathBuf::from(target)),
                _ => None,
            })
            .collect();
        (!paths.is_empty()).then_some(paths)
    }

    /// The menu for a list of tagged files: what works on files wherever they
    /// are, and the way to the folder one is in.
    fn tag_menu(&mut self, on_row: bool, at: egui::Pos2) {
        use neutron_core::MenuItem;
        fn add(next: &mut u32, label: &str, action: Action, actions: &mut Vec<(u32, Action)>, default: bool) -> MenuItem {
            *next += 1;
            actions.push((*next, action));
            MenuItem { id: *next, label: label.to_owned(), enabled: true, default, ..Default::default() }
        }
        self.menu_actions.clear();
        let paths = self.chosen_paths().unwrap_or_default();
        if !on_row || paths.is_empty() {
            return;
        }
        let mut next = MENU_ID_OURS;
        let mut items = Vec::new();
        if let Some(cursor) = self.workspace.active_tab().and_then(|t| t.selection.cursor()) {
            items.push(add(&mut next, "Open", Action::Activate(cursor), &mut self.menu_actions, true));
        }
        items.push(add(&mut next, "Quick Look", Action::QuickLookSelection, &mut self.menu_actions, false));
        if let [only] = paths.as_slice() {
            items.push(add(&mut next, "Show in Enclosing Folder", Action::ShowInFolder(only.clone()), &mut self.menu_actions, false));
        }
        items.push(MenuItem::separator());
        items.push(add(&mut next, "Copy", Action::Clip { cut: false }, &mut self.menu_actions, false));
        items.push(add(&mut next, "Get Info", Action::GetInfo(paths.clone()), &mut self.menu_actions, false));
        let tags = self.tags_submenu(&mut next);
        items.push(tags);
        items.push(MenuItem::separator());
        items.push(add(&mut next, "Move to Trash", Action::Delete { permanent: false }, &mut self.menu_actions, false));
        self.menu = Some(neutron_ui::MenuState::new(items, at));
        self.menu_reply = None;
    }

    /// The Tags submenu for the selection: each colour, ticked when every
    /// selected item has it.
    fn tags_submenu(&mut self, next: &mut u32) -> neutron_core::MenuItem {
        use neutron_core::MenuItem;
        let bits: Vec<u8> = self
            .workspace
            .active_tab()
            .map(|t| visible_selection(t).into_iter().map(|i| t.list.tag_colors(i)).collect())
            .unwrap_or_default();
        let children = neutron_shell::tags::COLORS
            .iter()
            .map(|&(name, color)| {
                let all = !bits.is_empty() && bits.iter().all(|b| b & (1 << (color - 1)) != 0);
                *next += 1;
                self.menu_actions.push((*next, Action::ToggleTag { name, on: !all }));
                MenuItem { id: *next, label: name.to_owned(), enabled: true, checked: all, ..Default::default() }
            })
            .collect();
        MenuItem { label: "Tags".into(), enabled: true, children, ..Default::default() }
    }

    /// Whether the focused tab is inside an archive.
    fn in_archive(&self) -> bool {
        self.workspace.active_tab().is_some_and(|t| t.location().as_archive().is_some())
    }

    /// The archive the focused tab is inside, and the selected entries' paths
    /// within it — or the cursor's, with nothing selected.
    fn archive_selection(&self) -> Option<(PathBuf, Vec<String>)> {
        let tab = self.workspace.active_tab()?;
        let (archive, inner) = tab.location().as_archive()?;
        let mut rows = visible_selection(tab);
        if rows.is_empty() {
            rows.extend(tab.selection.cursor().filter(|&i| i < tab.list.len()));
        }
        if rows.is_empty() {
            return None;
        }
        let entries = rows.iter().map(|&i| crate::archives::join(inner, tab.list.name(i))).collect();
        Some((archive.to_path_buf(), entries))
    }

    /// Copy inside an archive: the entries are taken out into the cache and
    /// those files go on the clipboard, so a paste anywhere copies them out.
    fn copy_out_of_archive(&mut self) {
        let Some((archive, entries)) = self.archive_selection() else {
            return;
        };
        let notices = self.notice_tx.clone();
        let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.sta.submit(move || {
            let copied = crate::archives::materialise(&archive, &entries).and_then(|paths| {
                neutron_shell::clipboard::write(&paths, neutron_shell::fileops::Transfer::Copy).map(|()| paths.len())
            });
            match copied {
                Ok(1) => report_info(&notices, format!("Copied 1 item out of {name} — paste to put it somewhere")),
                Ok(n) => report_info(&notices, format!("Copied {n} items out of {name} — paste to put them somewhere")),
                Err(e) => report(&notices, format!("Could not copy out of {name}: {e}")),
            }
        });
    }

    /// Whether a question or a menu has the window: keys and menu commands
    /// wait until it is answered.
    fn modal(&self) -> bool {
        self.confirm.is_some() || !self.clash_questions.is_empty() || self.menu.is_some() || self.connect.open
    }

    /// What a menu bar item does, now.
    fn menu_command(&self, cmd: crate::menubar::Cmd) -> Option<Action> {
        use crate::menubar::Cmd as C;
        use neutron_core::SortColumn;
        let selection = || self.selection_paths().map(|(_, _, paths)| paths);
        Some(match cmd {
            C::NewWindow => Action::NewWindow,
            C::NewTab => Action::NewTab(self.workspace.focused_group()?.id),
            C::NewFolder => Action::NewFolder,
            C::Open => Action::Activate(self.workspace.active_tab()?.selection.cursor()?),
            C::CloseTab => Action::CloseActiveTab,
            C::GetInfo => Action::GetInfoSelection,
            C::Rename => Action::BeginRename,
            C::Duplicate => Action::Duplicate,
            C::MakeAlias => Action::MakeAlias(selection()?),
            C::QuickLook => Action::QuickLookSelection,
            C::Compress => Action::CompressSelection,
            C::Extract => Action::ExtractSelection,
            C::MoveToTrash => Action::Delete { permanent: false },
            C::DeleteImmediately => Action::Ask(Box::new(Action::Delete { permanent: true })),
            C::Eject => Action::EjectCurrent,
            C::AddToSidebar => Action::TogglePin,
            C::Undo => Action::Undo,
            C::Redo => Action::Redo,
            C::Cut => Action::Clip { cut: true },
            C::Copy => Action::Clip { cut: false },
            C::Paste => Action::Paste,
            C::SelectAll => Action::SelectAll,
            C::CopyPath => Action::CopyPath,
            C::Filter => Action::FocusFilter,
            C::SearchEverywhere => Action::ToggleFinder(Mode::Everything),
            C::FindFile => Action::ToggleFinder(Mode::Files),
            C::CommandPalette => Action::ToggleFinder(Mode::Commands),
            C::AsGrid => Action::SetView(file_list::ViewMode::Grid),
            C::AsList => Action::SetView(file_list::ViewMode::List),
            C::AsColumns => Action::SetView(file_list::ViewMode::Columns),
            C::SortName => Action::SortBy(SortColumn::Name),
            C::SortKind => Action::SortBy(SortColumn::Kind),
            C::SortModified => Action::SortBy(SortColumn::Modified),
            C::SortCreated => Action::SortBy(SortColumn::Created),
            C::SortSize => Action::SortBy(SortColumn::Size),
            C::ShowPreview => Action::TogglePreview,
            C::ShowHidden => Action::ToggleHidden,
            C::SplitRight => Action::Split(Axis::Horizontal),
            C::SplitDown => Action::Split(Axis::Vertical),
            C::AddressBar | C::GoToFolder => Action::ToggleAddress,
            C::ToggleTheme => Action::ToggleTheme,
            C::Back => Action::Back,
            C::Forward => Action::Forward,
            C::Enclosing => Action::Up,
            C::Dashboard => Action::Navigate(NodeId::Home),
            C::Home => Action::GoKnown("Home"),
            C::Desktop => Action::GoKnown("Desktop"),
            C::Documents => Action::GoKnown("Documents"),
            C::Downloads => Action::GoKnown("Downloads"),
            C::Applications => Action::GoKnown("Applications"),
            C::Trash => Action::Navigate(NodeId::path(&trash_folder()?)),
            C::Connect => Action::OpenConnect,
            C::AirDrop => Action::OpenAirDrop,
            C::Terminal => Action::OpenTerminal,
            C::NextTab => Action::CycleTab(1),
            C::PreviousTab => Action::CycleTab(-1),
            C::NextPane => Action::FocusNextPane,
        })
    }

    /// Tells the menu bar which items are available and which are ticked.
    fn publish_menu_state(&self, ctx: &egui::Context) {
        if !cfg!(target_os = "macos") {
            return;
        }
        use crate::menubar::Cmd as C;
        let mut disabled = std::collections::HashSet::new();
        let mut checked = std::collections::HashSet::new();
        if self.modal() {
            disabled.extend(C::all().map(C::tag));
            neutron_shell::menubar::set_state(disabled, checked);
            return;
        }
        // A text field has the keyboard: its editing keys must reach it.
        let typing = ctx.egui_wants_keyboard_input();
        let tab = self.workspace.active_tab();
        let selected = tab.map(visible_selection).unwrap_or_default();
        let dir = tab.and_then(|t| t.location().as_path());
        let archive = match (tab, dir) {
            (Some(t), Some(d)) => selected.iter().any(|&i| neutron_archive::format_of(&d.join(t.list.name(i))).is_some()),
            _ => false,
        };
        let ejectable = dir.is_some_and(|d| self.ejectable.iter().any(|v| d.starts_with(v)));
        let tabs = self.workspace.focused_group().map_or(0, |g| g.tabs.len());
        for cmd in C::all() {
            let off = match cmd {
                c if typing && c.edits_text() => true,
                C::Undo => self.history.busy || self.history.undo_label().is_none(),
                C::Redo => self.history.busy || self.history.redo_label().is_none(),
                C::Open | C::GetInfo | C::Rename | C::Duplicate | C::MakeAlias | C::QuickLook | C::Compress
                | C::MoveToTrash | C::DeleteImmediately | C::Cut | C::Copy => selected.is_empty(),
                C::Extract => !archive,
                C::NewFolder | C::Paste | C::Terminal => dir.is_none(),
                C::Back => !tab.is_some_and(|t| t.history.can_go_back()),
                C::Forward => !tab.is_some_and(|t| t.history.can_go_forward()),
                C::Eject => !ejectable,
                C::NextTab | C::PreviousTab => tabs < 2,
                _ => false,
            };
            if off {
                disabled.insert(cmd.tag());
            }
        }
        if let Some(t) = tab {
            checked.insert(match t.view.view {
                file_list::ViewMode::Grid => C::AsGrid,
                file_list::ViewMode::List => C::AsList,
                file_list::ViewMode::Columns => C::AsColumns,
            }.tag());
            checked.insert(match t.view.sort.column {
                neutron_core::SortColumn::Name => C::SortName,
                neutron_core::SortColumn::Kind => C::SortKind,
                neutron_core::SortColumn::Modified => C::SortModified,
                neutron_core::SortColumn::Created => C::SortCreated,
                neutron_core::SortColumn::Size => C::SortSize,
            }.tag());
            if t.view.show_hidden {
                checked.insert(C::ShowHidden.tag());
            }
        }
        if self.preview.open {
            checked.insert(C::ShowPreview.tag());
        }
        neutron_shell::menubar::set_state(disabled, checked);
        // "Undo Move to Trash", as the Finder names it.
        let mut titles = std::collections::HashMap::new();
        titles.insert(C::Undo.tag(), self.history.undo_label().map_or("Undo".to_owned(), |l| format!("Undo {l}")));
        titles.insert(C::Redo.tag(), self.history.redo_label().map_or("Redo".to_owned(), |l| format!("Redo {l}")));
        neutron_shell::menubar::set_titles(titles);
    }

    fn clear_cut_marks(&mut self) {
        self.cut_dir = None;
        self.cut_names.clear();
    }

    /// Pastes the clipboard into the focused tab's folder.
    ///
    /// Reading the clipboard is itself a blocking call — another process may
    /// hold it — so reading it and checking the destination happen on the
    /// worker. What comes back is a checked transfer, or a question about
    /// clashing names; see `pump_transfers`.
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

        let wake = self.ctx.clone();
        let notices = self.notice_tx.clone();
        let planned = self.planned_tx.clone();
        self.sta.submit(move || {
            let Some((paths, how)) = neutron_shell::clipboard::read() else {
                report_info(&notices, "No files on the clipboard — copy or cut files first".into());
                wake.request_repaint();
                return;
            };
            // On the worker, so a stat is fine here. A cut that has already
            // been pasted is the usual case: the clipboard still names the
            // files, but they have moved, and "into itself" was the wrong
            // thing to say about it.
            if !paths.iter().any(|p| p.exists()) {
                report_info(
                    &notices,
                    "The files on the clipboard are no longer there — a cut can only be pasted once".into(),
                );
                wake.request_repaint();
                return;
            }
            if !neutron_shell::clipboard::can_paste_into(&paths, &dir) {
                report(
                    &notices,
                    "Cannot paste a folder into itself".to_owned(),
                );
                wake.request_repaint();
                return;
            }

            // Checked here, carried out by a job of its own: see
            // `transfers`. The listing is refreshed when the job ends.
            let request = crate::transfers::Request { tab: tab_id, paths, destination: dir, how };
            let _ = planned.send(crate::transfers::plan(request));
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
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(paths) = self.chosen_paths() else {
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
        let undo = self.undo_tx.clone();
        self.sta.submit(move || {
            // To the Trash, the way that says where each item went, so Undo
            // can put them back. A permanent delete has nothing to undo.
            let outcome = if permanent {
                neutron_shell::fileops::delete(&paths, disposal, owner)
            } else {
                neutron_shell::fileops::trash(&paths).map(|went| {
                    let items: Vec<(PathBuf, PathBuf)> = paths
                        .iter()
                        .cloned()
                        .zip(went)
                        .filter_map(|(was, went)| went.map(|w| (was, w)))
                        .collect();
                    let _ = undo.send(UndoEvent::Did(crate::undo::Record {
                        label: "Move to Trash".into(),
                        step: crate::undo::Step::Trashed(items),
                    }));
                })
            };
            if let Err(e) = outcome {
                report(&notices, format!("Delete failed: {e}"));
            } else {
                report_info(&notices, format!("{} {} items",
                    if permanent { "Permanently deleted" } else if cfg!(target_os = "macos") { "Moved to Trash:" } else { "Recycled" }, paths.len()));
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
        let wake = self.ctx.clone();
        let planned = self.planned_tx.clone();
        self.sta.submit(move || {
            // Option forces a copy, as in the Finder. Without it the volumes
            // decide: a move within one disk, a copy onto another. Worked out
            // here because it stats both ends.
            let how = if copy {
                Transfer::Copy
            } else {
                neutron_shell::fileops::default_transfer(&paths[0], &dir)
            };
            tracing::info!(count = paths.len(), ?how, dest = %dir.display(), "drop");
            let request = crate::transfers::Request { tab: tab_id, paths, destination: dir, how };
            let _ = planned.send(crate::transfers::plan(request));
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

        // Inside an archive the shell has nothing to say — its menu is for
        // files on disk — so the menu is Neutron's own.
        if self.in_archive() {
            self.archive_menu(idx.is_some(), pos);
            return;
        }
        if self.in_tag_view() {
            self.tag_menu(idx.is_some(), pos);
            return;
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
        self.menu_window = Some(self.viewport);

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
        // Only in the window it was asked for in.
        let mine = self.menu_window.is_none_or(|w| w == self.viewport);
        let pending_menus = if mine { std::mem::take(&mut self.mail.menus) } else { Vec::new() };
        for pending in pending_menus {
            // Plugin rows are appended here rather than on the apartment
            // thread: this is the side that knows what is installed, and it is
            // also where the ids handed out have to be remembered.
            let mut items = pending.items;

            // The shell's Paste and Duplicate, taken over: Neutron's go
            // through the transfer job, with its progress, Stop, and question
            // about names already taken.
            self.menu_actions.clear();
            for item in items.iter_mut() {
                let ours = match item.label.as_str() {
                    "Paste" => Some((MENU_ID_OURS + 0xF0, Action::Paste)),
                    "Duplicate" => Some((MENU_ID_OURS + 0xF1, Action::Duplicate)),
                    _ => None,
                };
                if let Some((id, action)) = ours {
                    item.id = id;
                    self.menu_actions.push((id, action));
                }
            }
            let rows = self.menu_rows();
            if !rows.is_empty() {
                items.push(neutron_core::MenuItem::separator());
                items.extend(rows);
                let built = std::mem::take(&mut *self.pending_menu_actions.borrow_mut());
                self.menu_actions.extend(built);
            }

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

        let edit_site = (answer == Some(MENU_ID_EDIT_SITE))
            .then(|| self.edit_site_target.take())
            .flatten();

        let plugin = answer.and_then(|id| self.plugins.menu_choice(id));
        let ours = answer.and_then(|id| {
            let found = self.menu_actions.iter().position(|(i, _)| *i == id);
            found.map(|at| self.menu_actions.swap_remove(at).1)
        });
        self.menu_actions.clear();
        let for_shell = match (&pin, &new_tab, &plugin, &edit_site, &ours) {
            (None, None, None, None, None) => answer,
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

        if let Some(site) = edit_site {
            self.edit_site(&site);
        }

        if let Some(reference) = plugin {
            let ctx = ctx.clone();
            self.run_plugin(&ctx, &reference);
        }

        if let Some(action) = ours {
            let ctx = ctx.clone();
            self.apply(&ctx, action);
        }
    }

    /// Opens the connect dialog on a saved server, to change it in place.
    fn edit_site(&mut self, id: &str) {
        let Some(site) = self.sites.get(id).cloned() else {
            self.notices.error(format!("No saved server called {id:?}"));
            return;
        };
        self.connect.edit(&site);
        // Opened from a refusal over an unrecognised certificate, the dialog
        // arrives with that certificate's fingerprint filled in. Trusting it
        // still takes the user's own Save; this only spares them copying a
        // ninety-five character string out of an error message.
        let refused = self
            .workspace
            .active_tab()
            .filter(|t| t.location().as_remote().is_some_and(|(s, _)| s == id))
            .and_then(|t| match &t.status {
                Status::Error(e) => certificate_in(e),
                _ => None,
            });
        if let Some(fingerprint) = refused {
            self.connect.trust_certificate(&fingerprint);
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
    /// Opens what the system handed over: a folder in a new tab, a file in a
    /// new tab on its folder with the file selected.
    fn drain_opened(&mut self) {
        for paths in std::mem::take(&mut self.mail.opened) {
            let Some(group) = self.workspace.focused_group().map(|g| g.id) else {
                return;
            };
            for path in paths {
                let (folder, select) = if path.is_dir() {
                    (path, None)
                } else {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                    match path.parent() {
                        Some(parent) => (parent.to_path_buf(), name),
                        None => continue,
                    }
                };
                let view = self.workspace.view_for_new_tab();
                let tab = self.workspace.open_tab(group, NodeId::Path(folder.clone()), view);
                self.reload(tab, NodeId::Path(folder));
                if let Some(t) = self.workspace.tabs.get_mut(&tab) {
                    t.reselect = select.map(|n| crate::workspace::Reselect { names: vec![n.clone()], cursor: Some(n) });
                }
            }
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

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

    /// What the window is cleared to before anything is drawn. On the Mac,
    /// nothing: the window is transparent, so the desktop shows between the
    /// cards. Elsewhere eframe's own default.
    #[cfg(target_os = "macos")]
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let frame_started = Instant::now();
        let ctx = ui.ctx().clone();

        // Once a frame, for every window.
        self.shared_frame(&ctx);

        // The first window, then the others, each with its own state.
        self.window_frame(ui);
        // Closing the first window while others are open hands one of them
        // the first window's place, rather than ending the application.
        let hand_over = !self.windows.is_empty() && ctx.input(|i| i.viewport().close_requested());
        if hand_over {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.other_windows(&ctx);
        if hand_over {
            self.hand_over_first_window();
        }
        // Anything no window took — work for a tab closed meanwhile.
        self.mail = Mail::default();

        self.startup.mark_frame(frame_started.elapsed());
    }
}

/// What belongs to one window: its panes and tabs, and whatever is open in it.
///
/// The window being drawn has its state in the app's own fields, where the
/// code that draws and acts has always found it. The others wait here, and
/// each is swapped in for its own frame — so every function that reads
/// `self.workspace` reads the workspace of the window it is running for,
/// without being told there is more than one.
struct WindowState {
    viewport: egui::ViewportId,
    /// Where a new window first appears; the OS keeps it after that.
    origin: Option<egui::Pos2>,
    workspace: Workspace,
    confirm: Option<(crate::confirm::Confirm, Box<Action>, Option<Subject>)>,
    address: Option<(GroupId, String)>,
    type_ahead: String,
    type_ahead_at: Option<Instant>,
    quick_look_shown: Option<QuickLookKey>,
    menu: Option<neutron_ui::MenuState>,
    menu_reply: Option<crossbeam_channel::Sender<Option<u32>>>,
    menu_actions: Vec<(u32, Action)>,
    pending_menu_actions: std::cell::RefCell<Vec<(u32, Action)>>,
    pin_target: Option<PathBuf>,
    tab_target: Option<NodeId>,
    edit_site_target: Option<String>,
    tab_drag: Option<TabDrag>,
    last_title: String,
    finder: Finder,
    connect: crate::connect::ConnectDialog,
    preview: crate::preview::PreviewState,
    previews: crate::preview::PreviewLoader,
    rename: Option<RenameEdit>,
    pending_rename: Option<(TabId, String)>,
}

impl WindowState {
    fn new(ctx: &egui::Context, viewport: egui::ViewportId, origin: Option<egui::Pos2>, location: NodeId) -> Self {
        Self {
            viewport,
            origin,
            workspace: Workspace::new(location),
            confirm: None,
            address: None,
            type_ahead: String::new(),
            type_ahead_at: None,
            quick_look_shown: None,
            menu: None,
            menu_reply: None,
            menu_actions: Vec::new(),
            pending_menu_actions: std::cell::RefCell::new(Vec::new()),
            pin_target: None,
            tab_target: None,
            edit_site_target: None,
            tab_drag: None,
            last_title: String::new(),
            finder: Finder::default(),
            connect: crate::connect::ConnectDialog::default(),
            preview: crate::preview::PreviewState::default(),
            previews: crate::preview::PreviewLoader::spawn(ctx.clone()),
            rename: None,
            pending_rename: None,
        }
    }
}

/// What background work reported this frame, held until the window it is for
/// has had its turn.
///
/// The channels are shared by every window, so they are emptied once, here,
/// and each window takes what concerns its own tabs. Whatever is left when all
/// have drawn — a listing for a tab closed meanwhile — is dropped.
#[derive(Default)]
struct Mail {
    loads: Vec<(TabId, u64, LoadResult)>,
    refreshes: Vec<TabId>,
    open_as: Vec<(TabId, PathBuf, bool)>,
    goto: Vec<(TabId, NodeId)>,
    created: Vec<(TabId, String)>,
    /// Folders that changed on disk. Every window looks at all of them.
    changes: std::collections::HashSet<PathBuf>,
    /// Files and folders the system asked Neutron to open.
    opened: Vec<Vec<PathBuf>>,
    /// Context menus built by the shell, for the window that asked.
    menus: Vec<PendingMenu>,
    /// Menu bar items chosen, for the window with focus.
    chosen: Vec<u32>,
    /// Arrow keys pressed in Quick Look, for the window that opened it.
    moves: Vec<isize>,
}

/// Takes the items `mine` claims out of `all`, leaving the rest.
fn take_mine<T>(all: &mut Vec<T>, mine: impl Fn(&T) -> bool) -> Vec<T> {
    let (taken, rest): (Vec<T>, Vec<T>) = std::mem::take(all).into_iter().partition(|item| mine(item));
    *all = rest;
    taken
}

/// What a worker reports to the undo history.
enum UndoEvent {
    /// A change was made.
    Did(crate::undo::Record),
    /// An undo (or, with `undo` false, a redo) finished.
    Reversed { undo: bool, record: crate::undo::Record, failed: Vec<String>, blocked: Vec<PathBuf> },
}

/// What the Quick Look panel was given: the tab, its cursor, and how many
/// items — enough to tell when the panel has to follow.
type QuickLookKey = (TabId, Option<usize>, usize);

impl NeutronApp {
    /// What is done once a frame however many windows are open: background
    /// work pumped, shared caches fed, and the channels emptied into `mail`.
    fn shared_frame(&mut self, ctx: &egui::Context) {
        self.drain_places();
        self.drain_sites();
        while let Ok(state) = self.drive_rx.try_recv() {
            // An answer ends any sign-in in progress; the sign-in's own
            // result is the only thing sent while one runs.
            self.drive_signing_in = false;
            self.drive_state = state;
        }
        if self.clip_fail_rx.try_recv().is_ok() {
            self.clear_cut_marks();
        }
        self.pump_archive_job();
        self.pump_transfers();
        self.pump_plugin(ctx);
        self.pump_download();
        self.pump_remote_job();
        self.drain_notices(ctx);
        self.sync_watchers(ctx);
        self.pump_columns();
        // Before drawing: installs icons that arrived since the last frame, so
        // rows painted below pick them up immediately rather than a frame late.
        self.icons.pump();
        self.thumbs.pump();
        self.icon_texture = self.icons.texture(ctx);
        let previous_index_state = std::mem::discriminant(self.index.state());
        self.index.pump();
        if previous_index_state != std::mem::discriminant(self.index.state()) {
            self.refresh_finder();
        }
        neutron_shell::quicklook::pump();
        self.collect_mail();
    }

    /// Empties the shared channels into `mail`, for each window to take its
    /// own from.
    fn collect_mail(&mut self) {
        while let Some(load) = self.loader.poll() {
            self.mail.loads.push(load);
        }
        self.mail.refreshes.extend(self.refresh_rx.try_iter());
        self.mail.open_as.extend(self.open_as_rx.try_iter());
        self.mail.goto.extend(self.goto_rx.try_iter());
        self.mail.created.extend(self.created_rx.try_iter());
        self.mail.changes.extend(self.changed_rx.try_iter());
        self.mail.opened.extend(self.opened.try_iter());
        self.mail.menus.extend(self.menu_rx.try_iter());
        self.mail.chosen.extend(neutron_shell::menubar::take_chosen());
        self.mail.moves.extend(neutron_shell::quicklook::take_moves());
    }

    /// One window's frame: its share of the mail, its input, its drawing, and
    /// the actions taken in it.
    fn window_frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let p = self.palette();
        let mut actions: Vec<Action> = Vec::new();

        // The ground the cards sit on. Painted onto a background layer so it
        // stays beneath every panel regardless of declaration order.
        let screen = ctx.content_rect();
        let bg = ui
            .painter()
            .clone()
            .with_layer_id(egui::LayerId::background());
        if cfg!(target_os = "macos") {
            // The window is transparent here only so that its corners can be
            // rounded: the ground is painted, solid, inside them. Under it, one
            // part in 255 of black across the whole window — the window server
            // sends a click on a pixel with no alpha at all to whatever is
            // behind, and the very corners would otherwise stop belonging to
            // Neutron.
            bg.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(1));
            theme::paint_ground(&bg, screen, &p);
        } else {
            neutron_ui::ambient::paint(&bg, screen, &p);
        }

        self.drain_loads(&ctx);
        self.drain_refreshes();
        self.drain_open_as();
        self.drain_goto();
        self.clipboard_keys(&ctx, &mut actions);
        let tabs = &self.workspace.tabs;
        for (tab, name) in take_mine(&mut self.mail.created, |(t, _)| tabs.contains_key(t)) {
            self.pending_rename = Some((tab, name));
        }
        self.open_pending_rename(&ctx);
        // Opened from the Finder or the Dock: into the first window.
        if self.viewport == egui::ViewportId::ROOT {
            self.drain_opened();
        }
        self.drain_changes();
        self.pump_preview(&ctx);
        self.sync_finder_results();
        self.context_menu_overlay(&ctx, &p);
        self.keyboard(&ctx, &mut actions);

        // The menu bar acts on the window with focus; Quick Look's arrows on
        // the window that opened it.
        let focused = ctx
            .input(|i| i.viewport().focused)
            .unwrap_or(self.viewport == egui::ViewportId::ROOT && self.windows.is_empty());
        let looks_here = self.look_window.map_or(focused, |w| w == self.viewport);
        if looks_here {
            for delta in std::mem::take(&mut self.mail.moves) {
                actions.push(Action::MoveCursor { delta, extend: false });
            }
        }
        if focused {
            for tag in std::mem::take(&mut self.mail.chosen) {
                if self.modal() {
                    continue;
                }
                if let Some(action) = crate::menubar::Cmd::from_tag(tag).and_then(|c| self.menu_command(c)) {
                    actions.push(action);
                }
            }
        }

        // Before every other panel: it claims the top strip, and the rest
        // lay out inside what is left.
        self.title_bar(ui, &p);

        self.status_bar(ui, &p, &mut actions);
        self.sidebar(ui, &p, &mut actions);
        self.action_rail(ui, &p, &mut actions);
        self.preview_pane(ui, &p);
        self.panes(ui, &p, &mut actions);
        self.workspace.consume_scroll_requests();

        crate::chrome::resize_edges(ui, screen);

        // Overlays draw after the panes so they sit on top of them, and the
        // overlay's own keys win while it is open.
        self.finder_overlay(ui, &p, &mut actions);
        self.connect_overlay(ui, &p, &mut actions);

        // Last of all: a question is answered before anything underneath it is
        // touched, and its scrim has to cover the overlays too.
        self.confirm_overlay(ui, &p);
        self.clash_overlay(ui, &p);

        // Queued *after* drawing so a `DropTab` pushed by a tab strip this
        // frame is applied first. Pushed before it, this would clear the drag
        // and turn every drop into a no-op.
        if self.tab_drag.is_some() && ctx.input(|i| i.pointer.any_released()) {
            actions.push(Action::EndTabDrag);
        }

        // Actions update the model after the widgets have drawn. Present that
        // new state even if no further mouse or keyboard event arrives.
        if !actions.is_empty() {
            ctx.request_repaint();
        }
        for action in actions {
            self.apply(&ctx, action);
        }
        if looks_here {
            self.quick_look_follow();
        }
        if focused {
            self.publish_menu_state(&ctx);
        }
    }

    /// Draws every window but the first, each with its state swapped in.
    fn other_windows(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.windows.len() {
            let mut window = self.windows.remove(i);
            self.swap_window(&mut window);
            let mut builder = crate::window_builder().with_inner_size([1100.0, 720.0]);
            if let Some(at) = self.origin {
                builder = builder.with_position(at);
            }
            let mut closing = false;
            ctx.show_viewport_immediate(self.viewport, builder, |ui, _class| {
                self.window_frame(ui);
                closing = ui.ctx().input(|i| i.viewport().close_requested());
            });
            self.swap_window(&mut window);
            if closing {
                self.forget_window(window);
            } else {
                self.windows.insert(i, window);
                i += 1;
            }
        }
    }

    /// The first window was closed while others are open: the first of those
    /// moves into it, and what the first window showed is let go.
    fn hand_over_first_window(&mut self) {
        if self.windows.is_empty() {
            return;
        }
        let mut window = self.windows.remove(0);
        self.swap_window(&mut window);
        // The state moves; the windows stay. The first window is still the
        // root viewport, and the one emptied is not drawn again, so it closes.
        std::mem::swap(&mut self.viewport, &mut window.viewport);
        self.origin = None;
        self.forget_window(window);
    }

    /// Lets go of a closed window's tabs.
    fn forget_window(&mut self, window: WindowState) {
        for tab in window.workspace.tabs.keys() {
            self.loader.forget(*tab);
        }
        if self.look_window == Some(window.viewport) {
            self.look_window = None;
        }
        if self.menu_window == Some(window.viewport) {
            self.menu_window = None;
        }
    }

    /// Exchanges the window being drawn's state with `window`'s.
    fn swap_window(&mut self, window: &mut WindowState) {
        use std::mem::swap;
        swap(&mut self.viewport, &mut window.viewport);
        swap(&mut self.origin, &mut window.origin);
        swap(&mut self.workspace, &mut window.workspace);
        swap(&mut self.confirm, &mut window.confirm);
        swap(&mut self.address, &mut window.address);
        swap(&mut self.type_ahead, &mut window.type_ahead);
        swap(&mut self.type_ahead_at, &mut window.type_ahead_at);
        swap(&mut self.quick_look_shown, &mut window.quick_look_shown);
        swap(&mut self.menu, &mut window.menu);
        swap(&mut self.menu_reply, &mut window.menu_reply);
        swap(&mut self.menu_actions, &mut window.menu_actions);
        swap(&mut self.pending_menu_actions, &mut window.pending_menu_actions);
        swap(&mut self.pin_target, &mut window.pin_target);
        swap(&mut self.tab_target, &mut window.tab_target);
        swap(&mut self.edit_site_target, &mut window.edit_site_target);
        swap(&mut self.tab_drag, &mut window.tab_drag);
        swap(&mut self.last_title, &mut window.last_title);
        swap(&mut self.finder, &mut window.finder);
        swap(&mut self.connect, &mut window.connect);
        swap(&mut self.preview, &mut window.preview);
        swap(&mut self.previews, &mut window.previews);
        swap(&mut self.rename, &mut window.rename);
        swap(&mut self.pending_rename, &mut window.pending_rename);
    }

    /// An id for one of a window's own panels. The first window keeps the
    /// plain name, so the widths egui saved for its panels still apply; the
    /// others add their viewport, so two windows' sidebars are never taken
    /// for the same one where viewports share a pass.
    fn window_id(&self, name: &str) -> egui::Id {
        if self.viewport == egui::ViewportId::ROOT {
            egui::Id::new(name)
        } else {
            egui::Id::new((name, self.viewport))
        }
    }

    /// Every window's workspace: the one being drawn, and the others.
    fn workspaces(&self) -> impl Iterator<Item = &Workspace> {
        std::iter::once(&self.workspace).chain(self.windows.iter().map(|w| &w.workspace))
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
    /// The folder the entry is in — on disk or on a server — fixed when the
    /// rename opens.
    place: NodeId,
    /// What the file is called now, to tell an unchanged rename from a real one.
    original: String,
    text: String,
    /// Watcher changes wait until the editor closes so its row stays stable.
    refresh_pending: bool,
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
                PlacesUpdate::Ejectable(path) => {
                    self.ejectable.insert(path);
                }
                PlacesUpdate::DrivesReset => {
                    self.drives.clear();
                    self.ejectable.clear();
                    self.scanning_drives = true;
                }
                PlacesUpdate::Done => {
                    self.scanning_drives = false;
                    self.leave_missing_volumes();
                }
            }
        }
    }

    /// Re-lists tabs whose contents a finished shell operation may have changed.
    fn drain_refreshes(&mut self) {
        // Collected first because refreshing borrows `self` mutably.
        let owned = &self.workspace.tabs;
        let tabs = take_mine(&mut self.mail.refreshes, |tab| owned.contains_key(tab));
        for tab in tabs {
            self.refresh_in_place(tab);
        }
    }

    fn drain_loads(&mut self, ctx: &egui::Context) {
        // Set when a remote folder arrives, and applied after the loop — `tab`
        // borrows `self.workspace` mutably for the whole body.
        let mut cleared_remote_note = false;

        let tabs = &self.workspace.tabs;
        let mine = take_mine(&mut self.mail.loads, |(tab, ..)| tabs.contains_key(tab));
        for (tab_id, generation, result) in mine {
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
                    // The loader sorts and hides, but it never sees the filter
                    // field — a refresh keeps the filter text, so without this
                    // F5 or any change on disk listed the whole folder under a
                    // box still saying "doc". Only when there is a filter: the
                    // re-sort is the work the loader exists to keep off this
                    // thread.
                    if !tab.view.filter.trim().is_empty() {
                        tab.refilter();
                    }
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

        // Nor while a question is open. A key that moved the selection would
        // change what the dialog's button acts on, after the dialog counted it.
        if self.confirm.is_some() || !self.clash_questions.is_empty() {
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
                            // Space is Quick Look on the Mac — except partway
                            // through typing a name that has a space in it.
                            let typing = !self.type_ahead.is_empty()
                                && self.type_ahead_at.is_some_and(|t| t.elapsed() <= TYPE_AHEAD_TIMEOUT);
                            if c == ' ' && cfg!(target_os = "macos") && !typing {
                                actions.push(Action::QuickLookSelection);
                            } else if !c.is_control() {
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
                if cfg!(target_os = "macos") && ctrl {
                    bindings.push((egui::Key::Backspace, Action::Delete { permanent: false }));
                } else {
                    bindings.push((egui::Key::Backspace, Action::Up));
                }
            }

            bindings.push((
                egui::Key::Delete,
                // Shift is the long-standing Windows convention for bypassing
                // the Recycle Bin, and the shell still confirms it.
                if cfg!(target_os = "macos") && shift {
                    Action::Ask(Box::new(Action::Delete { permanent: true }))
                } else { Action::Delete { permanent: shift } },
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

            // Hidden files. Not Cmd+H on the Mac: the application menu owns
            // that as "Hide Neutron", so the window vanished instead. The
            // Finder's own binding, Cmd+Shift+., is the one Mac users know.
            if cfg!(target_os = "macos") {
                if i.modifiers.command && shift && i.key_pressed(egui::Key::Period) {
                    actions.push(Action::ToggleHidden);
                }
            } else if ctrl {
                bindings.push((egui::Key::H, Action::ToggleHidden));
            }

            // The Finder's layout, where it differs from the one above: the
            // views on Cmd+1–3, Duplicate on Cmd+D, the Go menu's folders, tabs
            // on Cmd+Shift+[ and ], Return to rename. Worked out first so the
            // shared table can stand aside for the keys it claims.
            let mac = cfg!(target_os = "macos") && i.modifiers.command;
            if mac {
                let control = i.modifiers.ctrl;
                let pressed = |k: egui::Key| i.key_pressed(k);
                let mut go = |action: Action| actions.push(action);
                if shift && !alt {
                    for (key, name) in [
                        (egui::Key::H, "Home"),
                        (egui::Key::D, "Desktop"),
                        (egui::Key::O, "Documents"),
                        (egui::Key::A, "Applications"),
                    ] {
                        if pressed(key) {
                            go(Action::GoKnown(name));
                        }
                    }
                    if pressed(egui::Key::G) {
                        go(Action::ToggleAddress);
                    }
                    if pressed(egui::Key::Z) {
                        go(Action::Redo);
                    }
                    // Shifted, a bracket arrives as a brace on most layouts.
                    if pressed(egui::Key::CloseBracket) || pressed(egui::Key::CloseCurlyBracket) {
                        go(Action::CycleTab(1));
                    }
                    if pressed(egui::Key::OpenBracket) || pressed(egui::Key::OpenCurlyBracket) {
                        go(Action::CycleTab(-1));
                    }
                }
                if alt && !shift && pressed(egui::Key::L) {
                    go(Action::GoKnown("Downloads"));
                }
                if alt && pressed(egui::Key::Backspace) {
                    go(Action::Ask(Box::new(Action::Delete { permanent: true })));
                }
                if !shift && !alt && !control {
                    for (key, view) in [
                        (egui::Key::Num1, file_list::ViewMode::Grid),
                        (egui::Key::Num2, file_list::ViewMode::List),
                        (egui::Key::Num3, file_list::ViewMode::Columns),
                    ] {
                        if pressed(key) {
                            go(Action::SetView(view));
                        }
                    }
                    if pressed(egui::Key::D) {
                        go(Action::Duplicate);
                    }
                    if pressed(egui::Key::K) {
                        go(Action::OpenConnect);
                    }
                    if pressed(egui::Key::Y) {
                        go(Action::QuickLookSelection);
                    }
                    if pressed(egui::Key::E) {
                        go(Action::EjectCurrent);
                    }
                    if pressed(egui::Key::I) {
                        go(Action::GetInfoSelection);
                    }
                    if pressed(egui::Key::Z) {
                        go(Action::Undo);
                    }
                }
                // Control-Command-T: the Finder's "Add to Sidebar".
                if control && pressed(egui::Key::T) {
                    go(Action::TogglePin);
                }
            }
            // Control-Tab through the tabs, as in every tabbed Mac app.
            if cfg!(target_os = "macos") && i.modifiers.ctrl && !i.modifiers.command && i.key_pressed(egui::Key::Tab) {
                actions.push(Action::CycleTab(if shift { -1 } else { 1 }));
            }

            if mac && !(shift || alt || i.modifiers.ctrl) {
                bindings.extend([
                    (egui::Key::A, Action::SelectAll),
                    (egui::Key::R, Action::Refresh),
                    (egui::Key::W, Action::CloseActiveTab),
                ]);
            }

            // Everywhere but the Mac, whose layout is the block above.
            if ctrl && !cfg!(target_os = "macos") {
                bindings.extend([
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
                    (egui::Key::Z, if shift { Action::Redo } else { Action::Undo }),
                    (egui::Key::Y, Action::Redo),
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

            // What the Mac keeps of the table above, with Command.
            if mac && !alt && !i.modifiers.ctrl {
                bindings.extend([
                    (egui::Key::L, if shift { Action::ToggleView } else { Action::ToggleAddress }),
                    (egui::Key::F, if shift { Action::ToggleFinder(Mode::Everything) } else { Action::FocusFilter }),
                    (egui::Key::P, Action::ToggleFinder(if shift { Mode::Commands } else { Mode::Files })),
                    (egui::Key::Backslash, Action::Split(if shift { Axis::Vertical } else { Axis::Horizontal })),
                    (egui::Key::N, if shift { Action::NewFolder } else { Action::NewWindow }),
                ]);
                if !shift {
                    if let Some(group) = self.workspace.focused_group().map(|g| g.id) {
                        bindings.push((egui::Key::T, Action::NewTab(group)));
                    }
                }
            }
            // The real Control key for the terminal on the Mac, where Cmd+` is
            // the system's own "next window" and never arrives here.
            if cfg!(target_os = "macos") && i.modifiers.ctrl && !i.modifiers.command {
                bindings.push((egui::Key::Backtick, Action::OpenTerminal));
            }

            // The Finder's navigation keys: Cmd+Up to the enclosing folder,
            // Cmd+Down into the one under the cursor, Cmd+[ and Cmd+] through
            // history. They replace plain Up and Down while Command is held,
            // where the table above would otherwise also move the cursor.
            let finder_keys = cfg!(target_os = "macos") && i.modifiers.command && !alt;
            // On the Mac, Return renames and Command-O or Command-Down opens,
            // as in the Finder.
            let mut open_cursor = if cfg!(target_os = "macos") {
                if i.key_pressed(egui::Key::Enter) && !i.modifiers.command && !alt {
                    actions.push(Action::BeginRename);
                }
                false
            } else {
                i.key_pressed(egui::Key::Enter)
            };
            if finder_keys {
                bindings.retain(|(key, _)| !matches!(key, egui::Key::ArrowUp | egui::Key::ArrowDown));
                bindings.push((egui::Key::ArrowUp, Action::Up));
                if !shift {
                    bindings.push((egui::Key::OpenBracket, Action::Back));
                    bindings.push((egui::Key::CloseBracket, Action::Forward));
                }
                open_cursor |= i.key_pressed(egui::Key::ArrowDown);
                open_cursor |= !shift && i.key_pressed(egui::Key::O);
            }

            if open_cursor {
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
        // One sign-in at a time. Each waits on a worker for the browser, and a
        // second one would only compete for the same loopback redirect.
        if self.drive_signing_in {
            self.notices.info("Already waiting for Google sign-in in the browser");
            return;
        }
        self.drive_signing_in = true;

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
        let mut state = egui::text_edit::TextEditState::default();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(text.chars().count()),
        )));
        egui::TextEdit::store_state(ctx, id, state);
        ctx.memory_mut(|m| m.request_focus(id));
        self.address = Some((group, text));
        ctx.request_repaint();
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
        let subject = self.subject();
        self.confirm = Some((question, Box::new(then), subject));
    }

    /// What a question asked now would be about. See [`Subject`].
    fn subject(&self) -> Option<Subject> {
        let tab = self.workspace.active_tab_id()?;
        let t = self.workspace.tabs.get(&tab)?;
        Some(Subject {
            tab,
            location: t.location().clone(),
            names: visible_selection(t)
                .into_iter()
                .map(|i| t.list.name(i).to_owned())
                .collect(),
        })
    }

    /// The wording for `action`, or `None` if it does not warrant asking.
    fn question_for(&self, action: &Action) -> Option<crate::confirm::Confirm> {
        match action {
            #[cfg(target_os = "macos")]
            Action::Delete { permanent: true } if self.workspace.active_tab()?.location().is_filesystem() => {
                let count = visible_selection(self.workspace.active_tab()?).len();
                Some(crate::confirm::Confirm {
                    title: "Delete permanently?".into(),
                    body: match count {
                        1 => "The selected item will be permanently deleted. This cannot be undone.".to_owned(),
                        n => format!("{n} selected items will be permanently deleted. This cannot be undone."),
                    },
                    proceed: "Delete permanently".into(),
                })
            }
            // Only on a server. A local delete goes to the Recycle Bin and is
            // undone from there; asking about it would train people to dismiss
            // the dialog that matters.
            Action::DeleteOnServer => {
                let place = self.workspace.active_tab()?.location();
                let (site, _) = place.as_remote()?;
                let name = self
                    .sites
                    .get(site)
                    .map(|s| s.display().to_owned())
                    .unwrap_or_else(|| "that server".to_owned());

                let tab = self.workspace.active_tab()?;
                let selected = visible_selection(tab);
                let what = match selected.len() {
                    1 => "1 item".to_owned(),
                    n => format!("{n} items"),
                };
                // Said whenever it is true, because it is the part that is
                // easy not to think about: one row can be a thousand files.
                let folders = if selected.iter().any(|&i| tab.list.kind(i) == neutron_core::EntryKind::Directory) {
                    " Folders are deleted with everything inside them."
                } else {
                    ""
                };
                Some(crate::confirm::Confirm {
                    title: format!("Delete from {name}?"),
                    body: format!(
                        "{what} will be deleted from {name}.{folders} A server \
                         has no {bin}, so this cannot be undone.",
                        bin = if cfg!(target_os = "macos") { "Trash" } else { "Recycle Bin" }
                    ),
                    proceed: format!("Delete {what}"),
                })
            }

            Action::EmptyTrash => Some(crate::confirm::Confirm {
                title: "Empty the Trash?".into(),
                body: "Everything in the Trash will be deleted permanently. This cannot be undone.".into(),
                proceed: "Empty Trash".into(),
            }),

            Action::ForgetServer => {
                let (site, _) = self.workspace.active_tab()?.location().as_remote()?;
                let name = self
                    .sites
                    .get(site)
                    .map(|s| s.display().to_owned())
                    .unwrap_or_else(|| "that server".to_owned());
                Some(crate::confirm::Confirm {
                    title: format!("Forget {name}?"),
                    body: format!(
                        "Its saved password is removed from {}. \
                         If you do not know the password, this cannot be undone.",
                        if cfg!(target_os = "macos") { "your keychain" } else { "Credential Manager" }
                    ),
                    proceed: "Forget it".to_owned(),
                })
            }

            _ => None,
        }
    }

    /// The confirmation dialog, drawn over everything.
    fn confirm_overlay(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let Some((question, ..)) = self.confirm.as_ref() else {
            return;
        };
        match crate::confirm::show(ui, p, question) {
            crate::confirm::Answer::Waiting => {}
            crate::confirm::Answer::Cancelled => self.confirm = None,
            crate::confirm::Answer::Confirmed => {
                let Some((_, then, asked_about)) = self.confirm.take() else {
                    return;
                };
                // Input is blocked while the dialog is up, but a refresh can
                // still change the listing underneath it. A yes to one set of
                // files is not a yes to another.
                if asked_about != self.subject() {
                    self.notices
                        .error("The selection changed while the question was open, so nothing was done");
                    return;
                }
                self.apply_now(*then);
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
                label: pin_label(pinned, None),
                accel: neutron_ui::format::shortcut(if cfg!(target_os = "macos") { "Control+Cmd+T" } else { "Ctrl+Shift+D" }),
                enabled: true,
                ..Default::default()
            });
        }

        // Neutron's own verbs for particular rows.
        self.menu_actions.clear();
        if let Some(path) = id.as_path() {
            if self.ejectable.contains(path) {
                items.push(neutron_core::MenuItem::separator());
                items.push(neutron_core::MenuItem {
                    id: MENU_ID_EJECT,
                    label: format!("Eject {}", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()),
                    accel: neutron_ui::format::shortcut("Cmd+E"),
                    enabled: true,
                    ..Default::default()
                });
                self.menu_actions.push((MENU_ID_EJECT, Action::Eject(path.to_path_buf())));
            }
            if Some(path) == trash_folder().as_deref() {
                items.push(neutron_core::MenuItem::separator());
                items.push(neutron_core::MenuItem {
                    id: MENU_ID_EMPTY_TRASH,
                    label: "Empty Trash…".to_owned(),
                    enabled: true,
                    ..Default::default()
                });
                self.menu_actions.push((MENU_ID_EMPTY_TRASH, Action::Ask(Box::new(Action::EmptyTrash))));
            }
        }

        // A saved server can be edited from its own row: its password, how it
        // signs in, a certificate to trust.
        self.edit_site_target = id.as_remote().map(|(site, _)| site.to_owned());
        if self.edit_site_target.is_some() {
            items.push(neutron_core::MenuItem::separator());
            items.push(neutron_core::MenuItem {
                id: MENU_ID_EDIT_SITE,
                label: "Edit server…".to_owned(),
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
                label: pin_label(pinned, Some(&name)),
                accel: neutron_ui::format::shortcut(if cfg!(target_os = "macos") { "Control+Cmd+T" } else { "Ctrl+Shift+D" }),
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
                "{} holds {} folders — remove one first",
                if cfg!(target_os = "macos") { "The sidebar" } else { "Quick access" },
                crate::pins::MAX
            ));
            return;
        }

        // Applied here so the sidebar changes on this frame; the write below
        // only makes it outlast the session.
        self.pins = pins.clone();
        self.notices.info(match outcome {
            crate::pins::Toggled::Pinned if cfg!(target_os = "macos") => format!("Added {name} to the sidebar"),
            crate::pins::Toggled::Pinned => format!("Pinned {name} to Quick access"),
            _ if cfg!(target_os = "macos") => format!("Removed {name} from the sidebar"),
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
                    IndexState::Unavailable(why) => {
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
            // A refused query says why in place of results — a helper from an
            // older build that does not know the request, or on the Mac a
            // folder that has gone. The next query is asked as normal.
            None => {
                if let Some(why) = self.index.rejection() {
                    if self.finder.status != why || !self.finder.rows.is_empty() {
                        self.finder.set_rows(Vec::new(), why.to_owned());
                    }
                }
            }
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
            C::Duplicate => Some(Action::Duplicate),
            C::Undo => Some(Action::Undo),
            C::Redo => Some(Action::Redo),
            C::DeleteSelection => Some(Action::Delete { permanent: false }),

            C::ToggleHidden => Some(Action::ToggleHidden),
            C::ToggleView => Some(Action::ToggleView),
            C::ListView => Some(Action::SetView(file_list::ViewMode::List)),
            C::GridView => Some(Action::SetView(file_list::ViewMode::Grid)),
            C::ColumnView => Some(Action::SetView(file_list::ViewMode::Columns)),
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
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 2)))
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

    /// Whether anything needs saying at the bottom of the window: a notice,
    /// a job in progress, or frames running over budget.
    fn has_status(&self) -> bool {
        self.notices.current().is_some()
            || self.archive_job.is_some()
            || !self.transfers.is_empty()
            || self.plugins.running_label().is_some()
            || self.download.is_some()
            || self.remote_job.is_some()
            || self.startup.frame_percentiles().is_some_and(|(_, p99)| p99 > 8.0)
    }

    /// Notices and jobs, as a pill floating at the bottom of the window.
    ///
    /// Only there when there is something to say. What a folder holds is said
    /// in that folder's own pane — see `pane_footer` — so with nothing going on
    /// the window ends at the bottom of its panes.
    fn status_bar(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        if !self.has_status() {
            return;
        }

        // A toast over the foot of the window, sized to what it says, rather
        // than a panel: a panel takes its height from every pane for as long
        // as a notice stands, and sits wherever the layout puts it.
        egui::Area::new(self.window_id("status_bar"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
            .show(ui.ctx(), |ui| {
                ui.set_max_width(560.0);
                egui::Frame::new()
                    .fill(p.raised)
                    .corner_radius(theme::pill(34.0))
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .inner_margin(egui::Margin::symmetric(16, 8))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 10],
                        blur: 28,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(160),
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some((p50, p99)) = self.startup.frame_percentiles() {
                                if p99 > 8.0 {
                                    ui.colored_label(p.warning, format!("{p99:.0}ms p99"))
                                        .on_hover_text(format!(
                                            "Frame build p50 {p50:.1}ms / p99 {p99:.1}ms — over the 8ms budget"
                                        ));
                                }
                            }
                            self.notice_bar(ui, p, actions);
                            self.download_status(ui, p, actions);
                            self.plugin_status(ui, p, actions);
                            self.archive_status(ui, p, actions);
                            self.transfer_status(ui, p, actions);
                        });
                    });
            });
    }

    /// The strip along the bottom of a folder pane: what the folder holds and
    /// what is selected on the left, the keys worth knowing on the right.
    fn pane_footer(&self, ui: &egui::Ui, p: &Palette, rect: egui::Rect, tab: &crate::workspace::Tab) {
        let painter = ui.painter();
        let font = egui::FontId::proportional(12.0);
        let y = rect.center().y;

        let text = match &tab.status {
            Status::Loading => "Loading…".to_owned(),
            Status::Error(_) => String::new(),
            Status::Loaded { entries, .. } => {
                let shown = tab.list.order().len();
                // Said explicitly when filtered, or a narrowed view looks like
                // a folder that lost files.
                let mut text = if shown == *entries {
                    format!("{shown} items")
                } else {
                    format!("{shown} of {entries} items")
                };
                let selected = visible_selection(tab);
                if !selected.is_empty() {
                    let bytes = tab.selection.total_size(&tab.list);
                    text.push_str(&format!(" · {} selected", selected.len()));
                    if bytes > 0 {
                        text.push_str(&format!(" · {}", neutron_ui::format::size(Some(bytes))));
                    }
                }
                text
            }
        };
        let counted = painter.text(pos(rect.left() + 12.0, y), egui::Align2::LEFT_CENTER, text, font.clone(), p.text_faint);
        if let Status::Loaded { elapsed, .. } = &tab.status {
            ui.interact(counted, ui.id().with("footer-count"), egui::Sense::hover())
                .on_hover_text(format!("Listed and sorted in {:.0} ms", elapsed.as_secs_f64() * 1000.0));
        }

        // The keys, right-aligned: the two that open everything else.
        let mac = cfg!(target_os = "macos");
        let columns = tab.view.view == file_list::ViewMode::Columns;
        let hints: Vec<(&str, &str)> = if columns {
            vec![("←", ""), ("→", "move between columns")]
        } else {
            vec![
                (if mac { "⌘P" } else { "Ctrl+P" }, "find"),
                (if mac { "⌘⇧P" } else { "Ctrl+Shift+P" }, "commands"),
            ]
        };
        let mut x = rect.right() - 10.0;
        for (key, label) in hints.iter().rev() {
            if !label.is_empty() {
                let galley = painter.layout_no_wrap(label.to_string(), font.clone(), p.text_faint);
                x -= galley.size().x;
                painter.galley(pos(x, y - galley.size().y / 2.0), galley, p.text_faint);
                x -= 6.0;
            }
            let cap = painter.layout_no_wrap(key.to_string(), theme::strong(ui.ctx(), 11.0), p.text_muted);
            let size = cap.size() + egui::vec2(14.0, 4.0);
            let cap_rect = egui::Rect::from_min_size(pos(x - size.x, y - size.y / 2.0), size);
            painter.rect_filled(cap_rect, egui::CornerRadius::same(7), p.elevated);
            painter.galley(pos(cap_rect.left() + 7.0, y - cap.size().y / 2.0), cap, p.text_muted);
            x = cap_rect.left() - if label.is_empty() { 4.0 } else { 14.0 };
        }
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
        let ctx = self.ctx.clone();
        self.start_archive_job(move || crate::archive_ops::extract(archive, format, ctx));
    }

    /// Zips the selection into a new archive beside it.
    fn compress_selection(&mut self) {
        let Some((_, dir, paths)) = self.selection_paths() else {
            return;
        };

        let ctx = self.ctx.clone();
        self.start_archive_job(move || crate::archive_ops::compress(paths, dir, ctx));
    }

    fn start_archive_job(&mut self, start: impl FnOnce() -> crate::archive_ops::ArchiveJob) {
        if self.archive_job.is_some() {
            // Refused rather than queued: archive work is disk-bound, so two at
            // once makes both slower, and the status bar has room for one line.
            self.notices
                .info("Already busy with an archive — wait for it to finish");
            return;
        }
        self.notices.dismiss_info();
        self.archive_job = Some(start());
    }

    /// Applies whatever the archive worker has reported.
    fn pump_archive_job(&mut self) {
        let Some(job) = self.archive_job.as_mut() else {
            return;
        };
        let Some(finished) = job.poll() else {
            return;
        };

        let directory = job.directory.clone();
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

        // Refresh the originating folder even when no watcher is available or
        // the user has changed panes while the job was running.
        let _ = self.changed_tx.send(directory);
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
                .error(format!("There is no {} to keep plugins in", crate::persist::settings_folder_name()));
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
    /// Opens another window on the focused tab's location, in this process:
    /// its own panes and tabs, sharing everything else — the icon cache, the
    /// places, the jobs, the undo history.
    fn new_window(&mut self) {
        let location = self.workspace.active_tab().map(|t| t.location().clone()).unwrap_or(NodeId::Home);
        let view = self.workspace.view_for_new_tab();
        self.windows_opened += 1;
        let viewport = egui::ViewportId::from_hash_of(("neutron-window", self.windows_opened));
        // A step down and across from the window it was opened from, as each
        // new document window steps from the last.
        let step = 28.0 * (self.windows.len() + 1) as f32;
        let origin = self.ctx.input(|i| i.viewport().outer_rect).map(|r| r.min + egui::vec2(step, step));
        let mut window = WindowState::new(&self.ctx, viewport, origin, location.clone());
        if let Some(tab) = window.workspace.active_tab_mut() {
            tab.view = view;
        }
        // Loaded with the new window's state in place, so the load is for its
        // tab and lands there.
        self.swap_window(&mut window);
        if let Some(tab) = self.workspace.active_tab_id() {
            self.reload(tab, location);
        }
        self.swap_window(&mut window);
        self.windows.push(window);
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
        if let Some(job) = self.remote_job.as_ref() {
            if ui
                .add(egui::Button::new(egui::RichText::new("Stop").size(11.0)).small())
                .on_hover_text("Stop after the item in progress. A partly sent file is removed.")
                .clicked()
            {
                actions.push(Action::CancelRemoteJob);
            }
            let sent = job.progress.bytes.load(std::sync::atomic::Ordering::Relaxed);
            let label = if sent > 0 {
                format!("{} — {}", job.label, neutron_ui::format::size(Some(sent)))
            } else {
                format!("{}…", job.label)
            };
            ui.colored_label(p.accent, label);
        }
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
        let field_id = crate::connect::field_id;
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
                .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 2)))
                .desired_width(rect.width())
                .font(egui::FontId::proportional(13.0))
                .text_color(p.text)
                .hint_text(
                    egui::RichText::new(self.connect.hint(field))
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
                        .color(if error { p.danger } else { p.text_soft() }),
                )
                .truncate()
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
        // Every window's tabs: a watcher dropped because the window being
        // drawn does not show its folder would leave another window stale.
        let wanted: HashSet<PathBuf> = self
            .workspaces()
            .flat_map(|w| w.tabs.values())
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
        // Every window looks at every change: a folder can be open in several.
        let changed = self.mail.changes.clone();
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
            if let Some(edit) = self.rename.as_mut().filter(|edit| edit.tab == tab) {
                edit.refresh_pending = true;
                continue;
            }
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
                NodeId::Archive { archive, inner } => crate::preview::Source::Archive {
                    archive: archive.clone(),
                    inner: crate::archives::join(inner, name),
                },
                // Each row leads to its own file.
                NodeId::Tag { .. } => match tab.list.target(idx) {
                    Some((target, true)) => crate::preview::Source::Local(PathBuf::from(target)),
                    _ => return None,
                },
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

    /// A large preview of entry `idx` of `tab`, at `path`, if it is the kind
    /// of file that has one — requested on first sight.
    fn preview_of(&self, path: &Path, tab: &crate::workspace::Tab, idx: usize) -> Option<neutron_ui::thumb::Thumb> {
        let (name, kind) = (tab.list.name(idx), tab.list.kind(idx));
        // Not for a file still in the cloud: previewing it could fetch it.
        if !neutron_ui::thumb::wants_preview(name, kind) || tab.list.sync(idx) == neutron_core::entry::SyncState::CloudOnly {
            return None;
        }
        self.thumbs.get(path, tab.list.modified(idx), crate::thumbs::Grade::Large)
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
                thumb: tab
                    .location()
                    .as_path()
                    .and_then(|dir| self.preview_of(&dir.join(tab.list.name(idx)), tab, idx)),
            })
        });

        egui::Panel::right(self.window_id("preview"))
            .show_separator_line(false)
            .default_size(320.0)
            .size_range(egui::Rangef::new(220.0, 640.0))
            .frame(
                theme::bare_card()
                    .outer_margin(egui::Margin {
                        left: 0,
                        right: GUTTER as i8,
                        top: GUTTER as i8,
                        bottom: GUTTER as i8,
                    })
                    .inner_margin(egui::Margin::symmetric(14, 14)),
            )
            .show(ui, |ui| {
                paint_panel_surface(ui, egui::Margin::symmetric(14, 14), p);
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
            local_folder: tab.is_some_and(|t| t.location().is_filesystem()),
            server_folder: tab.is_some_and(|t| t.location().as_remote().is_some()),
            archive_folder: tab.is_some_and(|t| t.location().as_archive().is_some()),
            selection_count: tab.map_or(0, |t| t.selection.iter().filter(|&i| t.list.rank(i).is_some()).count()),
            has_cursor: tab.is_some_and(|t| t.selection.cursor().is_some_and(|i| i < t.list.len())),
            selected_archive: tab.is_some_and(|t| t.selection.iter().any(|i| {
                i < t.list.len() && !t.list.kind(i).is_container()
                    && neutron_archive::format_of(Path::new(t.list.name(i))).is_some()
            })),
            archive_busy: self.archive_job.is_some(),
        };

        egui::Panel::left(self.window_id("action-rail"))
            .show_separator_line(false)
            .resizable(false)
            .exact_size(crate::rail::WIDTH)
            .frame(
                theme::bare_card()
                    .outer_margin(egui::Margin {
                        left: GUTTER as i8,
                        right: 0,
                        top: GUTTER as i8,
                        bottom: GUTTER as i8,
                    })
                    .inner_margin(egui::Margin::symmetric(0, 14)),
            )
            .show(ui, |ui| {
                paint_panel_surface(ui, egui::Margin::symmetric(0, 14), p);
                if let Some(chosen) = crate::rail::show(ui, p, &state) {
                    actions.push(Action::RunCommand(chosen.command()));
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

        egui::Panel::top(self.window_id("title-bar"))
            .show_separator_line(false)
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

        egui::Panel::left(self.window_id("sidebar"))
            .show_separator_line(false)
            .default_size(232.0)
            .size_range(egui::Rangef::new(180.0, 420.0))
            .frame(
                // A card like the panes, with the gutter carried on its outer
                // margin so it lines up with them on every edge.
                theme::bare_card()
                    .outer_margin(egui::Margin {
                        left: GUTTER as i8,
                        right: 0,
                        top: GUTTER as i8,
                        bottom: GUTTER as i8,
                    })
                    .inner_margin(egui::Margin { left: 12, right: 12, top: 18, bottom: 12 }),
            )
            .show(ui, |ui| {
                paint_panel_surface(ui, egui::Margin { left: 12, right: 12, top: 18, bottom: 12 }, p);
                ui.set_min_width(ui.available_width());

                sidebar::brand(ui, p);
                ui.add_space(14.0);
                if sidebar::find_pill(ui, p) {
                    actions.push(Action::ToggleFinder(Mode::Files));
                }
                ui.add_space(4.0);

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
                match sidebar::footer(ui, p, footer, &self.drives, self.theme_mode) {
                    sidebar::FooterAction::Connect => actions.push(Action::OpenConnect),
                    sidebar::FooterAction::ToggleTheme => actions.push(Action::ToggleTheme),
                    sidebar::FooterAction::None => {}
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
                sidebar::section(ui, p, "Places");
                let dashboard = neutron_core::places::Place {
                    name: "Dashboard".to_owned(),
                    id: NodeId::Home,
                    kind: neutron_core::places::PlaceKind::KnownFolder,
                    capacity: None,
                };
                match sidebar::row(ui, p, &dashboard, current) {
                        sidebar::RowHit::Open => actions.push(Action::Navigate(NodeId::Home)),
                        sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(dashboard.id.clone(), at)),
                        sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                }
                for place in &self.known_folders {
                    match sidebar::row(ui, p, place, current) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                            sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                    }
                }
                // The Mac's own two: AirDrop, which is the Finder's window,
                // and the Trash, browsed here like any folder.
                if cfg!(target_os = "macos") {
                    let airdrop = Place {
                        name: "AirDrop".to_owned(),
                        id: NodeId::shell("neutron:airdrop", "AirDrop"),
                        kind: neutron_core::places::PlaceKind::KnownFolder,
                        capacity: None,
                    };
                    if matches!(sidebar::row(ui, p, &airdrop, current), sidebar::RowHit::Open) {
                        actions.push(Action::OpenAirDrop);
                    }
                    if let Some(trash) = trash_folder() {
                        let place = Place {
                            name: "Trash".to_owned(),
                            id: NodeId::path(&trash),
                            kind: neutron_core::places::PlaceKind::KnownFolder,
                            capacity: None,
                        };
                        match sidebar::row(ui, p, &place, current) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                            sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                        }
                    }
                }

                // Pins get a group of their own, and only when there are any:
                // they are the user's list, not the system's.
                let pinned: Vec<Place> = self
                    .pinned_places()
                    .into_iter()
                    .filter(|pin| !self.known_folders.iter().any(|k| k.id == pin.id))
                    .collect();
                if !pinned.is_empty() {
                    sidebar::section(ui, p, "Pinned");
                    for place in &pinned {
                        match sidebar::row(ui, p, place, current) {
                                sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                                sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                                sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                        }
                    }
                }

                if !self.shell_roots.is_empty() {
                    sidebar::section(ui, p, "System");
                    for place in &self.shell_roots {
                        match sidebar::row(ui, p, place, current) {
                                sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                                sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                                sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                        }
                    }
                }

                // Everywhere files live: the disks, the cloud, the servers.
                sidebar::section(ui, p, "Locations");
                for drive in &self.drives {
                    let path = drive.id.as_path();
                    let ejectable = path.is_some_and(|p| self.ejectable.contains(p));
                    match sidebar::drive_row(ui, p, drive, current, ejectable) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(drive.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(drive.id.clone(), at)),
                            sidebar::RowHit::Eject => {
                                if let Some(path) = path {
                                    actions.push(Action::Eject(path.to_path_buf()));
                                }
                            }
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
                for place in &self.cloud {
                    match sidebar::row(ui, p, place, current) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                            sidebar::RowHit::None | sidebar::RowHit::Eject => {}
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
                                sidebar::RowHit::None | sidebar::RowHit::Eject => {}
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
                    // A row that does something about it: an app opened from
                    // the Dock never sees an environment variable, so the
                    // file is the way, and this makes it and shows it.
                    DriveState::NotConfigured => {
                        if sidebar::action_row_with_hint(
                            ui,
                            p,
                            "Set up Google Drive",
                            neutron_ui::icons::Glyph::Cloud,
                            "Drive needs a Google OAuth client of your own. This creates google.json \
                             in Neutron's settings folder for its client_id and client_secret.",
                        ) {
                            actions.push(Action::SetUpDrive);
                        }
                    }
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
                            sidebar::RowHit::None | sidebar::RowHit::Eject => {}
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
                                sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                        }
                    }
                }

                // The Finder's colour tags, each showing every file that
                // carries it.
                if cfg!(target_os = "macos") {
                    sidebar::section(ui, p, "Tags");
                    for &(name, color) in neutron_shell::tags::COLORS.iter() {
                        let selected = matches!(current, Some(NodeId::Tag { name: n }) if &**n == name);
                        if matches!(sidebar::tag_row(ui, p, name, color, selected), sidebar::RowHit::Open) {
                            actions.push(Action::Navigate(NodeId::Tag { name: name.into() }));
                        }
                    }
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

                panes::set_multi_pane(ui.ctx(), self.workspace.layout.len() > 1);
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
                TabAction::Reorder { from, to } => Action::ReorderTab { group: group_id, from, to },
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
            view: tab.view.view,
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
                header::HeaderAction::SetView(mode) => Action::SetView(mode),
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
                    if cfg!(target_os = "macos") { i.modifiers.alt } else { i.modifiers.ctrl },
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
                // The drop lands here, and the checks that run on it — is this
                // a folder with a path at all? — read the focused pane. So this
                // one is focused first; otherwise a drop on a local pane while
                // a server pane had focus was refused in the server's name.
                if group_id != self.workspace.focused {
                    actions.push(Action::FocusGroup(group_id));
                }
                actions.push(Action::DropFiles {
                    group: group_id,
                    paths: dropped,
                    copy: ctrl,
                });
            }
        }

        // A folder's footer is carved off the bottom of its pane before the
        // listing is laid out, so the rows end above it rather than under it.
        let mut content_ui;
        let ui = if matches!(tab.location(), NodeId::Home) {
            ui
        } else {
            let area = ui.available_rect_before_wrap();
            let footer = egui::Rect::from_min_max(egui::pos2(area.left(), area.bottom() - 34.0), area.max);
            self.pane_footer(ui, p, footer, tab);
            content_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_max(area.min, egui::pos2(area.right(), footer.top())))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            &mut content_ui
        };

        if let Status::Error(e) = &tab.status {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.colored_label(p.danger, "Could not open this location");
                ui.colored_label(p.text_muted, e.as_str());
                // macOS keeps the Trash from apps until they are given Full
                // Disk Access, and the error alone does not say so.
                if tab.location().as_path() == trash_folder().as_deref() {
                    ui.add_space(10.0);
                    ui.colored_label(
                        p.text_muted,
                        "macOS shows the Trash only to apps with Full Disk Access. Turn it on for Neutron in \
                         System Settings, then come back here.",
                    );
                    ui.add_space(6.0);
                    if ui.button("Open Full Disk Access settings").clicked() {
                        actions.push(Action::OpenFullDiskAccess);
                    }
                }
                // On a server most of what goes wrong is fixed in its settings
                // — a password that was never saved, a certificate to trust,
                // a changed port — so the way there is right under the error.
                if let Some((site, _)) = tab.location().as_remote() {
                    ui.add_space(10.0);
                    if ui.button("Edit server…").clicked() {
                        actions.push(Action::EditSite(site.to_owned()));
                    }
                }
            });
            return;
        }

        // The dashboard is not a listing, so it does not go through the list
        // widget at all. It draws from what the sidebar scan already found.
        if matches!(tab.location(), NodeId::Home) {
            // Cloud storage and saved servers, beside the disks.
            let mut elsewhere: Vec<Place> = self.cloud.clone();
            elsewhere.extend(self.sites.sites.iter().map(|site| Place {
                name: site.display().to_owned(),
                id: NodeId::remote(site.id.as_str(), site.start_path().as_str()),
                kind: neutron_core::places::PlaceKind::Server,
                capacity: None,
            }));
            if let Some(hit) = crate::dashboard::show(
                ui,
                p,
                &self.drives,
                &self.quick_access(),
                &self.recent.files,
                &elsewhere,
            ) {
                actions.push(match hit {
                    crate::dashboard::Chosen::Go(id) => Action::Navigate(id),
                    crate::dashboard::Chosen::Open(path) => Action::OpenRecent(path),
                    crate::dashboard::Chosen::Forget(path) => Action::ForgetRecent(path),
                });
            }
            return;
        }

        // A folder in the column view. Anything else — a server, Drive, This
        // PC — has no path to build columns from and stays a list.
        if tab.view.view == file_list::ViewMode::Columns {
            if let Some(dir) = tab.location().as_path() {
                self.draw_columns(ui, p, group_id, tab, dir, actions);
                return;
            }
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
            thumbs: &self.thumbs,
            texture: self.icon_texture,
            dir: tab.location().as_path().map(|p| p.to_path_buf()),
        };

        // Only when the cut happened in the folder this pane is showing:
        // otherwise a file called `notes.txt` cut from somewhere else would
        // fade an unrelated `notes.txt` here.
        let cut = (self.cut_dir.as_deref() == tab.location().as_path())
            .then_some(&self.cut_names)
            .filter(|names| !names.is_empty());

        let marks = file_list::Marks {
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
        };
        let icons = Some(&source as &dyn file_list::IconSource);
        // Scoped by tab and folder, so each keeps its own scroll position: a
        // folder just entered opens at the top rather than wherever the last
        // one was left, a second tab in the pane does not share the first
        // one's, and Back returns to where the folder was.
        let drawn = ui
            .push_id((tab.id, tab.location()), |ui| {
                file_list::show(ui, &mut view, &tab.list, &marks, p, icons)
            })
            .inner;

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
            actions.push(list_action(a));
        }
    }

    /// The column view of a folder: the folders on the way to it, then the
    /// folder itself, then what is inside the selected folder.
    fn draw_columns(
        &self,
        ui: &mut egui::Ui,
        p: &Palette,
        group_id: GroupId,
        tab: &crate::workspace::Tab,
        dir: &Path,
        actions: &mut Vec<Action>,
    ) {
        use neutron_ui::columns::{Column, ColumnAction, Contents, Role};

        let chain = crate::columns::chain(dir, self.home.as_deref());
        let peek = crate::columns::peek(tab);
        let arriving = crate::columns::arriving(tab);
        let current = chain.len() - 1;

        // The entry in each folder that leads to the next, owned here so the
        // columns can borrow it.
        let through: Vec<String> = chain
            .iter()
            .skip(1)
            .map(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            .collect();
        let mut dirs: Vec<&Path> = chain.iter().map(PathBuf::as_path).collect();
        if let Some(peek) = &peek {
            dirs.push(peek);
        }
        // One per column: an executable's icon is keyed by its full path, so
        // each column's rows need their own folder to root it.
        let icons: Vec<RowIcons> = dirs
            .iter()
            .map(|d| RowIcons {
                service: &self.icons,
                thumbs: &self.thumbs,
                texture: self.icon_texture,
                dir: Some(d.to_path_buf()),
            })
            .collect();

        let columns: Vec<Column> = dirs
            .iter()
            .enumerate()
            .map(|(i, &d)| {
                let (contents, role) = if i < current {
                    (self.column_contents(d), Role::Named { highlight: Some(through[i].as_str()) })
                } else if i == current && !arriving {
                    (
                        Contents::Ready(&tab.list),
                        Role::Current {
                            selection: &tab.selection,
                            rename: self
                                .rename
                                .as_ref()
                                .filter(|r| r.tab == tab.id)
                                .map(|r| (r.idx, r.text.as_str(), r.generation)),
                            scroll_to: tab.view.scroll_to,
                        },
                    )
                } else if i == current {
                    // Still loading: drawn from the listing the column view
                    // already had, with what will be selected marked.
                    let pending = tab.reselect.as_ref().and_then(|r| r.cursor.as_deref());
                    (self.column_contents(d), Role::Named { highlight: pending })
                } else {
                    (self.column_contents(d), Role::Named { highlight: None })
                };
                Column {
                    dir: d,
                    contents,
                    role,
                    cut: (self.cut_dir.as_deref() == Some(d))
                        .then_some(&self.cut_names)
                        .filter(|names| !names.is_empty()),
                    icons: Some(&icons[i] as &dyn file_list::IconSource),
                }
            })
            .collect();

        // The selected file, described in a column at the end — when exactly
        // one thing is selected and it is not a folder, whose contents take
        // that place instead.
        let described = (!arriving && peek.is_none())
            .then(|| visible_selection(tab))
            .filter(|selected| selected.len() == 1)
            .map(|selected| selected[0])
            .filter(|&idx| !tab.list.kind(idx).is_container() || crate::columns::is_package(&dir.join(tab.list.name(idx))));
        let info = described.map(|idx| neutron_ui::columns::Info {
            name: tab.list.name(idx),
            kind: tab.list.kind(idx),
            kind_label: file_list::kind_label(tab.list.kind(idx), tab.list.name(idx)),
            size: Some(tab.list.size(idx)),
            modified: tab.list.modified(idx),
            location: tidy_location(dir),
            quick_look: cfg!(target_os = "macos"),
            preview: self.preview_of(&dir.join(tab.list.name(idx)), tab, idx),
        });

        if let Some(a) = neutron_ui::columns::show(ui, p, &columns, info.as_ref()) {
            if group_id != self.workspace.focused {
                actions.push(Action::FocusGroup(group_id));
            }
            match a {
                ColumnAction::Current(a) => actions.push(list_action(a)),
                ColumnAction::Pick { dir, entry, double } => {
                    actions.push(Action::ColumnPick { dir, entry, double })
                }
                ColumnAction::Open => {
                    if let Some(idx) = described {
                        actions.push(Action::Activate(idx));
                    }
                }
                ColumnAction::QuickLook => {
                    if let Some(idx) = described {
                        actions.push(Action::QuickLook(dir.join(tab.list.name(idx))));
                    }
                }
            }
        }
    }

    /// A column's listing as the cache has it.
    fn column_contents(&self, dir: &Path) -> neutron_ui::columns::Contents<'_> {
        use crate::columns::Slot;
        use neutron_ui::columns::Contents;
        match self.columns.get(dir) {
            Some(Slot::Ready { list, .. }) => Contents::Ready(list.as_ref()),
            Some(Slot::Failed(e)) => Contents::Failed(e),
            Some(Slot::Loading) | None => Contents::Loading,
        }
    }

    /// Whether the focused tab is showing a folder in the column view.
    fn in_columns(&self) -> bool {
        self.workspace.active_tab().is_some_and(|t| {
            t.view.view == file_list::ViewMode::Columns && t.location().as_path().is_some()
        })
    }

    /// Takes the column listings that have arrived, and tells the cache which
    /// folders the column views on screen need next.
    fn pump_columns(&mut self) {
        self.columns.pump();

        let mut wanted = Vec::new();
        // Every pane of every window is on screen, so every pane's active tab
        // counts — the cache keeps only what is asked for.
        let panes = self.workspaces().flat_map(|w| w.groups.values().map(move |g| (w, g)));
        for (workspace, group) in panes {
            let Some(tab) = group.active_tab().and_then(|t| workspace.tabs.get(&t)) else {
                continue;
            };
            if tab.view.view != file_list::ViewMode::Columns {
                continue;
            }
            let Some(dir) = tab.location().as_path() else {
                continue;
            };
            let (sort, show_hidden) = (tab.view.sort, tab.view.show_hidden);
            let want = |dir: PathBuf| crate::columns::Want { dir, sort, show_hidden };

            // The folders before the current one; and the current one as well
            // while the tab is still arriving, because its column is drawn
            // from here until the tab's own listing lands.
            let chain = crate::columns::chain(dir, self.home.as_deref());
            let upto = if crate::columns::arriving(tab) { chain.len() } else { chain.len() - 1 };
            wanted.extend(chain.into_iter().take(upto).map(want));
            if let Some(peek) = crate::columns::peek(tab) {
                wanted.push(want(peek));
            }
        }
        self.columns.want(&wanted);
    }

    /// Right and Left in the column view: into the folder under the cursor, or
    /// back out to the folder that contains this one.
    ///
    /// Out selects the folder just left, so Left then Right is a round trip.
    /// In selects the first entry when its listing is already in the column to
    /// the right — which it is once the cursor has rested on the folder.
    fn column_step(&mut self, delta: isize) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(t) = self.workspace.tabs.get(&tab_id) else {
            return;
        };
        let Some(dir) = t.location().as_path().map(Path::to_path_buf) else {
            return;
        };

        let (target, select) = if delta > 0 {
            if crate::columns::arriving(t) {
                return;
            }
            let Some(idx) = t.selection.cursor().filter(|&i| t.list.rank(i).is_some()) else {
                return;
            };
            let full = dir.join(t.list.name(idx));
            if !t.list.kind(idx).is_container() || crate::columns::is_package(&full) {
                return;
            }
            let first = crate::columns::first_name(self.columns.get(&full));
            (full, first)
        } else {
            let Some(parent) = dir.parent() else {
                // A drive root has no parent folder, but it does have a place
                // above it — This PC — and Up has always gone there.
                if let Some(above) = t.location().parent() {
                    self.navigate(tab_id, above);
                }
                return;
            };
            let left = dir.file_name().map(|n| n.to_string_lossy().into_owned());
            (parent.to_path_buf(), left)
        };

        self.navigate(tab_id, NodeId::Path(target));
        if let Some(t) = self.workspace.tabs.get_mut(&tab_id) {
            t.reselect = select.map(|name| crate::workspace::Reselect {
                names: vec![name.clone()],
                cursor: Some(name),
            });
        }
    }

    /// A click in a column other than the current one: that folder becomes
    /// the current one, with the entry clicked selected.
    fn column_pick(
        &mut self,
        dir: PathBuf,
        entry: Option<(String, neutron_core::EntryKind)>,
        double: bool,
    ) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };

        // A double-click opens, as Enter does: a folder becomes the current
        // one and anything else goes to its application. The first click of
        // the pair has already selected it.
        if double {
            if let Some((name, kind)) = entry {
                let full = dir.join(&name);
                if kind.is_container() && !crate::columns::is_package(&full) {
                    self.navigate(tab_id, NodeId::Path(full));
                } else {
                    self.open_local(full);
                }
            }
            return;
        }

        let target = NodeId::Path(dir);
        let Some(t) = self.workspace.tabs.get_mut(&tab_id) else {
            return;
        };
        // Already there and loaded: select by name now rather than reloading.
        if t.location() == &target && !crate::columns::arriving(t) {
            let found = entry
                .as_ref()
                .and_then(|(name, _)| (0..t.list.len()).find(|&i| t.list.name(i) == name));
            match found {
                Some(idx) => {
                    t.selection.apply(&t.list, idx, SelectMode::Replace);
                    t.view.scroll_to = Some(idx);
                }
                None => t.selection.clear(),
            }
            return;
        }

        // A no-op when the tab is already on its way there; the selection is
        // then applied when that load lands.
        self.navigate(tab_id, target);
        if let Some(t) = self.workspace.tabs.get_mut(&tab_id) {
            t.reselect = entry.map(|(name, _)| crate::workspace::Reselect {
                names: vec![name.clone()],
                cursor: Some(name),
            });
        }
    }
}

// --- writing to servers -----------------------------------------------------

impl NeutronApp {
    /// Whether the focused tab is a folder on a saved server.
    fn on_server(&self) -> bool {
        self.workspace.active_tab().is_some_and(|t| t.location().as_remote().is_some())
    }

    /// Names in a tab's listing, for checking what a write would collide with.
    fn listed_names(&self, tab: TabId) -> Vec<String> {
        self.workspace.tabs.get(&tab).map_or_else(Vec::new, |t| {
            (0..t.list.len()).map(|i| t.list.name(i).to_owned()).collect()
        })
    }

    /// Runs `work` against the server `tab` is showing, on a thread of its own.
    ///
    /// One at a time: a second write while the first is still going would
    /// race it for the same names, and the status bar has room for one.
    fn start_remote(
        &mut self,
        tab: TabId,
        label: String,
        rename_after: Option<String>,
        work: impl FnOnce(&neutron_remote::Site, &str, &crate::remote_ops::Progress) -> Result<String, String>
        + Send
        + 'static,
    ) {
        if let Some(running) = self.remote_job.as_ref() {
            self.notices.info(format!("Still busy: {}", running.label));
            return;
        }
        let Some((site_id, dir)) = self
            .workspace
            .tabs
            .get(&tab)
            .and_then(|t| t.location().as_remote())
            .map(|(site, dir)| (site.to_owned(), dir.to_owned()))
        else {
            return;
        };
        let Some(site) = self.sites.get(&site_id).cloned() else {
            self.notices.error(format!("No saved server called {site_id:?}"));
            return;
        };

        let progress = std::sync::Arc::new(crate::remote_ops::Progress::default());
        let (tx, rx) = crossbeam_channel::bounded(1);
        let worker_progress = std::sync::Arc::clone(&progress);
        let wake = self.ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("neutron-remote-write".into())
            .spawn(move || {
                let _ = tx.send(work(&site, &dir, &worker_progress));
                wake.request_repaint();
            });
        if let Err(e) = spawned {
            self.notices.error(format!("Could not start: {e}"));
            return;
        }
        self.remote_job = Some(RemoteJob { label, tab, progress, done: rx, rename_after });
    }

    fn pump_remote_job(&mut self) {
        let Some(job) = self.remote_job.as_ref() else {
            return;
        };
        let Ok(outcome) = job.done.try_recv() else {
            return;
        };
        let Some(job) = self.remote_job.take() else {
            return;
        };
        match outcome {
            Ok(receipt) => {
                self.notices.info(receipt);
                if let Some(name) = job.rename_after {
                    self.pending_rename = Some((job.tab, name));
                }
            }
            Err(reason) => self.notices.error(format!("{} — {reason}", job.label)),
        }
        // Re-read whatever happened, including a write that failed partway:
        // the listing is the only account of the server that is true.
        self.refresh_in_place(job.tab);
    }

    /// The server's display name for the focused tab, for labels.
    fn server_name(&self, tab: TabId) -> String {
        self.workspace
            .tabs
            .get(&tab)
            .and_then(|t| t.location().as_remote())
            .and_then(|(site, _)| self.sites.get(site))
            .map_or_else(|| "the server".to_owned(), |s| s.display().to_owned())
    }

    fn remote_new_folder(&mut self) {
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };
        let taken = self.listed_names(tab);
        let name = neutron_core::naming::next_available("New folder", |c| taken.iter().any(|t| t == c));
        let op = crate::remote_ops::Op::CreateDir { name: name.clone() };
        let label = format!("Creating {name} on {}", self.server_name(tab));
        self.start_remote(tab, label, Some(name), move |site, dir, progress| {
            crate::remote_ops::run(site, dir, &op, progress)
        });
    }

    fn remote_rename(&mut self, tab: TabId, place: &NodeId, from: String, to: String) {
        // Checked against the listing, because a server's rename may quietly
        // replace what is already there. Only when the tab still shows the
        // folder the rename began in; otherwise there is nothing to check
        // against, and guessing is how a file gets overwritten.
        if self.workspace.tabs.get(&tab).map(|t| t.location()) != Some(place) {
            self.notices.info("The folder changed before the rename finished, so nothing was renamed");
            return;
        }
        if !from.eq_ignore_ascii_case(&to) && self.listed_names(tab).contains(&to) {
            self.notices.error(format!("{to} is already here — pick another name"));
            return;
        }
        let op = crate::remote_ops::Op::Rename { from, to: to.clone() };
        let label = format!("Renaming to {to}");
        self.start_remote(tab, label, None, move |site, dir, progress| {
            crate::remote_ops::run(site, dir, &op, progress)
        });
    }

    fn remote_delete(&mut self) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(t) = self.workspace.tabs.get(&tab_id) else {
            return;
        };
        let entries: Vec<(String, neutron_core::EntryKind)> = visible_selection(t)
            .into_iter()
            .map(|i| (t.list.name(i).to_owned(), t.list.kind(i)))
            .collect();
        if entries.is_empty() {
            return;
        }
        let label = format!("Deleting from {}", self.server_name(tab_id));
        let op = crate::remote_ops::Op::Delete { entries };
        self.start_remote(tab_id, label, None, move |site, dir, progress| {
            crate::remote_ops::run(site, dir, &op, progress)
        });
    }

    /// Uploads whatever local files are on the clipboard.
    ///
    /// A cut onto a server copies: the originals stay, because removing local
    /// files after a network transfer is a second, separate risk, and nothing
    /// here can undo it if the upload turns out to have been incomplete.
    fn remote_paste(&mut self) {
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };
        let taken = self.listed_names(tab);
        let label = format!("Uploading to {}", self.server_name(tab));
        self.start_remote(tab, label, None, move |site, dir, progress| {
            // Read here, on the worker: another process can hold the clipboard.
            let Some((sources, _)) = neutron_shell::clipboard::read() else {
                return Err("there are no files on the clipboard — copy files first".to_owned());
            };
            upload_new(site, dir, sources, &taken, progress)
        });
    }

    fn remote_upload(&mut self, tab: TabId, sources: Vec<PathBuf>) {
        let taken = self.listed_names(tab);
        let label = format!("Uploading to {}", self.server_name(tab));
        self.start_remote(tab, label, None, move |site, dir, progress| {
            upload_new(site, dir, sources, &taken, progress)
        });
    }
}

/// Uploads `sources`, refusing up front any whose name the server folder
/// already has — an upload replaces, and there is no bin behind it.
fn upload_new(
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
fn certificate_in(text: &str) -> Option<String> {
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
fn list_action(a: FileListAction) -> Action {
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
struct RowIcons<'a> {
    service: &'a IconService,
    thumbs: &'a crate::thumbs::Thumbnails,
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

    fn preview(&self, name: &str, _kind: neutron_core::EntryKind, modified: i64) -> Option<neutron_ui::thumb::Thumb> {
        // A folder with no path — a server, a shell location — has nothing
        // the system could preview.
        let dir = self.dir.as_deref()?;
        self.thumbs.get(&dir.join(name), modified, crate::thumbs::Grade::Small)
    }
}

/// The interface's font files: Manrope where it has been bundled, otherwise
/// the system's own face on the Mac, otherwise egui's built-in one. With
/// Manrope, the system's face stands behind it for the symbols it lacks.
///
/// Read once at start-up from the app bundle's resources, or from the source
/// tree's `assets/fonts` during development.
fn interface_faces() -> Option<theme::Faces> {
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
fn trash_folder() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".Trash"))
}

/// The menu wording for pinning: the Mac calls the list the sidebar.
fn pin_label(pinned: bool, name: Option<&str>) -> String {
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

fn pos(x: f32, y: f32) -> egui::Pos2 {
    egui::pos2(x, y)
}

/// Paints a side panel's pane surface under its contents.
///
/// The panel's frame paints nothing — a frame can only fill flat — so this
/// runs first inside it, on the rectangle the frame would have filled, and
/// everything the panel draws afterwards lands on top. Clipped to the window
/// rather than to the panel, so the shadow is not cut off at the panel's edge.
fn paint_panel_surface(ui: &egui::Ui, inner: egui::Margin, p: &Palette) {
    let rect = ui.max_rect() + inner;
    let painter = ui.painter().with_clip_rect(ui.ctx().content_rect());
    theme::paint_pane(&painter, rect, theme::RADIUS_CARD as f32, p);
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
fn window_handle(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

#[cfg(test)]
mod certificate_tests {
    use super::certificate_in;

    #[test]
    fn the_fingerprint_is_found_in_a_refusal() {
        let fp = vec!["AB"; 32].join(":");
        let message = format!("files.example.com presented a certificate nobody vouches for (SHA-256 {fp}). Edit the server to trust it.");
        assert_eq!(certificate_in(&message), Some(fp));
        assert_eq!(certificate_in("connection refused"), None);
    }
}

#[cfg(test)]
mod screens {
    //! Renders the real interface to PNG files, for looking at a visual change
    //! without driving the app by hand.
    //!
    //! Ignored by default: it needs a GPU and writes files. Run with
    //! `NEUTRON_SHOTS=<dir> cargo test -p neutron-app screens -- --ignored`;
    //! `NEUTRON_SHOTS_FOLDER` picks the folder shown (the repository otherwise).
    use super::*;

    fn settle(harness: &mut egui_kittest::Harness<'_, NeutronApp>) {
        // Listings arrive from worker threads, so frames alone are not enough:
        // give the workers wall-clock time between them.
        for _ in 0..30 {
            harness.run_steps(2);
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn shoot(harness: &mut egui_kittest::Harness<'_, NeutronApp>, out: &Path, name: &str) {
        let mut image = harness.render().expect("render");
        // The window is transparent on the Mac; lay it over something like a
        // dark desktop so the picture reads the way the app does.
        for pixel in image.pixels_mut() {
            let [r, g, b, a] = pixel.0;
            let a = a as f32 / 255.0;
            let over = |c: u8, bg: f32| (c as f32 + bg * (1.0 - a)).round().min(255.0) as u8;
            pixel.0 = [over(r, 22.0), over(g, 22.0), over(b, 28.0), 255];
        }
        image.save(out.join(name)).expect("save");
    }

    fn act(harness: &mut egui_kittest::Harness<'_, NeutronApp>, action: Action) {
        let ctx = harness.ctx.clone();
        harness.state_mut().apply(&ctx, action);
    }

    #[test]
    #[ignore]
    fn screens() {
        let Some(out) = std::env::var_os("NEUTRON_SHOTS").map(PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&out).unwrap();
        let folder = std::env::var("NEUTRON_SHOTS_FOLDER").unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap().display().to_string()
        });

        let (_keep, opened) = crossbeam_channel::unbounded();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .with_pixels_per_point(1.0)
            .wgpu()
            .build_eframe(|cc| NeutronApp::new(cc, StartupTimer::new(Instant::now()), Some(folder), opened));

        act(&mut harness, Action::SetView(file_list::ViewMode::List));
        settle(&mut harness);
        // A selected row, so the selection is in the picture.
        let idx = {
            let t = harness.state().workspace.active_tab().unwrap();
            (0..t.list.len()).find(|&i| t.list.name(i) == "Cargo.toml").unwrap_or(0)
        };
        act(&mut harness, Action::Select { idx, mode: SelectMode::Replace });
        settle(&mut harness);
        shoot(&mut harness, &out, "list.png");

        act(&mut harness, Action::TogglePreview);
        harness.state_mut().notices.info("Copied 1 item");
        settle(&mut harness);
        shoot(&mut harness, &out, "preview.png");
        act(&mut harness, Action::TogglePreview);
        act(&mut harness, Action::DismissNotice);

        act(&mut harness, Action::SetView(file_list::ViewMode::Columns));
        settle(&mut harness);
        shoot(&mut harness, &out, "columns.png");

        act(&mut harness, Action::SetView(file_list::ViewMode::Grid));
        settle(&mut harness);
        shoot(&mut harness, &out, "grid.png");

        // The overlays, over the list.
        act(&mut harness, Action::SetView(file_list::ViewMode::List));
        act(&mut harness, Action::ContextMenu { idx: Some(idx), pos: egui::pos2(760.0, 470.0) });
        settle(&mut harness);
        shoot(&mut harness, &out, "menu.png");
        // Dropping the reply releases the thread parked on the menu.
        harness.state_mut().menu = None;
        harness.state_mut().menu_reply = None;

        // The folder's own menu, with nothing selected.
        act(&mut harness, Action::ClearSelection);
        act(&mut harness, Action::ContextMenu { idx: None, pos: egui::pos2(760.0, 300.0) });
        settle(&mut harness);
        shoot(&mut harness, &out, "menu-folder.png");
        harness.state_mut().menu = None;
        harness.state_mut().menu_reply = None;
        act(&mut harness, Action::Select { idx, mode: SelectMode::Replace });

        act(&mut harness, Action::ToggleFinder(crate::finder::Mode::Files));
        settle(&mut harness);
        shoot(&mut harness, &out, "finder.png");
        act(&mut harness, Action::CloseFinder);

        // The question alone, set directly: `apply` would delete. What follows
        // a yes is made harmless in case anything answers it.
        {
            let app = harness.state_mut();
            let question = app.question_for(&Action::Delete { permanent: true });
            let subject = app.subject();
            app.confirm = question.map(|q| (q, Box::new(Action::DismissNotice), subject));
        }
        settle(&mut harness);
        shoot(&mut harness, &out, "confirm.png");
        harness.state_mut().confirm = None;

        // A question about clashing names. Nothing is transferred: the
        // request names no files, and the question is cleared unanswered.
        {
            let app = harness.state_mut();
            let tab = app.workspace.active_tab_id().unwrap();
            app.clash_questions.push_back(crate::transfers::Question {
                request: crate::transfers::Request {
                    tab,
                    paths: vec![],
                    destination: PathBuf::from("/Users/someone/Downloads"),
                    how: neutron_shell::fileops::Transfer::Copy,
                },
                names: vec!["report.pdf".into(), "notes.md".into(), "photo.heic".into()],
            });
        }
        settle(&mut harness);
        shoot(&mut harness, &out, "clash.png");
        harness.state_mut().clash_questions.clear();

        act(&mut harness, Action::OpenConnect);
        settle(&mut harness);
        shoot(&mut harness, &out, "connect.png");
        harness.state_mut().connect.close();

        // Inside an archive, when one is given.
        if let Some(archive) = std::env::var_os("NEUTRON_SHOTS_ARCHIVE").map(PathBuf::from) {
            act(&mut harness, Action::SetView(file_list::ViewMode::List));
            act(&mut harness, Action::Navigate(NodeId::archive(archive.clone(), "project")));
            settle(&mut harness);
            act(&mut harness, Action::Select { idx: 0, mode: SelectMode::Replace });
            settle(&mut harness);
            shoot(&mut harness, &out, "archive.png");
            act(&mut harness, Action::ContextMenu { idx: Some(0), pos: egui::pos2(700.0, 260.0) });
            settle(&mut harness);
            shoot(&mut harness, &out, "archive-menu.png");
            harness.state_mut().menu = None;
            act(&mut harness, Action::Back);
            settle(&mut harness);
        }

        // Tags, in a folder given for them.
        if let Some(folder) = std::env::var_os("NEUTRON_SHOTS_TAGS").map(PathBuf::from) {
            act(&mut harness, Action::SetView(file_list::ViewMode::List));
            act(&mut harness, Action::Navigate(NodeId::path(folder)));
            settle(&mut harness);
            act(&mut harness, Action::Select { idx: 1, mode: SelectMode::Replace });
            settle(&mut harness);
            shoot(&mut harness, &out, "tags.png");
            act(&mut harness, Action::SetView(file_list::ViewMode::Grid));
            settle(&mut harness);
            shoot(&mut harness, &out, "tags-grid.png");
            act(&mut harness, Action::SetView(file_list::ViewMode::List));
            act(&mut harness, Action::Back);
            settle(&mut harness);
        }

        // A second window: its own tabs, its listing routed to it.
        act(&mut harness, Action::NewWindow);
        settle(&mut harness);
        {
            let app = harness.state();
            let second = &app.windows[0].workspace;
            let tab = second.active_tab().expect("the new window has a tab");
            assert!(!tab.list.is_empty(), "the new window's listing did not reach it");
            assert!(
                app.workspace.active_tab_id() != second.active_tab_id(),
                "the windows share a tab id"
            );
        }
        shoot(&mut harness, &out, "windows.png");
        harness.state_mut().windows.clear();

        act(&mut harness, Action::Navigate(NodeId::Home));
        settle(&mut harness);
        shoot(&mut harness, &out, "dashboard.png");

        // The other theme, back where the list was. Flipped back afterwards:
        // the choice is saved, and a test has no business changing it.
        act(&mut harness, Action::Back);
        act(&mut harness, Action::ToggleTheme);
        settle(&mut harness);
        shoot(&mut harness, &out, "light.png");
        act(&mut harness, Action::ToggleTheme);
        settle(&mut harness);
    }
}
