//! Turning visible rows into atlas slots, without ever blocking the UI thread.
//!
//! # The loop
//!
//! 1. While painting, the file list asks [`IconService::slot`] for each visible
//!    row. That is a hash lookup and nothing more.
//! 2. A miss records the key as wanted and returns `None`; the row draws its
//!    outline glyph for now.
//! 3. After the frame, [`IconService::pump`] submits the wanted keys to the STA
//!    pool.
//! 4. Resolved pixels arrive on a channel, are written into the atlas, and the
//!    row picks them up on a later frame.
//!
//! Rows therefore never wait for an icon, which is the entire point: Explorer's
//! most visible stall is a directory that scrolls at the speed of its slowest
//! icon handler.
//!
//! # Why requests are gathered during paint rather than at load
//!
//! A directory can hold 500,000 entries and the window shows thirty. Resolving
//! at load time would do sixteen thousand times the necessary work, most of it
//! for rows the user scrolls straight past. Asking as rows are painted means
//! the work tracks what is actually on screen.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crossbeam_channel::{Receiver, Sender};
use neutron_shell::icons::{IconImage, IconKey};
use neutron_shell::sta::StaPool;
use neutron_ui::atlas::{IconAtlas, Slot};

/// Upper bound on keys submitted per frame.
///
/// A screenful is ~30 rows, so this is generous. It exists for the case where
/// the user drags the scrollbar across a large directory: without it, every
/// intermediate position's worth of keys queues up and the pool spends the next
/// several seconds resolving icons for rows that were on screen for one frame.
const MAX_SUBMISSIONS_PER_FRAME: usize = 48;

/// Don't queue more when the pool is this far behind, for the same reason.
const POOL_BACKLOG_LIMIT: usize = 96;

/// What became of a key.
enum State {
    /// Submitted, not yet answered.
    Pending,
    /// Resolved and placed in the atlas.
    Ready(Slot),
    /// The shell had no icon, or the atlas was full. Never asked again — a key
    /// that failed once will fail identically every frame, and retrying would
    /// burn the pool on a permanent no.
    Unavailable,
}

/// The parts `slot` has to mutate from the paint path.
///
/// Behind a `RefCell` because rows are drawn through `&self` — the app's draw
/// functions deliberately take a shared borrow so they cannot mutate state
/// mid-frame (see the module docs on `app.rs`). Requesting an icon is the one
/// thing a row genuinely must record while painting, and it is confined to
/// this cell rather than being an excuse to loosen that rule everywhere.
#[derive(Default)]
struct Wants {
    known: HashMap<IconKey, State>,
    /// Keys seen this frame that are not yet known. Drained by `pump`.
    queue: Vec<IconKey>,
    /// Membership test for `queue`, so a screenful of identical extensions
    /// does not queue the same key thirty times.
    queued: HashSet<IconKey>,
}

pub struct IconService {
    pool: StaPool,
    wants: RefCell<Wants>,
    results: Receiver<(IconKey, Option<IconImage>)>,
    sender: Sender<(IconKey, Option<IconImage>)>,
    atlas: IconAtlas,
    /// Woken when a resolved icon arrives; a worker's channel send does not
    /// itself wake the event loop.
    ctx: egui::Context,
}

impl IconService {
    pub fn new(pool: StaPool, ctx: egui::Context) -> Self {
        let (sender, results) = crossbeam_channel::unbounded();
        Self {
            pool,
            wants: RefCell::new(Wants::default()),
            results,
            sender,
            atlas: IconAtlas::new(),
            ctx,
        }
    }

    /// The atlas slot for `key`, requesting it if this is the first sighting.
    ///
    /// Called from the paint path, so it does no I/O and allocates only when a
    /// key is genuinely new.
    pub fn slot(&self, key: &IconKey) -> Option<Slot> {
        let mut wants = self.wants.borrow_mut();

        match wants.known.get(key) {
            Some(State::Ready(slot)) => return Some(*slot),
            Some(State::Pending | State::Unavailable) => return None,
            None => {}
        }

        if wants.queued.insert(key.clone()) {
            wants.queue.push(key.clone());
        }
        None
    }

    /// Submits queued lookups and installs anything that has come back.
    ///
    /// Returns true when the atlas changed, which the caller uses to decide
    /// whether a repaint is warranted — icons arriving on a worker do not
    /// themselves wake the event loop.
    pub fn pump(&mut self) -> bool {
        let installed = self.drain();
        self.submit();
        installed
    }

    fn drain(&mut self) -> bool {
        let mut changed = false;

        let mut wants = self.wants.borrow_mut();
        while let Ok((key, image)) = self.results.try_recv() {
            let state = match image {
                Some(img) => match self.atlas.insert(&img.rgba) {
                    Some(slot) => {
                        changed = true;
                        State::Ready(slot)
                    }
                    // Atlas full, or the resolver produced a wrongly sized
                    // image. Either way this key has no cell and never will.
                    None => State::Unavailable,
                },
                None => State::Unavailable,
            };
            wants.known.insert(key, state);
        }

        changed
    }

