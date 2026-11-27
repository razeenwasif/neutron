//! Interactive Storage Lens: visualises disk usage for any local or remote drive/folder.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender};
use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use neutron_core::NodeId;
use neutron_remote::Site;
use neutron_remote::sftp::{shell_quote_posix, windows_path_from_sftp};
use neutron_ui::format as ui_format;
use neutron_ui::icons::{self, Glyph};
use neutron_ui::theme::{self, Palette, micro_caps};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageEntry {
    pub name: String,
    pub node: NodeId,
    pub size: u64,
    pub is_dir: bool,
    pub is_heavy_cache: bool,
}

#[derive(Debug, Clone)]
pub struct StorageScan {
    pub _root: NodeId,
    pub label: String,
    pub total_bytes: u64,
    pub entries: Vec<StorageEntry>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum LensAction {
    Close,
    Drill(NodeId, String),
    GoUp,
    Rescan,
    Open(NodeId),
    TrashLocal(PathBuf),
}

pub struct StorageLens {
    pub open: bool,
    root: Option<NodeId>,
    label: String,
    loading: bool,
    scan: Option<StorageScan>,
    generation: Arc<AtomicU64>,
    current_gen: u64,
    tx: Sender<(u64, StorageScan)>,
    rx: Receiver<(u64, StorageScan)>,
    pending_trash_confirm: Option<PathBuf>,
}

impl Default for StorageLens {
    fn default() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            open: false,
            root: None,
            label: String::new(),
            loading: false,
            scan: None,
            generation: Arc::new(AtomicU64::new(0)),
            current_gen: 0,
            tx,
            rx,
            pending_trash_confirm: None,
        }
    }
}

impl StorageLens {
    pub fn open(&mut self, root: NodeId, label: String, sites: &[Site], ctx: &egui::Context) {
        self.open = true;
        self.root = Some(root.clone());
        self.label = label.clone();
        self.loading = true;
        self.pending_trash_confirm = None;

        self.current_gen = self.current_gen.wrapping_add(1);
        let gen_num = self.current_gen;
        self.generation.store(gen_num, Ordering::Relaxed);

        let tx = self.tx.clone();
        let wake = ctx.clone();
        let active_gen = Arc::clone(&self.generation);
        let sites_owned = sites.to_vec();

        let _ = std::thread::Builder::new()
            .name("neutron-storage-lens".into())
            .spawn(move || {
                let result = scan_root(&root, &label, &sites_owned, &active_gen, gen_num);
                if active_gen.load(Ordering::Relaxed) == gen_num {
                    let _ = tx.send((gen_num, result));
                    wake.request_repaint();
                }
            });
    }

