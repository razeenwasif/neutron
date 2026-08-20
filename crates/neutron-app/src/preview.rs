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

/// Longest edge kept when decoding.
///
/// A 6000×4000 photograph is 96 MB as RGBA, and the pane is a few hundred
/// pixels wide. Downscaling on the worker means that never reaches the UI
/// thread, the GPU, or a texture that has to be freed later.
const MAX_IMAGE_EDGE: u32 = 1024;

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
    Unreadable(String),
}

/// What the pane is showing, and what it is waiting for.
pub struct PreviewState {
    pub open: bool,
    /// The file the current content belongs to. Compared on arrival so a late
    /// answer for a file the user has moved off is discarded.
    pub showing: Option<PathBuf>,
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
    path: PathBuf,
    is_dir: bool,
}

/// Background reader for the preview pane.
pub struct PreviewLoader {
    requests: Sender<Request>,
    results: Receiver<(u64, PathBuf, Preview)>,
    newest: Arc<Mutex<u64>>,
    next_generation: u64,
    /// The request waiting out the debounce, if any.
    pending: Option<(PathBuf, bool, Instant)>,
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

    /// Asks for `path`, after the debounce.
    ///
    /// Repeating the same path is free, so this can be called every frame with
    /// whatever the cursor is on.
    pub fn request(&mut self, path: &Path, is_dir: bool, showing: Option<&PathBuf>) {
        if showing.is_some_and(|s| s == path) {
            return;
        }
        if self.pending.as_ref().is_some_and(|(p, _, _)| p == path) {
            return;
        }
        self.pending = Some((path.to_path_buf(), is_dir, Instant::now()));
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
        let Some((path, is_dir, asked)) = self.pending.as_ref() else {
            return;
        };
        if asked.elapsed() < DEBOUNCE {
            return;
        }
        let (path, is_dir) = (path.clone(), *is_dir);
        self.pending = None;

        self.next_generation += 1;
        *self.newest.lock() = self.next_generation;
        let _ = self.requests.send(Request {
            generation: self.next_generation,
            path,
            is_dir,
        });
    }

    /// The newest finished preview, if one has arrived.
    pub fn poll(&self) -> Option<(PathBuf, Preview)> {
        let mut latest = None;
        while let Ok((generation, path, preview)) = self.results.try_recv() {
            if generation == *self.newest.lock() {
                latest = Some((path, preview));
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
    results: Sender<(u64, PathBuf, Preview)>,
    newest: Arc<Mutex<u64>>,
    ctx: egui::Context,
) {
    while let Ok(request) = requests.recv() {
        // Before opening anything: the user may have moved on while this sat
        // in the queue, and opening the file is the expensive part.
        if request.generation != *newest.lock() {
            continue;
        }

        let preview = read(&request.path, request.is_dir);

        if request.generation != *newest.lock() {
            continue;
        }
        if results
            .send((request.generation, request.path, preview))
            .is_ok()
        {
            ctx.request_repaint();
        }
    }
}

/// Works out what a file is and reads as much of it as the pane needs.
fn read(path: &Path, is_dir: bool) -> Preview {
    if is_dir {
        return Preview::Opaque;
    }

    let size = match std::fs::metadata(path) {
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
fn looks_like_image(path: &Path) -> bool {
    const IMAGE: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif", "tiff",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE.iter().any(|k| e.eq_ignore_ascii_case(k)))
}

fn decode_image(path: &Path) -> Preview {
    use image::ImageReader;

    let reader = match ImageReader::open(path).and_then(|r| r.with_guessed_format()) {
        Ok(r) => r,
        Err(e) => return Preview::Unreadable(e.to_string()),
    };

    // A decoder is a parser fed untrusted input, and an image header can claim
    // any dimensions it likes. Without a limit a forty-byte file can ask for a
    // sixteen-gigabyte allocation.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(256 * 1024 * 1024);

    let mut reader = reader;
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

#[cfg(test)]
mod tests {
    use super::*;

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

    match &state.content {
        Preview::Nothing => {}
        Preview::Loading => empty(ui, p, "Reading…"),
        Preview::Opaque if subject.is_dir => empty(ui, p, "Folder"),
        Preview::Opaque => empty(ui, p, "No preview for this kind of file"),
        Preview::TooLarge => empty(ui, p, "Too large to preview"),
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
            .size(13.0)
            .strong(),
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
            .size(11.0),
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

    egui::Frame::new()
        .fill(p.inset)
        .corner_radius(egui::CornerRadius::same(neutron_ui::theme::RADIUS_SMALL))
        .inner_margin(egui::Margin::same(8))
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
                                .size(11.0)
                                .color(p.text),
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
