//! Saved searches: Smart Folders kept in the sidebar, in `searches.json` in
//! Neutron's Application Support folder.

use std::path::PathBuf;

use neutron_core::query::{Kind, Query, Within};
use neutron_ui::theme::{self, Palette};

/// One saved search.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Saved {
    pub name: String,
    /// A [`Query`], written down.
    pub query: String,
}

impl Saved {
    pub fn location(&self) -> neutron_core::NodeId {
        neutron_core::NodeId::Search { name: self.name.as_str().into(), query: self.query.as_str().into() }
    }
}

fn path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/Neutron/searches.json"))
}

/// The saved searches; none if there is no file, or one that will not read.
pub fn load() -> Vec<Saved> {
    path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Writes them down. Blocks on the disk: call it from a worker.
pub fn save(saved: &[Saved]) -> Result<(), String> {
    let file = path().ok_or("there is no home folder")?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(saved).map_err(|e| e.to_string())?;
    std::fs::write(file, text).map_err(|e| e.to_string())
}

/// Where a search looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Everywhere,
    Home,
    Folder,
}

/// The New Smart Folder dialog.
pub struct Dialog {
    pub name: String,
    pub text: String,
    pub contents: bool,
    pub kind: Option<Kind>,
    pub within: Option<Within>,
    pub tag: String,
    pub scope: Scope,
    /// The folder "This folder" means.
    pub folder: Option<PathBuf>,
    /// The saved search being edited, by its name.
    pub editing: Option<String>,
    focused: bool,
}

/// What the dialog was answered with.
pub enum Answer {
    Cancel,
    /// Show what it finds; and keep it in the sidebar under a name, if given.
    Search { query: Query, save_as: Option<String>, replacing: Option<String> },
}

impl Dialog {
    pub fn new(folder: Option<PathBuf>) -> Self {
        Dialog {
            name: String::new(),
            text: String::new(),
            contents: false,
            kind: None,
            within: None,
            tag: String::new(),
            scope: if folder.is_some() { Scope::Folder } else { Scope::Everywhere },
            folder,
            editing: None,
            focused: false,
        }
    }

