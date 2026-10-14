//! Tabs, tab groups, and the workspace that owns them.
//!
//! A **tab** is one browsing context: a location, its history, its listing, and
//! its selection. A **group** is one pane of the layout and holds a stack of
//! tabs with one active. The [`Layout`] tree (in `neutron-core`) says how the
//! groups are arranged on screen.
//!
//! Tabs live in a flat map keyed by [`TabId`] rather than inside their group.
//! Moving a tab between panes is then a change to two small `Vec<TabId>`s
//! instead of relocating the tab's entire state — which includes a directory
//! listing that can be tens of megabytes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use neutron_core::{Axis, EntryList, GroupId, History, Layout, NodeId, Selection};
use neutron_ui::file_list::FileListState;

use crate::loader::{Loader, TabId};

/// What a tab's status bar is reporting.
#[derive(Debug, Clone)]
pub enum Status {
    Loading,
    Loaded { entries: usize, elapsed: Duration },
    Error(String),
}

pub struct Tab {
    pub id: TabId,
    pub history: History,
    pub list: EntryList,
    pub selection: Selection,
    pub view: FileListState,
    pub status: Status,
    /// Generation of the load this tab is waiting for. Results with any other
    /// generation belong to a navigation the user has already left.
    pub pending: Option<u64>,
    /// Names to select again once the pending load lands.
    ///
    /// A refresh re-reads the directory, which rebuilds the entry list and
    /// invalidates every index into it — including the selection, which is
    /// stored as indices so it can survive a re-sort. Names are the only
    /// identity that outlives the rebuild.
    ///
    /// `None` for a navigation, where the previous selection means nothing in
    /// the new folder.
    pub reselect: Option<Reselect>,
}

/// What was selected before a refresh, by name.
#[derive(Debug, Clone, Default)]
pub struct Reselect {
    pub names: Vec<String>,
    /// The cursor row, kept separately because it drives the preview pane and
    /// is not necessarily part of the selection.
    pub cursor: Option<String>,
}

impl Tab {
    fn new(id: TabId, location: NodeId, view: FileListState) -> Self {
        Self {
            id,
            history: History::new(location),
            list: EntryList::new(),
            selection: Selection::new(),
            view,
            status: Status::Loading,
            pending: None,
            reselect: None,
        }
    }

    /// Records what is selected now, so a refresh can restore it.
    pub fn remember_selection(&self) -> Reselect {
        Reselect {
            names: self
                .selection
                .iter()
                .filter(|&i| i < self.list.len())
                .map(|i| self.list.name(i).to_owned())
                .collect(),
            cursor: self
                .selection
                .cursor()
                .filter(|&i| i < self.list.len())
                .map(|i| self.list.name(i).to_owned()),
        }
    }

    /// Selects those names again, ignoring any that are gone.
    ///
    /// Deleted files simply do not come back, which is the desired behaviour
    /// after the operation that most often triggers a refresh.
    pub fn restore_selection(&mut self, wanted: &Reselect) {
        use std::collections::HashSet;

        let names: HashSet<&str> = wanted.names.iter().map(String::as_str).collect();
        let indices: Vec<usize> = (0..self.list.len())
            .filter(|&i| names.contains(self.list.name(i)))
            .collect();

        let cursor = wanted
            .cursor
            .as_deref()
            .and_then(|name| (0..self.list.len()).find(|&i| self.list.name(i) == name));

        self.selection.set(&indices, cursor);
    }

    pub fn location(&self) -> &NodeId {
        self.history.current()
    }

    /// Short label for the tab strip.
    pub fn title(&self) -> String {
        let name = self.location().display_name();
        if name.is_empty() {
            self.location().to_string()
        } else {
            name
        }
    }

    /// Rebuilds the display order from the listing already in memory, applying
    /// the current hidden-files setting and name filter.
    ///
    /// Never touches the disk: this runs on every keystroke in the filter
    /// field, and the whole reason the filter feels instant is that it is a
    /// pass over an in-memory array rather than a re-enumeration.
    pub fn refilter(&mut self) {
        neutron_core::sort::apply_filtered(
            &mut self.list,
            self.view.sort,
            self.view.show_hidden,
            &self.view.filter,
        );
        // Entries the filter now hides cannot stay selected, or the next
        // Delete would reach files the user cannot see.
        self.selection.retain_visible(&self.list);
        self.view.scroll_to = self.selection.cursor();
    }

    /// Entries currently listed, and the total before the filter.
    pub fn counts(&self) -> (usize, usize) {
        (self.list.order().len(), self.list.len())
    }
}

/// One pane's stack of tabs.
pub struct Group {
    pub id: GroupId,
    pub tabs: Vec<TabId>,
    pub active: usize,
}

impl Group {
    /// Takes `tab` out of the strip, keeping the same tab in front.
    ///
    /// Closing a tab to the left of the active one shifts every index after
    /// it down by one, so the active index has to follow — clamping alone
    /// silently brought the *next* tab to the front. When the active tab
    /// itself goes, the one that slides into its slot takes over, which is
    /// what every tabbed UI does.
    fn remove_tab(&mut self, tab: TabId) {
        let Some(position) = self.tabs.iter().position(|t| *t == tab) else {
            return;
        };
        self.tabs.remove(position);
        if position < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
    }

