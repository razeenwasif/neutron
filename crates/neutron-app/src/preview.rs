//! The preview pane: what the selected file actually contains.
//!
//! # What it previews, and what it does not
//!
//! Images and text, plus a card of facts for everything else. That covers what
//! a file manager is asked to show while browsing — is this the right photo, is
//! this the right config file — without pretending to be a document viewer.
//!
//! Explorer goes further by hosting `IPreviewHandler` COM objects, which is how
//! it shows PDFs and Office documents. Those handlers render into an `HWND` the
//! host supplies, which would mean parenting a child window over the wgpu
//! surface and keeping it aligned through every scroll, split and resize. It
//! also hands arbitrary third-party code a window inside our process. Not worth
//! it for a pane that answers "is this the file I meant".
//!
//! # Nothing here runs on the UI thread
//!
//! Reading a file can block for as long as the disk it is on — a sleeping
//! external drive, a network share, a cloud placeholder that has to be
//! downloaded first. Decoding a 40-megapixel photograph takes longer than a
//! frame on its own. Both happen on a worker, and the pane shows what it has.
//!
//! # Superseded work is abandoned, not queued
//!
//! Holding Down through a folder of photographs asks for a preview per row.
//! Each request carries a generation, the worker drops anything already
//! superseded before it opens the file, and the UI ignores late answers. The
//! request is also debounced, so a fast scroll asks for almost nothing.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;

/// How long the selection must hold still before its preview is fetched.
///
/// Long enough that arrowing through a folder does not read every file on the
/// way, short enough to feel like it happened because you stopped rather than
/// because you waited.
const DEBOUNCE: Duration = Duration::from_millis(120);

/// Most text read into the pane.
///
/// A preview is read, not audited. 256 KB is far more than fits on screen and
/// keeps a 2 GB log from being pulled through memory to show its first page.
const MAX_TEXT_BYTES: u64 = 256 * 1024;

/// Largest image file opened.
///
/// The decoder is also given a pixel limit; this is the cheaper check, made
/// before anything is read.
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Most that is pulled over a network connection for a preview.
///
/// A local read costs a seek. A remote one costs bandwidth and, for FTP, a
/// login — transfers open their own connection, because FTP cannot interleave a
/// listing with a download. So the size is checked against the listing *before*
/// anything is fetched, and a file over this says so instead. Under it, a
/// preview is worth one round trip; over it, the pane would be spending a
/// minute on a file nobody asked to open.
const MAX_REMOTE_BYTES: u64 = 2 * 1024 * 1024;

/// Longest edge kept when decoding.
///
/// A 6000×4000 photograph is 96 MB as RGBA, and the pane is a few hundred
/// pixels wide. Downscaling on the worker means that never reaches the UI
/// thread, the GPU, or a texture that has to be freed later.
const MAX_IMAGE_EDGE: u32 = 1024;

/// Where the bytes for a preview come from.
///
/// Not a `PathBuf`. Three of the four kinds of location a pane can be showing
/// have no filesystem path, and the pane used to reach for one, find nothing
/// and go blank in all three — inside a zip, on a server, and in Drive.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// A file on this machine.
    Local(PathBuf),
    /// A file on a saved server. Carries the whole site because a preview
    /// opens its own connection rather than waiting behind the listing's.
    Remote {
        site: Box<neutron_remote::Site>,
        path: String,
        name: String,
    },
    /// A Drive object, addressed by id like everything else in Drive.
    Drive { id: String },
    /// A shell namespace item — the inside of a zip, a Control Panel entry.
    /// The bytes exist only through COM, if they exist at all.
    Shell,
    /// An entry inside an archive being browsed, by its path in the archive.
    Archive { archive: PathBuf, inner: String },
}

/// What the pane has to show.
#[derive(Debug, Clone, PartialEq)]
pub enum Preview {
    /// Nothing selected, or several things.
    Nothing,
    /// Asked for, not arrived. Distinct from `Nothing` so the pane can say so
    /// rather than flickering empty on every keystroke.
    Loading,
    Text {
        text: String,
        /// True when the file is longer than what is shown.
        truncated: bool,
    },
    Image {
        rgba: Arc<Vec<u8>>,
        width: usize,
        height: usize,
        /// Dimensions of the file itself, which may be larger than what was
        /// decoded — the pane says so rather than implying the photo is small.
        source: [u32; 2],
    },
    /// Readable, but not something this pane renders.
    Opaque,
    /// Too big to be worth reading for a preview.
    TooLarge,
    /// There are no bytes to reach from here. Said rather than left blank: an
    /// empty pane is indistinguishable from one that is broken.
    Unreachable(String),
    Unreadable(String),
}