    /// The dialog filled in from a search, for changing it.
    pub fn editing(name: &str, query: &Query) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let scope = match &query.scope {
            None => Scope::Everywhere,
            Some(s) if Some(s) == home.as_ref() => Scope::Home,
            Some(_) => Scope::Folder,
        };
        Dialog {
            name: name.to_owned(),
            text: query.text.clone(),
            contents: query.contents,
            kind: query.kind,
            within: query.within,
            tag: query.tag.clone().unwrap_or_default(),
            scope,
            folder: query.scope.clone(),
            editing: (!name.is_empty()).then(|| name.to_owned()),
            focused: false,
        }
    }

    fn query(&self) -> Query {
        Query {
            text: self.text.trim().to_owned(),
            contents: self.contents,
            kind: self.kind,
            within: self.within,
            tag: Some(self.tag.trim().to_owned()).filter(|t| !t.is_empty()),
            scope: match self.scope {
                Scope::Everywhere => None,
                Scope::Home => std::env::var_os("HOME").map(PathBuf::from),
                Scope::Folder => self.folder.clone(),
            },
        }
    }

    /// Draws the dialog; `Some` once it is answered.
    pub fn show(&mut self, ui: &mut egui::Ui, p: &Palette) -> Option<Answer> {
        let screen = ui.ctx().content_rect();
        ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(90));
        let _ = ui.allocate_rect(screen, egui::Sense::click());
        let mut answer = None;
        egui::Area::new(egui::Id::new("neutron-smart-folder"))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                theme::popover(p)
                    .corner_radius(egui::CornerRadius::same(theme::RADIUS_CARD))
                    .inner_margin(egui::Margin::symmetric(20, 18))
                    .show(ui, |ui| {
                        ui.set_width(420.0);
                        let title = if self.editing.is_some() { "Edit Smart Folder" } else { "New Smart Folder" };
                        ui.label(egui::RichText::new(title).font(theme::strong(ui.ctx(), 15.0)).color(p.text));
                        ui.add_space(10.0);
                        egui::Grid::new("smart-folder-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                            ui.label(egui::RichText::new("Name has").color(p.text_muted));
                            let field = ui.add(egui::TextEdit::singleline(&mut self.text).hint_text("Any name").desired_width(260.0));
                            if !self.focused {
                                field.request_focus();
                                self.focused = true;
                            }
                            ui.end_row();
                            ui.label("");
                            ui.toggle_value(&mut self.contents, "Look inside files too");
                            ui.end_row();

                            ui.label(egui::RichText::new("Kind").color(p.text_muted));
                            egui::ComboBox::from_id_salt("smart-kind")
                                .selected_text(self.kind.map_or("Any", |k| k.label()))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.kind, None, "Any");
                                    for kind in Kind::ALL {
                                        ui.selectable_value(&mut self.kind, Some(kind), kind.label());
                                    }
                                });
                            ui.end_row();

                            ui.label(egui::RichText::new("Last changed").color(p.text_muted));
                            egui::ComboBox::from_id_salt("smart-within")
                                .selected_text(self.within.map_or("Any time", |w| w.label()))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.within, None, "Any time");
                                    for within in Within::ALL {
                                        ui.selectable_value(&mut self.within, Some(within), within.label());
                                    }
                                });
                            ui.end_row();

                            ui.label(egui::RichText::new("Tag").color(p.text_muted));
                            ui.add(egui::TextEdit::singleline(&mut self.tag).hint_text("Any or none").desired_width(260.0));
                            ui.end_row();

                            ui.label(egui::RichText::new("Search in").color(p.text_muted));
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut self.scope, Scope::Everywhere, "This Mac");
                                ui.selectable_value(&mut self.scope, Scope::Home, "Home");
                                if let Some(folder) = &self.folder {
                                    let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
                                    ui.selectable_value(&mut self.scope, Scope::Folder, format!("“{name}”"));
                                }
                            });
                            ui.end_row();

                            ui.label(egui::RichText::new("Save as").color(p.text_muted));
                            ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("A name, to keep it in the sidebar").desired_width(260.0));
                            ui.end_row();
                        });
                        ui.add_space(6.0);
                        let query = self.query();
                        ui.label(egui::RichText::new(format!("Finds: {}", query.describe())).size(12.0).color(p.text_faint));
                        ui.add_space(10.0);
                        let ready = !query.is_empty();
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            answer = Some(Answer::Cancel);
                        }
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                answer = Some(Answer::Cancel);
                            }
                            let name = self.name.trim().to_owned();
                            let label = if name.is_empty() { "Search" } else { "Save and Search" };
                            if (ui.add_enabled(ready, egui::Button::new(label)).clicked() || (enter && ready)) && answer.is_none() {
                                answer = Some(Answer::Search {
                                    query: query.clone(),
                                    save_as: (!name.is_empty()).then_some(name),
                                    replacing: self.editing.clone(),
                                });
                            }
                        });
                    });
            });
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialog_filled_from_a_search_gives_it_back() {
        let query = Query { text: "tax".into(), kind: Some(Kind::Pdf), tag: Some("Red".into()), ..Default::default() };
        let dialog = Dialog::editing("Taxes", &query);
        assert_eq!(dialog.query(), query);
        assert_eq!(dialog.editing.as_deref(), Some("Taxes"));
        let saved = Saved { name: "Taxes".into(), query: query.encode() };
        assert!(matches!(saved.location(), neutron_core::NodeId::Search { .. }));
        let round: Vec<Saved> = serde_json::from_str(&serde_json::to_string(&vec![saved.clone()]).unwrap()).unwrap();
        assert_eq!(round, vec![saved]);
    }
}