    pub fn rescan(&mut self, sites: &[Site], ctx: &egui::Context) {
        if let Some(root) = self.root.clone() {
            let label = self.label.clone();
            self.open(root, label, sites, ctx);
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.loading = false;
        self.pending_trash_confirm = None;
        self.current_gen = self.current_gen.wrapping_add(1);
        self.generation.store(self.current_gen, Ordering::Relaxed);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn is_loading(&self) -> bool {
        self.open && self.loading
    }

    pub fn pump(&mut self) -> bool {
        let mut updated = false;
        while let Ok((gen_num, result)) = self.rx.try_recv() {
            if gen_num == self.current_gen {
                self.scan = Some(result);
                self.loading = false;
                updated = true;
            }
        }
        updated
    }

    pub fn parent_root(&self) -> Option<(NodeId, String)> {
        let root = self.root.as_ref()?;
        match root {
            NodeId::Path(path) => {
                let parent = path.parent()?;
                if parent == path {
                    return None;
                }
                let label = parent
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| parent.display().to_string());
                Some((NodeId::Path(parent.to_path_buf()), label))
            }
            NodeId::Remote { site, path } => {
                let trimmed = path.trim_end_matches('/');
                if trimmed.is_empty() || trimmed == "/" {
                    return None;
                }
                // Stop at Windows drive root `/C:`
                if is_windows_drive_root(trimmed) {
                    return None;
                }
                let parent = match trimmed.rsplit_once('/') {
                    Some(("", _)) => "/",
                    Some((p, _)) if p.is_empty() => "/",
                    Some((p, _)) => p,
                    None => "/",
                };
                let label = if parent == "/" {
                    format!("{site}: /")
                } else {
                    parent.rsplit('/').next().unwrap_or(parent).to_owned()
                };
                Some((NodeId::remote(site.as_ref(), parent), label))
            }
            _ => None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, p: &Palette) -> Option<LensAction> {
        if !self.open {
            return None;
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            return Some(LensAction::Close);
        }

        let mut action = None;
        let screen = ctx.content_rect();
        let backdrop = Color32::from_black_alpha(170);

        egui::Area::new(egui::Id::new("neutron-storage-lens-scrim"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter().rect_filled(rect, CornerRadius::ZERO, backdrop);
                if resp.clicked() {
                    action = Some(LensAction::Close);
                }
            });

        let card_w = (screen.width() - 64.0).clamp(380.0, 780.0);
        let card_h = (screen.height() - 64.0).clamp(320.0, 560.0);

        egui::Area::new(egui::Id::new("neutron-storage-lens-card"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let frame = egui::Frame::new()
                    .fill(p.card)
                    .corner_radius(CornerRadius::same(theme::RADIUS_CARD))
                    .stroke(Stroke::new(1.0, p.border_strong))
                    .inner_margin(egui::Margin::same(18))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 18],
                        blur: 48,
                        spread: 0,
                        color: Color32::from_black_alpha(200),
                    });
                frame.show(ui, |ui| {
                    ui.set_width(card_w);
                    ui.set_height(card_h);

                    // Header row
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(micro_caps("Storage Lens"))
                                    .font(theme::strong(ui.ctx(), 10.0))
                                    .color(p.text_faint),
                            );
                            let subtitle = self
                                .root
                                .as_ref()
                                .map(NodeId::display_name)
                                .unwrap_or_else(|| self.label.clone());
                            ui.label(
                                egui::RichText::new(subtitle)
                                    .font(theme::strong(ui.ctx(), 16.0))
                                    .color(p.text),
                            );
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if header_pill(ui, p, "Close", false) {
                                action = Some(LensAction::Close);
                            }
                            if header_pill(ui, p, "Rescan", false) {
                                action = Some(LensAction::Rescan);
                            }
                            if let Some(root) = self.root.clone()
                                && header_pill(ui, p, "Open Folder", true)
                            {
                                action = Some(LensAction::Open(root));
                            }
                            if self.parent_root().is_some() && header_pill(ui, p, "← Up", false) {
                                action = Some(LensAction::GoUp);
                            }
                        });
                    });

                    ui.add_space(12.0);

                    if self.loading {
                        ui.add_space(card_h * 0.34);
                        ui.vertical_centered(|ui| {
                            ui.spinner();
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(format!("Scanning {}…", self.label))
                                    .font(FontId::proportional(13.0))
                                    .color(p.text_muted),
                            );
                        });
                        return;
                    }

                    let Some(scan) = self.scan.clone() else {
                        return;
                    };

                    if let Some(err) = &scan.error {
                        ui.add_space(16.0);
                        ui.colored_label(p.danger, err);
                        return;
                    }

                    // Stacked proportional bar
                    let bar_colours = segment_palette(p);
                    draw_stacked_bar(ui, p, &scan, &bar_colours);
                    ui.add_space(12.0);

                    // Summary line
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {} across {} items",
                                scan.label,
                                ui_format::size(Some(scan.total_bytes)),
                                scan.entries.len()
                            ))
                            .font(FontId::proportional(12.0))
                            .color(p.text_muted),
                        );
                    });
                    ui.add_space(8.0);

                    if scan.entries.is_empty() {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new("Empty folder").color(p.text_faint));
                        });
                        return;
                    }

                    // Breakdown list
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let avail_w = ui.available_width();
                            for (idx, entry) in scan.entries.iter().enumerate() {
                                let swatch = if idx < 7 {
                                    bar_colours[idx % bar_colours.len()]
                                } else {
                                    p.text_faint
                                };
                                let share = if scan.total_bytes > 0 {
                                    entry.size as f32 / scan.total_bytes as f32
                                } else {
                                    0.0
                                };

                                let row_h = 34.0;
                                let (rect, resp) = ui.allocate_exact_size(vec2(avail_w, row_h), Sense::click());
                                let bg = if resp.hovered() { p.hover } else { Color32::TRANSPARENT };
                                ui.painter().rect_filled(rect, CornerRadius::same(theme::RADIUS_CONTROL), bg);

                                // Action pills on the right edge (Open + optional Trash for local)
                                let open_rect = Rect::from_min_size(
                                    pos2(rect.right() - 56.0, rect.center().y - 11.0),
                                    vec2(48.0, 22.0),
                                );
                                let open_resp = ui.interact(
                                    open_rect,
                                    ui.id().with(("lens-open", idx, &entry.name)),
                                    Sense::click(),
                                );
                                draw_mini_pill(ui.painter(), p, open_rect, "Open", open_resp.hovered(), false);

                                let mut right_edge = open_rect.left() - 6.0;
                                let mut trash_clicked = false;
                                let mut trash_hovered = false;
                                if let NodeId::Path(local_path) = &entry.node {
                                    let is_confirming = self.pending_trash_confirm.as_ref() == Some(local_path);
                                    let pill_w = if is_confirming { 82.0 } else { 52.0 };
                                    let trash_rect = Rect::from_min_size(
                                        pos2(right_edge - pill_w, rect.center().y - 11.0),
                                        vec2(pill_w, 22.0),
                                    );
                                    let trash_resp = ui.interact(
                                        trash_rect,
                                        ui.id().with(("lens-trash", idx, &entry.name)),
                                        Sense::click(),
                                    );
                                    trash_hovered = trash_resp.hovered();
                                    let label = if is_confirming { "Confirm Trash" } else { "Trash" };
                                    draw_mini_pill(ui.painter(), p, trash_rect, label, trash_hovered, is_confirming);
                                    if trash_resp.clicked() {
                                        trash_clicked = true;
                                        if is_confirming {
                                            self.pending_trash_confirm = None;
                                            action = Some(LensAction::TrashLocal(local_path.clone()));
                                        } else {
                                            self.pending_trash_confirm = Some(local_path.clone());
                                        }
                                    }
                                    right_edge = trash_rect.left() - 10.0;
                                }

                                // Swatch dot + glyph + name
                                let painter = ui.painter();
                                painter.circle_filled(pos2(rect.left() + 12.0, rect.center().y), 4.0, swatch);
                                icons::draw_scaled(
                                    painter,
                                    pos2(rect.left() + 29.0, rect.center().y),
                                    if entry.is_dir { Glyph::Folder } else { Glyph::File },
                                    if entry.is_dir { p.accent } else { p.text_muted },
                                    0.78,
                                );

                                // Size & % + mini bar right of name
                                let size_text = format!(
                                    "{} · {:.1}%",
                                    ui_format::size(Some(entry.size)),
                                    share * 100.0
                                );
                                let size_galley = painter.layout_no_wrap(
                                    size_text,
                                    FontId::proportional(11.5),
                                    p.text_muted,
                                );
                                let size_w = size_galley.size().x;
                                let bar_w = 68.0_f32;
                                let bar_rect = Rect::from_min_size(
                                    pos2(right_edge - bar_w, rect.center().y - 3.0),
                                    vec2(bar_w, 6.0),
                                );
                                painter.rect_filled(bar_rect, theme::pill(6.0), p.raised);
                                let fill_w = (bar_w * share.clamp(0.0, 1.0)).max(if entry.size > 0 { 2.0 } else { 0.0 });
                                if fill_w > 0.0 {
                                    painter.rect_filled(
                                        Rect::from_min_size(bar_rect.min, vec2(fill_w, 6.0)),
                                        theme::pill(6.0),
                                        swatch,
                                    );
                                }
                                let size_x = bar_rect.left() - 10.0 - size_w;
                                painter.galley(
                                    pos2(size_x, rect.center().y - size_galley.size().y / 2.0),
                                    size_galley,
                                    p.text_muted,
                                );

                                let mut name_right = size_x - 10.0;
                                if entry.is_heavy_cache {
                                    let badge_rect = Rect::from_min_size(
                                        pos2(name_right - 46.0, rect.center().y - 8.5),
                                        vec2(46.0, 17.0),
                                    );
                                    if badge_rect.left() > rect.left() + 90.0 {
                                        painter.rect_filled(
                                            badge_rect,
                                            CornerRadius::same(4),
                                            p.warning.gamma_multiply(0.18),
                                        );
                                        painter.text(
                                            badge_rect.center(),
                                            Align2::CENTER_CENTER,
                                            "CACHE",
                                            theme::strong(ui.ctx(), 9.0),
                                            p.warning,
                                        );
                                        name_right = badge_rect.left() - 8.0;
                                    }
                                }

                                let name_rect = Rect::from_min_max(
                                    pos2(rect.left() + 42.0, rect.top()),
                                    pos2(name_right.max(rect.left() + 48.0), rect.bottom()),
                                );
                                let clipped = painter.with_clip_rect(name_rect);
                                clipped.text(
                                    pos2(name_rect.left(), rect.center().y),
                                    Align2::LEFT_CENTER,
                                    &entry.name,
                                    FontId::proportional(13.0),
                                    p.text,
                                );

                                if open_resp.clicked() {
                                    action = Some(LensAction::Open(entry.node.clone()));
                                } else if resp.clicked() && !open_resp.hovered() && !trash_hovered && !trash_clicked && entry.is_dir {
                                    action = Some(LensAction::Drill(entry.node.clone(), entry.name.clone()));
                                }
                            }
                        });
                });
            });

        action
    }
}