/// What the pane is showing, and what it is waiting for.
pub struct PreviewState {
    pub open: bool,
    /// What the current content belongs to. Compared on arrival so a late
    /// answer for something the user has moved off is discarded.
    pub showing: Option<Source>,
    pub content: Preview,
    /// Uploaded from `content` when it is an image, and dropped when it is not,
    /// so a texture never outlives the preview that produced it.
    pub texture: Option<egui::TextureHandle>,
}

impl Default for PreviewState {
    fn default() -> Self {
        Self {
            open: false,
            showing: None,
            content: Preview::Nothing,
            texture: None,
        }
    }
}

struct Request {
    generation: u64,
    source: Source,
    /// What the listing says the file is. Checked against the network cap
    /// before anything is fetched, so an oversized file costs no round trip.
    size: u64,
    is_dir: bool,
}

/// Background reader for the preview pane.
pub struct PreviewLoader {
    requests: Sender<Request>,
    results: Receiver<(u64, Source, Preview)>,
    newest: Arc<Mutex<u64>>,
    next_generation: u64,
    /// The request waiting out the debounce, if any.
    pending: Option<(Source, u64, bool, Instant)>,
}

impl PreviewLoader {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (req_tx, req_rx) = crossbeam_channel::unbounded::<Request>();
        let (res_tx, res_rx) = crossbeam_channel::unbounded();
        let newest = Arc::new(Mutex::new(0u64));

        let worker_newest = Arc::clone(&newest);
        std::thread::Builder::new()
            .name("neutron-preview".into())
            .spawn(move || worker(req_rx, res_tx, worker_newest, ctx))
            .expect("failed to spawn preview thread");

        Self {
            requests: req_tx,
            results: res_rx,
            newest,
            next_generation: 0,
            pending: None,
        }
    }

    /// Asks for `source`, after the debounce.
    ///
    /// Repeating the same source is free, so this can be called every frame
    /// with whatever the cursor is on.
    pub fn request(&mut self, source: Source, size: u64, is_dir: bool, showing: Option<&Source>) {
        if showing.is_some_and(|s| *s == source) {
            return;
        }
        if self.pending.as_ref().is_some_and(|(s, _, _, _)| *s == source) {
            return;
        }
        self.pending = Some((source, size, is_dir, Instant::now()));
    }

    /// Cancels anything pending and in flight.
    pub fn clear(&mut self) {
        self.pending = None;
        // Bumping the generation is what abandons work already on the worker:
        // it checks this before opening the file and again before sending.
        self.next_generation += 1;
        *self.newest.lock() = self.next_generation;
    }

    /// Sends the debounced request once it has waited long enough.
    pub fn tick(&mut self) {
        let Some((source, size, is_dir, asked)) = self.pending.as_ref() else {
            return;
        };
        if asked.elapsed() < DEBOUNCE {
            return;
        }
        let (source, size, is_dir) = (source.clone(), *size, *is_dir);
        self.pending = None;

        self.next_generation += 1;
        *self.newest.lock() = self.next_generation;
        let _ = self.requests.send(Request {
            generation: self.next_generation,
            source,
            size,
            is_dir,
        });
    }

    /// The newest finished preview, if one has arrived.
    pub fn poll(&self) -> Option<(Source, Preview)> {
        let mut latest = None;
        while let Ok((generation, source, preview)) = self.results.try_recv() {
            if generation == *self.newest.lock() {
                latest = Some((source, preview));
            }
        }
        latest
    }

    /// Whether a request is outstanding, so the pane can say "loading" rather
    /// than showing the previous file's contents under a new name.
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }
}