    pub fn active_tab(&self) -> Option<TabId> {
        self.tabs.get(self.active).copied()
    }
}

pub struct Workspace {
    pub layout: Layout,
    pub groups: HashMap<GroupId, Group>,
    pub tabs: HashMap<TabId, Tab>,
    /// Pane that receives keyboard input.
    pub focused: GroupId,
    next_tab: u64,
    next_group: u64,
}

impl Workspace {
    /// Creates a workspace with a single pane holding one tab at `location`.
    pub fn new(location: NodeId) -> Self {
        let group_id = GroupId(0);
        let tab_id = TabId(0);

        let mut groups = HashMap::new();
        groups.insert(
            group_id,
            Group {
                id: group_id,
                tabs: vec![tab_id],
                active: 0,
            },
        );

        let mut tabs = HashMap::new();
        tabs.insert(
            tab_id,
            Tab::new(tab_id, location, FileListState::default()),
        );

        Self {
            layout: Layout::leaf(group_id),
            groups,
            tabs,
            focused: group_id,
            next_tab: 1,
            next_group: 1,
        }
    }

    pub fn focused_group(&self) -> Option<&Group> {
        self.groups.get(&self.focused)
    }

    pub fn active_tab_id(&self) -> Option<TabId> {
        self.focused_group()?.active_tab()
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(&self.active_tab_id()?)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        let id = self.active_tab_id()?;
        self.tabs.get_mut(&id)
    }

    /// Clears the scroll request of every tab on screen, once a frame has
    /// drawn them.
    ///
    /// Panes draw from a clone of each tab's view state, so the widget's own
    /// "consumed once" never reaches the tab. Left standing, a request pins
    /// the listing to that row and the wheel cannot move it. Background tabs
    /// keep theirs until they are shown.
    pub fn consume_scroll_requests(&mut self) {
        for group in self.groups.values() {
            if let Some(tab) = group.active_tab().and_then(|id| self.tabs.get_mut(&id)) {
                tab.view.scroll_to = None;
            }
        }
    }

    /// Opens a tab in `group` and focuses it. Returns the new tab's id.
    pub fn open_tab(&mut self, group: GroupId, location: NodeId, view: FileListState) -> TabId {
        let id = TabId(self.next_tab);
        self.next_tab += 1;

        self.tabs.insert(id, Tab::new(id, location, view));
        if let Some(g) = self.groups.get_mut(&group) {
            g.tabs.push(id);
            g.active = g.tabs.len() - 1;
        }
        id
    }

    /// Closes a tab. If it was the last in its pane, the pane closes too and
    /// the layout collapses.
    ///
    /// Returns `false` when this was the very last tab — the caller decides
    /// what that means (Neutron closes the window).
    pub fn close_tab(&mut self, tab: TabId, loader: &mut Loader) -> bool {
        let Some(group_id) = self.group_of(tab) else {
            return true;
        };

        self.tabs.remove(&tab);
        loader.forget(tab);

        let Some(group) = self.groups.get_mut(&group_id) else {
            return true;
        };
        group.remove_tab(tab);

        if !group.tabs.is_empty() {
            return true;
        }

        // Pane is empty — remove it and collapse the layout.
        self.groups.remove(&group_id);
        let layout = std::mem::replace(&mut self.layout, Layout::leaf(group_id));
        match layout.remove(group_id) {
            Some(remaining) => {
                self.layout = remaining;
                // Focus must land somewhere real, or every keystroke is
                // silently dropped.
                if !self.layout.contains(self.focused) {
                    self.focused = self.layout.groups()[0];
                }
                true
            }
            None => false,
        }
    }

    /// Splits the focused pane, moving a copy of its active location into the
    /// new pane. Returns the new tab so the caller can start its load.
    pub fn split(&mut self, axis: Axis) -> Option<TabId> {
        let group = self.focused;
        let source = self.tabs.get(&self.active_tab_id()?)?;
        let location = source.location().clone();
        let view = source.view.clone();

        let new_group = GroupId(self.next_group);
        self.next_group += 1;

        if !self.layout.split(group, axis, new_group) {
            return None;
        }

        self.groups.insert(
            new_group,
            Group {
                id: new_group,
                tabs: Vec::new(),
                active: 0,
            },
        );

        let tab = self.open_tab(new_group, location, view);
        self.focused = new_group;
        Some(tab)
    }

    /// Moves `tab` into `target` group, closing the source pane if it empties.
    ///
    /// Used by drag-and-drop of a tab between panes.
    pub fn move_tab(&mut self, tab: TabId, target: GroupId) {
        let Some(source) = self.group_of(tab) else {
            return;
        };
        if source == target || !self.groups.contains_key(&target) {
            return;
        }

        if let Some(g) = self.groups.get_mut(&source) {
            g.remove_tab(tab);
        }
        if let Some(g) = self.groups.get_mut(&target) {
            g.tabs.push(tab);
            g.active = g.tabs.len() - 1;
        }
        self.focused = target;

        // Source pane may now be empty; collapse it.
        if self.groups.get(&source).is_some_and(|g| g.tabs.is_empty()) {
            self.groups.remove(&source);
            let layout = std::mem::replace(&mut self.layout, Layout::leaf(target));
            if let Some(remaining) = layout.remove(source) {
                self.layout = remaining;
            }
        }
    }

