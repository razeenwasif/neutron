//! What the column view shows, and the listings it needs to show it.
//!
//! The current column is the tab's own listing, loaded and watched like any
//! other. Every other column — the folders on the way here, and the contents of
//! the selected folder — is a listing the tab does not have, and they are read
//! here, on a worker of their own.
//!
//! # Wanted, frame by frame
//!
//! The app says which folders are on screen before each frame is drawn, and
//! anything not in that list is forgotten. That is the whole cache policy, and
//! it is what keeps the contents of a folder fresh: arrowing past a folder
//! shows what is in it now, not what was in it the last time it was passed.
//! Folders on the path stay wanted for as long as the user works below them,
//! and F5 throws them away along with everything else.
//!
//! The worker skips a request whose folder has stopped being wanted by the
//! time it reaches it, so holding Down through a column of folders reads the
//! one the cursor stops on rather than every one it passed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender};
use neutron_core::{EntryList, Namespace, NodeId, SortSpec, sort};
use parking_lot::Mutex;

use crate::workspace::{Status, Tab};

/// The most columns shown before the current one. A path deeper than this
/// starts partway down, rather than costing a directory read per level and a
/// strip of columns nobody will scroll back through.
const MAX_ANCESTORS: usize = 16;

/// A folder's listing, as far as it has got.
pub enum Slot {
    Loading,
    Ready {
        // Boxed: a listing is several hundred bytes of vectors, and most slots
        // spend their short lives loading.
        list: Box<EntryList>,
        /// What it was sorted with, so a change of setting re-sorts it rather
        /// than reading it again.
        sort: SortSpec,
        show_hidden: bool,
    },
    Failed(String),
}

/// A folder the app wants listed, with the settings it is to be listed with.
pub struct Want {
    pub dir: NodeId,
    pub sort: SortSpec,
    pub show_hidden: bool,
}

type Request = (NodeId, SortSpec, bool);
type Reply = (NodeId, SortSpec, bool, Result<EntryList, String>);

pub struct ColumnCache {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
    slots: HashMap<NodeId, Slot>,
    /// What is wanted right now, shared so the worker can skip what is not.
    wanted: Arc<Mutex<HashSet<NodeId>>>,
    /// Folders whose listings arrived since `take_arrived` last asked.
    arrived: Vec<NodeId>,
    /// The folder sizes' version the listings were last given sizes from.
    sizes_seen: u64,
    /// Bumped when `sites.json` is reloaded so the worker drops stale connections.
    sites_version: Arc<AtomicU64>,
}

impl ColumnCache {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (requests, jobs) = crossbeam_channel::unbounded::<Request>();
        let (done, replies) = crossbeam_channel::unbounded::<Reply>();
        let wanted = Arc::new(Mutex::new(HashSet::new()));

        let sites_version = Arc::new(AtomicU64::new(0));
        let worker_wanted = Arc::clone(&wanted);
        let worker_sites = Arc::clone(&sites_version);
        std::thread::Builder::new()
            .name("neutron-columns".into())
            .spawn(move || worker(jobs, done, worker_wanted, worker_sites, ctx))
            .expect("failed to spawn the column loader thread");

