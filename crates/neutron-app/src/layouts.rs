//! Saved multi-machine workspace layouts (`layouts.json`).

use std::path::PathBuf;

use egui::{Align2, Color32, CornerRadius, FontId, Stroke, StrokeKind, vec2};
use neutron_ui::theme::{self, Palette, micro_caps};
use serde::{Deserialize, Serialize};

use crate::workspace::{PersistedLocation, PersistedWorkspace};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedLayout {
    pub name: String,
    pub saved_at: String,
    pub workspace: PersistedWorkspace,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Layouts {
    #[serde(default)]
    pub items: Vec<SavedLayout>,
    #[serde(skip)]
    pub revision: u64,
}

impl Layouts {
    pub fn upsert(&mut self, name: String, workspace: PersistedWorkspace) {
        let trimmed = name.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        self.items.retain(|l| !l.name.eq_ignore_ascii_case(&trimmed));
        self.items.insert(
            0,
            SavedLayout {
                name: trimmed,
                saved_at: unix_stamp(),
                workspace,
            },
        );
        self.revision += 1;
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|l| l.name != name);
        if self.items.len() != before {
            self.revision += 1;
            true
        } else {
            false
        }
    }

    pub fn get(&self, name: &str) -> Option<&SavedLayout> {
        self.items.iter().find(|l| l.name == name)
    }
}

fn unix_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

pub fn describe_layout(ws: &PersistedWorkspace) -> String {
    let panes = ws.panes.len();
    let tabs: usize = ws.panes.iter().map(|p| p.tabs.len()).sum();
    let mut sites: Vec<String> = Vec::new();
    for pane in &ws.panes {
        for tab in &pane.tabs {
            if let PersistedLocation::Remote { site, .. } = tab
                && !sites.contains(site)
            {
                sites.push(site.clone());
            }
        }
    }
    let mut summary = format!(
        "{panes} pane{} · {tabs} tab{}",
        if panes == 1 { "" } else { "s" },
        if tabs == 1 { "" } else { "s" }
    );
    if !sites.is_empty() {
        summary.push_str(" · ");
        summary.push_str(&sites.join(", "));
    }
    summary
}

pub fn path() -> Option<PathBuf> {
    crate::pins::path().map(|p| p.with_file_name("layouts.json"))
}

pub fn load() -> Layouts {
    let Some(file) = path() else {
        return Layouts::default();
    };
    match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Layouts::default(),
    }
}

pub fn save(layouts: &Layouts) -> Result<PathBuf, String> {
    let Some(file) = path() else {
        return Err(format!("there is no {} to save layouts in", crate::persist::settings_folder_name()));
    };
    let json = serde_json::to_string_pretty(layouts).map_err(|e| e.to_string())?;
    crate::persist::write(&file, &json, layouts.revision)?;
    Ok(file)
}

#[derive(Default)]
pub struct LayoutDialog {
    pub open: bool,
    pub name_input: String,
    pub focus_input: bool,
}

pub enum LayoutAction {
    Close,
    Save(String),
    Apply(String),
    Delete(String),
}

