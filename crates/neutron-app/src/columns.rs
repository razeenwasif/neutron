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
    pub dir: PathBuf,
    pub sort: SortSpec,
    pub show_hidden: bool,
}

type Request = (PathBuf, SortSpec, bool);
type Reply = (PathBuf, SortSpec, bool, Result<EntryList, String>);

pub struct ColumnCache {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
    slots: HashMap<PathBuf, Slot>,
    /// What is wanted right now, shared so the worker can skip what is not.
    wanted: Arc<Mutex<HashSet<PathBuf>>>,
}

impl ColumnCache {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (requests, jobs) = crossbeam_channel::unbounded::<Request>();
        let (done, replies) = crossbeam_channel::unbounded::<Reply>();
        let wanted = Arc::new(Mutex::new(HashSet::new()));

        let worker_wanted = Arc::clone(&wanted);
        std::thread::Builder::new()
            .name("neutron-columns".into())
            .spawn(move || worker(jobs, done, worker_wanted, ctx))
            .expect("failed to spawn the column loader thread");

        Self {
            requests,
            replies,
            slots: HashMap::new(),
            wanted,
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
        let paths: HashSet<PathBuf> = wanted.iter().map(|w| w.dir.clone()).collect();
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

    pub fn get(&self, dir: &Path) -> Option<&Slot> {
        self.slots.get(dir)
    }

    /// Forgets everything, so every column on screen is read again.
    pub fn clear(&mut self) {
        self.slots.clear();
    }
}

fn worker(
    jobs: Receiver<Request>,
    done: Sender<Reply>,
    wanted: Arc<Mutex<HashSet<PathBuf>>>,
    ctx: egui::Context,
) {
    // The local filesystem only: the column view is offered for folders with
    // a path, and those never need COM, a server session or the Drive client.
    let fs = neutron_shell::fs::FsNamespace;

    while let Ok((dir, sort_spec, show_hidden)) = jobs.recv() {
        if !wanted.lock().contains(&dir) {
            continue;
        }
        let outcome = fs
            .enumerate(&NodeId::Path(dir.clone()))
            .map(|mut list| {
                sort::apply(&mut list, sort_spec, show_hidden);
                list
            })
            .map_err(|e| e.to_string());
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
pub fn peek(tab: &Tab) -> Option<PathBuf> {
    if arriving(tab) || tab.selection.len() != 1 {
        return None;
    }
    let idx = tab.selection.iter().next()?;
    tab.list.rank(idx)?;
    let dir = tab.location().as_path()?;
    let full = dir.join(tab.list.name(idx));
    (tab.list.kind(idx).is_container() && !is_package(&full)).then_some(full)
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
}
