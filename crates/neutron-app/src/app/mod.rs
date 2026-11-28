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
use neutron_ui::theme::{self, Palette, ThemeMode};

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

/// Key under which the windows after the first are stored.
const WINDOWS_KEY: &str = "neutron.windows";

/// A window after the first, as saved between sessions.
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedWindow {
    workspace: crate::workspace::PersistedWorkspace,
    preview: bool,
    /// Where its top-left corner was, and how big it was, in points.
    at: Option<[f32; 2]>,
    size: Option<[f32; 2]>,
}

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
    /// Gather the listing into groups under headings, or stop.
    GroupBy(neutron_core::GroupBy),
    ClearSelection,
    /// A rubber band covered these display positions.
    Marquee { from: usize, to: usize, additive: bool },
    SelectAll,
    ToggleHidden,
    /// Add up the size of every folder shown, or stop.
    ToggleAllSizes,
    /// Put the sidebar away, or bring it back; the same for the action rail.
    ToggleSidebar,
    ToggleToolbar,
    /// Draw the grid's tiles at this scale of the standard size.
    SetTileScale(f32),
    /// Add up the size of these folders.
    CalculateSizes(Vec<PathBuf>),
    /// Step to the next view: list, grid, columns.
    ToggleView,
    /// Show the focused tab in this view.
    SetView(file_list::ViewMode),
    /// A click in a column other than the current one: go to `dir` and select
    /// `entry`. See `neutron_ui::columns::ColumnAction::Pick`.
    ColumnPick {
        dir: NodeId,
        entry: Option<(String, neutron_core::EntryKind)>,
        double: bool,
    },
    ToggleTheme,
    SetTheme(ThemeMode),
    /// Narrow the focused tab's listing. Empty clears the filter.
    SetFilter(String),
    /// Put the caret in the focused pane's filter field.
    FocusFilter,
    /// Drag the selection out to another application.
    DragOut,
    /// Drag these items out, whatever is selected.
    DragPaths(Vec<PathBuf>),
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
    /// Make a file in the focused tab — empty, or a copy of the named
    /// template — then rename it.
    NewFile(Option<String>),
    /// Show the folder templates are kept in.
    OpenTemplates,
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
    /// The system's Share menu for the chosen items.
    Share,
    /// Send the chosen items with AirDrop.
    AirDrop,
    /// Give the chosen items a Finder tag, or take it away.
    ToggleTag { color: u8, on: bool },
    /// Open the Edit Tags dialog on the chosen items.
    EditTags,
    /// Give these items exactly these tags.
    SetTags(Vec<PathBuf>, Vec<String>),
    /// Rename each `(old, new)` in the folder, as one undoable change.
    BatchRename(PathBuf, Vec<(String, String)>),
    /// Go to the folder `.0` is in, with it selected.
    ShowInFolder(PathBuf),
    /// Open a file in a viewer tab beside the folder.
    OpenInViewer(PathBuf),
    /// Open a file with its own app.
    LaunchPath(PathBuf),
    /// Go to what an alias or a symbolic link stands for, selected.
    ShowOriginal(PathBuf),
    /// The Finder's Info window on these items.
    FinderInfo(Vec<PathBuf>),
    /// Move items in the Trash back where they came from.
    PutBack(Vec<PathBuf>),
    /// Open the New Smart Folder dialog — on the search the tab shows, if it
    /// shows one, to change it.
    NewSmartFolder,
    /// Change, or forget, a saved search, by its name.
    EditSearch(String),
    RemoveSearch(String),
    /// Show what a search finds, keeping it in the sidebar if named.
    RunSearch { query: neutron_core::query::Query, save_as: Option<String>, replacing: Option<String> },
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
    /// Stop the running server transfer by job id.
    CancelRemoteJob(u64),
    /// Download selected server items directly into the local Downloads folder.
    DownloadRemoteToDownloads,
    /// Compute recursive directory sizes on an SFTP server.
    CalculateRemoteSizes(String, Vec<String>),
    /// Copy the focused repo's Git branch name to the clipboard.
    CopyGitBranch,
    /// Open a terminal tab running `ntr status` / `git status` for the current repo.
    RunNtrStatus,
    /// Open a terminal tab inspecting the latest Neutrino CI run for the current repo.
    InspectNtrCi,
    /// Open interactive Storage Lens for a local or remote drive/folder.
    OpenStorageLens(NodeId),
    /// Close Storage Lens overlay.
    CloseStorageLens,
    /// Drill into a folder inside Storage Lens.
    DrillStorageLens(NodeId, String),
    /// Go up one level inside Storage Lens.
    UpStorageLens,
    /// Rescan current location inside Storage Lens.
    RescanStorageLens,
    /// Move a local folder/file to Trash from Storage Lens.
    TrashFromStorageLens(PathBuf),
    /// Toggle side-by-side directory diff in split panes.
    ToggleComparePanes,
    /// Sync missing and newer items from `src` pane's active tab to `dst` pane's active tab.
    SyncPaneToOther { src: GroupId, dst: GroupId },
    /// Open saved workspace layouts manager.
    OpenLayouts,
    CloseLayouts,
    SaveLayout(String),
    ApplyLayout(String),
    DeleteLayout(String),
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
    /// Open an AI agent in a terminal tab on the focused folder, told what is
    /// selected.
    AskAgent(crate::agent::Agent),
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
    /// Files dropped onto a folder: a row, or a place in the sidebar.
    DropInto { dir: PathBuf, paths: Vec<PathBuf>, copy: bool },

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
const MENU_ID_EDIT_SEARCH: u32 = 0x0800_0012;
const MENU_ID_REMOVE_SEARCH: u32 = 0x0800_0013;
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

