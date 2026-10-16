//! Folder sizes: adding up everything in a folder, on a thread of its own,
//! when asked for one folder (Calculate Size) or for every folder shown
//! (View › Calculate All Sizes).
//!
//! Adding up a large folder can take a long while — a home folder is a walk
//! of hundreds of thousands of items — so it never shares a worker with
//! renames and copies, the newest request is answered first, and a folder is
//! given up on as soon as no tab shows the folder it is in.
//!
//! Sizes found are kept by path. The views read them from here rather than
//! being told of each one, so a listing loaded again picks them up too.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender};

pub struct Sizer {
    requests: Sender<Vec<PathBuf>>,
    results: Receiver<(PathBuf, Option<u64>)>,
    /// The folders tabs show. A folder whose parent leaves this set is no
    /// longer worth adding up.
    shown: Arc<Mutex<HashSet<PathBuf>>>,
    /// What has been added up, by path.
    sizes: HashMap<PathBuf, u64>,
    /// Asked for and not yet answered.
    pending: HashSet<PathBuf>,
    /// Counts changes to `sizes`, so a tab knows when to look again.
    version: u64,
}

impl Sizer {
    pub fn spawn(wake: egui::Context) -> Self {
        let (requests, inbox) = crossbeam_channel::unbounded::<Vec<PathBuf>>();
        let (answer, results) = crossbeam_channel::unbounded();
        let shown: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
        let still_shown = shown.clone();
        let spawned = std::thread::Builder::new().name("folder-sizes".into()).spawn(move || {
            let mut queue: VecDeque<PathBuf> = VecDeque::new();
            loop {
                if queue.is_empty() {
                    let Ok(batch) = inbox.recv() else { return };
                    queue.extend(batch);
                }
                // Newer requests first: they are for what is on screen now.
                for batch in inbox.try_iter() {
                    for path in batch.into_iter().rev() {
                        queue.push_front(path);
                    }
                }
                let Some(folder) = queue.pop_front() else { continue };
                let wanted = || {
                    let parent = folder.parent().map(Path::to_path_buf).unwrap_or_default();
                    still_shown.lock().is_ok_and(|s| s.contains(&parent))
                };
                let size = if wanted() { measure(&folder, wanted) } else { None };
                if answer.send((folder, size)).is_err() {
                    return;
                }
                wake.request_repaint();
            }
        });
        if let Err(e) = spawned {
            tracing::warn!("no thread for folder sizes: {e}");
        }
        Self { requests, results, shown, sizes: HashMap::new(), pending: HashSet::new(), version: 0 }
    }

    /// Adds up these folders, unless they are already being added up.
    pub fn request(&mut self, folders: Vec<PathBuf>) {
        let fresh: Vec<PathBuf> = folders.into_iter().filter(|f| self.pending.insert(f.clone())).collect();
        // Shown as of now: the next frame's `set_shown` may not come before
        // the worker looks.
        if let Ok(mut shown) = self.shown.lock() {
            shown.extend(fresh.iter().filter_map(|f| f.parent().map(Path::to_path_buf)));
        }
        if !fresh.is_empty() {
            let _ = self.requests.send(fresh);
        }
    }

    /// Takes in what has been added up since the last frame.
    pub fn pump(&mut self) {
        for (folder, size) in self.results.try_iter() {
            self.pending.remove(&folder);
            if let Some(size) = size {
                self.sizes.insert(folder, size);
                self.version += 1;
            }
        }
    }

    /// Whether anything is still being added up.
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Tells the worker which folders tabs show now.
    pub fn set_shown(&self, folders: HashSet<PathBuf>) {
        if let Ok(mut shown) = self.shown.lock() {
            *shown = folders;
        }
    }

    pub fn get(&self, folder: &Path) -> Option<u64> {
        self.sizes.get(folder).copied()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// Forgets every size, as when Calculate All Sizes is turned off.
    pub fn clear(&mut self) {
        self.sizes.clear();
        self.version += 1;
    }
}

/// The total size of the files in `folder` and every folder in it, or `None`
/// if `keep_going` said to stop. Symbolic links count as themselves and are
/// not followed; what cannot be read is left out.
pub fn measure(folder: &Path, keep_going: impl Fn() -> bool) -> Option<u64> {
    let mut total = 0u64;
    let mut stack = vec![folder.to_path_buf()];
    let mut seen = 0u32;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen % 1024 == 0 && !keep_going() {
                return None;
            }
            // Not followed: `DirEntry::metadata` describes a link itself.
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_the_sum_of_everything_in_it_and_stops_when_told() {
        let dir = std::env::temp_dir().join(format!("neutron-sizes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("one"), [0u8; 100]).unwrap();
        std::fs::write(dir.join("a/two"), [0u8; 20]).unwrap();
        std::fs::write(dir.join("a/b/three"), [0u8; 3]).unwrap();
        assert_eq!(measure(&dir, || true), Some(123));

        for i in 0..1100 {
            std::fs::write(dir.join(format!("f{i}")), "").unwrap();
        }
        assert_eq!(measure(&dir, || false), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
