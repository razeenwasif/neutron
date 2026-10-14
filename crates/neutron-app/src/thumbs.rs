//! Previews of what is inside files, from request to texture without the UI
//! thread ever waiting.
//!
//! # The loop
//!
//! The same shape as the icon service: a view asks while painting, a miss is
//! noted, and after the frame [`Thumbnails::pump`] hands the misses to workers.
//! What differs is what a request is worth. An icon is shared by every file of
//! a type; a preview belongs to one file, costs a trip to the system's preview
//! service, and is a texture of its own. So:
//!
//! - Only what was asked for *this frame* is sent. A request left over from a
//!   row scrolled past a second ago is dropped rather than queued, and asked
//!   again if the row comes back. Flinging through a folder of photographs
//!   therefore previews where you stop, not everything you passed.
//! - Previews are kept on disk, named after the file, its date and the size
//!   asked for, so a folder opened again is previewed from the cache. An
//!   edited file has a new date and is previewed afresh.
//! - Textures are capped. The least recently drawn go first.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, Sender};
use neutron_ui::thumb::Thumb;

/// How large a preview is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Grade {
    /// For a tile or a row.
    Small,
    /// For the info column and the preview pane.
    Large,
}

impl Grade {
    /// Pixels on the longer side. Twice the largest each is drawn at, for a
    /// Retina screen.
    fn px(self) -> u32 {
        match self {
            Grade::Small => 256,
            Grade::Large => 640,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    modified: i64,
    grade: Grade,
}

enum State {
    Pending,
    Ready { texture: egui::TextureHandle, size: egui::Vec2, used: u64 },
    /// The system had no preview. Not asked again for this date of the file.
    Missing,
}

/// The parts the paint path writes to. In a cell for the reason given on
/// `icon_service::Wants`: views draw through `&self`.
#[derive(Default)]
struct Wants {
    known: HashMap<Key, State>,
    /// Asked for this frame and not yet known, in the order asked.
    wanted: Vec<Key>,
    seen: HashSet<Key>,
    frame: u64,
}

/// Most textures kept at once: a few screens of tiles, and the large ones.
const MAX_TEXTURES: usize = 240;
/// Most requests in the workers' hands at once.
const MAX_IN_FLIGHT: usize = 24;
/// Previews to one helper call: enough to pay for starting it.
const BATCH: usize = 8;
const WORKERS: usize = 2;

pub struct Thumbnails {
    wants: RefCell<Wants>,
    jobs: Sender<Vec<Key>>,
    results: Receiver<(Key, Option<egui::ColorImage>)>,
    in_flight: usize,
    ctx: egui::Context,
}

impl Thumbnails {
    pub fn new(ctx: egui::Context) -> Self {
        Self::with_cache(ctx, cache_dir())
    }

    /// With previews kept in `dir` — or, with none, never made at all, which
    /// is what a test wants: no system calls, nothing written to the user's
    /// caches.
    fn with_cache(ctx: egui::Context, dir: Option<PathBuf>) -> Self {
        let (jobs, inbox) = crossbeam_channel::unbounded::<Vec<Key>>();
        let (done, results) = crossbeam_channel::unbounded();
        for n in 0..WORKERS {
            let inbox = inbox.clone();
            let done = done.clone();
            let ctx = ctx.clone();
            let dir = dir.clone();
            let _ = std::thread::Builder::new()
                .name(format!("neutron-previews-{n}"))
                .spawn(move || {
                    if n == 0 {
                        if let Some(dir) = &dir {
                            prune(dir);
                        }
                    }
                    while let Ok(batch) = inbox.recv() {
                        for answer in make(dir.as_deref(), batch) {
                            if done.send(answer).is_err() {
                                return;
                            }
                        }
                        // A worker's send does not wake the event loop.
                        ctx.request_repaint();
                    }
                });
        }
        Self { wants: RefCell::new(Wants::default()), jobs, results, in_flight: 0, ctx }
    }

    /// The preview of `path`, asking for it if this is the first time.
    ///
    /// Called while painting: a lookup, and a push when the key is new.
    pub fn get(&self, path: &Path, modified: i64, grade: Grade) -> Option<Thumb> {
        let key = Key { path: path.to_path_buf(), modified, grade };
        let mut wants = self.wants.borrow_mut();
        let frame = wants.frame;
        match wants.known.get_mut(&key) {
            Some(State::Ready { texture, size, used }) => {
                *used = frame;
                return Some(Thumb { texture: texture.id(), size: *size });
            }
            Some(State::Pending | State::Missing) => return None,
            None => {}
        }
        if wants.seen.insert(key.clone()) {
            wants.wanted.push(key);
        }
        None
    }

    /// Installs what has come back and sends what this frame asked for.
    pub fn pump(&mut self) {
        let mut wants = self.wants.borrow_mut();
        wants.frame += 1;

        while let Ok((key, image)) = self.results.try_recv() {
            self.in_flight = self.in_flight.saturating_sub(1);
            let state = match image {
                Some(image) => {
                    let size = egui::vec2(image.size[0] as f32, image.size[1] as f32);
                    let texture = self.ctx.load_texture(
                        format!("preview:{}", key.path.display()),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    State::Ready { texture, size, used: wants.frame }
                }
                None => State::Missing,
            };
            wants.known.insert(key, state);
        }

        // This frame's requests only; see the module docs.
        let wanted = std::mem::take(&mut wants.wanted);
        wants.seen.clear();
        let room = MAX_IN_FLIGHT.saturating_sub(self.in_flight);
        let mut sending: HashMap<Grade, Vec<Key>> = HashMap::new();
        for key in wanted.into_iter().take(room) {
            wants.known.insert(key.clone(), State::Pending);
            sending.entry(key.grade).or_default().push(key);
        }
        for keys in sending.into_values() {
            for batch in keys.chunks(BATCH) {
                self.in_flight += batch.len();
                if self.jobs.send(batch.to_vec()).is_err() {
                    return;
                }
            }
        }

        evict(&mut wants.known);
    }
}

/// Drops the least recently drawn textures past the cap.
fn evict(known: &mut HashMap<Key, State>) {
    let mut ready: Vec<(u64, Key)> = known
        .iter()
        .filter_map(|(k, s)| match s {
            State::Ready { used, .. } => Some((*used, k.clone())),
            _ => None,
        })
        .collect();
    if ready.len() > MAX_TEXTURES {
        ready.sort_unstable_by_key(|(used, _)| *used);
        let excess = ready.len() - MAX_TEXTURES;
        for (_, key) in ready.into_iter().take(excess) {
            // Forgotten rather than marked: drawn again, it is reloaded from
            // the disk cache, which costs a decode and no trip to the system.
            known.remove(&key);
        }
    }
    // Answers that there is no preview are small, but a long session in large
    // folders would keep every one of them.
    if known.len() > 20_000 {
        known.retain(|_, s| !matches!(s, State::Missing));
    }
}

/// Makes one batch: from the disk cache where it can, from the system where
/// it cannot.
fn make(dir: Option<&Path>, batch: Vec<Key>) -> Vec<(Key, Option<egui::ColorImage>)> {
    let Some(dir) = dir else {
        return batch.into_iter().map(|k| (k, None)).collect();
    };
    let files: Vec<PathBuf> = batch.iter().map(|k| dir.join(file_name(k))).collect();
    let refused = |file: &Path| file.with_extension("none");

    let todo: Vec<usize> = (0..batch.len())
        .filter(|&i| !files[i].is_file() && !refused(&files[i]).is_file())
        .collect();
    if !todo.is_empty() {
        let paths: Vec<PathBuf> = todo.iter().map(|&i| batch[i].path.clone()).collect();
        let outputs: Vec<PathBuf> = todo.iter().map(|&i| files[i].clone()).collect();
        let px = batch[0].grade.px();
        match neutron_shell::thumbnails::render(&paths, &outputs, px) {
            Ok(made) => {
                // Remembered on disk too, so a file with no preview is not
                // asked about on every launch.
                for (&i, made) in todo.iter().zip(made) {
                    if !made {
                        let _ = std::fs::write(refused(&files[i]), b"");
                    }
                }
            }
            Err(e) => tracing::debug!(error = %e, "previews could not be made"),
        }
    }

    batch
        .into_iter()
        .zip(files)
        .map(|(key, file)| {
            let image = decode(&file);
            (key, image)
        })
        .collect()
}

fn decode(file: &Path) -> Option<egui::ColorImage> {
    let image = image::open(file).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}

/// Where previews are kept between launches.
fn cache_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("HOME")?).join("Library/Caches/Neutron/thumbnails");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// A preview's file name: a hash of what it is a preview of.
///
/// FNV-1a rather than the standard hasher, whose output is allowed to change
/// between Rust releases — which would orphan the whole cache on an update.
fn file_name(key: &Key) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(key.path.as_os_str().as_encoded_bytes());
    feed(&key.modified.to_le_bytes());
    feed(&key.grade.px().to_le_bytes());
    format!("{hash:016x}.png")
}

/// Keeps the cache to a few thousand files, oldest out first.
fn prune(dir: &Path) {
    const KEEP: usize = 4000;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".png") || name.ends_with(".none")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= KEEP + KEEP / 4 {
        return;
    }
    files.sort_unstable_by_key(|(at, _)| *at);
    let excess = files.len() - KEEP;
    for (_, path) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str, modified: i64, grade: Grade) -> Key {
        Key { path: path.into(), modified, grade }
    }

    #[test]
    fn a_preview_is_named_for_the_file_its_date_and_its_size() {
        let a = file_name(&key("/a.png", 1, Grade::Small));
        assert_eq!(a, file_name(&key("/a.png", 1, Grade::Small)), "not stable");
        assert_ne!(a, file_name(&key("/a.png", 2, Grade::Small)), "an edit kept the old preview");
        assert_ne!(a, file_name(&key("/a.png", 1, Grade::Large)));
        assert_ne!(a, file_name(&key("/b.png", 1, Grade::Small)));
    }

    #[test]
    fn only_this_frames_requests_are_sent() {
        let mut t = Thumbnails::with_cache(egui::Context::default(), None);
        assert!(t.get(Path::new("/x.png"), 1, Grade::Small).is_none());
        assert!(t.get(Path::new("/x.png"), 1, Grade::Small).is_none());
        assert_eq!(t.wants.borrow().wanted.len(), 1, "asked twice in one frame, queued twice");
        t.pump();
        assert!(t.wants.borrow().wanted.is_empty());
        // Sent, so pending: not asked for again.
        assert!(t.get(Path::new("/x.png"), 1, Grade::Small).is_none());
        assert!(t.wants.borrow().wanted.is_empty());
    }
}