fn header_pill(ui: &mut Ui, p: &Palette, label: &str, accent: bool) -> bool {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(12.0),
        if accent { p.text } else { p.text_muted },
    );
    let size = vec2(galley.size().x + 20.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if accent {
        if resp.hovered() { p.accent } else { p.accent.gamma_multiply(0.85) }
    } else if resp.hovered() {
        p.hover
    } else {
        p.raised
    };
    ui.painter().rect_filled(rect, CornerRadius::same(theme::RADIUS_CONTROL), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(theme::RADIUS_CONTROL),
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        pos2(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0),
        galley,
        if accent { Color32::WHITE } else { p.text_muted },
    );
    resp.clicked()
}

fn draw_mini_pill(painter: &egui::Painter, p: &Palette, rect: Rect, label: &str, hovered: bool, danger: bool) {
    let fill = if danger {
        p.danger.gamma_multiply(0.24)
    } else if hovered {
        p.hover
    } else {
        p.raised
    };
    let fg = if danger {
        p.danger
    } else if hovered {
        p.text
    } else {
        p.text_muted
    };
    painter.rect_filled(rect, CornerRadius::same(theme::RADIUS_CONTROL), fill);
    painter.rect_stroke(rect, CornerRadius::same(theme::RADIUS_CONTROL), Stroke::new(1.0, p.border), StrokeKind::Inside);
    painter.text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.0), fg);
}