    /// Moves the tab at `from` to `to` within one pane, keeping the same tab
    /// in front.
    pub fn reorder_tab(&mut self, group: GroupId, from: usize, to: usize) {
        let Some(g) = self.groups.get_mut(&group) else {
            return;
        };
        if from >= g.tabs.len() || to >= g.tabs.len() || from == to {
            return;
        }
        let front = g.active_tab();
        let tab = g.tabs.remove(from);
        g.tabs.insert(to, tab);
        if let Some(front) = front {
            g.active = g.tabs.iter().position(|t| *t == front).unwrap_or(0);
        }
    }

    pub fn group_of(&self, tab: TabId) -> Option<GroupId> {
        self.groups
            .iter()
            .find(|(_, g)| g.tabs.contains(&tab))
            .map(|(id, _)| *id)
    }

    /// Moves focus to the next pane, wrapping.
    pub fn focus_next_pane(&mut self) {
        if let Some(next) = self.layout.next_group(self.focused) {
            self.focused = next;
        }
    }

    /// Selects a tab within the focused pane by index.
    pub fn focus_tab_index(&mut self, index: usize) {
        if let Some(g) = self.groups.get_mut(&self.focused) {
            if index < g.tabs.len() {
                g.active = index;
            }
        }
    }

    /// Steps the focused pane's active tab by `delta`, wrapping at the ends.
    pub fn cycle_tab(&mut self, delta: isize) {
        if let Some(g) = self.groups.get_mut(&self.focused) {
            let n = g.tabs.len() as isize;
            if n > 1 {
                g.active = (g.active as isize + delta).rem_euclid(n) as usize;
            }
        }
    }
}

// --- persistence -----------------------------------------------------------

/// Serializable snapshot of the workspace.
///
/// Only filesystem locations survive a restart. Shell and cloud nodes are
/// identified by PIDLs and provider ids whose validity is not guaranteed across
/// sessions, so restoring them could silently land the user somewhere else —
/// worse than starting at a known-good default.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedWorkspace {
    pub panes: Vec<PersistedPane>,
    /// Flattened split structure, replayed in order on restore.
    pub splits: Vec<PersistedSplit>,
    pub focused: usize,
    /// Whether the preview pane was showing.
    ///
    /// `default` so a session saved before the pane existed still restores —
    /// the field is simply absent and reads as closed.
    #[serde(default)]
    pub preview: bool,
    /// How the user had it set up: theme, hidden files, view mode, sort.
    ///
    /// `default` for the same reason as `preview` — a session saved by an
    /// older build simply has no preferences in it and gets the defaults.
    #[serde(default)]
    pub prefs: PersistedPrefs,
}

/// The settings a session carries.
///
/// # Why these are global rather than per folder
///
/// Explorer remembers a view mode per folder, and it is one of the most
/// complained-about things it does: a folder opens in a layout you set months
/// ago for a reason you no longer remember, and there is no way to see the rule
/// you are subject to. One setting that means what it says is easier to live
/// with than several hundred you cannot enumerate.
///
/// So these are taken from the focused tab when the session is saved and
/// applied to every tab when it is restored.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PersistedPrefs {
    /// `true` for light. Named for the value rather than the enum so the JSON
    /// stays readable to a human editing it.
    #[serde(default)]
    pub light_theme: bool,
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default)]
    pub grid: bool,
    /// The column view. Its own flag rather than a change to `grid`, so a
    /// session saved by an older build still reads.
    #[serde(default)]
    pub columns: bool,
    /// One of `name`, `size`, `modified`, `created`, `kind`. An unknown value
    /// reads as `name` rather than failing the whole session.
    #[serde(default)]
    pub sort: String,
    #[serde(default)]
    pub descending: bool,
}

impl PersistedPrefs {
    pub fn column(&self) -> neutron_core::SortColumn {
        use neutron_core::SortColumn as C;
        match self.sort.as_str() {
            "size" => C::Size,
            "modified" => C::Modified,
            "created" => C::Created,
            "kind" => C::Kind,
            _ => C::Name,
        }
    }