fn worker(
    requests: Receiver<Request>,
    results: Sender<(u64, Source, Preview)>,
    newest: Arc<Mutex<u64>>,
    ctx: egui::Context,
) {
    // One client for the thread's life, like the loader's. It caches an access
    // token, and a fresh one per preview would re-authenticate on every
    // keystroke that lands on a Drive file.
    let drive = neutron_cloud::google::GoogleDrive::new();
    // One server connection, reused between previews. See `PreviewConn`.
    let mut conn: Option<PreviewConn> = None;

    while let Ok(request) = requests.recv() {
        // Before opening anything: the user may have moved on while this sat
        // in the queue, and opening the file is the expensive part.
        if request.generation != *newest.lock() {
            continue;
        }

        // Checked *during* a transfer as well as before it. A local read is
        // over in microseconds, but a remote one can outlive the selection it
        // was made for, and there is no reason to keep pulling a file the user
        // has already arrowed past.
        let generation = request.generation;
        let still_wanted = || generation == *newest.lock();

        // Anything but a server means the cursor has left one, so the held
        // connection is dropped rather than kept open against a server nobody
        // is looking at — some of them permit very few at a time.
        if !matches!(request.source, Source::Remote { .. }) {
            conn = None;
        }

        let preview = match &request.source {
            Source::Local(path) => read(path, request.is_dir),
            Source::Remote { site, path, name } => read_remote(
                &mut conn,
                site,
                path,
                name,
                request.size,
                request.is_dir,
                &still_wanted,
            ),
            Source::Drive { id } => read_drive(&drive, id, request.is_dir, &still_wanted),
            // A drive letter under This PC, a Control Panel applet, an entry
            // inside a zip. The first two are not files; the third is, but its
            // bytes exist only through the shell's own extraction.
            Source::Archive { .. } if request.is_dir => Preview::Opaque,
            // Taken out into the cache, then read like any file.
            Source::Archive { archive, inner } => match crate::archives::materialise(archive, std::slice::from_ref(inner)) {
                Ok(paths) => read(&paths[0], false),
                Err(why) => Preview::Unreadable(why),
            },
            Source::Shell if request.is_dir => Preview::Opaque,
            Source::Shell => Preview::Unreachable(
                "No preview here — these contents are only reachable through the shell"
                    .to_owned(),
            ),
        };

        if request.generation != *newest.lock() {
            continue;
        }
        if results
            .send((request.generation, request.source, preview))
            .is_ok()
        {
            ctx.request_repaint();
        }
    }
}

/// One connection kept between previews.
///
/// Its own, not the listing's: FTP carries one command at a time, so previewing
/// over the listing connection would make every folder wait behind whatever the
/// cursor last touched. Kept rather than opened per file, because the
/// alternative is a login — for SFTP a full key exchange — every time the
/// cursor rests on a different row.
struct PreviewConn {
    site: String,
    transport: Box<dyn neutron_remote::Transport>,
}

/// Fetches enough of a file on a server to show it.
fn read_remote(
    held: &mut Option<PreviewConn>,
    site: &neutron_remote::Site,
    path: &str,
    name: &str,
    size: u64,
    is_dir: bool,
    still_wanted: &dyn Fn() -> bool,
) -> Preview {
    if is_dir {
        return Preview::Opaque;
    }
    // Answered from the listing, before a connection is opened. The cheapest
    // round trip is the one not made.
    if size > MAX_REMOTE_BYTES {
        return Preview::TooLarge;
    }

    // A connection to a different server is no use here. Dropped rather than
    // kept in a map: previews follow one cursor, so there is only ever one
    // server being looked at.
    if held.as_ref().is_some_and(|c| c.site != site.id) {
        *held = None;
    }

    // Exactly one retry, and only after a failure on a connection that was
    // already open. Servers close idle connections and the failure looks like
    // an ordinary error on the next command; retrying more than once against
    // one that is rate-limiting logins is how an account gets locked.
    for attempt in 0..2 {
        let reused = held.is_some();
        if held.is_none() {
            match neutron_remote::connect(site) {
                Ok(transport) => {
                    *held = Some(PreviewConn {
                        site: site.id.clone(),
                        transport,
                    })
                }
                Err(e) => return Preview::Unreadable(e.to_string()),
            }
        }
        let conn = held.as_mut().expect("just connected");

        let mut bytes: Vec<u8> = Vec::new();
        let outcome = conn.transport.download(path, &mut bytes, &mut |done| {
            // The size came from the server's own listing, and a server that
            // understated it would otherwise pull an unbounded file into
            // memory.
            if done > MAX_REMOTE_BYTES || !still_wanted() {
                neutron_remote::Continue::Stop
            } else {
                neutron_remote::Continue::Yes
            }
        });

        match outcome {
            Ok(_) => return from_bytes(name, bytes, size),
            // Stopped because the cursor moved. The answer is about to be
            // discarded anyway, and saying "unreadable" would be a lie about
            // the file. The connection is still good.
            Err(neutron_remote::RemoteError::Cancelled) => return Preview::Nothing,
            Err(e) => {
                *held = None;
                // A fresh connection that failed has nothing to retry with.
                if !reused || attempt == 1 {
                    return Preview::Unreadable(e.to_string());
                }
            }
        }
    }
    unreachable!("the loop returns on every path")
}

