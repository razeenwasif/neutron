//! A terminal's screen, drawn, and the keys and mouse sent to it.
//!
//! Draws a `vt100` screen as a grid of monospaced cells and turns what the
//! user does into the bytes a terminal would send: keys, pastes, the wheel.
//! It owns no process — the app's session does — so it is told the screen and
//! reports what to write, how big the grid is, and what was copied.

use egui::{Align2, Color32, FontId, Id, Key, Modifiers, Rect, Sense, Stroke, Ui, pos2, vec2};

use crate::theme::{self, Palette};

/// Points of space around the grid.
const PAD: f32 = 10.0;
const FONT_SIZE: f32 = 13.0;

/// What happened to the terminal this frame.
#[derive(Debug, Default)]
pub struct TermOutput {
    /// Bytes to send to the program.
    pub input: Vec<u8>,
    /// Rows and columns that fit, for resizing the program to.
    pub grid: (u16, u16),
    /// Lines to scroll back (negative: forwards).
    pub scroll: i32,
    /// Text the user copied.
    pub copied: Option<String>,
}

/// Cell coordinates, row then column, on the visible screen.
type Cell = (u16, u16);

/// Draws `screen` filling `ui`'s space and takes the user's input to it.
/// `focus` asks for the keyboard, as when the tab was just opened.
pub fn show(ui: &mut Ui, p: &Palette, id: Id, screen: &vt100::Screen, exited: bool, focus: bool) -> TermOutput {
    let mut out = TermOutput::default();
    let rect = ui.available_rect_before_wrap();
    let response = ui.interact(rect, id, Sense::click_and_drag());
    ui.painter().rect_filled(rect, theme::RADIUS_PANEL as f32, p.inset);

    let font = FontId::monospace(FONT_SIZE);
    let (cell_w, cell_h) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let grid = rect.shrink(PAD);
    let cols = ((grid.width() / cell_w).floor() as u16).max(10);
    let rows = ((grid.height() / cell_h).floor() as u16).max(2);
    out.grid = (rows, cols);

    if focus || response.clicked() || response.drag_started() {
        response.request_focus();
    }
    let focused = response.has_focus();
    if focused {
        // Every key is the program's: Tab, the arrows and Escape included.
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            )
        });
    }

    // --- selection, in cells ---
    let cell_at = |pos: egui::Pos2| -> Cell {
        let col = ((pos.x - grid.left()) / cell_w).floor().clamp(0.0, (cols - 1) as f32) as u16;
        let row = ((pos.y - grid.top()) / cell_h).floor().clamp(0.0, (rows - 1) as f32) as u16;
        (row, col)
    };
    let sel_id = id.with("selection");
    let mut selection: Option<(Cell, Cell)> = ui.data(|d| d.get_temp(sel_id));
    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            selection = Some((cell_at(pos), cell_at(pos)));
        }
    } else if response.dragged() {
        if let (Some(pos), Some((start, _))) = (response.interact_pointer_pos(), selection) {
            selection = Some((start, cell_at(pos)));
        }
    } else if response.clicked() {
        selection = None;
    }
    ui.data_mut(|d| match selection {
        Some(s) => {
            d.insert_temp(sel_id, s);
        }
        None => d.remove::<(Cell, Cell)>(sel_id),
    });
    let ordered = selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });

    // --- the cells ---
    let (screen_rows, screen_cols) = screen.size();
    let painter = ui.painter_at(rect);
    for row in 0..screen_rows.min(rows) {
        let y = grid.top() + row as f32 * cell_h;
        let mut col = 0;
        while col < screen_cols.min(cols) {
            let Some(cell) = screen.cell(row, col) else { break };
            let (fg, bg) = colours(cell, p);
            let x = grid.left() + col as f32 * cell_w;
            let width = if cell.is_wide() { 2.0 } else { 1.0 };
            let selected = ordered.is_some_and(|((r0, c0), (r1, c1))| {
                (row > r0 || (row == r0 && col >= c0)) && (row < r1 || (row == r1 && col <= c1))
            });
            let cell_rect = Rect::from_min_size(pos2(x, y), vec2(cell_w * width, cell_h));
            if selected {
                painter.rect_filled(cell_rect, 0.0, p.selection);
            } else if let Some(bg) = bg {
                painter.rect_filled(cell_rect, 0.0, bg);
            }
            let text = cell.contents();
            if !text.is_empty() && text != " " {
                painter.text(pos2(x, y), Align2::LEFT_TOP, text, font.clone(), fg);
            }
            if cell.underline() {
                painter.hline(x..=x + cell_w * width, y + cell_h - 1.5, Stroke::new(1.0, fg));
            }
            col += if cell.is_wide() { 2 } else { 1 };
        }
    }

    // --- cursor ---
    if !screen.hide_cursor() && !exited && screen.scrollback() == 0 {
        let (row, col) = screen.cursor_position();
        if row < rows && col < cols {
            let r = Rect::from_min_size(
                pos2(grid.left() + col as f32 * cell_w, grid.top() + row as f32 * cell_h),
                vec2(cell_w, cell_h),
            );
            if focused {
                painter.rect_filled(r, 1.0, theme::with_alpha(p.accent, 170));
                if let Some(cell) = screen.cell(row, col) {
                    let text = cell.contents();
                    if !text.is_empty() {
                        painter.text(r.min, Align2::LEFT_TOP, text, font.clone(), p.inset);
                    }
                }
            } else {
                painter.rect_stroke(r, 1.0, Stroke::new(1.0, p.accent), egui::StrokeKind::Inside);
            }
        }
    }
    if exited {
        painter.text(
            pos2(rect.center().x, rect.bottom() - PAD - 4.0),
            Align2::CENTER_BOTTOM,
            "The process has ended — close the tab, or press Return to close it",
            FontId::proportional(12.0),
            p.text_faint,
        );
    }

    // --- the wheel: scrollback, or the program's own scrolling ---
    if response.hovered() {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        let acc_id = id.with("wheel");
        let mut acc: f32 = ui.data(|d| d.get_temp(acc_id)).unwrap_or(0.0) + dy;
        let lines = (acc / cell_h).trunc() as i32;
        acc -= lines as f32 * cell_h;
        ui.data_mut(|d| d.insert_temp(acc_id, acc));
        if lines != 0 {
            if screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None {
                let (row, col) = ui.input(|i| i.pointer.hover_pos()).map_or((0, 0), cell_at);
                let button = if lines > 0 { 64 } else { 65 };
                for _ in 0..lines.unsigned_abs() {
                    out.input.extend_from_slice(format!("\x1b[<{button};{};{}M", col + 1, row + 1).as_bytes());
                }
            } else if screen.alternate_screen() {
                // A full-screen program with no mouse: the wheel as arrows,
                // as Terminal does.
                let key = if lines > 0 { arrow(b'A', screen.application_cursor()) } else { arrow(b'B', screen.application_cursor()) };
                for _ in 0..lines.unsigned_abs() {
                    out.input.extend_from_slice(&key);
                }
            } else {
                out.scroll = lines;
            }
        }
    }

    // --- keys ---
    if focused {
        let events = ui.input(|i| i.events.clone());
        for event in events {
            match event {
                egui::Event::Text(text) => out.input.extend_from_slice(text.as_bytes()),
                egui::Event::Paste(text) => {
                    let text = text.replace("\r\n", "\r").replace('\n', "\r");
                    if screen.bracketed_paste() {
                        out.input.extend_from_slice(b"\x1b[200~");
                        out.input.extend_from_slice(text.as_bytes());
                        out.input.extend_from_slice(b"\x1b[201~");
                    } else {
                        out.input.extend_from_slice(text.as_bytes());
                    }
                }
                egui::Event::Copy | egui::Event::Cut => {
                    if let Some(((r0, c0), (r1, c1))) = ordered {
                        out.copied = Some(screen.contents_between(r0, c0, r1, c1 + 1));
                    }
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    if let Some(bytes) = key_bytes(key, modifiers, screen.application_cursor()) {
                        out.input.extend_from_slice(&bytes);
                    }
                }
                _ => {}
            }
        }
        if !out.input.is_empty() {
            ui.data_mut(|d| d.remove::<(Cell, Cell)>(sel_id));
        }
    }
    out
}