fn segment_palette(p: &Palette) -> [Color32; 7] {
    [
        p.accent,
        p.cloud,
        p.success,
        p.warning,
        Color32::from_rgb(168, 85, 247),
        Color32::from_rgb(236, 72, 153),
        p.danger,
    ]
}

fn draw_stacked_bar(ui: &mut Ui, p: &Palette, scan: &StorageScan, colours: &[Color32; 7]) {
    let bar_h = 14.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), bar_h), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, theme::pill(bar_h), p.raised);
    if scan.total_bytes == 0 || scan.entries.is_empty() {
        return;
    }

    let mut x = rect.left();
    let max_segments = scan.entries.len().min(7);
    let mut accounted: u64 = 0;

    for (idx, entry) in scan.entries.iter().take(max_segments).enumerate() {
        accounted = accounted.saturating_add(entry.size);
        let frac = entry.size as f32 / scan.total_bytes as f32;
        let w = (rect.width() * frac).max(if entry.size > 0 { 3.0 } else { 0.0 });
        let seg = Rect::from_min_max(pos2(x, rect.top()), pos2((x + w).min(rect.right()), rect.bottom()));
        if seg.width() > 0.5 {
            painter.rect_filled(seg, CornerRadius::ZERO, colours[idx % colours.len()]);
            if let Some(pointer) = ui.ctx().pointer_hover_pos()
                && seg.contains(pointer)
            {
                egui::Tooltip::always_open(
                    ui.ctx().clone(),
                    ui.layer_id(),
                    egui::Id::new(("lens-seg-tip", idx)),
                    egui::PopupAnchor::Pointer,
                )
                .show(|ui| {
                    ui.label(format!(
                        "{} — {} ({:.1}%)",
                        entry.name,
                        ui_format::size(Some(entry.size)),
                        frac * 100.0
                    ));
                });
            }
        }
        x += w;
    }

    let rest = scan.total_bytes.saturating_sub(accounted);
    if rest > 0 && x < rect.right() {
        let seg = Rect::from_min_max(pos2(x, rect.top()), rect.max);
        painter.rect_filled(seg, CornerRadius::ZERO, p.text_faint.gamma_multiply(0.45));
    }
    painter.rect_stroke(rect, theme::pill(bar_h), Stroke::new(1.0, p.border), StrokeKind::Inside);

    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(12.0, 4.0);
        for (idx, entry) in scan.entries.iter().take(5).enumerate() {
            let pct = entry.size as f32 / scan.total_bytes as f32 * 100.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let (dot, _) = ui.allocate_exact_size(vec2(8.0, 12.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.5, colours[idx % colours.len()]);
                ui.label(
                    egui::RichText::new(format!("{} ({:.0}%)", entry.name, pct))
                        .font(FontId::proportional(11.5))
                        .color(p.text_muted),
                );
            });
        }
    });
}