/// Fetches enough of a Drive object to show it.
fn read_drive(
    drive: &neutron_cloud::google::GoogleDrive,
    id: &str,
    is_dir: bool,
    still_wanted: &dyn Fn() -> bool,
) -> Preview {
    use neutron_cloud::drive::{Continue, Fetch};

    if is_dir {
        return Preview::Opaque;
    }

    // The listing's size is 0 for a Google Doc, which is not the same as an
    // empty file, and its MIME type is the only thing that says which. One
    // small request settles both, and it is cheap next to the transfer it
    // decides whether to make.
    let file = match drive.stat(id) {
        Ok(f) => f,
        Err(e) => return Preview::Unreadable(e),
    };

    if !matches!(neutron_cloud::drive::fetch_for(&file.mime_type), Fetch::Bytes) {
        // A Doc exports to .docx, which this pane cannot render either, so
        // fetching one would spend a round trip to arrive at the same answer.
        return Preview::Unreachable(
            "Google documents have no preview — open it to see it".to_owned(),
        );
    }

    let size = file.size.as_deref().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    if size > MAX_REMOTE_BYTES {
        return Preview::TooLarge;
    }

    let mut bytes: Vec<u8> = Vec::new();
    let outcome = drive.download(id, &Fetch::Bytes, &mut bytes, &mut |done| {
        if done > MAX_REMOTE_BYTES || !still_wanted() {
            Continue::Stop
        } else {
            Continue::Yes
        }
    });

    match outcome {
        Ok(_) => from_bytes(&file.name, bytes, size),
        Err(e) if e == "stopped" => Preview::Nothing,
        Err(e) => Preview::Unreadable(e),
    }
}

/// Decides what bytes already in memory are, and renders them.
///
/// `name` decides only whether to *try* the image decoder; `total` is the
/// file's real length, so text cut short can say that it was.
fn from_bytes(name: &str, bytes: Vec<u8>, total: u64) -> Preview {
    if looks_like_image(Path::new(name)) {
        return decode_image_bytes(&bytes);
    }
    let shown = bytes.len().min(MAX_TEXT_BYTES as usize);
    match decode_text(&bytes[..shown]) {
        Some(text) => Preview::Text {
            truncated: total > shown as u64,
            text,
        },
        None => Preview::Opaque,
    }
}

/// Works out what a file is and reads as much of it as the pane needs.
fn read(path: &Path, is_dir: bool) -> Preview {
    if is_dir {
        return Preview::Opaque;
    }

    let size = match std::fs::metadata(path) {
        // Only an ordinary file is opened. Opening a named pipe blocks until
        // something writes to it — forever, usually — and a device or socket
        // has no contents to show. Any of them left the one preview worker
        // stuck, and every preview after it read "Reading…" for good.
        Ok(meta) if !meta.is_file() => return Preview::Opaque,
        Ok(meta) => meta.len(),
        Err(e) => return Preview::Unreadable(e.to_string()),
    };

    if looks_like_image(path) {
        return if size > MAX_IMAGE_BYTES {
            Preview::TooLarge
        } else {
            decode_image(path)
        };
    }

    read_text(path, size)
}

/// Whether the extension claims an image the decoder is built for.
///
/// By extension rather than by sniffing the header: the decoder sniffs anyway,
/// and this decides whether to *try*, which is a question about what the user
/// meant the file to be.
pub(crate) fn looks_like_image(path: &Path) -> bool {
    const IMAGE: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif", "tiff",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE.iter().any(|k| e.eq_ignore_ascii_case(k)))
}

fn decode_image(path: &Path) -> Preview {
    use image::ImageReader;

    // Streamed from the file rather than read into a buffer first: the decoder
    // reads what it needs, and a 60 MB photograph never exists twice.
    match ImageReader::open(path).and_then(|r| r.with_guessed_format()) {
        Ok(reader) => finish_image(reader),
        Err(e) => Preview::Unreadable(e.to_string()),
    }
}

