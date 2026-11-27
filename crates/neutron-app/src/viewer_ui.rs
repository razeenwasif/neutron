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
        Doc::Markdown(md) => {
            let lines = md.raw.lines.len();
            format!("Markdown · {lines} line{}", if lines == 1 { "" } else { "s" })
        }
        Doc::Table(tbl) => format!(
            "{} · {} rows × {} cols{}",
            tbl.format_label,
            tbl.total_rows,
            tbl.columns.len(),
            if tbl.truncated { " (capped)" } else { "" }
        ),
        Doc::Tensor(ts) => format!(
            "{} · {} params · {} tensors",
            ts.format,
            crate::viewer_formats::format_params(ts.total_params),
            ts.tensors.len()
        ),
        Doc::Pdf(pdf) => format!("PDF · {} page{}", pdf.pages.len(), if pdf.pages.len() == 1 { "" } else { "s" }),
        Doc::Image(img) => format!("Image · {} × {}", img.source[0], img.source[1]),
        Doc::Failed(_) => String::new(),
    };
    bar_ui.label(egui::RichText::new(info).size(12.0).color(p.text_faint));
    bar_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.button("Open in App").on_hover_text("Open with the app that opens this kind of file").clicked() {
            ask = Some(Ask::Launch);
        }
        if matches!(view.doc, Doc::Markdown(_)) {
            let lbl = if view.markdown_raw { "Rendered" } else { "Raw" };
            if ui.button(lbl).on_hover_text("Toggle between rendered Markdown and raw source").clicked() {
                view.markdown_raw = !view.markdown_raw;
            }
        }
        if matches!(view.doc, Doc::Text(_) | Doc::Markdown(_) | Doc::Pdf(_) | Doc::Image(_)) {
            if ui.button("+").on_hover_text("Zoom in").clicked() {
                view.zoom = (view.zoom * 1.25).min(6.0);
            }
            ui.label(egui::RichText::new(format!("{:.0}%", view.zoom * 100.0)).size(12.0).color(p.text_muted));
            if ui.button("−").on_hover_text("Zoom out").clicked() {
                view.zoom = (view.zoom / 1.25).max(0.25);
            }
        }
        if matches!(view.doc, Doc::Tensor(_)) {
            ui.add(egui::TextEdit::singleline(&mut view.find).hint_text("Filter tensors…").desired_width(190.0));
        }
        if matches!(view.doc, Doc::Text(_)) || (matches!(view.doc, Doc::Markdown(_)) && view.markdown_raw) {
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
        Doc::Markdown(md) => {
            if view.markdown_raw {
                if let Some(a) = text_body(&mut body_ui, p, &md.raw, zoom, &view.matches, view.current, &mut view.selected, reveal) {
                    ask = Some(a);
                }
            } else {
                markdown_body(&mut body_ui, p, md, zoom);
            }
        }
        Doc::Table(tbl) => {
            if let Some(a) = table_body(&mut body_ui, p, tbl) {
                ask = Some(a);
            }
        }
        Doc::Tensor(ts) => {
            if let Some(a) = tensor_body(&mut body_ui, p, ts, &view.find) {
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


fn markdown_body(ui: &mut Ui, p: &Palette, md: &crate::viewer_formats::MarkdownDoc, zoom: f32) {
    use crate::viewer_formats::MdBlock;
    egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
        ui.add_space(18.0);
        let max_w = (ui.available_width() - 48.0).clamp(280.0, 820.0);
        let margin = ((ui.available_width() - max_w) / 2.0).max(20.0);
        ui.horizontal(|ui| {
            ui.add_space(margin);
            ui.vertical(|ui| {
                ui.set_width(max_w);
                for block in &md.blocks {
                    match block {
                        MdBlock::Heading { level, text } => {
                            let sz = match level {
                                1 => 24.0,
                                2 => 19.0,
                                3 => 16.0,
                                _ => 14.0,
                            } * zoom;
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new(text).font(theme::strong(ui.ctx(), sz)).color(p.text));
                            if *level <= 2 {
                                let r = ui.min_rect();
                                ui.painter().hline(r.x_range(), r.bottom() + 2.0, Stroke::new(1.0, p.border));
                                ui.add_space(4.0);
                            }
                            ui.add_space(4.0);
                        }
                        MdBlock::Paragraph(text) => {
                            ui.label(egui::RichText::new(text).size(13.5 * zoom).color(p.text_soft()));
                            ui.add_space(8.0);
                        }
                        MdBlock::Bullet { indent, text } => {
                            ui.horizontal(|ui| {
                                ui.add_space(12.0 + (*indent as f32) * 16.0);
                                ui.label(egui::RichText::new("•").size(13.5 * zoom).color(p.accent));
                                ui.add(egui::Label::new(egui::RichText::new(text).size(13.5 * zoom).color(p.text_soft())).wrap());
                            });
                            ui.add_space(3.0);
                        }
                        MdBlock::Quote(text) => {
                            let resp = egui::Frame::new()
                                .fill(p.raised)
                                .inner_margin(egui::Margin::symmetric(12, 8))
                                .show(ui, |ui| {
                                    ui.add(egui::Label::new(egui::RichText::new(text).italics().size(13.0 * zoom).color(p.text_muted)).wrap());
                                })
                                .response;
                            let bar = Rect::from_min_max(resp.rect.min, pos2(resp.rect.left() + 3.0, resp.rect.bottom()));
                            ui.painter().rect_filled(bar, 0.0, p.accent);
                            ui.add_space(6.0);
                        }
                        MdBlock::CodeBlock { lang, code } => {
                            egui::Frame::new()
                                .fill(p.raised)
                                .corner_radius(egui::CornerRadius::same(theme::RADIUS_CONTROL))
                                .stroke(Stroke::new(1.0, p.border))
                                .inner_margin(egui::Margin::same(10))
                                .show(ui, |ui| {
                                    ui.set_width(max_w);
                                    if !lang.is_empty() {
                                        ui.label(egui::RichText::new(lang).size(10.5).color(p.text_faint));
                                    }
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(code)
                                                .font(FontId::monospace(12.0 * zoom))
                                                .color(p.text),
                                        )
                                        .wrap(),
                                    );
                                });
                            ui.add_space(8.0);
                        }
                        MdBlock::Rule => {
                            ui.add_space(4.0);
                            let (r, _) = ui.allocate_exact_size(vec2(max_w, 2.0), Sense::hover());
                            ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, p.border));
                            ui.add_space(8.0);
                        }
                    }
                }
            });
        });
        ui.add_space(24.0);
    });
}

