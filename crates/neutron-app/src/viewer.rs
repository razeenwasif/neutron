//! The viewer: a file opened in a tab of its own to be read — a PDF page by
//! page, an image, or text and code with its lines numbered and coloured — as
//! the Mac's Preview reads them.
//!
//! # Threading
//!
//! A file is read and taken apart on a worker, once, when its tab first asks
//! for it. PDF pages are drawn on a thread of their own, newest request first,
//! at the width they are shown at; the paint thread only uploads finished
//! pixels. Documents are kept by path, so two tabs on one file share it, and
//! dropped once no tab shows them.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use neutron_ui::highlight::{self, Carry, Language};

/// Largest text file read whole; past it the first this many bytes are shown.
const MAX_TEXT: usize = 64 * 1024 * 1024;
/// Longest edge an image is shown at; larger ones are scaled down once.
const MAX_IMAGE_EDGE: u32 = 8192;
/// Most page images kept on the GPU at once.
const PAGE_CACHE: usize = 24;

/// What the viewer has of a file.
pub enum Doc {
    Loading,
    Text(TextDoc),
    Pdf(PdfDoc),
    Image(ImageDoc),
    Failed(String),
}

pub struct TextDoc {
    pub text: String,
    /// Each line's bytes in `text`, without its line break.
    pub lines: Vec<Range<usize>>,
    /// The state each line begins in, for colouring it on its own.
    pub carries: Vec<Carry>,
    pub language: Option<&'static Language>,
    /// The longest line, in characters, for how wide the page scrolls.
    pub widest: usize,
    /// Whether the file was longer than what was read.
    pub truncated: bool,
}

impl TextDoc {
    pub fn line(&self, i: usize) -> &str {
        &self.text[self.lines[i].clone()]
    }
}

pub struct PdfDoc {
    /// Each page's size, in points.
    pub pages: Vec<[f32; 2]>,
    /// Pages drawn, by page and the width in pixels they were drawn at.
    pub drawn: HashMap<(usize, u32), egui::TextureHandle>,
    asked: HashSet<(usize, u32)>,
    /// Pages in the order they were last shown, oldest first, for letting go.
    used: Vec<(usize, u32)>,
}

pub struct ImageDoc {
    pub size: [usize; 2],
    /// The image's own size, which may be larger than what is shown.
    pub source: [u32; 2],
    rgba: Option<Arc<Vec<u8>>>,
    pub texture: Option<egui::TextureHandle>,
}

/// One open file and how it is being looked at.
pub struct View {
    pub doc: Doc,
    pub zoom: f32,
    pub find: String,
    /// Where the text being looked for appears: line, and bytes in the line.
    pub matches: Vec<(usize, Range<usize>)>,
    /// Which match is the current one.
    pub current: usize,
    /// The matches were found for this text.
    found_for: String,
    /// Lines selected for copying, first and last.
    pub selected: Option<(usize, usize)>,
    /// A line or page to bring into view on the next frame.
    pub reveal: Option<usize>,
}

type Loaded = (PathBuf, Doc);
type Drawn = (PathBuf, usize, u32, usize, usize, Vec<u8>);

/// Every file open in a viewer tab.
pub struct Viewers {
    views: HashMap<PathBuf, View>,
    loaded_tx: Sender<Loaded>,
    loaded_rx: Receiver<Loaded>,
    draw_tx: Sender<(PathBuf, usize, u32)>,
    drawn_rx: Receiver<Drawn>,
    wake: egui::Context,
}