/// The same, for bytes already in memory — which is how anything fetched over a
/// connection arrives.
fn decode_image_bytes(bytes: &[u8]) -> Preview {
    use image::ImageReader;

    match ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format() {
        Ok(reader) => finish_image(reader),
        Err(e) => Preview::Unreadable(e.to_string()),
    }
}

fn finish_image<R: std::io::BufRead + std::io::Seek>(
    mut reader: image::ImageReader<R>,
) -> Preview {
    // A decoder is a parser fed untrusted input, and an image header can claim
    // any dimensions it likes. Without a limit a forty-byte file can ask for a
    // sixteen-gigabyte allocation.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);

    let decoded = match reader.decode() {
        Ok(d) => d,
        Err(e) => return Preview::Unreadable(short_decode_error(&e)),
    };

    let source = [decoded.width(), decoded.height()];
    // Downscaled here rather than by the GPU, so a 96 MB texture never exists.
    // `thumbnail` is a box filter — cheaper than Lanczos and indistinguishable
    // at the size this ends up.
    let scaled = if source[0] > MAX_IMAGE_EDGE || source[1] > MAX_IMAGE_EDGE {
        decoded.thumbnail(MAX_IMAGE_EDGE, MAX_IMAGE_EDGE)
    } else {
        decoded
    };

    let rgba = scaled.to_rgba8();
    Preview::Image {
        width: rgba.width() as usize,
        height: rgba.height() as usize,
        rgba: Arc::new(rgba.into_raw()),
        source,
    }
}

/// The decoder's message without the file path it usually repeats back.
fn short_decode_error(e: &image::ImageError) -> String {
    match e {
        image::ImageError::Unsupported(_) => "not a format Neutron can decode".to_owned(),
        other => other.to_string(),
    }
}

fn read_text(path: &Path, size: u64) -> Preview {
    use std::io::Read;

    if size > MAX_TEXT_BYTES * 64 {
        return Preview::TooLarge;
    }

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return Preview::Unreadable(e.to_string()),
    };

    let mut bytes = Vec::new();
    if let Err(e) = file
        .by_ref()
        .take(MAX_TEXT_BYTES)
        .read_to_end(&mut bytes)
    {
        return Preview::Unreadable(e.to_string());
    }

    match decode_text(&bytes) {
        Some(text) => Preview::Text {
            truncated: size > bytes.len() as u64,
            text,
        },
        None => Preview::Opaque,
    }
}

/// `bytes` as text, or `None` if it is not text.
///
/// A NUL byte is the giveaway: it cannot appear in UTF-8 text and appears in
/// nearly every binary within the first few hundred bytes. Checked before
/// decoding, because a truncated read can also split a multi-byte character
/// and that is not a reason to call a file binary.
pub fn decode_text(bytes: &[u8]) -> Option<String> {
    if bytes.contains(&0) {
        return None;
    }

    match std::str::from_utf8(bytes) {
        Ok(text) => Some(text.to_owned()),
        // `error_len() == None` means the input ended mid-character rather
        // than containing an invalid one — which is what reading the first
        // 256 KB of a longer file gives. Keep the part that is whole.
        Err(e) if e.error_len().is_none() => std::str::from_utf8(&bytes[..e.valid_up_to()])
            .ok()
            .map(str::to_owned),
        Err(_) => None,
    }
}