        Self {
            requests,
            replies,
            slots: HashMap::new(),
            wanted,
            arrived: Vec::new(),
            sizes_seen: 0,
            sites_version,
        }
    }

    /// Every local folder in every listing held.
    pub fn folders(&self) -> Vec<PathBuf> {
        self.slots
            .iter()
            .filter_map(|(dir, slot)| match (dir.as_path(), slot) {
                (Some(dir), Slot::Ready { list, .. }) => Some(
                    (0..list.len()).filter(|&i| list.kind(i).is_container()).map(|i| dir.join(list.name(i))).collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// The folders whose listings arrived since this was last asked.
    pub fn take_arrived(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.arrived)
    }

    /// Gives the folders in every local listing the sizes found for them,
    /// when there are new ones — or new listings to give them to.
    pub fn apply_sizes(&mut self, version: u64, fresh: bool, size_of: impl Fn(&Path) -> Option<u64>) {
        if version == self.sizes_seen && !fresh {
            return;
        }
        self.sizes_seen = version;
        for (dir, slot) in &mut self.slots {
            let Some(dir) = dir.as_path() else { continue };
            let Slot::Ready { list, .. } = slot else { continue };
            for i in 0..list.len() {
                if list.kind(i).is_container() {
                    if let Some(size) = size_of(&dir.join(list.name(i))) {
                        list.set_folder_size(i, size);
                    }
                }
            }
        }
    }

    /// Takes the listings that have arrived. Never blocks.
    pub fn pump(&mut self) {
        while let Ok((dir, sort, show_hidden, outcome)) = self.replies.try_recv() {
            // A folder that stopped being wanted while it was read is dropped:
            // keeping it would be keeping a listing nobody is looking at.
            let Some(slot) = self.slots.get_mut(&dir) else {
                continue;
            };
            if outcome.is_ok() {
                self.arrived.push(dir.clone());
            }
            *slot = match outcome {
                Ok(list) => Slot::Ready {
                    list: Box::new(list),
                    sort,
                    show_hidden,
                },
                Err(e) => Slot::Failed(e),
            };
        }
    }

    /// Declares the folders on screen this frame.
    ///
    /// Asks for any that are not loaded, forgets any that are no longer shown,
    /// and re-sorts any whose settings changed since they were read.
    pub fn want(&mut self, wanted: &[Want]) {
        let paths: HashSet<NodeId> = wanted.iter().map(|w| w.dir.clone()).collect();
        self.slots.retain(|dir, _| paths.contains(dir));
        *self.wanted.lock() = paths;

        for w in wanted {
            match self.slots.get_mut(&w.dir) {
                None => {
                    self.slots.insert(w.dir.clone(), Slot::Loading);
                    let _ = self.requests.send((w.dir.clone(), w.sort, w.show_hidden));
                }
                // On this thread, as the tab's own re-filter is: a re-sort is a
                // pass over memory, and it happens once per change of setting.
                Some(Slot::Ready {
                    list,
                    sort: current,
                    show_hidden,
                }) if (*current, *show_hidden) != (w.sort, w.show_hidden) => {
                    sort::apply(list, w.sort, w.show_hidden);
                    *current = w.sort;
                    *show_hidden = w.show_hidden;
                }
                Some(_) => {}
            }
        }
    }

    pub fn get(&self, dir: &NodeId) -> Option<&Slot> {
        self.slots.get(dir)
    }

    /// Forgets everything, so every column on screen is read again.
    pub fn clear(&mut self) {
        self.slots.clear();
    }

    /// Tells the column worker that `sites.json` changed so stale server
    /// sessions are dropped before the next listing.
    pub fn reload_sites(&self) {
        self.sites_version.fetch_add(1, Ordering::Relaxed);
    }
}

fn worker(
    jobs: Receiver<Request>,
    done: Sender<Reply>,
    wanted: Arc<Mutex<HashSet<NodeId>>>,
    sites_version: Arc<AtomicU64>,
    ctx: egui::Context,
) {
    let fs = neutron_shell::fs::FsNamespace;
    let mut sessions: Option<crate::remote::Sessions> = None;
    let mut seen_sites_ver = sites_version.load(Ordering::Relaxed);

    while let Ok((dir, sort_spec, show_hidden)) = jobs.recv() {
        if !wanted.lock().contains(&dir) {
            continue;
        }
        let listed = match &dir {
            NodeId::Path(_) => fs.enumerate(&dir).map_err(|e| e.to_string()),
            NodeId::Remote { site, path } => {
                let ver = sites_version.load(Ordering::Relaxed);
                if ver != seen_sites_ver {
                    seen_sites_ver = ver;
                    if let Some(s) = sessions.as_mut() {
                        s.reload();
                    }
                }
                sessions
                    .get_or_insert_with(crate::remote::Sessions::load)
                    .list(site, path)
            }
            _ => Err("column view does not list this location".to_owned()),
        };
        let outcome = listed.map(|mut list| {
            sort::apply(&mut list, sort_spec, show_hidden);
            list
        });
        if done.send((dir, sort_spec, show_hidden, outcome)).is_err() {
            break;
        }
        ctx.request_repaint();
    }
}

/// The folders shown as columns for `location`, outermost first and ending
/// with `location` itself.
///
/// Starts at the home folder for anything inside it, as the Finder does when a
/// window is opened there, and at the root of the volume otherwise. A function
/// of the location alone, so Back, the sidebar, the address bar and a restored
/// session all produce the same columns for the same place.
pub fn chain(location: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let root = match home {
        Some(home) if location.starts_with(home) => home,
        _ => location.ancestors().last().unwrap_or(location),
    };
    let mut chain: Vec<PathBuf> = location
        .ancestors()
        .take_while(|dir| dir.starts_with(root))
        .take(MAX_ANCESTORS + 1)
        .map(Path::to_path_buf)
        .collect();
    chain.reverse();
    chain
}

/// The remote folders shown as columns for `location` on `site`, outermost
/// first and ending with `location` itself.
pub fn chain_remote(site: &str, location: &str, root: Option<&str>) -> Vec<NodeId> {
    let loc = neutron_remote::path::normalize(location);
    let stop = match root.map(neutron_remote::path::normalize) {
        Some(r) if loc == r || r == "/" || loc.starts_with(&format!("{r}/")) => r,
        _ => "/".to_owned(),
    };

    let mut paths = Vec::new();
    let mut cur = loc;
    while paths.len() < MAX_ANCESTORS + 1 {
        paths.push(cur.clone());
        if cur == stop {
            break;
        }
        let Some(parent) = neutron_remote::path::parent(&cur) else {
            break;
        };
        cur = parent;
    }
    paths.reverse();
    paths
        .into_iter()
        .map(|path| NodeId::remote(site, path))
        .collect()
}

/// Whether the tab is on its way to a new folder, so its listing is still the
/// one it is leaving.
///
/// Not just "a load is pending": a refresh of the same folder is one too, and
/// during a refresh the listing is still the right folder's — treating it as
/// arriving would blank the column every time anything on disk changed.
pub fn arriving(tab: &Tab) -> bool {
    tab.pending.is_some() && matches!(tab.status, Status::Loading)
}

/// The folder whose contents fill the column after the current one: the
/// selection, when it is exactly one folder.
///
/// `None` while the tab is arriving, because its listing is then the folder it
/// is leaving and an index into it means nothing here.
pub fn peek(tab: &Tab) -> Option<NodeId> {
    if arriving(tab) || tab.selection.len() != 1 {
        return None;
    }
    let idx = tab.selection.iter().next()?;
    tab.list.rank(idx)?;
    if !tab.list.kind(idx).is_container() {
        return None;
    }
    let name = tab.list.name(idx);
    match tab.location() {
        NodeId::Path(dir) => {
            let full = dir.join(name);
            (!is_package(&full)).then(|| NodeId::Path(full))
        }
        NodeId::Remote { site, path } => {
            let joined = neutron_remote::path::join(path, name).ok()?;
            Some(NodeId::remote(site.as_ref(), joined))
        }
        _ => None,
    }
}

/// A folder the Mac presents as a single file — an application bundle.
///
/// Opened rather than browsed into, as everywhere else in Neutron.
pub fn is_package(path: &Path) -> bool {
    cfg!(target_os = "macos") && path.extension().is_some_and(|ext| ext == "app")
}

/// The first entry of a listing in display order, if it has one.
pub fn first_name(slot: Option<&Slot>) -> Option<String> {
    match slot {
        Some(Slot::Ready { list, .. }) if !list.order().is_empty() => {
            Some(list.name(list.at(0)).to_owned())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(chain: &[PathBuf]) -> Vec<String> {
        chain.iter().map(|p| p.to_string_lossy().into_owned()).collect()
    }

    #[cfg(unix)]
    #[test]
    fn inside_home_the_columns_start_at_home() {
        let chain = chain(
            Path::new("/Users/me/code/neutron"),
            Some(Path::new("/Users/me")),
        );
        assert_eq!(paths(&chain), ["/Users/me", "/Users/me/code", "/Users/me/code/neutron"]);
    }

    #[cfg(unix)]
    #[test]
    fn outside_home_the_columns_start_at_the_root() {
        let chain = chain(Path::new("/usr/local/bin"), Some(Path::new("/Users/me")));
        assert_eq!(paths(&chain), ["/", "/usr", "/usr/local", "/usr/local/bin"]);
    }

    #[cfg(unix)]
    #[test]
    fn home_itself_is_one_column() {
        let chain = chain(Path::new("/Users/me"), Some(Path::new("/Users/me")));
        assert_eq!(paths(&chain), ["/Users/me"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_whose_name_merely_starts_like_home_is_not_inside_it() {
        // `starts_with` on paths compares components, so `/Users/meg` is not
        // under `/Users/me` — checked because a string prefix test would say
        // it is.
        let chain = chain(Path::new("/Users/meg/x"), Some(Path::new("/Users/me")));
        assert_eq!(paths(&chain), ["/", "/Users", "/Users/meg", "/Users/meg/x"]);
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_the_root_is_the_drive() {
        let chain = chain(Path::new(r"C:\Windows\System32"), None);
        assert_eq!(paths(&chain), [r"C:\", r"C:\Windows", r"C:\Windows\System32"]);
    }

    #[test]
    fn a_very_deep_path_keeps_its_deepest_folders() {
        let deep: PathBuf = std::iter::once(std::path::MAIN_SEPARATOR.to_string())
            .chain((0..40).map(|i| format!("d{i}")))
            .collect();
        let chain = chain(&deep, None);
        assert_eq!(chain.len(), MAX_ANCESTORS + 1);
        assert_eq!(chain.last(), Some(&deep));
    }

    fn remote_paths(chain: &[NodeId]) -> Vec<&str> {
        chain
            .iter()
            .map(|n| n.as_remote().expect("remote node").1)
            .collect()
    }

    #[test]
    fn remote_inside_site_root_starts_at_site_root() {
        let chain = chain_remote("mini", "/Users/me/code/neutron", Some("/Users/me"));
        assert_eq!(remote_paths(&chain), ["/Users/me", "/Users/me/code", "/Users/me/code/neutron"]);
    }

    #[test]
    fn remote_outside_site_root_starts_at_slash() {
        let chain = chain_remote("mini", "/var/log/nginx", Some("/Users/me"));
        assert_eq!(remote_paths(&chain), ["/", "/var", "/var/log", "/var/log/nginx"]);
    }

    #[test]
    fn remote_with_slash_root_starts_at_slash() {
        let chain = chain_remote("mini", "/srv/data", Some("/"));
        assert_eq!(remote_paths(&chain), ["/", "/srv", "/srv/data"]);
    }

    #[test]
    fn remote_very_deep_path_keeps_its_deepest_folders() {
        let deep = format!("/{}", (0..40).map(|i| format!("d{i}")).collect::<Vec<_>>().join("/"));
        let chain = chain_remote("mini", &deep, None);
        assert_eq!(chain.len(), MAX_ANCESTORS + 1);
        assert_eq!(chain.last(), Some(&NodeId::remote("mini", deep)));
    }
}
