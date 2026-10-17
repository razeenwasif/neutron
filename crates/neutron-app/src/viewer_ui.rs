//! Drawing a viewer tab: its toolbar, then the text, the pages or the image.

use std::path::Path;

use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};
use neutron_ui::highlight::{self, Token};
use neutron_ui::theme::{self, Palette};

use crate::viewer::{Doc, PageAsker, PdfDoc, TextDoc, View};

/// What a viewer tab asks the app to do.
pub enum Ask {
    /// Open the file with its own app.
    Launch,
    Copy(String),
}

const BAR: f32 = 44.0;
const TEXT_SIZE: f32 = 13.0;
const PAGE_GAP: f32 = 18.0;

/// Draws the viewer for `path` filling `ui`.
pub fn show(ui: &mut Ui, p: &Palette, path: &Path, view: &mut View, pages: PageAsker<'_>) -> Option<Ask> {
    let mut ask = None;
    let area = ui.available_rect_before_wrap();
    let bar = Rect::from_min_size(area.min, vec2(area.width(), BAR));
    let body = Rect::from_min_max(pos2(area.left(), bar.bottom()), area.max);

    // --- toolbar ---
    let mut bar_ui = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(12.0, 6.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    bar_ui.label(egui::RichText::new(name).font(theme::strong(ui.ctx(), 14.0)).color(p.text));
    let info = match &view.doc {
        Doc::Loading => "Opening…".to_owned(),
        Doc::Text(t) => {
            let lines = t.lines.len();
            let kind = t.language.map_or("Text", |l| l.name);
            format!("{kind} · {lines} line{}{}", if lines == 1 { "" } else { "s" }, if t.truncated { " · first 64 MB" } else { "" })
        }
        Doc::Pdf(pdf) => format!("PDF · {} page{}", pdf.pages.len(), if pdf.pages.len() == 1 { "" } else { "s" }),
        Doc::Image(img) => format!("Image · {} × {}", img.source[0], img.source[1]),
        Doc::Failed(_) => String::new(),
    };
    bar_ui.label(egui::RichText::new(info).size(12.0).color(p.text_faint));
    bar_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.button("Open in App").on_hover_text("Open with the app that opens this kind of file").clicked() {
            ask = Some(Ask::Launch);
        }
        if !matches!(view.doc, Doc::Failed(_) | Doc::Loading) {
            if ui.button("+").on_hover_text("Zoom in").clicked() {
                view.zoom = (view.zoom * 1.25).min(6.0);
            }
            ui.label(egui::RichText::new(format!("{:.0}%", view.zoom * 100.0)).size(12.0).color(p.text_muted));
            if ui.button("−").on_hover_text("Zoom out").clicked() {
                view.zoom = (view.zoom / 1.25).max(0.25);
            }
        }
        if matches!(view.doc, Doc::Text(_)) {
            if ui.button("Copy").on_hover_text("Copy the selected lines, or everything").clicked() {
                ask = view.copy_text().map(Ask::Copy);
            }
            let total = view.matches.len();
            if ui.add_enabled(total > 0, egui::Button::new("↓")).clicked() {
                view.step(false);
            }
            if ui.add_enabled(total > 0, egui::Button::new("↑")).clicked() {
                view.step(true);
            }
            if !view.find.is_empty() {
                let label = if total == 0 { "None".to_owned() } else { format!("{} of {total}", view.current + 1) };
                ui.label(egui::RichText::new(label).size(12.0).color(p.text_faint));
            }
            let field = ui.add(egui::TextEdit::singleline(&mut view.find).hint_text("Find").desired_width(180.0));
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                view.step(ui.input(|i| i.modifiers.shift));
                field.request_focus();
            }
            view.refind();
        }
    });
    ui.painter().hline(area.x_range(), bar.bottom(), Stroke::new(1.0, theme::with_alpha(p.text_faint, 40)));

    // --- the document ---
    let mut body_ui = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
    body_ui.set_clip_rect(body.intersect(ui.clip_rect()));
    let zoom = view.zoom;
    let reveal = view.reveal.take();
    match &mut view.doc {
        Doc::Loading => note(&body_ui, body, "Opening…", p.text_faint),
        Doc::Failed(why) => note(&body_ui, body, why, p.text_muted),
        Doc::Text(text) => {
            if let Some(a) = text_body(&mut body_ui, p, text, zoom, &view.matches, view.current, &mut view.selected, reveal) {
                ask = Some(a);
            }
        }
        Doc::Pdf(pdf) => pdf_body(&mut body_ui, p, pdf, zoom, &pages),
        Doc::Image(_) => {
            let size = match &view.doc {
                Doc::Image(img) => img.size,
                _ => unreachable!(),
            };
            if let Some(texture) = view.image_texture(ui.ctx(), path) {
                image_body(&mut body_ui, texture, size, zoom);
            }
        }
    }
    ask
}