// The drawing half of this module follows the tests rather than preceding
// them, because these tests are about what a preview *is* — decoding, capping,
// what counts as text — and belong beside that half rather than after several
// hundred lines of egui calls they say nothing about.
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes fetched over a connection take a different route into the pane
    /// than a local file, and this is where the two rejoin.
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_not_opened() {
        let dir = std::env::temp_dir().join(format!("neutron-preview-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(made.success());

        // Returns at all: opening the pipe would block this thread forever.
        assert!(matches!(read(&fifo, false), Preview::Opaque));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetched_text_is_shown_as_text() {
        let Preview::Text { text, truncated } =
            from_bytes("notes.txt", b"host = example\nport = 22\n".to_vec(), 25)
        else {
            panic!("a config file is text");
        };
        assert!(text.starts_with("host = example"));
        assert!(!truncated);
    }

    /// A remote transfer is stopped at the cap, so what arrives can be less
    /// than the file. The pane says so rather than implying the file is short.
    #[test]
    fn a_partial_fetch_is_marked_truncated() {
        let Preview::Text { truncated, .. } =
            from_bytes("big.log", b"first page".to_vec(), 900_000)
        else {
            panic!("a log is text");
        };
        assert!(truncated);
    }

    /// Text longer than the cap is cut at it, whatever the transport handed
    /// over — a server is free to send more than it was asked for.
    #[test]
    fn more_bytes_than_the_cap_are_cut_to_it() {
        let huge = vec![b'a'; (MAX_TEXT_BYTES as usize) + 5_000];
        let total = huge.len() as u64;
        let Preview::Text { text, truncated } = from_bytes("big.txt", huge, total) else {
            panic!("still text");
        };
        assert_eq!(text.len(), MAX_TEXT_BYTES as usize);
        assert!(truncated);
    }

    #[test]
    fn fetched_binary_is_opaque_rather_than_garbled_text() {
        assert_eq!(from_bytes("app.exe", vec![0x4d, 0x5a, 0x90, 0x00, 0x03], 5), Preview::Opaque);
    }

    /// The extension decides whether to *try* the image decoder, and a file
    /// that is not really an image must fail as unreadable rather than being
    /// shown as text with an image's name.
    #[test]
    fn something_named_like_an_image_but_isnt_says_so() {
        let preview = from_bytes("photo.png", b"this is not a png".to_vec(), 17);
        assert!(
            matches!(preview, Preview::Unreadable(_)),
            "{preview:?}"
        );
    }

    /// An empty file is not a failure, and not binary.
    #[test]
    fn nothing_fetched_is_empty_text_not_an_error() {
        assert_eq!(
            from_bytes("empty.txt", Vec::new(), 0),
            Preview::Text { text: String::new(), truncated: false }
        );
    }

    #[test]
    fn plain_text_is_text() {
        assert_eq!(decode_text(b"hello\nworld").as_deref(), Some("hello\nworld"));
    }

    #[test]
    fn a_nul_byte_means_binary() {
        // Every executable, archive and compiled artefact has one early on.
        assert!(decode_text(b"MZ\x90\x00\x03").is_none());
    }

    #[test]
    fn invalid_utf8_means_binary() {
        assert!(decode_text(&[0xff, 0xfe, 0xfd, 0xfc]).is_none());
    }

    #[test]
    fn text_cut_mid_character_keeps_what_is_whole() {
        // Reading the first N bytes of a longer file lands here routinely, and
        // calling the file binary because of where the read stopped would be
        // wrong about the whole file.
        let mut bytes = "héllo wörld".as_bytes().to_vec();
        bytes.pop();
        let text = decode_text(&bytes).expect("a split character is still text");
        assert!(text.starts_with("héllo w"));
    }

    #[test]
    fn empty_is_text() {
        // An empty file previews as empty, not as binary.
        assert_eq!(decode_text(b"").as_deref(), Some(""));
    }

    #[test]
    fn image_extensions_are_recognised_whatever_their_case() {
        assert!(looks_like_image(Path::new("a.PNG")));
        assert!(looks_like_image(Path::new("a.jpeg")));
        assert!(!looks_like_image(Path::new("a.txt")));
        assert!(!looks_like_image(Path::new("a")));
    }

    #[test]
    fn a_name_that_merely_contains_an_extension_is_not_an_image() {
        assert!(!looks_like_image(Path::new("png")));
        assert!(!looks_like_image(Path::new("notes.png.txt")));
    }
}

// --- drawing ---------------------------------------------------------------

/// Facts about the previewed file, shown above the content.
pub struct Subject<'a> {
    pub name: &'a str,
    pub kind: String,
    pub size: Option<u64>,
    pub modified: i64,
    pub is_dir: bool,
    /// The system's own preview, for what the pane cannot read itself: a PDF,
    /// a film, a picture in a format it has no decoder for, or one too large.
    pub thumb: Option<neutron_ui::thumb::Thumb>,
}

/// Draws the pane's contents into `ui`.
///
/// Takes what to show rather than reading anything: this runs on the paint
/// thread, where the rule is that nothing blocks.
pub fn show(
    ui: &mut egui::Ui,
    p: &neutron_ui::Palette,
    state: &PreviewState,
    subject: Option<Subject<'_>>,
) {
    ui.spacing_mut().item_spacing.y = 8.0;

    let Some(subject) = subject else {
        empty(ui, p, "Nothing selected");
        return;
    };

    header(ui, p, &subject);
    ui.add_space(2.0);

    // Text and decoded pictures are the pane's own and drawn at full size;
    // for anything else, the system's preview stands in.
    let own = matches!(state.content, Preview::Text { .. } | Preview::Image { .. });
    if let Some(thumb) = subject.thumb.filter(|_| !own) {
        let space = egui::vec2(ui.available_width(), ui.available_height().min(480.0));
        let shown = neutron_ui::thumb::fitted_size(space, thumb.size);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(space.x, shown.y), egui::Sense::hover());
        neutron_ui::thumb::paint(ui.painter(), rect, thumb, 14.0, false, false);
        return;
    }

    match &state.content {
        Preview::Nothing => {}
        Preview::Loading => empty(ui, p, "Reading…"),
        Preview::Opaque if subject.is_dir => empty(ui, p, "Folder"),
        Preview::Opaque => empty(ui, p, "No preview for this kind of file"),
        Preview::TooLarge => empty(ui, p, "Too large to preview"),
        Preview::Unreachable(why) => empty(ui, p, why),
        Preview::Unreadable(why) => {
            ui.colored_label(p.danger, "Could not be read");
            ui.label(egui::RichText::new(why).color(p.text_faint).size(11.0));
        }
        Preview::Text { text, truncated } => {
            text_body(ui, p, text, *truncated);
        }
        Preview::Image { width, height, source, .. } => {
            image_body(ui, p, state, [*width, *height], *source);
        }
    }
}