impl Viewers {
    pub fn spawn(wake: egui::Context) -> Self {
        let (loaded_tx, loaded_rx) = crossbeam_channel::unbounded();
        let (draw_tx, draw_rx) = crossbeam_channel::unbounded::<(PathBuf, usize, u32)>();
        let (drawn_tx, drawn_rx) = crossbeam_channel::unbounded();
        let waker = wake.clone();
        let _ = std::thread::Builder::new().name("viewer-pages".into()).spawn(move || {
            let mut queue: Vec<(PathBuf, usize, u32)> = Vec::new();
            loop {
                if queue.is_empty() {
                    match draw_rx.recv() {
                        Ok(job) => queue.push(job),
                        Err(_) => return,
                    }
                }
                queue.extend(draw_rx.try_iter());
                // The newest request is for what is on screen now.
                let Some((path, page, width)) = queue.pop() else { continue };
                if let Ok((w, h, pixels)) = neutron_shell::pdf::render(&path, page, width as usize) {
                    if drawn_tx.send((path, page, width, w, h, pixels)).is_err() {
                        return;
                    }
                    waker.request_repaint();
                }
            }
        });
        Viewers { views: HashMap::new(), loaded_tx, loaded_rx, draw_tx, drawn_rx, wake }
    }

    /// The view of `path`, starting to read it if nothing has yet.
    pub fn view(&mut self, path: &Path) -> &mut View {
        if !self.views.contains_key(path) {
            let tx = self.loaded_tx.clone();
            let wake = self.wake.clone();
            let owned = path.to_path_buf();
            let _ = std::thread::Builder::new().name("viewer-load".into()).spawn(move || {
                let doc = load(&owned);
                let _ = tx.send((owned, doc));
                wake.request_repaint();
            });
            self.views.insert(
                path.to_path_buf(),
                View {
                    doc: Doc::Loading,
                    zoom: 1.0,
                    find: String::new(),
                    matches: Vec::new(),
                    current: 0,
                    found_for: String::new(),
                    selected: None,
                    reveal: None,
                },
            );
        }
        self.views.get_mut(path).expect("just inserted")
    }

    /// Takes in files read and pages drawn since the last frame.
    pub fn pump(&mut self, ctx: &egui::Context) {
        for (path, doc) in self.loaded_rx.try_iter() {
            if let Some(view) = self.views.get_mut(&path) {
                view.doc = doc;
            }
        }
        for (path, page, width, w, h, pixels) in self.drawn_rx.try_iter() {
            let Some(View { doc: Doc::Pdf(pdf), .. }) = self.views.get_mut(&path) else { continue };
            let image = egui::ColorImage::from_rgba_premultiplied([w, h], &pixels);
            let texture = ctx.load_texture(format!("pdf:{}:{page}:{width}", path.display()), image, egui::TextureOptions::LINEAR);
            pdf.drawn.insert((page, width), texture);
            pdf.used.push((page, width));
            // Let go of the pages shown longest ago.
            while pdf.drawn.len() > PAGE_CACHE && !pdf.used.is_empty() {
                let old = pdf.used.remove(0);
                if !pdf.used.contains(&old) {
                    pdf.drawn.remove(&old);
                    pdf.asked.remove(&old);
                }
            }
        }
    }

    /// Lets go of every file no tab shows.
    pub fn retain(&mut self, shown: &HashSet<PathBuf>) {
        self.views.retain(|path, _| shown.contains(path));
    }

    /// The view of `path` and the channel for drawing its pages, borrowed
    /// together.
    pub fn parts(&mut self, path: &Path) -> (&mut View, PageAsker<'_>) {
        let draw_tx = &self.draw_tx;
        let view = self.views.get_mut(path).expect("asked for after `view`");
        (view, PageAsker { draw_tx, path: path.to_path_buf() })
    }
}

/// Asks for a PDF's pages to be drawn.
pub struct PageAsker<'a> {
    draw_tx: &'a Sender<(PathBuf, usize, u32)>,
    path: PathBuf,
}

impl PageAsker<'_> {
    pub fn ask(&self, pdf: &mut PdfDoc, page: usize, width: u32) {
        if pdf.drawn.contains_key(&(page, width)) {
            if let Some(at) = pdf.used.iter().position(|k| *k == (page, width)) {
                let key = pdf.used.remove(at);
                pdf.used.push(key);
            }
            return;
        }
        if pdf.asked.insert((page, width)) {
            let _ = self.draw_tx.send((self.path.clone(), page, width));
        }
    }
}