fn note(ui: &Ui, rect: Rect, text: &str, colour: Color32) {
    ui.painter().text(rect.center() - vec2(0.0, 40.0), Align2::CENTER_CENTER, text, FontId::proportional(13.0), colour);
}

/// Text and code: lines numbered, coloured, found, and selected by the line.
#[allow(clippy::too_many_arguments)]
fn text_body(
    ui: &mut Ui,
    p: &Palette,
    text: &TextDoc,
    zoom: f32,
    matches: &[(usize, std::ops::Range<usize>)],
    current: usize,
    selected: &mut Option<(usize, usize)>,
    reveal: Option<usize>,
) -> Option<Ask> {
    let font = FontId::monospace(TEXT_SIZE * zoom);
    let (char_w, row_h) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let digits = text.lines.len().to_string().len() as f32;
    let gutter = digits * char_w + 28.0;
    let width = gutter + text.widest as f32 * char_w + 40.0;
    let rect = ui.max_rect();

    let mut scroll = egui::ScrollArea::both().auto_shrink([false; 2]).id_salt("viewer-text");
    if let Some(line) = reveal {
        scroll = scroll.vertical_scroll_offset((line as f32 * row_h - rect.height() / 3.0).max(0.0));
    }
    let palette_for = |t: Token| match t {
        Token::Plain => p.text_soft(),
        Token::Keyword => Color32::from_rgb(0xc6, 0x78, 0xdd),
        Token::String => Color32::from_rgb(0x98, 0xc3, 0x79),
        Token::Comment => p.text_faint,
        Token::Number => Color32::from_rgb(0xd1, 0x9a, 0x66),
        Token::Type => Color32::from_rgb(0xe5, 0xc0, 0x7b),
        Token::Function => Color32::from_rgb(0x61, 0xaf, 0xef),
    };
    let mut copy = None;
    scroll.show_rows(ui, row_h, text.lines.len(), |ui, rows| {
        ui.set_min_width(width);
        let left = ui.max_rect().left();
        let top = ui.max_rect().top();
        // The whole run of rows takes clicks and drags, for selecting lines.
        let span = Rect::from_min_size(pos2(left, top), vec2(width.max(ui.available_width()), rows.len() as f32 * row_h));
        let response = ui.interact(span, ui.id().with("lines"), Sense::click_and_drag());
        let line_at = |y: f32| rows.start + ((y - top) / row_h).max(0.0) as usize;
        if let Some(pos) = response.interact_pointer_pos() {
            let line = line_at(pos.y).min(text.lines.len() - 1);
            if response.drag_started() || (response.clicked() && !ui.input(|i| i.modifiers.shift)) {
                *selected = Some((line, line));
            } else if response.dragged() || (response.clicked() && ui.input(|i| i.modifiers.shift)) {
                if let Some((start, _)) = *selected {
                    *selected = Some((start, line));
                }
            }
        }
        if response.has_focus() || response.clicked() {
            response.request_focus();
            if ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
                copy = Some(());
            }
        }
        let sel = selected.map(|(a, b)| (a.min(b), a.max(b)));
        for (k, i) in rows.clone().enumerate() {
            let y = top + k as f32 * row_h;
            let line_rect = Rect::from_min_size(pos2(left, y), vec2(width.max(ui.available_width()), row_h));
            if sel.is_some_and(|(a, b)| (a..=b).contains(&i)) {
                ui.painter().rect_filled(line_rect, 0.0, p.selection);
            }
            ui.painter().text(
                pos2(left + gutter - 14.0, y),
                Align2::RIGHT_TOP,
                (i + 1).to_string(),
                font.clone(),
                theme::with_alpha(p.text_faint, 150),
            );
            let line = text.line(i);
            let x0 = left + gutter;
            // Matches behind the text; the current one stronger.
            let at = matches.partition_point(|(l, _)| *l < i);
            for (n, (_, range)) in matches[at..].iter().take_while(|(l, _)| *l == i).enumerate() {
                let from = line[..range.start].chars().count() as f32;
                let len = line[range.clone()].chars().count() as f32;
                let r = Rect::from_min_size(pos2(x0 + from * char_w, y), vec2(len * char_w, row_h));
                let strong = at + n == current;
                ui.painter().rect_filled(r, 2.0, theme::with_alpha(p.accent, if strong { 150 } else { 60 }));
            }
            let mut job = egui::text::LayoutJob::default();
            match text.language {
                Some(lang) => {
                    let (spans, _) = highlight::line(lang, line, text.carries[i]);
                    for (range, token) in spans {
                        job.append(&line[range], 0.0, egui::TextFormat::simple(font.clone(), palette_for(token)));
                    }
                }
                None => job.append(line, 0.0, egui::TextFormat::simple(font.clone(), p.text_soft())),
            }
            ui.painter().galley(pos2(x0, y), ui.fonts_mut(|f| f.layout_job(job)), p.text);
        }
    });
    copy.and_then(|()| {
        let (a, b) = (*selected)?;
        let (a, b) = (a.min(b), a.max(b));
        Some(Ask::Copy((a..=b).map(|i| text.line(i)).collect::<Vec<_>>().join("\n")))
    })
}