fn header(ui: &mut egui::Ui, p: &neutron_ui::Palette, subject: &Subject<'_>) {
    ui.label(
        egui::RichText::new(subject.name)
            .color(p.text)
            .font(neutron_ui::theme::strong(ui.ctx(), 15.0)),
    );

    // One line of facts rather than a table: the pane is narrow, and a label
    // for every value would cost more width than the values.
    let mut facts = vec![subject.kind.clone()];
    if let Some(bytes) = subject.size.filter(|_| !subject.is_dir) {
        facts.push(neutron_ui::format::size(Some(bytes)));
    }
    facts.push(neutron_ui::format::timestamp(subject.modified));

    ui.label(
        egui::RichText::new(facts.join("  ·  "))
            .color(p.text_muted)
            .size(12.0),
    );
}

fn text_body(ui: &mut egui::Ui, p: &neutron_ui::Palette, text: &str, truncated: bool) {
    if truncated {
        ui.label(
            egui::RichText::new("first 256 KB")
                .color(p.text_faint)
                .size(10.0),
        );
    }

    // Pressed into the pane, as every well is.
    egui::Frame::new()
        .fill(p.inset)
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_white_alpha(9)))
        .corner_radius(egui::CornerRadius::same(neutron_ui::theme::RADIUS_WELL))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            egui::ScrollArea::both()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    // Monospaced and unwrapped. A preview of a config file or a
                    // log is being read for its structure, and soft-wrapping
                    // long lines destroys exactly that.
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(text)
                                .monospace()
                                .size(11.5)
                                .color(p.text_soft()),
                        )
                        .wrap_mode(egui::TextWrapMode::Extend),
                    );
                });
        });
}

fn image_body(
    ui: &mut egui::Ui,
    p: &neutron_ui::Palette,
    state: &PreviewState,
    decoded: [usize; 2],
    source: [u32; 2],
) {
    ui.label(
        egui::RichText::new(format!("{} × {}", source[0], source[1]))
            .color(p.text_faint)
            .size(10.0),
    );

    let Some(texture) = state.texture.as_ref() else {
        return;
    };

    // Fitted to the pane, never enlarged. Blowing a 32×32 icon up to fill the
    // width tells the user nothing and looks like a mistake.
    let available = ui.available_width().max(1.0);
    let scale = (available / decoded[0] as f32).min(1.0);
    let size = egui::vec2(decoded[0] as f32 * scale, decoded[1] as f32 * scale);

    ui.add(egui::Image::new(texture).fit_to_exact_size(size));
}

fn empty(ui: &mut egui::Ui, p: &neutron_ui::Palette, message: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(message)
            .color(p.text_faint)
            .size(11.5),
    );
}