fn table_body(ui: &mut Ui, p: &Palette, tbl: &mut crate::viewer_formats::TableDoc) -> Option<Ask> {
    let mut ask = None;
    if !tbl.meta.is_empty() {
        egui::Frame::new()
            .fill(p.raised)
            .inner_margin(egui::Margin::symmetric(14, 8))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (k, v) in &tbl.meta {
                        ui.label(egui::RichText::new(format!("{k}:")).size(11.5).color(p.text_faint));
                        ui.label(egui::RichText::new(v).size(11.5).color(p.text));
                        ui.add_space(12.0);
                    }
                });
            });
    }

    if tbl.columns.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(egui::RichText::new("No rows to display").color(p.text_faint));
        });
        return None;
    }

    let col_w = 160.0_f32;
    let total_w = (tbl.columns.len() as f32 * col_w).max(ui.available_width());
    let row_h = 26.0_f32;

    egui::ScrollArea::horizontal().auto_shrink([false; 2]).show(ui, |ui| {
        ui.set_min_width(total_w);
        // Header strip
        let (hdr_rect, _) = ui.allocate_exact_size(vec2(total_w, 28.0), Sense::hover());
        ui.painter().rect_filled(hdr_rect, 0.0, p.elevated);
        let mut click_col = None;
        for (c, col_name) in tbl.columns.iter().enumerate() {
            let cell = Rect::from_min_size(pos2(hdr_rect.left() + c as f32 * col_w, hdr_rect.top()), vec2(col_w, 28.0));
            let resp = ui.interact(cell, ui.id().with(("tbl-hdr", c)), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(cell, 0.0, p.hover);
            }
            let arrow = match tbl.sort_col {
                Some((sc, true)) if sc == c => " ▲",
                Some((sc, false)) if sc == c => " ▼",
                _ => "",
            };
            let clipped = ui.painter().with_clip_rect(cell.shrink2(vec2(8.0, 2.0)));
            clipped.text(
                pos2(cell.left() + 8.0, cell.center().y),
                Align2::LEFT_CENTER,
                format!("{col_name}{arrow}"),
                theme::strong(ui.ctx(), 11.5),
                p.text,
            );
            if resp.clicked() {
                click_col = Some(c);
            }
        }
        if let Some(c) = click_col {
            tbl.sort_by(c);
        }

        // Virtualized rows
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show_rows(ui, row_h, tbl.order.len(), |ui, range| {
            for idx in range {
                let (r_rect, _) = ui.allocate_exact_size(vec2(total_w, row_h), Sense::hover());
                if idx % 2 == 1 {
                    ui.painter().rect_filled(r_rect, 0.0, theme::with_alpha(p.raised, 90));
                }
                if let Some(row) = tbl.ordered_row(idx) {
                    for (c, val) in row.iter().enumerate() {
                        let cell = Rect::from_min_size(pos2(r_rect.left() + c as f32 * col_w, r_rect.top()), vec2(col_w, row_h));
                        let resp = ui.interact(cell, ui.id().with(("tbl-cell", idx, c)), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(cell, 0.0, p.hover);
                        }
                        if resp.clicked() {
                            ask = Some(Ask::Copy(val.clone()));
                        }
                        let clipped = ui.painter().with_clip_rect(cell.shrink2(vec2(8.0, 2.0)));
                        clipped.text(
                            pos2(cell.left() + 8.0, cell.center().y),
                            Align2::LEFT_CENTER,
                            val,
                            FontId::monospace(11.5),
                            p.text_soft(),
                        );
                    }
                }
            }
        });
    });
    ask
}