impl View {
    /// Finds the text being looked for again if it changed, case blind, and
    /// brings the first match into view.
    pub fn refind(&mut self) {
        if self.find == self.found_for {
            return;
        }
        self.found_for = self.find.clone();
        self.matches.clear();
        self.current = 0;
        let Doc::Text(text) = &self.doc else { return };
        let needle = self.find.to_lowercase();
        if needle.is_empty() {
            return;
        }
        for (i, range) in text.lines.iter().enumerate() {
            let line = text.text[range.clone()].to_lowercase();
            // Lowercasing can change byte lengths outside ASCII; matches are
            // only kept where it did not, so the ranges fit the original.
            if line.len() != range.len() {
                continue;
            }
            let mut from = 0;
            while let Some(at) = line[from..].find(&needle) {
                self.matches.push((i, from + at..from + at + needle.len()));
                from += at + needle.len().max(1);
                if self.matches.len() >= 100_000 {
                    return;
                }
            }
        }
        self.reveal = self.matches.first().map(|(line, _)| *line);
    }

    /// Moves to the next match, or the one before.
    pub fn step(&mut self, back: bool) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len();
        self.current = if back { (self.current + n - 1) % n } else { (self.current + 1) % n };
        self.reveal = Some(self.matches[self.current].0);
    }

    /// The selected lines, or the whole text, to copy.
    pub fn copy_text(&self) -> Option<String> {
        let Doc::Text(text) = &self.doc else { return None };
        let (a, b) = self.selected.unwrap_or((0, text.lines.len().saturating_sub(1)));
        let (a, b) = (a.min(b), a.max(b).min(text.lines.len().saturating_sub(1)));
        Some((a..=b).map(|i| text.line(i)).collect::<Vec<_>>().join("\n"))
    }

    /// Uploads an image the first time it is drawn.
    pub fn image_texture(&mut self, ctx: &egui::Context, path: &Path) -> Option<&egui::TextureHandle> {
        let Doc::Image(image) = &mut self.doc else { return None };
        if image.texture.is_none() {
            let rgba = image.rgba.take()?;
            let color = egui::ColorImage::from_rgba_unmultiplied(image.size, &rgba);
            image.texture = Some(ctx.load_texture(format!("viewer:{}", path.display()), color, egui::TextureOptions::LINEAR));
        }
        image.texture.as_ref()
    }
}

/// Reads and takes apart a file for viewing. Blocks on the disk.
pub fn load(path: &Path) -> Doc {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if ext == "pdf" {
        return pdf_doc(path);
    }
    if crate::preview::looks_like_image(path) {
        return load_image(path);
    }
    let bytes = match std::fs::File::open(path).and_then(|f| {
        use std::io::Read;
        let mut bytes = Vec::new();
        f.take(MAX_TEXT as u64 + 1).read_to_end(&mut bytes).map(|_| bytes)
    }) {
        Ok(b) => b,
        Err(e) => return Doc::Failed(format!("The file cannot be read: {e}")),
    };
    // A PDF named as something else is still read as one.
    if bytes.starts_with(b"%PDF") {
        return pdf_doc(path);
    }
    // A file with nothing but text has no zero bytes; a few kilobytes is
    // enough to tell.
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Doc::Failed("This is not a text file, an image or a PDF. Open it with its own app to see it.".into());
    }
    let truncated = bytes.len() > MAX_TEXT;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_TEXT)]).into_owned();
    text_doc(text, path, truncated)
}