/// Remote files copied or cut from a server for download, same-server move/copy,
/// or cross-server transfer.
#[derive(Clone)]
struct RemoteClipboard {
    site_id: String,
    dir: String,
    entries: Vec<(String, neutron_core::EntryKind)>,
    cut: bool,
}

/// A remote read/write/transfer job, running on its own worker with a pooled SSH handle.
struct RemoteJob {
    id: u64,
    /// "Uploading 3 items to files.example.com", for the status bar.
    label: String,
    /// The tab to re-read when it finishes.
    tab: TabId,
    /// Optional source tab to refresh too (after a cross-pane Cut).
    extra_refresh_tab: Option<TabId>,
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
    /// `(site_id, remote_path, display_name)` when fetching from a server,
    /// so saves to the downloaded file sync back.
    remote_source: Option<(String, String, String)>,
}

/// A remote file opened locally whose edits are uploaded back on save.
struct WatchedRemoteEdit {
    site_id: String,
    remote_path: String,
    name: String,
    local_path: PathBuf,
    last_mtime: Option<std::time::SystemTime>,
    last_len: u64,
    syncing: bool,
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
type RemoteSearchReply = (u64, String, String, Result<Vec<neutron_remote::RemoteSearchHit>, String>);

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
    pin_target: Option<NodeId>,
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
    /// Where a drag over this window is, this frame, and the panes it could
    /// land in; see `resolve_drop`.
    drop_at: Option<egui::Pos2>,
    drop_panes: std::cell::RefCell<Vec<(GroupId, egui::Rect)>>,
    /// The folder a drag has rested on, since when, and whether it has
    /// already sprung open.
    spring: Option<(PathBuf, Instant, bool)>,
    #[cfg(test)]
    drop_debug: Vec<neutron_ui::drop::Target>,
    /// The first window's frame as last seen, saved in eframe's own format;
    /// see `root_window_settings`.
    root_frame: Option<String>,
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
    /// Active remote transfer/write queue. See `crate::remote_ops`.
    remote_jobs: Vec<RemoteJob>,
    next_remote_job: u64,
    /// Remote selection clipboard (Copy/Cut off a server).
    remote_clipboard: Option<RemoteClipboard>,
    /// Opened server files watched so local edits sync back on save.
    remote_edits: Vec<WatchedRemoteEdit>,
    remote_edits_checked: Instant,
    remote_edit_tx: crossbeam_channel::Sender<(PathBuf, String, String, Result<String, String>)>,
    remote_edit_rx: crossbeam_channel::Receiver<(PathBuf, String, String, Result<String, String>)>,
    /// Mounted storage volumes reported by connected SFTP servers.
    remote_drives: std::collections::HashMap<String, Vec<Place>>,
    remote_drives_tx: crossbeam_channel::Sender<(String, Vec<Place>)>,
    remote_drives_rx: crossbeam_channel::Receiver<(String, Vec<Place>)>,
    probing_sites: std::collections::HashSet<String>,
    last_remote_probe: Instant,
    remote_search_gen: u64,
    remote_search_site: Option<String>,
    remote_search_tx: crossbeam_channel::Sender<RemoteSearchReply>,
    remote_search_rx: crossbeam_channel::Receiver<RemoteSearchReply>,
    remote_sizes: std::collections::HashMap<(String, String), u64>,
    remote_sizes_pending: std::collections::HashSet<(String, String)>,
    remote_sizes_version: u64,
    remote_sizes_tx: crossbeam_channel::Sender<(String, String, Option<u64>)>,
    remote_sizes_rx: crossbeam_channel::Receiver<(String, String, Option<u64>)>,
    vcs: crate::vcs::VcsService,
    storage_lens: crate::storage_lens::StorageLens,
    compare_panes: bool,
    layouts: crate::layouts::Layouts,
    layout_dialog: crate::layouts::LayoutDialog,

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
    /// Files taken out of an archive to be shared: where to point the share
    /// menu, or `None` for AirDrop. Shared on the main thread, as AppKit needs.
    share_tx: crossbeam_channel::Sender<ShareRequest>,
    share_rx: crossbeam_channel::Receiver<ShareRequest>,
    /// The window being drawn: the first is the root viewport.
    viewport: egui::ViewportId,
    /// Origin of the window being drawn, as `WindowState::origin`.
    origin: Option<egui::Pos2>,
    /// The other windows, each with its state; see `WindowState`.
    windows: Vec<WindowState>,
    /// Folder sizes, added up on a thread of their own.
    sizer: crate::folder_sizes::Sizer,
    /// The terminals tabs show, by the id in their location. Behind a cell
    /// because panes are drawn through `&self`, and drawing a terminal sends it
    /// keys and fits it to its pane.
    terminals: std::cell::RefCell<std::collections::HashMap<u64, crate::term::Session>>,
    /// Neutron's Get Info windows.
    infos: crate::info::Infos,
    /// Searches kept in the sidebar, and the dialog that makes them.
    saved_searches: Vec<crate::searches::Saved>,
    search_dialog: Option<crate::searches::Dialog>,
    /// What aliases stand for, found on a worker, to be shown selected.
    originals_tx: crossbeam_channel::Sender<PathBuf>,
    originals_rx: crossbeam_channel::Receiver<PathBuf>,
    /// Files open in viewer tabs. Behind a cell for the reason the
    /// terminals are.
    viewers: std::cell::RefCell<crate::viewer::Viewers>,
    /// Terminals just opened, which take the keyboard when first drawn.
    terminal_focus: std::cell::RefCell<std::collections::HashSet<u64>>,
    /// Whether every folder shown is added up; see View › Calculate All Sizes.
    all_sizes: bool,
    /// Whether the sidebar and the action rail are put away, in every window.
    hide_sidebar: bool,
    hide_toolbar: bool,
    /// The sizer's version each tab last took sizes from.
    sizes_seen: std::collections::HashMap<TabId, u64>,
    /// Counts windows opened, to name each viewport.
    windows_opened: u64,
    mail: Mail,
    /// The window that opened Quick Look, which its arrow keys move.
    look_window: Option<egui::ViewportId>,
    /// The window a context menu being built was asked for in.
    menu_window: Option<egui::ViewportId>,
    /// The window that last had focus, which shows notices and jobs. Kept
    /// while Neutron is in the background, so they do not vanish with it.
    last_focused: egui::ViewportId,
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
    tag_editor: Option<TagEditor>,
    batch_rename: Option<BatchRenameDialog>,
    /// The names in the Templates folder, as last read on a worker.
    templates: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
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