fn arrow(letter: u8, application: bool) -> Vec<u8> {
    if application { vec![0x1b, b'O', letter] } else { vec![0x1b, b'[', letter] }
}

/// The bytes a terminal sends for a key that is not typed text: Return, the
/// arrows, Control with a letter. `None` for a key that sends nothing, or that
/// arrives as text instead, or that is the application's — anything with
/// Command held.
pub fn key_bytes(key: Key, mods: Modifiers, application_cursor: bool) -> Option<Vec<u8>> {
    if mods.mac_cmd || (mods.command && !mods.ctrl) {
        return None;
    }
    if mods.ctrl {
        let name = key.name();
        if name.len() == 1 {
            let c = name.as_bytes()[0].to_ascii_lowercase();
            if c.is_ascii_lowercase() {
                return Some(vec![c - b'a' + 1]);
            }
        }
        return match key {
            Key::OpenBracket => Some(vec![0x1b]),
            Key::Backslash => Some(vec![0x1c]),
            Key::CloseBracket => Some(vec![0x1d]),
            Key::Space => Some(vec![0x00]),
            _ => None,
        };
    }
    let seq: &[u8] = match key {
        Key::Enter => b"\r",
        Key::Tab if mods.shift => b"\x1b[Z",
        Key::Tab => b"\t",
        Key::Backspace if mods.alt => b"\x1b\x7f",
        Key::Backspace => b"\x7f",
        Key::Escape => b"\x1b",
        Key::ArrowUp => return Some(arrow(b'A', application_cursor)),
        Key::ArrowDown => return Some(arrow(b'B', application_cursor)),
        // Option with the arrows moves by word, as in Terminal.
        Key::ArrowRight if mods.alt => b"\x1bf",
        Key::ArrowLeft if mods.alt => b"\x1bb",
        Key::ArrowRight => return Some(arrow(b'C', application_cursor)),
        Key::ArrowLeft => return Some(arrow(b'D', application_cursor)),
        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        Key::Delete => b"\x1b[3~",
        Key::Insert => b"\x1b[2~",
        _ => return None,
    };
    Some(seq.to_vec())
}