/// Text taken apart into lines, each with the colouring state it begins in.
pub fn text_doc(text: String, path: &Path, truncated: bool) -> Doc {
    let mut lines = Vec::new();
    let mut start = 0;
    for (at, _) in text.match_indices('\n') {
        let end = if at > start && text.as_bytes()[at - 1] == b'\r' { at - 1 } else { at };
        lines.push(start..end);
        start = at + 1;
    }
    if start < text.len() || lines.is_empty() {
        lines.push(start..text.len());
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let language = highlight::language_for(&name);
    let carries = match language {
        Some(lang) => highlight::carries(lang, lines.iter().map(|r| &text[r.clone()])),
        None => vec![Carry::None; lines.len()],
    };
    let widest = lines.iter().map(|r| text[r.clone()].chars().count()).max().unwrap_or(0);
    Doc::Text(TextDoc { text, lines, carries, language, widest, truncated })
}

fn pdf_doc(path: &Path) -> Doc {
    match neutron_shell::pdf::pages(path) {
        Ok(pages) if pages.is_empty() => Doc::Failed("This PDF has no pages.".into()),
        Ok(pages) => Doc::Pdf(PdfDoc { pages, drawn: HashMap::new(), asked: HashSet::new(), used: Vec::new() }),
        Err(e) => Doc::Failed(format!("This PDF cannot be shown: {e}")),
    }
}

fn load_image(path: &Path) -> Doc {
    let reader = match image::ImageReader::open(path).and_then(|r| r.with_guessed_format()) {
        Ok(r) => r,
        Err(e) => return Doc::Failed(format!("The image cannot be read: {e}")),
    };
    let mut reader = reader;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(40_000);
    limits.max_image_height = Some(40_000);
    limits.max_alloc = Some(1024 * 1024 * 1024);
    reader.limits(limits);
    let decoded = match reader.decode() {
        Ok(d) => d,
        Err(e) => return Doc::Failed(format!("The image cannot be shown: {e}")),
    };
    let source = [decoded.width(), decoded.height()];
    let scaled = if source[0] > MAX_IMAGE_EDGE || source[1] > MAX_IMAGE_EDGE {
        decoded.thumbnail(MAX_IMAGE_EDGE, MAX_IMAGE_EDGE)
    } else {
        decoded
    };
    let rgba = scaled.to_rgba8();
    Doc::Image(ImageDoc {
        size: [rgba.width() as usize, rgba.height() as usize],
        source,
        rgba: Some(Arc::new(rgba.into_raw())),
        texture: None,
    })
}

/// Whether the viewer can show `name`: a PDF, an image, or something that is
/// probably text. Decided by name, for whether to offer it in a menu.
pub fn can_show(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    const TEXT: &[&str] = &[
        "txt", "md", "markdown", "rst", "log", "csv", "tsv", "text", "tex", "org", "adoc", "srt", "vtt", "diff",
        "patch",
    ];
    ext == "pdf"
        || crate::preview::looks_like_image(Path::new(&lower))
        || TEXT.contains(&ext)
        || highlight::language_for(&lower).is_some()
        || !lower.contains('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_split_into_lines_with_their_colouring_state() {
        let Doc::Text(doc) = text_doc("fn a() {\r\n/* x\ny */\n".into(), Path::new("a.rs"), false) else { panic!() };
        assert_eq!(doc.lines.len(), 3);
        assert_eq!(doc.line(0), "fn a() {", "the CR goes with the break");
        assert_eq!(doc.carries, vec![Carry::None, Carry::None, Carry::Comment]);
        assert_eq!(doc.language.unwrap().name, "Rust");
        assert_eq!(doc.widest, 8);
    }

    #[test]
    fn finding_is_case_blind_and_steps_round() {
        let mut view = View {
            doc: text_doc("Alpha beta\nALPHA\nnone".into(), Path::new("t.txt"), false),
            zoom: 1.0,
            find: "alpha".into(),
            matches: Vec::new(),
            current: 0,
            found_for: String::new(),
            selected: None,
            reveal: None,
        };
        view.refind();
        assert_eq!(view.matches, vec![(0, 0..5), (1, 0..5)]);
        view.step(false);
        assert_eq!(view.reveal, Some(1));
        view.step(false);
        assert_eq!(view.current, 0, "round to the first");
        view.selected = Some((1, 0));
        assert_eq!(view.copy_text().unwrap(), "Alpha beta\nALPHA");
    }

    #[test]
    fn a_binary_file_is_refused_and_a_pdf_named_otherwise_is_read_as_one() {
        let dir = std::env::temp_dir().join(format!("neutron-viewer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
        assert!(matches!(load(&dir.join("blob.bin")), Doc::Failed(_)));
        std::fs::write(dir.join("notes"), "hello\nworld").unwrap();
        assert!(matches!(load(&dir.join("notes")), Doc::Text(_)));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(can_show("paper.pdf") && can_show("main.rs") && can_show("README") && !can_show("movie.mov"));
    }
}