    pub fn set_column(&mut self, column: neutron_core::SortColumn) {
        use neutron_core::SortColumn as C;
        self.sort = match column {
            C::Name => "name",
            C::Size => "size",
            C::Modified => "modified",
            C::Created => "created",
            C::Kind => "kind",
        }
        .to_owned();
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedPane {
    pub tabs: Vec<PersistedLocation>,
    pub active: usize,
}

/// A saved tab location.
///
/// `untagged` so a filesystem path persists as a bare JSON string — which is
/// both the overwhelmingly common case and exactly the format sessions saved
/// before the shell namespace existed, so those keep restoring.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum PersistedLocation {
    /// The dashboard. Restorable and cheap: it names nothing that can go
    /// missing between sessions.
    Home,
    Path(String),
    Shell { parsing: String, display: String },
    /// A folder on a saved server. Restorable because it is named by the site's
    /// id and a path, both of which outlive the connection.
    Remote { site: String, path: String },
    /// A folder inside an archive on disk.
    Archive { archive: String, inner: String },
}

impl PersistedLocation {
    pub fn from_node(id: &NodeId) -> Option<Self> {
        match id {
            NodeId::Home => Some(PersistedLocation::Home),
            NodeId::Path(p) => Some(PersistedLocation::Path(p.to_string_lossy().into_owned())),
            NodeId::Shell { parsing, display } => Some(PersistedLocation::Shell {
                parsing: parsing.to_string(),
                display: display.to_string(),
            }),
            NodeId::Remote { site, path } => Some(PersistedLocation::Remote {
                site: site.to_string(),
                path: path.to_string(),
            }),
            NodeId::Archive { archive, inner } => Some(PersistedLocation::Archive {
                archive: archive.to_string_lossy().into_owned(),
                inner: inner.to_string(),
            }),
            // A cloud node's id is only meaningful while its provider is
            // connected, so restoring one would produce a tab that cannot load.
            NodeId::Cloud { .. } => None,
        }
    }