impl LayoutDialog {
    pub fn show(&mut self, ctx: &egui::Context, p: &Palette, layouts: &Layouts) -> Option<LayoutAction> {
        if !self.open {
            return None;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            return Some(LayoutAction::Close);
        }

        let mut action = None;
        let screen = ctx.content_rect();
        egui::Area::new(egui::Id::new("neutron-layouts-scrim"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
                ui.painter().rect_filled(rect, CornerRadius::ZERO, Color32::from_black_alpha(165));
                if resp.clicked() {
                    action = Some(LayoutAction::Close);
                }
            });

        let w = (screen.width() - 64.0).clamp(340.0, 560.0);
        let h = (screen.height() - 64.0).clamp(280.0, 440.0);

        egui::Area::new(egui::Id::new("neutron-layouts-card"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(p.card)
                    .corner_radius(CornerRadius::same(theme::RADIUS_CARD))
                    .stroke(Stroke::new(1.0, p.border_strong))
                    .inner_margin(egui::Margin::same(18))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 18],
                        blur: 48,
                        spread: 0,
                        color: Color32::from_black_alpha(200),
                    })
                    .show(ui, |ui| {
                        ui.set_width(w);
                        ui.set_height(h);

                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new(micro_caps("Workspaces"))
                                        .font(theme::strong(ui.ctx(), 10.0))
                                        .color(p.text_faint),
                                );
                                ui.label(
                                    egui::RichText::new("Workspace Layouts")
                                        .font(theme::strong(ui.ctx(), 16.0))
                                        .color(p.text),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if pill_btn(ui, p, "Close", false) {
                                    action = Some(LayoutAction::Close);
                                }
                            });
                        });

                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            let field = ui.add(
                                egui::TextEdit::singleline(&mut self.name_input)
                                    .hint_text("Layout name (e.g. GPU Box, Mac mini Deploy)")
                                    .desired_width(w - 130.0),
                            );
                            if self.focus_input {
                                self.focus_input = false;
                                field.request_focus();
                            }
                            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if (pill_btn(ui, p, "Save Current", true) || enter) && !self.name_input.trim().is_empty() {
                                let name = std::mem::take(&mut self.name_input);
                                action = Some(LayoutAction::Save(name));
                            }
                        });

                        ui.add_space(14.0);
                        if layouts.items.is_empty() {
                            ui.add_space(40.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("No saved workspace layouts yet.")
                                        .font(FontId::proportional(13.0))
                                        .color(p.text_faint),
                                );
                            });
                            return;
                        }

                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for saved in &layouts.items {
                                    let frame = egui::Frame::new()
                                        .fill(p.raised)
                                        .corner_radius(CornerRadius::same(theme::RADIUS_CONTROL))
                                        .stroke(Stroke::new(1.0, p.border))
                                        .inner_margin(egui::Margin::symmetric(12, 9));
                                    frame.show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    egui::RichText::new(&saved.name)
                                                        .font(theme::strong(ui.ctx(), 13.5))
                                                        .color(p.text),
                                                );
                                                ui.label(
                                                    egui::RichText::new(describe_layout(&saved.workspace))
                                                        .font(FontId::proportional(11.5))
                                                        .color(p.text_muted),
                                                );
                                            });
                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                if pill_btn(ui, p, "Delete", false) {
                                                    action = Some(LayoutAction::Delete(saved.name.clone()));
                                                }
                                                if pill_btn(ui, p, "Switch", true) {
                                                    action = Some(LayoutAction::Apply(saved.name.clone()));
                                                }
                                            });
                                        });
                                    });
                                    ui.add_space(8.0);
                                }
                            });
                    });
            });

        action
    }
}

fn pill_btn(ui: &mut egui::Ui, p: &Palette, label: &str, accent: bool) -> bool {
    let text_col = if accent { Color32::WHITE } else { p.text_muted };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(12.0), text_col);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 20.0, 26.0), egui::Sense::click());
    let fill = if accent {
        if resp.hovered() { p.accent } else { p.accent.gamma_multiply(0.85) }
    } else if resp.hovered() {
        p.hover
    } else {
        p.card
    };
    ui.painter().rect_filled(rect, CornerRadius::same(theme::RADIUS_CONTROL), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(theme::RADIUS_CONTROL),
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        egui::pos2(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0),
        galley,
        text_col,
    );
    resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{PersistedPane, PersistedPrefs};

    fn sample_ws() -> PersistedWorkspace {
        PersistedWorkspace {
            panes: vec![
                PersistedPane {
                    tabs: vec![PersistedLocation::Remote {
                        site: "kagutsuchi".into(),
                        path: "/Users/razeen/proj".into(),
                    }],
                    active: 0,
                },
                PersistedPane {
                    tabs: vec![PersistedLocation::Home],
                    active: 0,
                },
            ],
            splits: Vec::new(),
            focused: 0,
            preview: false,
            prefs: PersistedPrefs::default(),
        }
    }

    #[test]
    fn upsert_replaces_existing_and_moves_to_top() {
        let mut ls = Layouts::default();
        ls.upsert("Work".into(), sample_ws());
        ls.upsert("Deploy".into(), sample_ws());
        ls.upsert("work".into(), sample_ws());
        assert_eq!(ls.items.len(), 2);
        assert_eq!(ls.items[0].name, "work");
        assert_eq!(ls.items[1].name, "Deploy");
    }

    #[test]
    fn round_trip_json_preserves_panes_and_remote_tabs() {
        let mut ls = Layouts::default();
        ls.upsert("Cluster".into(), sample_ws());
        let json = serde_json::to_string(&ls).unwrap();
        let back: Layouts = serde_json::from_str(&json).unwrap();
        assert_eq!(back.items.len(), 1);
        assert_eq!(back.items[0].name, "Cluster");
        assert_eq!(back.items[0].workspace.panes.len(), 2);
    }

    #[test]
    fn describe_layout_counts_panes_tabs_and_servers() {
        let desc = describe_layout(&sample_ws());
        assert_eq!(desc, "2 panes · 2 tabs · kagutsuchi");
    }
}