/// A PDF: its pages one above the next, drawn as they come into view, at the
/// width they are shown.
fn pdf_body(ui: &mut Ui, p: &Palette, pdf: &mut PdfDoc, zoom: f32, pages: &PageAsker<'_>) {
    let rect = ui.max_rect();
    let fit = (rect.width() - 48.0).clamp(200.0, 1100.0);
    let width = fit * zoom;
    let tops: Vec<f32> = pdf
        .pages
        .iter()
        .scan(PAGE_GAP, |y, [w, h]| {
            let top = *y;
            *y += width * h / w + PAGE_GAP;
            Some(top)
        })
        .collect();
    let total = tops.last().copied().unwrap_or(0.0)
        + pdf.pages.last().map_or(0.0, |[w, h]| width * h / w)
        + PAGE_GAP;
    let ppp = ui.ctx().pixels_per_point();
    // Drawn at a width rounded up to a step, so zooming does not draw every
    // page again for every few points.
    let pixels = (((width * ppp) / 256.0).ceil() * 256.0) as u32;
    egui::ScrollArea::both().auto_shrink([false; 2]).id_salt("viewer-pdf").show_viewport(ui, |ui, shown| {
        let full = (width + 48.0).max(rect.width());
        ui.set_min_size(vec2(full, total));
        let origin = ui.max_rect().min;
        let left = origin.x + (full - width) / 2.0;
        for (page, (top, [w, h])) in tops.iter().zip(pdf.pages.clone()).enumerate() {
            let height = width * h / w;
            if top + height < shown.top() - 400.0 || *top > shown.bottom() + 400.0 {
                continue;
            }
            let r = Rect::from_min_size(pos2(left, origin.y + top), vec2(width, height));
            ui.painter().rect_filled(r.translate(vec2(0.0, 2.0)), 3.0, Color32::from_black_alpha(60));
            pages.ask(pdf, page, pixels);
            // The page at this width, or at any width while it is drawn.
            let texture = pdf.drawn.get(&(page, pixels)).or_else(|| {
                pdf.drawn.iter().find(|((n, _), _)| *n == page).map(|(_, t)| t)
            });
            match texture {
                Some(t) => {
                    ui.painter().image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    ui.painter().rect_filled(r, 2.0, Color32::WHITE);
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, format!("Page {}", page + 1), FontId::proportional(12.0), Color32::GRAY);
                }
            }
        }
    });
    let _ = p;
}

/// An image: fitted to the pane, never enlarged past its size, then zoomed.
fn image_body(ui: &mut Ui, texture: &egui::TextureHandle, size: [usize; 2], zoom: f32) {
    let rect = ui.max_rect();
    let [w, h] = [size[0] as f32, size[1] as f32];
    let fit = ((rect.width() - 32.0) / w).min((rect.height() - 32.0) / h).min(1.0);
    let shown = vec2(w * fit * zoom, h * fit * zoom);
    egui::ScrollArea::both().auto_shrink([false; 2]).id_salt("viewer-image").show(ui, |ui| {
        let area = vec2(shown.x.max(rect.width()), shown.y.max(rect.height()));
        let (space, _) = ui.allocate_exact_size(area, Sense::hover());
        let r = Rect::from_center_size(space.center(), shown);
        ui.painter().image(texture.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    });
}