fn tensor_body(ui: &mut Ui, p: &Palette, ts: &crate::viewer_formats::TensorDoc, filter: &str) -> Option<Ask> {
    let mut ask = None;
    let needle = filter.trim().to_ascii_lowercase();
    let filtered: Vec<&crate::viewer_formats::TensorInfo> = ts
        .tensors
        .iter()
        .filter(|t| needle.is_empty() || t.name.to_ascii_lowercase().contains(&needle) || t.dtype.to_ascii_lowercase().contains(&needle))
        .collect();

    egui::Frame::new()
        .fill(p.raised)
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (k, v) in [
                    ("Format", ts.format.to_owned()),
                    ("File Size", neutron_ui::format::size(Some(ts.file_size))),
                    ("Parameters", crate::viewer_formats::format_params(ts.total_params)),
                    ("Tensors", ts.tensors.len().to_string()),
                ] {
                    ui.label(egui::RichText::new(format!("{k}:")).size(11.5).color(p.text_faint));
                    ui.label(egui::RichText::new(v).font(theme::strong(ui.ctx(), 12.0)).color(p.text));
                    ui.add_space(12.0);
                }
                for (dtype, cnt, params) in &ts.dtype_counts {
                    ui.label(
                        egui::RichText::new(format!(
                            "{dtype}: {cnt} ({})",
                            crate::viewer_formats::format_params(*params)
                        ))
                        .size(11.0)
                        .color(p.cloud),
                    );
                    ui.add_space(8.0);
                }
            });
            if !ts.metadata.is_empty() {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    for (mk, mv) in ts.metadata.iter().take(8) {
                        ui.label(egui::RichText::new(format!("{mk}={mv}")).size(11.0).color(p.text_muted));
                        ui.add_space(10.0);
                    }
                });
            }
        });

    let row_h = 26.0_f32;
    let w = ui.available_width();
    let (hdr, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
    ui.painter().rect_filled(hdr, 0.0, p.elevated);
    ui.painter().text(pos2(hdr.left() + 14.0, hdr.center().y), Align2::LEFT_CENTER, "Tensor Name (click to copy)", theme::strong(ui.ctx(), 11.0), p.text_faint);
    ui.painter().text(pos2(hdr.right() - 280.0, hdr.center().y), Align2::LEFT_CENTER, "DType", theme::strong(ui.ctx(), 11.0), p.text_faint);
    ui.painter().text(pos2(hdr.right() - 205.0, hdr.center().y), Align2::LEFT_CENTER, "Shape", theme::strong(ui.ctx(), 11.0), p.text_faint);
    ui.painter().text(pos2(hdr.right() - 80.0, hdr.center().y), Align2::RIGHT_CENTER, "Params", theme::strong(ui.ctx(), 11.0), p.text_faint);
    ui.painter().text(pos2(hdr.right() - 14.0, hdr.center().y), Align2::RIGHT_CENTER, "Bytes", theme::strong(ui.ctx(), 11.0), p.text_faint);

    egui::ScrollArea::vertical().auto_shrink([false; 2]).show_rows(ui, row_h, filtered.len(), |ui, range| {
        for idx in range {
            let t = filtered[idx];
            let (rect, resp) = ui.allocate_exact_size(vec2(w, row_h), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, 0.0, p.hover);
            } else if idx % 2 == 1 {
                ui.painter().rect_filled(rect, 0.0, theme::with_alpha(p.raised, 70));
            }
            if resp.clicked() {
                ask = Some(Ask::Copy(t.name.clone()));
            }
            let shape_str = if t.shape.is_empty() {
                "[]".to_owned()
            } else {
                format!("[{}]", t.shape.iter().map(u64::to_string).collect::<Vec<_>>().join("×"))
            };
            let name_rect = Rect::from_min_max(pos2(rect.left() + 14.0, rect.top()), pos2((rect.right() - 290.0).max(rect.left() + 80.0), rect.bottom()));
            let clipped = ui.painter().with_clip_rect(name_rect);
            clipped.text(pos2(name_rect.left(), rect.center().y), Align2::LEFT_CENTER, &t.name, FontId::monospace(11.5), p.text);
            ui.painter().text(pos2(rect.right() - 280.0, rect.center().y), Align2::LEFT_CENTER, &t.dtype, FontId::monospace(11.5), p.cloud);
            ui.painter().text(pos2(rect.right() - 205.0, rect.center().y), Align2::LEFT_CENTER, shape_str, FontId::monospace(11.5), p.text_muted);
            ui.painter().text(pos2(rect.right() - 80.0, rect.center().y), Align2::RIGHT_CENTER, crate::viewer_formats::format_params(t.params), FontId::monospace(11.5), p.text_soft());
            ui.painter().text(pos2(rect.right() - 14.0, rect.center().y), Align2::RIGHT_CENTER, neutron_ui::format::size(Some(t.byte_size)), FontId::monospace(11.5), p.text_faint);
        }
    });
    ask
}