        // Read before the first paint, so a saved theme does not open in the
        // default palette and flip a frame later.
        let prefs = saved_prefs(cc);
        let theme_mode = prefs
            .theme
            .as_deref()
            .and_then(ThemeMode::from_id)
            .unwrap_or(if prefs.light_theme {
                ThemeMode::Light
            } else {
                ThemeMode::default()
            });
        theme::apply(&cc.egui_ctx, theme_mode);
        // The date groups — Today, Yesterday — are the user's days.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        neutron_core::sort::set_local_offset(neutron_ui::format::local_offset_secs(now));
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

        // Only a launch with no path asked for brings back the other windows:
        // `neutron ~/Downloads` is someone asking for one window, there.
        let restore_windows = start_path.is_none();
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
        let (share_tx, share_rx) = crossbeam_channel::unbounded();
        let originals = crossbeam_channel::unbounded();
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
        let (remote_edit_tx, remote_edit_rx) = crossbeam_channel::unbounded();
        let (remote_drives_tx, remote_drives_rx) = crossbeam_channel::unbounded();
        let (remote_search_tx, remote_search_rx) = crossbeam_channel::unbounded();
        let (remote_sizes_tx, remote_sizes_rx) = crossbeam_channel::unbounded();

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

        let mut app = Self {
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
            root_frame: None,
            drop_at: None,
            drop_panes: Default::default(),
            spring: None,
            #[cfg(test)]
            drop_debug: Vec::new(),
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
            remote_jobs: Vec::new(),
            next_remote_job: 1,
            remote_clipboard: None,
            remote_edits: Vec::new(),
            remote_edits_checked: Instant::now(),
            remote_edit_tx,
            remote_edit_rx,
            remote_drives: std::collections::HashMap::new(),
            remote_drives_tx,
            remote_drives_rx,
            probing_sites: std::collections::HashSet::new(),
            last_remote_probe: Instant::now(),
            remote_search_gen: 0,
            remote_search_site: None,
            remote_search_tx,
            remote_search_rx,
            remote_sizes: std::collections::HashMap::new(),
            remote_sizes_pending: std::collections::HashSet::new(),
            remote_sizes_version: 0,
            remote_sizes_tx,
            remote_sizes_rx,
            vcs: crate::vcs::VcsService::spawn(cc.egui_ctx.clone()),
            plugins,
            palette,
            archive_job: None,
            transfers: Vec::new(),
            next_transfer: 0,
            planned_tx,
            planned_rx,
            clash_questions: std::collections::VecDeque::new(),
            history: crate::undo::History::default(),
            share_tx,
            share_rx,
            viewport: egui::ViewportId::ROOT,
            origin: None,
            windows: Vec::new(),
            windows_opened: 0,
            mail: Mail::default(),
            look_window: None,
            menu_window: None,
            last_focused: egui::ViewportId::ROOT,
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
            tag_editor: None,
            batch_rename: None,
            sizer: crate::folder_sizes::Sizer::spawn(cc.egui_ctx.clone()),
            terminals: Default::default(),
            infos: Default::default(),
            saved_searches: crate::searches::load(),
            search_dialog: None,
            originals_tx: originals.0,
            originals_rx: originals.1,
            viewers: std::cell::RefCell::new(crate::viewer::Viewers::spawn(cc.egui_ctx.clone())),
            terminal_focus: Default::default(),
            all_sizes: saved_prefs(cc).all_sizes,
            hide_sidebar: saved_prefs(cc).hide_sidebar,
            hide_toolbar: saved_prefs(cc).hide_toolbar,
            sizes_seen: std::collections::HashMap::new(),
            templates: Default::default(),
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
            storage_lens: crate::storage_lens::StorageLens::default(),
            compare_panes: false,
            layouts: crate::layouts::load(),
            layout_dialog: crate::layouts::LayoutDialog::default(),
        };
        if restore_windows {
            app.restore_windows(cc);
        }
        app.refresh_templates();
        if !cfg!(test) {
            app.start_remote_volume_probe();
        }
        app
    }

    /// Reopens the windows after the first that were open when the last
    /// session ended, each with its tabs loading.
    fn restore_windows(&mut self, cc: &eframe::CreationContext<'_>) {
        let saved: Vec<SavedWindow> = cc
            .storage
            .and_then(|s| s.get_string(WINDOWS_KEY))
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        for saved in saved {
            let Some(workspace) = Workspace::restore(&saved.workspace) else { continue };
            self.windows_opened += 1;
            let viewport = egui::ViewportId::from_hash_of(("neutron-window", self.windows_opened));
            let origin = saved.at.map(|[x, y]| egui::pos2(x, y));
            let mut window = WindowState::new(&self.ctx, viewport, origin, NodeId::Home);
            window.workspace = workspace;
            window.size = saved.size.map(|[w, h]| egui::vec2(w, h));
            window.preview.open = saved.preview;
            // Loaded with the window's state in place, so each load is for one
            // of its tabs and lands there.
            self.swap_window(&mut window);
            let tabs: Vec<(TabId, NodeId)> =
                self.workspace.tabs.values().map(|t| (t.id, t.location().clone())).collect();
            for (tab, location) in tabs {
                self.reload(tab, location);
            }
            self.swap_window(&mut window);
            self.windows.push(window);
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
        // A terminal, or a file in the viewer, has nothing to list.
        if location.is_terminal() || matches!(location, NodeId::Viewer { .. }) {
            t.status = Status::Loaded { entries: 0, elapsed: Duration::ZERO };
            return;
        }
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
        refresh_tab_in(&mut self.workspace, &mut self.loader, tab);
    }

    fn navigate(&mut self, tab: TabId, id: NodeId) {
        let Some(t) = self.workspace.tabs.get_mut(&tab) else {
            return;
        };
        // Going somewhere from a terminal opens it beside the terminal rather
        // than ending the shell that is running.
        if t.location().is_terminal() {
            self.open_in_new_tab(id);
            return;
        }
        if t.location() == &id {
            return;
        }
        t.history.push(id.clone());
        self.reload(tab, id);
    }

}

mod actions;
mod chrome_ui;
mod helpers;
mod remote_state;
mod shortcuts;
#[cfg(test)]
mod tests;
mod window;

pub(crate) use helpers::*;
use window::*;
