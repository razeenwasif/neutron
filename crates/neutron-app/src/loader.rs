//! Background directory loading, routed per tab.
//!
//! Enumeration and sorting both run on a worker thread. Neither is fast enough
//! to do inline: a cold network path can block for seconds, and sorting 500k
//! entries takes long enough to drop frames. The UI thread only ever receives a
//! finished [`neutron_core::EntryList`] over a channel.
//!
//! # Staleness is per tab, not global
//!
//! Navigation is faster than loading. A user who clicks through four folders
//! while the first is still enumerating must end up in the fourth, and must not
//! see the first three flash by on the way.
//!
//! Each request carries a generation number, and the loader tracks the newest
//! generation **per tab**. A global counter would be wrong the moment a second
//! tab exists: loading in tab B would look like it superseded tab A's in-flight
//! request, and tab A would silently never populate.
//!
//! The worker re-checks its tab's generation immediately before enumerating and
//! again before sending, so superseded work is abandoned without paying for the
//! directory read. The UI re-checks on receipt as a final guard.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use neutron_core::{EntryList, Namespace, NodeId, SortSpec, sort};
use parking_lot::Mutex;

/// Identifies a tab. Assigned by the app; the loader only routes by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(pub u64);

struct Request {
    tab: TabId,
    generation: u64,
    id: NodeId,
    sort: SortSpec,
    show_hidden: bool,
}

/// A finished load, ready to display.
///
/// Carries no path: the `(tab, generation)` pair the result is delivered with
/// already identifies which navigation it belongs to, and a second copy of the
/// location would just be another thing that can disagree.
pub struct Loaded {
    pub list: EntryList,
    /// Time spent enumerating, excluding sort. Tracked against the <200ms
    /// target for a 100k-entry directory.
    pub enumerate_time: Duration,
    pub sort_time: Duration,
}

pub enum LoadResult {
    Ready(Box<Loaded>),
    Failed { id: NodeId, error: String },
}

/// Newest generation issued per tab, shared with the worker so it can abandon
/// superseded work early.
type Generations = Arc<Mutex<HashMap<TabId, u64>>>;

pub struct Loader {
    requests: Sender<Request>,
    results: Receiver<(TabId, u64, LoadResult)>,
    generations: Generations,
    next_generation: u64,
}

impl Loader {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (req_tx, req_rx) = crossbeam_channel::unbounded::<Request>();
        let (res_tx, res_rx) = crossbeam_channel::unbounded();
        let generations: Generations = Arc::new(Mutex::new(HashMap::new()));

        let worker_generations = Arc::clone(&generations);
        std::thread::Builder::new()
            .name("neutron-loader".into())
            .spawn(move || worker(req_rx, res_tx, worker_generations, ctx))
            .expect("failed to spawn loader thread");

        Self {
            requests: req_tx,
            results: res_rx,
            generations,
            next_generation: 0,
        }
    }

    /// Queues a load for `tab`, superseding only that tab's in-flight request.
    ///
    /// Returns the generation, which the caller stores so it can discard
    /// results from navigations the user has already left.
    pub fn load(&mut self, tab: TabId, id: NodeId, sort: SortSpec, show_hidden: bool) -> u64 {
        self.next_generation += 1;
        let generation = self.next_generation;

        // Publish before sending, so a worker that picks the request up
        // immediately sees the current value.
        self.generations.lock().insert(tab, generation);

        // Send failure means the worker thread died, which surfaces on the next
        // poll rather than panicking on the UI thread.
        let _ = self.requests.send(Request {
            tab,
            generation,
            id,
            sort,
            show_hidden,
        });

        generation
    }

    /// Drops a closed tab's bookkeeping.
    pub fn forget(&mut self, tab: TabId) {
        self.generations.lock().remove(&tab);
    }

    /// Returns finished loads. Never blocks.
    ///
    /// Yields `(tab, generation, result)`; the caller compares the generation
    /// against what that tab is waiting for.
    pub fn poll(&mut self) -> Option<(TabId, u64, LoadResult)> {
        // A disconnected channel is not an error worth reporting: it only
        // happens once the worker has been torn down at shutdown, when there
        // is nothing left to deliver anyway.
        self.results.try_recv().ok()
    }
}

fn worker(
    requests: Receiver<Request>,
    results: Sender<(TabId, u64, LoadResult)>,
    generations: Generations,
    ctx: egui::Context,
) {
    // This thread enumerates namespace extensions as well as directories, and
    // `IShellFolder` is apartment-threaded. Entered once for the thread's life
    // rather than per request: initialising an apartment costs more than most
    // listings do.
    let _apartment = neutron_shell::sta::Apartment::enter();

    let fs = neutron_shell::fs::FsNamespace;
    let shell = neutron_shell::shell_ns::ShellNamespace;
    // One provider for the thread's life: it caches the access token, and a
    // fresh one per request would refresh on every listing.
    let drive = neutron_cloud::google::GoogleDrive::new();

    while let Ok(req) = requests.recv() {
        // Cheapest possible staleness check: skip superseded work before
        // touching the filesystem at all.
        if is_stale(&generations, req.tab, req.generation) {
            continue;
        }

        let started = Instant::now();
        // Dispatch on what the location *is*. Ordinary directories must never
        // touch COM — that is the whole performance argument — so the shell
        // backend is asked only for the nodes it claims.
        let outcome = match &req.id {
            NodeId::Cloud { id, .. } => drive.list(id),
            id if shell.handles(id) => shell.enumerate(id),
            id => fs.enumerate(id),
        };
        let enumerate_time = started.elapsed();

        let result = match outcome {
            Ok(mut list) => {
                // Sorting belongs here, not on the UI thread — a 100k-entry
                // sort is ~35ms, which would drop frames.
                let sort_started = Instant::now();
                sort::apply(&mut list, req.sort, req.show_hidden);
                let sort_time = sort_started.elapsed();

                tracing::debug!(
                    path = %req.id,
                    entries = list.len(),
                    enumerate_ms = enumerate_time.as_secs_f64() * 1000.0,
                    sort_ms = sort_time.as_secs_f64() * 1000.0,
                    "loaded directory"
                );

                LoadResult::Ready(Box::new(Loaded {
                    list,
                    enumerate_time,
                    sort_time,
                }))
            }
            Err(e) => LoadResult::Failed {
                id: req.id,
                error: e.to_string(),
            },
        };

        // Re-check after the slow part: the user may have navigated away while
        // we were reading. Sending anyway would be harmless (the UI filters
        // again) but wakes the UI thread for nothing.
        if is_stale(&generations, req.tab, req.generation) {
            continue;
        }

        if results.send((req.tab, req.generation, result)).is_err() {
            break;
        }

        // Without this the loaded directory would not appear until the user
        // happened to move the mouse — egui only repaints in response to input
        // or an explicit request.
        ctx.request_repaint();
    }
}

fn is_stale(generations: &Generations, tab: TabId, generation: u64) -> bool {
    generations
        .lock()
        .get(&tab)
        // A missing entry means the tab was closed, so the work is unwanted.
        .is_none_or(|&latest| generation < latest)
}