    fn submit(&mut self) {
        let mut wants = self.wants.borrow_mut();
        if wants.queue.is_empty() {
            return;
        }

        // Throttle rather than drop: unsubmitted keys stay in `wanted` and go
        // out next frame. Dropping them would leave rows permanently glyphed
        // with no way to ask again, since `slot` only queues on first sighting.
        let budget = MAX_SUBMISSIONS_PER_FRAME
            .min(POOL_BACKLOG_LIMIT.saturating_sub(self.pool.pending()));
        let take = budget.min(wants.queue.len());

        for key in wants.queue.drain(..take).collect::<Vec<_>>() {
            wants.queued.remove(&key);
            wants.known.insert(key.clone(), State::Pending);

            let tx = self.sender.clone();
            let ctx = self.ctx.clone();
            let submitted = self.pool.submit(move || {
                let image = neutron_shell::icons::resolve(&key);
                if tx.send((key, image)).is_ok() {
                    // Without this the icon would not appear until the user
                    // happened to move the mouse.
                    ctx.request_repaint();
                }
            });

            if !submitted {
                // Pool gone — shutting down. Nothing further will resolve.
                return;
            }
        }
    }

    /// Uploads the atlas and returns the texture rows sample from.
    pub fn texture(&mut self, ctx: &egui::Context) -> Option<egui::TextureId> {
        self.atlas.texture(ctx).map(|t| t.id())
    }

    pub fn uv(&self, slot: Slot) -> egui::Rect {
        self.atlas.uv(slot)
    }

    /// Distinct icons resolved so far, for the status bar's diagnostics.
    pub fn resolved(&self) -> usize {
        self.atlas.len()
    }
}

/// Builds the key for one row, rooted at the directory being listed.
///
/// Split out so the file list does not need to know that per-path keys exist.
pub fn key_for(dir: Option<&std::path::Path>, name: &str, is_dir: bool) -> IconKey {
    let key = IconKey::for_entry(name, is_dir);
    match dir {
        Some(d) => key.rooted_at(d),
        // Without a directory a per-path key would name a file relative to
        // whatever the process's working directory happens to be, which is
        // meaningless. Fall back to the generic keyed-by-nothing case.
        None if key.touches_disk() => IconKey::Extensionless,
        None => key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn service() -> IconService {
        IconService::new(StaPool::spawn(), egui::Context::default())
    }

    /// Keys queued but not yet submitted.
    fn queued(s: &IconService) -> usize {
        s.wants.borrow().queue.len()
    }

    #[test]
    fn a_first_sighting_queues_exactly_once() {
        // Thirty visible `.txt` rows must produce one lookup, not thirty.
        let s = service();
        let key = IconKey::Extension("txt".into());

        for _ in 0..30 {
            assert!(s.slot(&key).is_none());
        }
        assert_eq!(queued(&s), 1);
    }

    #[test]
    fn a_failed_key_is_never_retried() {
        // A key the shell cannot resolve fails identically every frame, so
        // retrying spends the pool on a guaranteed no — forever, since rows are
        // re-painted continuously.
        let s = service();
        let key = IconKey::Extension("nope".into());
        s.wants
            .borrow_mut()
            .known
            .insert(key.clone(), State::Unavailable);

        assert!(s.slot(&key).is_none());
        assert_eq!(queued(&s), 0, "an unavailable key was queued again");
    }

    #[test]
    fn a_pending_key_is_not_queued_twice() {
        let s = service();
        let key = IconKey::Extension("txt".into());
        s.wants
            .borrow_mut()
            .known
            .insert(key.clone(), State::Pending);

        assert!(s.slot(&key).is_none());
        assert_eq!(queued(&s), 0);
    }

    #[test]
    fn submission_is_capped_per_frame() {
        // Dragging the scrollbar across a large directory sweeps thousands of
        // rows past the viewport. Submitting all of them would keep the pool
        // busy for seconds on rows that are already gone.
        let mut s = service();
        for i in 0..500 {
            s.slot(&IconKey::Extension(format!("e{i}")));
        }
        assert_eq!(queued(&s), 500);

        s.submit();
        assert!(
            queued(&s) >= 500 - MAX_SUBMISSIONS_PER_FRAME,
            "submitted more than the per-frame budget"
        );
    }

    #[test]
    fn throttled_keys_are_kept_rather_than_dropped() {
        // `slot` only queues on *first* sighting, so a dropped key would never
        // be asked for again and its rows would stay glyphed forever.
        let mut s = service();
        for i in 0..200 {
            s.slot(&IconKey::Extension(format!("e{i}")));
        }
        let before = queued(&s);
        s.submit();

        let submitted = before - queued(&s);
        assert!(submitted > 0);
        // Everything not submitted is still queued, and still deduplicated.
        let wants = s.wants.borrow();
        assert_eq!(wants.queue.len(), wants.queued.len());
        assert_eq!(wants.queue.len() + submitted, before);
    }

    #[test]
    fn per_path_keys_are_rooted_at_the_listed_directory() {
        let dir = Path::new(r"C:\games");
        assert_eq!(
            key_for(Some(dir), "game.exe", false),
            IconKey::Path(dir.join("game.exe"))
        );
        assert_eq!(
            key_for(Some(dir), "notes.txt", false),
            IconKey::Extension("txt".into())
        );
    }

    #[test]
    fn a_per_path_key_without_a_directory_degrades_rather_than_guessing() {
        // A bare name would be resolved relative to the process working
        // directory, which has nothing to do with what is being listed.
        let key = key_for(None, "game.exe", false);
        assert!(!key.touches_disk(), "would resolve against the wrong path");
    }
}