    pub fn to_node(&self) -> NodeId {
        match self {
            PersistedLocation::Home => NodeId::Home,
            PersistedLocation::Path(p) => NodeId::Path(PathBuf::from(p)),
            PersistedLocation::Shell { parsing, display } => {
                NodeId::shell(parsing.as_str(), display.as_str())
            }
            PersistedLocation::Remote { site, path } => {
                NodeId::remote(site.as_str(), path.as_str())
            }
            PersistedLocation::Archive { archive, inner } => {
                NodeId::archive(PathBuf::from(archive), inner.as_str())
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedSplit {
    /// Index into `panes` of the pane being split.
    pub target: usize,
    /// Index into `panes` of the pane the split creates — the first pane on
    /// its second side.
    ///
    /// Needed because splits are replayed in tree order while panes are saved
    /// in screen order, and the two differ whenever a split's first side is
    /// itself split. Without it the new pane was assumed to be the next index,
    /// and tabs came back in the wrong panes. `None` in sessions saved before
    /// it existed, which fall back to that assumption.
    #[serde(default)]
    pub creates: Option<usize>,
    /// `true` for a side-by-side split.
    pub horizontal: bool,
    pub ratio: f32,
}

impl Workspace {
    /// Captures the layout and each tab's location.
    pub fn persist(&self) -> PersistedWorkspace {
        let order = self.layout.groups();

        let panes = order
            .iter()
            .filter_map(|gid| self.groups.get(gid))
            .map(|g| {
                // Every kind of location, not just paths. Dropping shell tabs
                // here would silently delete a "This PC" tab on restart.
                let kept: Vec<(usize, PersistedLocation)> = g
                    .tabs
                    .iter()
                    .enumerate()
                    .filter_map(|(i, t)| Some((i, self.tabs.get(t)?)))
                    .filter_map(|(i, t)| Some((i, PersistedLocation::from_node(t.location())?)))
                    .collect();
                // The active index counts only the tabs that are kept, or a
                // dropped Drive tab before it put a different tab in front.
                let active = kept
                    .iter()
                    .position(|(i, _)| *i >= g.active)
                    .unwrap_or(kept.len().saturating_sub(1));
                let mut tabs: Vec<PersistedLocation> = kept.into_iter().map(|(_, l)| l).collect();
                // A pane of nothing but Drive tabs still has to come back —
                // an empty first pane made the whole saved layout unusable.
                if tabs.is_empty() {
                    tabs.push(PersistedLocation::Home);
                }
                PersistedPane { tabs, active }
            })
            .collect();

        let mut splits = Vec::new();
        collect_splits(&self.layout, &order, &mut splits);

        PersistedWorkspace {
            panes,
            splits,
            focused: order.iter().position(|g| *g == self.focused).unwrap_or(0),
            // Filled in by the caller. The preview pane is a property of the
            // window rather than of the pane tree, and the workspace does not
            // know it exists — but it rides in the same snapshot, because a
            // second saved file for one boolean would be worse.
            preview: false,
            // Taken from the focused tab: it is the one the user was actually
            // looking at when they set things how they wanted them. The theme
            // is filled in by the caller, for the same reason as `preview`.
            prefs: self.prefs(),
        }
    }

    /// View settings for a tab about to be opened.
    ///
    /// Inherited from the focused tab, minus everything that belongs to *that*
    /// tab rather than to the user's preference: the filter it is narrowed by
    /// and where it is scrolled to. Without this, `Ctrl+T` in a grid opens a
    /// list, and showing hidden files has to be turned on again per tab.
    pub fn view_for_new_tab(&self) -> FileListState {
        let Some(tab) = self.active_tab() else {
            return FileListState::default();
        };
        let mut view = FileListState::default();
        view.sort = tab.view.sort;
        view.show_hidden = tab.view.show_hidden;
        view.view = tab.view.view;
        view
    }

    /// The focused tab's view settings, for saving.
    fn prefs(&self) -> PersistedPrefs {
        let Some(tab) = self.active_tab() else {
            return PersistedPrefs::default();
        };
        let mut prefs = PersistedPrefs {
            light_theme: false,
            show_hidden: tab.view.show_hidden,
            grid: tab.view.view == neutron_ui::file_list::ViewMode::Grid,
            columns: tab.view.view == neutron_ui::file_list::ViewMode::Columns,
            sort: String::new(),
            descending: tab.view.sort.order == neutron_core::SortOrder::Descending,
        };
        prefs.set_column(tab.view.sort.column);
        prefs
    }

    /// Rebuilds a workspace from a snapshot, or `None` if it is unusable.
    ///
    /// Locations are *not* validated here — a drive that is currently
    /// disconnected should still restore its tab, and the load will report the
    /// error in that tab rather than silently dropping it.
    pub fn restore(saved: &PersistedWorkspace) -> Option<Self> {
        let first_pane = saved.panes.first()?;
        let first_location = first_pane.tabs.first()?;

        let mut ws = Workspace::new(first_location.to_node());
        let root = ws.layout.groups()[0];

        // Replay splits to rebuild the tree shape, mapping saved pane indices
        // to the groups they become. In tree order, so a split's target always
        // exists by the time it is replayed: it is the first pane of the
        // split's subtree, created by the split above it (or the root).
        let mut group_for_index: HashMap<usize, GroupId> = HashMap::from([(0, root)]);
        let mut next_index = 1;
        for split in &saved.splits {
            let Some(&target) = group_for_index.get(&split.target) else {
                continue;
            };
            let new_group = GroupId(ws.next_group);
            ws.next_group += 1;

            let axis = if split.horizontal {
                Axis::Horizontal
            } else {
                Axis::Vertical
            };
            if ws.layout.split(target, axis, new_group) {
                // Just split, so `target` and the new pane are this split's two
                // children and `set_ratio` cannot reach a different one.
                ws.layout.set_ratio(target, split.ratio);
                ws.groups.insert(
                    new_group,
                    Group {
                        id: new_group,
                        tabs: Vec::new(),
                        active: 0,
                    },
                );
                group_for_index.insert(split.creates.unwrap_or(next_index), new_group);
                next_index += 1;
            }
        }

        // Fill panes with their tabs. The first tab of the first pane already
        // exists from `Workspace::new`.
        for (index, pane) in saved.panes.iter().enumerate() {
            let Some(&gid) = group_for_index.get(&index) else {
                continue;
            };
            let skip = usize::from(index == 0);
            for location in pane.tabs.iter().skip(skip) {
                ws.open_tab(gid, location.to_node(), FileListState::default());
            }
            if let Some(g) = ws.groups.get_mut(&gid) {
                g.active = pane.active.min(g.tabs.len().saturating_sub(1));
            }
        }

        // Every tab gets the saved view settings. Applied here, after the tabs
        // exist, rather than threaded through `open_tab` — a tab created later
        // by the user picks them up from the focused tab it is opened beside.
        let sort = neutron_core::SortSpec {
            column: saved.prefs.column(),
            order: if saved.prefs.descending {
                neutron_core::SortOrder::Descending
            } else {
                neutron_core::SortOrder::Ascending
            },
            ..Default::default()
        };
        let mode = if saved.prefs.columns {
            neutron_ui::file_list::ViewMode::Columns
        } else if saved.prefs.grid {
            neutron_ui::file_list::ViewMode::Grid
        } else {
            neutron_ui::file_list::ViewMode::List
        };
        for tab in ws.tabs.values_mut() {
            tab.view.sort = sort;
            tab.view.show_hidden = saved.prefs.show_hidden;
            tab.view.view = mode;
        }

        // Drop any pane that ended up with no tabs, so a partially-restorable
        // snapshot cannot leave an empty pane on screen.
        let empty: Vec<GroupId> = ws
            .groups
            .iter()
            .filter(|(_, g)| g.tabs.is_empty())
            .map(|(id, _)| *id)
            .collect();
        for gid in empty {
            ws.groups.remove(&gid);
            let layout = std::mem::replace(&mut ws.layout, Layout::leaf(gid));
            ws.layout = layout.remove(gid)?;
        }

        let order = ws.layout.groups();
        ws.focused = order.get(saved.focused).copied().unwrap_or(order[0]);
        Some(ws)
    }
}

fn collect_splits(layout: &Layout, order: &[GroupId], out: &mut Vec<PersistedSplit>) {
    if let Layout::Split {
        axis,
        ratio,
        first,
        second,
    } = layout
    {
        // Record against the first leaf of the left subtree, which is the pane
        // `split` would have been called on.
        if let (Some(target), Some(created)) = (first.groups().first(), second.groups().first()) {
            if let Some(index) = order.iter().position(|g| g == target) {
                out.push(PersistedSplit {
                    target: index,
                    creates: order.iter().position(|g| g == created),
                    horizontal: *axis == Axis::Horizontal,
                    ratio: *ratio,
                });
            }
        }
        collect_splits(first, order, out);
        collect_splits(second, order, out);
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    #[test]
    fn a_legacy_session_of_bare_paths_still_restores() {
        // Sessions saved before the shell namespace existed stored tabs as
        // plain JSON strings. `untagged` is what keeps those loading rather
        // than resetting everyone's layout on upgrade.
        let json = r#"{"panes":[{"tabs":["C:\\Users"],"active":0}],"splits":[],"focused":0}"#;
        let saved: PersistedWorkspace = serde_json::from_str(json).expect("legacy shape parses");
        assert_eq!(
            saved.panes[0].tabs[0],
            PersistedLocation::Path(r"C:\Users".to_owned())
        );
    }

    #[test]
    fn a_shell_tab_survives_a_save_and_restore() {
        // It used to be dropped by a `filter_map` over paths, which deleted the
        // tab *and* shifted `active` past the tab it named.
        let ws = Workspace::new(NodeId::shell(
            "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}",
            "This PC",
        ));
        let saved = ws.persist();
        assert_eq!(saved.panes[0].tabs.len(), 1);

        let restored = Workspace::restore(&saved).expect("restores");
        let tab = restored.active_tab().expect("a tab");
        assert_eq!(
            tab.location().parsing_name(),
            Some("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}")
        );
        assert_eq!(tab.title(), "This PC");
    }

    #[test]
    fn a_mixed_pane_keeps_its_active_tab() {
        // The real damage of dropping a location on save: indices shift, so the
        // pane reopens on a different tab than the one that was in front.
        let mut ws = Workspace::new(NodeId::shell("::{A}", "This PC"));
        let group = ws.focused;
        ws.open_tab(
            group,
            NodeId::Path(PathBuf::from(r"C:\Users")),
            FileListState::default(),
        );
        if let Some(g) = ws.groups.get_mut(&group) {
            g.active = 1;
        }

        let restored = Workspace::restore(&ws.persist()).expect("restores");
        let g = restored.focused_group().expect("a pane");
        assert_eq!(g.tabs.len(), 2);
        assert_eq!(g.active, 1, "the active tab moved");
        assert_eq!(
            restored.active_tab().unwrap().location().as_path(),
            Some(std::path::Path::new(r"C:\Users"))
        );
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> Workspace {
        Workspace::new(NodeId::path("/start"))
    }

    /// A workspace whose focused tab is set up the way a user might leave it.
    fn configured() -> Workspace {
        let mut w = ws();
        let tab = w.active_tab_mut().expect("a tab");
        tab.view.show_hidden = true;
        tab.view.view = neutron_ui::file_list::ViewMode::Grid;
        tab.view.sort = neutron_core::SortSpec {
            column: neutron_core::SortColumn::Modified,
            order: neutron_core::SortOrder::Descending,
            ..Default::default()
        };
        w
    }

    #[test]
    fn reordering_a_tab_keeps_the_same_tab_in_front() {
        let mut w = ws();
        let group = w.focused_group().expect("a group").id;
        let a = w.active_tab_id().unwrap();
        let b = w.open_tab(group, NodeId::path("/b"), FileListState::default());
        let c = w.open_tab(group, NodeId::path("/c"), FileListState::default());
        // C is in front; move A to the end.
        w.reorder_tab(group, 0, 2);
        assert_eq!(w.groups[&group].tabs, [b, c, a]);
        assert_eq!(w.active_tab_id(), Some(c));
    }

    #[test]
    fn nested_panes_come_back_where_they_were() {
        // Split right, then split the left pane down: [[A / B] | C]. Panes are
        // saved left to right — A, B, C — but splits replay root first, so the
        // right-hand pane is created before the bottom-left one.
        let mut w = ws();
        let a = w.focused_group().expect("a group").id;
        w.split(Axis::Horizontal);
        let c = w.focused;
        w.tabs.values_mut().find(|t| w.groups[&c].tabs.contains(&t.id)).unwrap().history = History::new(NodeId::path("/c"));
        w.focused = a;
        w.split(Axis::Vertical);
        let b = w.focused;
        let b_tab = w.groups[&b].tabs[0];
        w.tabs.get_mut(&b_tab).unwrap().history = History::new(NodeId::path("/b"));

        let restored = Workspace::restore(&w.persist()).expect("should restore");
        let where_is = |path: &str| {
            let order = restored.layout.groups();
            order.iter().position(|g| {
                restored.groups[g].tabs.iter().any(|t| restored.tabs[t].location() == &NodeId::path(path))
            })
        };
        assert_eq!(where_is("/start"), Some(0));
        assert_eq!(where_is("/b"), Some(1), "B's tabs moved");
        assert_eq!(where_is("/c"), Some(2), "C's tabs moved");
    }

    #[test]
    fn a_dropped_drive_tab_does_not_move_which_tab_is_in_front() {
        let mut w = ws();
        let group = w.focused_group().expect("a group").id;
        w.open_tab(group, NodeId::cloud(neutron_core::namespace::CloudProviderId::GoogleDrive, "id", "Drive"), FileListState::default());
        w.open_tab(group, NodeId::path("/third"), FileListState::default());
        // The third tab is in front; the Drive tab before it is not saved.
        let saved = w.persist();
        assert_eq!(saved.panes[0].tabs.len(), 2);
        assert_eq!(saved.panes[0].tabs[saved.panes[0].active], PersistedLocation::Path("/third".into()));
    }

    #[test]
    fn closing_a_tab_left_of_the_active_one_keeps_the_active_one_in_front() {
        let mut w = ws();
        let mut loader = Loader::spawn(egui::Context::default());
        let group = w.focused_group().expect("a group").id;
        let a = w.active_tab_id().expect("a tab");
        let b = w.open_tab(group, NodeId::path("/b"), FileListState::default());
        let c = w.open_tab(group, NodeId::path("/c"), FileListState::default());
        w.groups.get_mut(&group).unwrap().active = 1;
        assert_eq!(w.active_tab_id(), Some(b));

        w.close_tab(a, &mut loader);
        assert_eq!(w.active_tab_id(), Some(b), "closing A brought C to the front");

        // Closing the active tab hands over to the one that slides into its slot.
        w.close_tab(b, &mut loader);
        assert_eq!(w.active_tab_id(), Some(c));
    }

    #[test]
    fn a_scroll_request_is_spent_once_drawn_but_a_background_tab_keeps_its_own() {
        let mut w = ws();
        let group = w.focused_group().expect("a group").id;
        let first = w.active_tab_id().expect("a tab");
        w.tabs.get_mut(&first).unwrap().view.scroll_to = Some(7);
        let second = w.open_tab(group, NodeId::path("/second"), FileListState::default());
        w.tabs.get_mut(&second).unwrap().view.scroll_to = Some(3);

        w.consume_scroll_requests();

        assert_eq!(w.tabs[&second].view.scroll_to, None, "the tab on screen keeps pinning");
        assert_eq!(w.tabs[&first].view.scroll_to, Some(7), "a hidden tab lost its request");
    }

    #[test]
    fn how_the_user_had_it_set_up_survives_a_restart() {
        // Theme aside, this is the whole of "it opens the way I left it".
        let saved = configured().persist();
        assert!(saved.prefs.show_hidden);
        assert!(saved.prefs.grid);
        assert_eq!(saved.prefs.sort, "modified");
        assert!(saved.prefs.descending);

        let restored = Workspace::restore(&saved).expect("should restore");
        let view = &restored.active_tab().expect("a tab").view;
        assert!(view.show_hidden);
        assert_eq!(view.view, neutron_ui::file_list::ViewMode::Grid);
        assert_eq!(view.sort.column, neutron_core::SortColumn::Modified);
        assert_eq!(view.sort.order, neutron_core::SortOrder::Descending);
    }

    #[test]
    fn every_restored_tab_gets_the_saved_settings() {
        // Not just the focused one — a session where half the tabs are in the
        // wrong view mode is worse than one that forgot entirely.
        let mut w = ws();
        let group = w.focused_group().expect("a group").id;
        w.open_tab(group, NodeId::path("/second"), FileListState::default());
        w.split(Axis::Horizontal);
        // Configured last, on whichever tab ended up focused — which is the
        // one the user would have been looking at when they set it.
        let tab = w.active_tab_mut().expect("a tab");
        tab.view.show_hidden = true;
        tab.view.view = neutron_ui::file_list::ViewMode::Grid;

        let restored = Workspace::restore(&w.persist()).expect("should restore");
        assert!(restored.tabs.len() >= 3);
        for tab in restored.tabs.values() {
            assert!(tab.view.show_hidden, "a tab restored with hidden files off");
            assert_eq!(tab.view.view, neutron_ui::file_list::ViewMode::Grid);
        }
    }

    #[test]
    fn a_session_saved_before_preferences_existed_still_restores() {
        // The field is simply absent, and absent has to mean "the defaults"
        // rather than "refuse to open".
        let json = r#"{"panes":[{"tabs":["/only"],"active":0}],"splits":[],"focused":0}"#;
        let saved: PersistedWorkspace = serde_json::from_str(json).expect("should parse");
        assert_eq!(saved.prefs, PersistedPrefs::default());
        assert!(Workspace::restore(&saved).is_some());
    }

    #[test]
    fn an_unknown_sort_column_reads_as_name_rather_than_failing() {
        // Someone editing the file by hand, or a column that a later build
        // added and this one does not know.
        let prefs = PersistedPrefs { sort: "colour".into(), ..PersistedPrefs::default() };
        assert_eq!(prefs.column(), neutron_core::SortColumn::Name);
    }

    #[test]
    fn a_new_tab_inherits_the_view_but_not_the_filter() {
        // Ctrl+T in a grid should open a grid. It should *not* open already
        // narrowed by whatever the previous tab was searching for.
        let mut w = configured();
        w.active_tab_mut().unwrap().view.filter = "readme".into();

        let view = w.view_for_new_tab();
        assert!(view.show_hidden);
        assert_eq!(view.view, neutron_ui::file_list::ViewMode::Grid);
        assert_eq!(view.sort.column, neutron_core::SortColumn::Modified);
        assert!(view.filter.is_empty(), "the new tab arrived pre-filtered");
    }

    #[test]
    fn a_new_workspace_has_one_pane_with_one_tab() {
        let w = ws();
        assert_eq!(w.layout.len(), 1);
        assert_eq!(w.tabs.len(), 1);
        assert_eq!(w.active_tab().unwrap().location(), &NodeId::path("/start"));
    }

    #[test]
    fn splitting_copies_the_location_into_the_new_pane() {
        let mut w = ws();
        let new_tab = w.split(Axis::Horizontal).expect("split should succeed");

        assert_eq!(w.layout.len(), 2);
        assert_eq!(w.tabs.len(), 2);
        // The new pane should open where the old one was looking, which is what
        // makes split-then-navigate useful for copying between folders.
        assert_eq!(
            w.tabs[&new_tab].location(),
            &NodeId::path("/start")
        );
        // Focus follows the split.
        assert_eq!(w.active_tab_id(), Some(new_tab));
    }

    #[test]
    fn opening_and_closing_tabs_keeps_the_active_index_in_range() {
        let mut w = ws();
        let group = w.focused;
        w.open_tab(group, NodeId::path("/a"), FileListState::default());
        let last = w.open_tab(group, NodeId::path("/b"), FileListState::default());

        assert_eq!(w.groups[&group].active, 2);

        let mut loader = test_loader();
        assert!(w.close_tab(last, &mut loader));
        assert_eq!(w.groups[&group].tabs.len(), 2);
        assert!(w.groups[&group].active < 2);
    }

    #[test]
    fn closing_the_last_tab_in_a_pane_collapses_the_split() {
        let mut w = ws();
        let new_tab = w.split(Axis::Horizontal).unwrap();
        assert_eq!(w.layout.len(), 2);

        let mut loader = test_loader();
        assert!(w.close_tab(new_tab, &mut loader));

        assert_eq!(w.layout.len(), 1, "the split should have collapsed");
        assert!(w.layout.contains(w.focused), "focus must stay valid");
    }

    #[test]
    fn closing_the_very_last_tab_reports_the_workspace_is_done() {
        let mut w = ws();
        let only = w.active_tab_id().unwrap();
        let mut loader = test_loader();
        assert!(!w.close_tab(only, &mut loader));
    }

    #[test]
    fn moving_a_tab_out_of_a_pane_collapses_it() {
        let mut w = ws();
        let first_group = w.focused;
        let moved = w.split(Axis::Horizontal).unwrap();

        w.move_tab(moved, first_group);

        assert_eq!(w.layout.len(), 1);
        assert_eq!(w.groups[&first_group].tabs.len(), 2);
    }

    #[test]
    fn a_round_trip_preserves_panes_and_locations() {
        let mut w = ws();
        let group = w.focused;
        w.open_tab(group, NodeId::path("/second"), FileListState::default());
        w.split(Axis::Vertical);

        let saved = w.persist();
        let restored = Workspace::restore(&saved).expect("should restore");

        assert_eq!(restored.layout.len(), w.layout.len());
        assert_eq!(restored.tabs.len(), w.tabs.len());

        // Compared against the original rather than a hand-written list: the
        // split copies whichever tab was *active*, so a literal expectation
        // encodes an assumption about that and silently tests the wrong thing
        // if the rule changes.
        let sorted = |w: &Workspace| {
            let mut v: Vec<String> = w.tabs.values().map(|t| t.location().to_string()).collect();
            v.sort();
            v
        };
        assert_eq!(sorted(&restored), sorted(&w));
    }

    #[test]
    fn restoring_an_empty_snapshot_is_rejected_rather_than_panicking() {
        let empty = PersistedWorkspace {
            panes: Vec::new(),
            splits: Vec::new(),
            focused: 0,
            preview: false,
            prefs: PersistedPrefs::default(),
        };
        assert!(Workspace::restore(&empty).is_none());
    }

    #[test]
    fn a_session_saved_before_the_preview_pane_existed_still_restores() {
        // The field is `serde(default)` precisely so an older session file,
        // which has no `preview` key at all, does not fail to parse and throw
        // the whole layout away.
        let json = r#"{"panes":[{"tabs":["/only"],"active":0}],"splits":[],"focused":0}"#;
        let saved: PersistedWorkspace =
            serde_json::from_str(json).expect("an older snapshot must still parse");
        assert!(!saved.preview);
        assert!(Workspace::restore(&saved).is_some());
    }

    #[test]
    fn restoring_tolerates_an_out_of_range_focus_index() {
        let saved = PersistedWorkspace {
            panes: vec![PersistedPane {
                tabs: vec![PersistedLocation::Path("/only".into())],
                active: 9,
            }],
            splits: Vec::new(),
            // Points at a pane that does not exist.
            focused: 7,
            preview: false,
            prefs: PersistedPrefs::default(),
        };
        let restored = Workspace::restore(&saved).expect("should still restore");
        assert!(restored.layout.contains(restored.focused));
        assert_eq!(restored.groups[&restored.focused].active, 0);
    }

    /// A loader whose worker is never exercised — `close_tab` only calls
    /// `forget`, which touches the shared map and nothing else.
    fn test_loader() -> Loader {
        Loader::spawn(egui::Context::default())
    }
}