pub fn is_heavy_cache_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "target"
            | "node_modules"
            | ".cache"
            | "deriveddata"
            | ".venv"
            | "venv"
            | "__pycache__"
            | "build"
            | "dist"
            | ".gradle"
            | ".next"
            | "pods"
            | "caches"
            | ".cargo"
    )
}

fn is_windows_drive_root(trimmed: &str) -> bool {
    let b = trimmed.as_bytes();
    b.len() == 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':'
}

fn is_windows_sftp_path(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 3
        && b[0] == b'/'
        && b[1].is_ascii_alphabetic()
        && b[2] == b':'
        && (b.len() == 3 || b[3] == b'/')
}

fn scan_root(
    root: &NodeId,
    label: &str,
    sites: &[Site],
    active_gen: &AtomicU64,
    gen_num: u64,
) -> StorageScan {
    match root {
        NodeId::Path(path) => scan_local(root, label, path, active_gen, gen_num),
        NodeId::Remote { site: site_id, path } => scan_remote(root, label, site_id, path, sites),
        _ => StorageScan {
            _root: root.clone(),
            label: label.to_owned(),
            total_bytes: 0,
            entries: Vec::new(),
            error: Some("Storage Lens only inspects local or SFTP folders.".into()),
        },
    }
}

fn scan_local(
    root: &NodeId,
    label: &str,
    path: &Path,
    active_gen: &AtomicU64,
    gen_num: u64,
) -> StorageScan {
    let read_dir = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) => {
            return StorageScan {
                _root: root.clone(),
                label: label.to_owned(),
                total_bytes: 0,
                entries: Vec::new(),
                error: Some(format!("Could not read {}: {e}", path.display())),
            };
        }
    };

    let mut files: Vec<StorageEntry> = Vec::new();
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();

    for item in read_dir.flatten() {
        if active_gen.load(Ordering::Relaxed) != gen_num {
            return StorageScan {
                _root: root.clone(),
                label: label.to_owned(),
                total_bytes: 0,
                entries: Vec::new(),
                error: None,
            };
        }
        let child_path = item.path();
        let name = item.file_name().to_string_lossy().into_owned();
        let Ok(meta) = std::fs::symlink_metadata(&child_path) else {
            continue;
        };
        if meta.file_type().is_dir() {
            dirs.push((name, child_path));
        } else {
            files.push(StorageEntry {
                is_heavy_cache: is_heavy_cache_name(&name),
                name,
                node: NodeId::Path(child_path),
                size: meta.len(),
                is_dir: false,
            });
        }
    }

    let next_idx = std::sync::atomic::AtomicUsize::new(0);
    let results = parking_lot::Mutex::new(Vec::with_capacity(dirs.len()));
    let workers = dirs.len().clamp(1, 4);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if active_gen.load(Ordering::Relaxed) != gen_num {
                    break;
                }
                let i = next_idx.fetch_add(1, Ordering::Relaxed);
                let Some((name, dir_path)) = dirs.get(i) else {
                    break;
                };
                let size = dir_size_local(dir_path, active_gen, gen_num);
                results.lock().push(StorageEntry {
                    is_heavy_cache: is_heavy_cache_name(name),
                    name: name.clone(),
                    node: NodeId::Path(dir_path.clone()),
                    size,
                    is_dir: true,
                });
            });
        }
    });

    let mut entries = files;
    entries.extend(results.into_inner());
    entries.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    let total_bytes: u64 = entries.iter().map(|e| e.size).fold(0u64, u64::saturating_add);

    StorageScan {
        _root: root.clone(),
        label: label.to_owned(),
        total_bytes,
        entries,
        error: None,
    }
}