/// A cell's text and background colours; `None` for the default background.
fn colours(cell: &vt100::Cell, p: &Palette) -> (Color32, Option<Color32>) {
    let mut fg = match cell.fgcolor() {
        vt100::Color::Default => p.text,
        // Bold brightens the first eight, as terminals have always done.
        vt100::Color::Idx(i) if cell.bold() && i < 8 => indexed(i + 8),
        vt100::Color::Idx(i) => indexed(i),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    };
    let mut bg = match cell.bgcolor() {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color32::from_rgb(r, g, b)),
    };
    if cell.inverse() {
        let was = fg;
        fg = bg.unwrap_or(p.inset);
        bg = Some(was);
    }
    if cell.dim() {
        fg = theme::with_alpha(fg, 150);
    }
    (fg, bg)
}

/// The 256-colour palette: sixteen named colours chosen to sit on Neutron's
/// ground, then the standard cube and greys.
fn indexed(i: u8) -> Color32 {
    const NAMED: [(u8, u8, u8); 16] = [
        (0x3b, 0x3f, 0x4c),
        (0xe0, 0x6c, 0x75),
        (0x98, 0xc3, 0x79),
        (0xe5, 0xc0, 0x7b),
        (0x61, 0xaf, 0xef),
        (0xc6, 0x78, 0xdd),
        (0x56, 0xb6, 0xc2),
        (0xab, 0xb2, 0xbf),
        (0x5c, 0x63, 0x70),
        (0xef, 0x7b, 0x84),
        (0xa8, 0xd5, 0x8a),
        (0xf0, 0xcf, 0x8f),
        (0x7a, 0xc0, 0xf5),
        (0xd4, 0x8f, 0xe8),
        (0x6f, 0xc8, 0xd3),
        (0xe6, 0xe9, 0xef),
    ];
    match i {
        0..=15 => {
            let (r, g, b) = NAMED[i as usize];
            Color32::from_rgb(r, g, b)
        }
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_send_what_a_terminal_sends() {
        let none = Modifiers::NONE;
        assert_eq!(key_bytes(Key::Enter, none, false), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(Key::ArrowUp, none, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(Key::ArrowUp, none, true), Some(b"\x1bOA".to_vec()), "application cursor keys");
        assert_eq!(key_bytes(Key::C, Modifiers::CTRL, false), Some(vec![3]), "Control-C");
        assert_eq!(key_bytes(Key::Tab, Modifiers::SHIFT, false), Some(b"\x1b[Z".to_vec()));
        assert_eq!(key_bytes(Key::A, none, false), None, "arrives as text");
        assert_eq!(key_bytes(Key::W, Modifiers::MAC_CMD, false), None, "Command is Neutron's");
    }

    #[test]
    fn the_palette_has_the_cube_and_the_greys() {
        assert_eq!(indexed(16), Color32::from_rgb(0, 0, 0));
        assert_eq!(indexed(231), Color32::from_rgb(255, 255, 255));
        assert_eq!(indexed(232), Color32::from_rgb(8, 8, 8));
    }
}