fn dir_size_local(root: &Path, active_gen: &AtomicU64, gen_num: u64) -> u64 {
    let mut total: u64 = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if active_gen.load(Ordering::Relaxed) != gen_num {
            return total;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in rd.flatten() {
            let p = item.path();
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.file_type().is_dir() {
                stack.push(p);
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn scan_remote(root: &NodeId, label: &str, site_id: &str, path: &str, sites: &[Site]) -> StorageScan {
    let Some(site) = sites.iter().find(|s| s.id == site_id) else {
        return StorageScan {
            _root: root.clone(),
            label: label.to_owned(),
            total_bytes: 0,
            entries: Vec::new(),
            error: Some(format!("Unknown server \"{site_id}\".")),
        };
    };

    let mut transport = match crate::remote::connect_checked(site) {
        Ok(t) => t,
        Err(e) => {
            return StorageScan {
                _root: root.clone(),
                label: label.to_owned(),
                total_bytes: 0,
                entries: Vec::new(),
                error: Some(e),
            };
        }
    };

    if is_windows_sftp_path(path) {
        let Some(win_path) = windows_path_from_sftp(path) else {
            return StorageScan {
                _root: root.clone(),
                label: label.to_owned(),
                total_bytes: 0,
                entries: Vec::new(),
                error: Some(format!("Unsupported Windows path \"{path}\".")),
            };
        };
        let escaped = win_path.replace('\'', "''");
        let ps = format!(
            "powershell -NoProfile -NonInteractive -Command \"$p = '{escaped}'; Get-ChildItem -LiteralPath $p -Force -ErrorAction SilentlyContinue | ForEach-Object {{ if ($_.PSIsContainer) {{ $s = (Get-ChildItem -LiteralPath $_.FullName -Recurse -File -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum; if ($null -eq $s) {{ $s = 0 }}; Write-Output ('D' + [char]9 + [int64]$s + [char]9 + $_.Name) }} else {{ Write-Output ('F' + [char]9 + [int64]$_.Length + [char]9 + $_.Name) }} }}\""
        );
        match transport.exec(&ps, std::time::Duration::from_secs(30)) {
            Ok(out) => {
                let entries = parse_windows_du_output(&out, site_id, path);
                let total_bytes = entries.iter().map(|e| e.size).fold(0u64, u64::saturating_add);
                StorageScan {
                    _root: root.clone(),
                    label: label.to_owned(),
                    total_bytes,
                    entries,
                    error: None,
                }
            }
            Err(e) => StorageScan {
                _root: root.clone(),
                label: label.to_owned(),
                total_bytes: 0,
                entries: Vec::new(),
                error: Some(e.to_string()),
            },
        }
    } else {
        // Query immediate children (both dirs and top-level files) and their types cleanly.
        let q = shell_quote_posix(path);
        let cmd = format!(
            "sh -c 'd={q}; for f in \"$d\"/* \"$d\"/.[!.]* \"$d\"/..?*; do [ -e \"$f\" ] || continue; s=$(du -sk \"$f\" 2>/dev/null | cut -f1); [ -n \"$s\" ] || s=0; if [ -d \"$f\" ]; then t=D; else t=F; fi; printf \"%s\\t%s\\t%s\\n\" \"$t\" \"$s\" \"${{f##*/}}\"; done'"
        );
        match transport.exec(&cmd, std::time::Duration::from_secs(30)) {
            Ok(out) => {
                let entries = parse_posix_du_output(&out, site_id, path);
                let total_bytes = entries.iter().map(|e| e.size).fold(0u64, u64::saturating_add);
                StorageScan {
                    _root: root.clone(),
                    label: label.to_owned(),
                    total_bytes,
                    entries,
                    error: None,
                }
            }
            Err(e) => StorageScan {
                _root: root.clone(),
                label: label.to_owned(),
                total_bytes: 0,
                entries: Vec::new(),
                error: Some(e.to_string()),
            },
        }
    }
}

pub fn parse_posix_du_output(stdout: &str, site_id: &str, base_path: &str) -> Vec<StorageEntry> {
    let base_trimmed = base_path.trim_end_matches('/');
    let mut entries = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim_end_matches('\r');
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(3, '\t');
        let (Some(first), Some(second), third) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let (is_dir, kib, name) = if let Some(name) = third {
            let Ok(kib) = second.trim().parse::<u64>() else { continue };
            (first == "D", kib, name.to_owned())
        } else {
            // Also support raw `du -d 1 -k` style `<kib>\t<path>`
            let Ok(kib) = first.trim().parse::<u64>() else { continue };
            let child = second.trim_end_matches('/');
            if child == base_trimmed || child == "." {
                continue;
            }
            let name = child.rsplit('/').next().unwrap_or(child).to_owned();
            (true, kib, name)
        };
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        let child_path = if base_trimmed.is_empty() {
            format!("/{name}")
        } else {
            format!("{base_trimmed}/{name}")
        };
        entries.push(StorageEntry {
            is_heavy_cache: is_heavy_cache_name(&name),
            name,
            node: NodeId::remote(site_id, child_path),
            size: kib.saturating_mul(1024),
            is_dir,
        });
    }
    entries.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    entries
}

pub fn parse_windows_du_output(stdout: &str, site_id: &str, base_path: &str) -> Vec<StorageEntry> {
    let base_trimmed = base_path.trim_end_matches('/');
    let mut entries = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim_end_matches('\r');
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(3, '\t');
        let (Some(kind), Some(size_str), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Ok(size) = size_str.trim().parse::<u64>() else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let child_path = format!("{base_trimmed}/{name}");
        entries.push(StorageEntry {
            is_heavy_cache: is_heavy_cache_name(name),
            name: name.to_owned(),
            node: NodeId::remote(site_id, child_path),
            size,
            is_dir: kind == "D",
        });
    }
    entries.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heavy_cache_detection_matches_common_build_and_cache_dirs() {
        assert!(is_heavy_cache_name("target"));
        assert!(is_heavy_cache_name("node_modules"));
        assert!(is_heavy_cache_name("DerivedData"));
        assert!(is_heavy_cache_name(".cache"));
        assert!(!is_heavy_cache_name("src"));
        assert!(!is_heavy_cache_name("Documents"));
    }

    #[test]
    fn parse_posix_du_output_sorts_largest_first_and_flags_caches() {
        let sample = "D\t2048\ttarget\nF\t10\tREADME.md\nD\t4096\tnode_modules\n";
        let entries = parse_posix_du_output(sample, "kagutsuchi", "/Users/razeen/proj");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "node_modules");
        assert_eq!(entries[0].size, 4096 * 1024);
        assert!(entries[0].is_dir);
        assert!(entries[0].is_heavy_cache);
        assert_eq!(entries[1].name, "target");
        assert_eq!(entries[2].name, "README.md");
        assert!(!entries[2].is_dir);
        assert!(!entries[2].is_heavy_cache);
    }

    #[test]
    fn parse_windows_du_output_builds_sftp_paths() {
        let sample = "D\t5368709120\tUsers\nF\t1024\tpagefile.sys\n";
        let entries = parse_windows_du_output(sample, "amaterasu", "/C:");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].node, NodeId::remote("amaterasu", "/C:/Users"));
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].node, NodeId::remote("amaterasu", "/C:/pagefile.sys"));
        assert!(!entries[1].is_dir);
    }

    #[test]
    fn local_scan_sorts_largest_first() {
        let unique = format!(
            "neutron-lens-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let sub = root.join("target");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join("small.txt"), b"hi").unwrap();
        std::fs::write(sub.join("big.bin"), vec![0u8; 4096]).unwrap();

        let gen_atomic = AtomicU64::new(1);
        let scan = scan_local(&NodeId::Path(root.clone()), "test", &root, &gen_atomic, 1);
        assert!(scan.error.is_none());
        assert_eq!(scan.entries.len(), 2);
        assert_eq!(scan.entries[0].name, "target");
        assert_eq!(scan.entries[0].size, 4096);
        assert!(scan.entries[0].is_heavy_cache);
        assert_eq!(scan.entries[1].name, "small.txt");
        assert_eq!(scan.entries[1].size, 2);

        let _ = std::fs::remove_dir_all(&root);
    }
}
