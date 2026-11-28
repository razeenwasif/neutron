use super::*;

// --- writing to servers -----------------------------------------------------

impl NeutronApp {
    /// Whether the focused tab is a folder on a saved server.
    pub(super) fn on_server(&self) -> bool {
        self.workspace.active_tab().is_some_and(|t| t.location().as_remote().is_some())
    }

    /// Names in a tab's listing, for checking what a write would collide with.
    pub(super) fn listed_names(&self, tab: TabId) -> Vec<String> {
        self.workspace.tabs.get(&tab).map_or_else(Vec::new, |t| {
            (0..t.list.len()).map(|i| t.list.name(i).to_owned()).collect()
        })
    }

    pub(super) fn enqueue_remote_job(
        &mut self,
        tab: TabId,
        extra_refresh_tab: Option<TabId>,
        label: String,
        rename_after: Option<String>,
        work: impl FnOnce(&crate::remote_ops::Progress) -> Result<String, String> + Send + 'static,
    ) {
        let id = self.next_remote_job;
        self.next_remote_job = self.next_remote_job.wrapping_add(1);
        let progress = std::sync::Arc::new(crate::remote_ops::Progress::default());
        let (tx, rx) = crossbeam_channel::bounded(1);
        let worker_progress = std::sync::Arc::clone(&progress);
        let wake = self.ctx.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("neutron-remote-{id}"))
            .spawn(move || {
                let _ = tx.send(work(&worker_progress));
                wake.request_repaint();
            });
        if let Err(e) = spawned {
            self.notices.error(format!("Could not start: {e}"));
            return;
        }
        self.remote_jobs.push(RemoteJob {
            id,
            label,
            tab,
            extra_refresh_tab,
            progress,
            done: rx,
            rename_after,
        });
    }

    /// Runs `work` against the server `tab` is showing, on a worker thread.
    pub(super) fn start_remote(
        &mut self,
        tab: TabId,
        label: String,
        rename_after: Option<String>,
        work: impl FnOnce(&neutron_remote::Site, &str, &crate::remote_ops::Progress) -> Result<String, String>
        + Send
        + 'static,
    ) {
        let Some((site_id, dir)) = self
            .workspace
            .tabs
            .get(&tab)
            .and_then(|t| t.location().as_remote())
            .map(|(site, dir)| (site.to_owned(), dir.to_owned()))
        else {
            return;
        };
        let Some(site) = self.sites.get(&site_id).cloned() else {
            self.notices.error(format!("No saved server called {site_id:?}"));
            return;
        };
        self.enqueue_remote_job(tab, None, label, rename_after, move |progress| {
            work(&site, &dir, progress)
        });
    }

    pub(super) fn pump_remote_job(&mut self) {
        if self.remote_jobs.is_empty() {
            return;
        }
        let mut completed = Vec::new();
        let mut remaining = Vec::with_capacity(self.remote_jobs.len());
        for job in std::mem::take(&mut self.remote_jobs) {
            match job.done.try_recv() {
                Ok(outcome) => completed.push((job, outcome)),
                Err(crossbeam_channel::TryRecvError::Empty) => remaining.push(job),
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    completed.push((job, Err("worker ended unexpectedly".to_owned())));
                }
            }
        }
        self.remote_jobs = remaining;
        if completed.is_empty() {
            return;
        }
        for (job, outcome) in completed {
            match outcome {
                Ok(receipt) => {
                    self.notices.info(receipt);
                    if let Some(name) = job.rename_after {
                        self.pending_rename = Some((job.tab, name));
                    }
                }
                Err(reason) => self.notices.error(format!("{} — {reason}", job.label)),
            }
            self.refresh_in_place(job.tab);
            if let Some(other) = job.extra_refresh_tab {
                if other != job.tab {
                    self.refresh_in_place(other);
                }
            }
        }
        self.columns.clear();
    }

    pub(super) fn remote_tab_named(&self, site_id: &str, dir: &str) -> Option<TabId> {
        self.workspace
            .tabs
            .iter()
            .find_map(|(&id, tab)| match tab.location().as_remote() {
                Some((s, p)) if s == site_id && p == dir => Some(id),
                _ => None,
            })
    }

    pub(super) fn remote_clip(&mut self, cut: bool) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(t) = self.workspace.tabs.get(&tab_id) else {
            return;
        };
        let Some((site_id, dir)) = t.location().as_remote().map(|(s, d)| (s.to_owned(), d.to_owned())) else {
            return;
        };
        let entries: Vec<(String, neutron_core::EntryKind)> = visible_selection(t)
            .into_iter()
            .map(|i| (t.list.name(i).to_owned(), t.list.kind(i)))
            .collect();
        if entries.is_empty() {
            return;
        }
        let count = entries.len();
        let server = self.server_name(tab_id);
        self.remote_clipboard = Some(RemoteClipboard { site_id, dir, entries, cut });
        let noun = if count == 1 { "1 item" } else { &format!("{count} items") };
        self.notices.info(if cut {
            format!("Cut {noun} from {server} — open destination and Paste")
        } else {
            format!("Copied {noun} from {server} — open destination and Paste")
        });
    }

    pub(super) fn paste_remote_to_local(&mut self) {
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(local_dest) = self
            .workspace
            .tabs
            .get(&tab)
            .and_then(|t| t.location().as_path())
            .map(|p| p.to_path_buf())
        else {
            return;
        };
        let Some(clip) = self.remote_clipboard.clone() else {
            return;
        };
        let Some(site) = self.sites.get(&clip.site_id).cloned() else {
            self.notices.error(format!("No saved server called {:?}", clip.site_id));
            return;
        };
        if clip.cut {
            self.remote_clipboard = None;
        }
        let src_tab = if clip.cut { self.remote_tab_named(&clip.site_id, &clip.dir) } else { None };
        let label = format!(
            "{} from {}",
            if clip.cut { "Moving" } else { "Downloading" },
            site.display(),
        );
        self.enqueue_remote_job(tab, src_tab, label, None, move |progress| {
            crate::remote_ops::download_entries(
                &site,
                &clip.dir,
                &clip.entries,
                &local_dest,
                clip.cut,
                progress,
            )
        });
    }

    pub(super) fn download_remote_to_downloads(&mut self) {
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(downloads) = self
            .known_folders
            .iter()
            .find(|p| p.name == "Downloads")
            .and_then(|p| p.id.as_path())
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Downloads")))
        else {
            self.notices.error("Could not find the Downloads folder");
            return;
        };
        let Some(t) = self.workspace.tabs.get(&tab) else {
            return;
        };
        let Some((site_id, dir)) = t.location().as_remote().map(|(s, d)| (s.to_owned(), d.to_owned())) else {
            return;
        };
        let entries: Vec<(String, neutron_core::EntryKind)> = visible_selection(t)
            .into_iter()
            .map(|i| (t.list.name(i).to_owned(), t.list.kind(i)))
            .collect();
        if entries.is_empty() {
            return;
        }
        let Some(site) = self.sites.get(&site_id).cloned() else {
            self.notices.error(format!("No saved server called {site_id:?}"));
            return;
        };
        let label = format!("Downloading from {} to Downloads", site.display());
        self.enqueue_remote_job(tab, None, label, None, move |progress| {
            crate::remote_ops::download_entries(&site, &dir, &entries, &downloads, false, progress)
        });
    }

    /// The server's display name for the focused tab, for labels.
    pub(super) fn server_name(&self, tab: TabId) -> String {
        self.workspace
            .tabs
            .get(&tab)
            .and_then(|t| t.location().as_remote())
            .and_then(|(site, _)| self.sites.get(site))
            .map_or_else(|| "the server".to_owned(), |s| s.display().to_owned())
    }

    pub(super) fn pump_remote_edits(&mut self) {
        while let Ok((local_path, site_id, remote_path, outcome)) = self.remote_edit_rx.try_recv() {
            if let Some(edit) = self.remote_edits.iter_mut().find(|e| e.local_path == local_path) {
                edit.syncing = false;
            }
            match outcome {
                Ok(receipt) => {
                    self.notices.info(receipt);
                    let parent = neutron_remote::path::parent(&remote_path)
                        .unwrap_or_else(|| "/".to_owned());
                    let target = NodeId::remote(site_id.as_str(), parent.as_str());
                    let root_tabs: Vec<TabId> = self
                        .workspace
                        .tabs
                        .values()
                        .filter(|t| t.location() == &target)
                        .map(|t| t.id)
                        .collect();
                    for tab in root_tabs {
                        refresh_tab_in(&mut self.workspace, &mut self.loader, tab);
                    }
                    for w in &mut self.windows {
                        let win_tabs: Vec<TabId> = w
                            .workspace
                            .tabs
                            .values()
                            .filter(|t| t.location() == &target)
                            .map(|t| t.id)
                            .collect();
                        for tab in win_tabs {
                            refresh_tab_in(&mut w.workspace, &mut self.loader, tab);
                        }
                    }
                    self.columns.clear();
                }
                Err(reason) => self.notices.error(reason),
            }
        }

        if self.remote_edits.is_empty() {
            return;
        }
        self.ctx.request_repaint_after(Duration::from_secs(1));
        if self.remote_edits_checked.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.remote_edits_checked = Instant::now();

        for edit in &mut self.remote_edits {
            if edit.syncing {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&edit.local_path) else {
                continue;
            };
            let mtime = meta.modified().ok();
            let len = meta.len();
            if (mtime, len) == (edit.last_mtime, edit.last_len) {
                continue;
            }
            let Some(site) = self.sites.get(&edit.site_id).cloned() else {
                continue;
            };
            edit.last_mtime = mtime;
            edit.last_len = len;
            edit.syncing = true;

            let server_name = site.display().to_owned();
            let site_id = edit.site_id.clone();
            let remote_path = edit.remote_path.clone();
            let name = edit.name.clone();
            let local_path = edit.local_path.clone();
            let tx = self.remote_edit_tx.clone();
            let wake = self.ctx.clone();
            let spawned = std::thread::Builder::new()
                .name("neutron-remote-sync".into())
                .spawn(move || {
                    let outcome = crate::remote_ops::upload_file(&site, &remote_path, &local_path)
                        .map(|()| format!("Saved {name} back to {server_name}"))
                        .map_err(|e| format!("Could not save {name} back to {server_name} — {e}"));
                    let _ = tx.send((local_path, site_id, remote_path, outcome));
                    wake.request_repaint();
                });
            if let Err(e) = spawned {
                edit.syncing = false;
                self.notices.error(format!("Could not sync {}: {e}", edit.name));
            }
        }
    }

    pub(super) fn remote_new_folder(&mut self) {
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };
        let taken = self.listed_names(tab);
        let name = neutron_core::naming::next_available("New folder", |c| taken.iter().any(|t| t == c));
        let op = crate::remote_ops::Op::CreateDir { name: name.clone() };
        let label = format!("Creating {name} on {}", self.server_name(tab));
        self.start_remote(tab, label, Some(name), move |site, dir, progress| {
            crate::remote_ops::run(site, dir, &op, progress)
        });
    }

    pub(super) fn remote_new_file(&mut self, template: Option<String>) {
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };
        let taken = self.listed_names(tab);
        let base = template.as_deref().unwrap_or(crate::new_file::UNTITLED);
        let name = crate::new_file::unique_name(base, |c| taken.iter().any(|t| t == c));
        let contents = match template {
            Some(tpl) => {
                let Some(dir) = crate::new_file::templates_dir() else {
                    self.notices.error("Could not find the Templates folder");
                    return;
                };
                match std::fs::read(dir.join(&tpl)) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        self.notices.error(format!("Could not read template {tpl}: {e}"));
                        return;
                    }
                }
            }
            None => Vec::new(),
        };
        let op = crate::remote_ops::Op::CreateFile { name: name.clone(), contents };
        let label = format!("Creating {name} on {}", self.server_name(tab));
        self.start_remote(tab, label, Some(name), move |site, dir, progress| {
            crate::remote_ops::run(site, dir, &op, progress)
        });
    }

    pub(super) fn remote_rename(&mut self, tab: TabId, place: &NodeId, from: String, to: String) {
        // Checked against the listing, because a server's rename may quietly
        // replace what is already there. Only when the tab still shows the
        // folder the rename began in; otherwise there is nothing to check
        // against, and guessing is how a file gets overwritten.
        if self.workspace.tabs.get(&tab).map(|t| t.location()) != Some(place) {
            self.notices.info("The folder changed before the rename finished, so nothing was renamed");
            return;
        }
        if !from.eq_ignore_ascii_case(&to) && self.listed_names(tab).contains(&to) {
            self.notices.error(format!("{to} is already here — pick another name"));
            return;
        }
        let op = crate::remote_ops::Op::Rename { from, to: to.clone() };
        let label = format!("Renaming to {to}");
        self.start_remote(tab, label, None, move |site, dir, progress| {
            crate::remote_ops::run(site, dir, &op, progress)
        });
    }

    pub(super) fn remote_delete(&mut self) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(t) = self.workspace.tabs.get(&tab_id) else {
            return;
        };
        let entries: Vec<(String, neutron_core::EntryKind)> = visible_selection(t)
            .into_iter()
            .map(|i| (t.list.name(i).to_owned(), t.list.kind(i)))
            .collect();
        if entries.is_empty() {
            return;
        }
        let label = format!("Deleting from {}", self.server_name(tab_id));
        let op = crate::remote_ops::Op::Delete { entries };
        self.start_remote(tab_id, label, None, move |site, dir, progress| {
            crate::remote_ops::run(site, dir, &op, progress)
        });
    }

    /// Uploads whatever local files are on the clipboard.
    ///
    /// A cut onto a server copies: the originals stay, because removing local
    /// files after a network transfer is a second, separate risk, and nothing
    /// here can undo it if the upload turns out to have been incomplete.
    pub(super) fn remote_paste(&mut self) {
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };
        let taken = self.listed_names(tab);

        if let Some(clip) = self.remote_clipboard.clone() {
            let Some((dst_site_id, dst_dir)) = self
                .workspace
                .tabs
                .get(&tab)
                .and_then(|t| t.location().as_remote())
                .map(|(s, d)| (s.to_owned(), d.to_owned()))
            else {
                return;
            };
            let taken_refs: Vec<&str> = taken.iter().map(String::as_str).collect();
            let clashes = crate::remote_ops::collisions(
                clip.entries.iter().map(|(n, _)| n.as_str()),
                &taken_refs,
            );
            if clip.site_id == dst_site_id && clip.dir == dst_dir {
                self.notices.info("Items are already in this folder");
                return;
            }
            if !clashes.is_empty() {
                self.notices.error(format!(
                    "{} already in this folder: {}",
                    if clashes.len() == 1 { "This name is" } else { "These names are" },
                    clashes.join(", "),
                ));
                return;
            }
            let src_tab = if clip.cut { self.remote_tab_named(&clip.site_id, &clip.dir) } else { None };
            if clip.cut {
                self.remote_clipboard = None;
            }
            if clip.site_id == dst_site_id {
                let label = format!(
                    "{} on {}",
                    if clip.cut { "Moving" } else { "Copying" },
                    self.server_name(tab),
                );
                let op = if clip.cut {
                    crate::remote_ops::Op::MoveRemote {
                        from_dir: clip.dir,
                        entries: clip.entries,
                    }
                } else {
                    crate::remote_ops::Op::CopyRemote {
                        from_dir: clip.dir,
                        entries: clip.entries,
                    }
                };
                let Some(site) = self.sites.get(&dst_site_id).cloned() else {
                    self.notices.error(format!("No saved server called {dst_site_id:?}"));
                    return;
                };
                self.enqueue_remote_job(tab, src_tab, label, None, move |progress| {
                    crate::remote_ops::run(&site, &dst_dir, &op, progress)
                });
                return;
            }
            let Some(src_site) = self.sites.get(&clip.site_id).cloned() else {
                self.notices.error(format!("No saved server called {:?}", clip.site_id));
                return;
            };
            let Some(dst_site) = self.sites.get(&dst_site_id).cloned() else {
                self.notices.error(format!("No saved server called {dst_site_id:?}"));
                return;
            };
            let label = format!("{} → {}", src_site.display(), dst_site.display());
            self.enqueue_remote_job(tab, src_tab, label, None, move |progress| {
                crate::remote_ops::transfer_between_servers(
                    &src_site,
                    &clip.dir,
                    &clip.entries,
                    &dst_site,
                    &dst_dir,
                    &taken,
                    clip.cut,
                    progress,
                )
            });
            return;
        }

        let label = format!("Uploading to {}", self.server_name(tab));
        self.start_remote(tab, label, None, move |site, dir, progress| {
            // Read here, on the worker: another process can hold the clipboard.
            let Some((sources, _)) = neutron_shell::clipboard::read() else {
                return Err("there are no files on the clipboard — copy files first".to_owned());
            };
            upload_new(site, dir, sources, &taken, progress)
        });
    }

    pub(super) fn layout_dialog_overlay(&mut self, ctx: &egui::Context, p: &Palette, actions: &mut Vec<Action>) {
        if let Some(hit) = self.layout_dialog.show(ctx, p, &self.layouts) {
            actions.push(match hit {
                crate::layouts::LayoutAction::Close => Action::CloseLayouts,
                crate::layouts::LayoutAction::Save(name) => Action::SaveLayout(name),
                crate::layouts::LayoutAction::Apply(name) => Action::ApplyLayout(name),
                crate::layouts::LayoutAction::Delete(name) => Action::DeleteLayout(name),
            });
        }
    }

    pub(super) fn storage_lens_overlay(&mut self, ctx: &egui::Context, p: &Palette, actions: &mut Vec<Action>) {
        if let Some(hit) = self.storage_lens.show(ctx, p) {
            actions.push(match hit {
                crate::storage_lens::LensAction::Close => Action::CloseStorageLens,
                crate::storage_lens::LensAction::Drill(node, label) => Action::DrillStorageLens(node, label),
                crate::storage_lens::LensAction::GoUp => Action::UpStorageLens,
                crate::storage_lens::LensAction::Rescan => Action::RescanStorageLens,
                crate::storage_lens::LensAction::Open(node) => {
                    self.storage_lens.close();
                    Action::Navigate(node)
                }
                crate::storage_lens::LensAction::TrashLocal(path) => Action::TrashFromStorageLens(path),
            });
        }
    }

    pub(super) fn compare_pair(&self) -> Option<(GroupId, TabId, GroupId, TabId)> {
        let groups = self.workspace.layout.groups();
        if groups.len() < 2 {
            return None;
        }
        let g1 = groups[0];
        let g2 = groups[1];
        let t1 = self.workspace.groups.get(&g1)?.active_tab()?;
        let t2 = self.workspace.groups.get(&g2)?.active_tab()?;
        Some((g1, t1, g2, t2))
    }

    pub(super) fn pane_diff_for(
        &self,
        group_id: GroupId,
        _tab: &crate::workspace::Tab,
    ) -> Option<(std::collections::HashMap<String, file_list::DiffTag>, GroupId, usize, usize, usize)> {
        if !self.compare_panes {
            return None;
        }
        let (lg, lt, rg, rt) = self.compare_pair()?;
        if group_id != lg && group_id != rg {
            return None;
        }
        let left_tab = self.workspace.tabs.get(&lt)?;
        let right_tab = self.workspace.tabs.get(&rt)?;
        if matches!(left_tab.location(), NodeId::Home) || matches!(right_tab.location(), NodeId::Home) {
            return None;
        }
        let diff = crate::diff::compare_lists(&left_tab.list, &right_tab.list);
        if group_id == lg {
            Some((
                diff.left_tags,
                rg,
                diff.summary.left_only,
                diff.summary.left_newer,
                diff.summary.different,
            ))
        } else {
            Some((
                diff.right_tags,
                lg,
                diff.summary.right_only,
                diff.summary.right_newer,
                diff.summary.different,
            ))
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_compare_strip(
        &self,
        ui: &mut egui::Ui,
        p: &Palette,
        src_group: GroupId,
        dst_group: GroupId,
        only_count: usize,
        newer_count: usize,
        diff_count: usize,
        actions: &mut Vec<Action>,
    ) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, egui::CornerRadius::ZERO, p.elevated);
        painter.line_segment(
            [egui::pos2(rect.left(), rect.bottom()), egui::pos2(rect.right(), rect.bottom())],
            egui::Stroke::new(1.0, p.border),
        );

        let badge = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 12.0, rect.center().y - 9.0),
            egui::vec2(62.0, 18.0),
        );
        painter.rect_filled(badge, egui::CornerRadius::same(4), p.cloud.gamma_multiply(0.18));
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            "COMPARE",
            theme::strong(ui.ctx(), 9.0),
            p.cloud,
        );

        let summary = if only_count == 0 && newer_count == 0 && diff_count == 0 {
            "Matches other pane".to_owned()
        } else {
            format!("{only_count} only here · {newer_count} newer · {diff_count} diff")
        };
        painter.text(
            egui::pos2(badge.right() + 10.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            summary,
            egui::FontId::proportional(11.5),
            p.text_muted,
        );

        let mut right_x = rect.right() - 10.0;
        let close_w = 50.0;
        let close_rect = egui::Rect::from_min_size(
            egui::pos2(right_x - close_w, rect.center().y - 10.0),
            egui::vec2(close_w, 20.0),
        );
        let close_resp = ui.interact(close_rect, ui.id().with(("compare-close", src_group)), egui::Sense::click());
        painter.rect_filled(
            close_rect,
            egui::CornerRadius::same(theme::RADIUS_CONTROL),
            if close_resp.hovered() { p.hover } else { p.raised },
        );
        painter.text(
            close_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Close",
            egui::FontId::proportional(11.0),
            p.text_muted,
        );
        if close_resp.clicked() {
            actions.push(Action::ToggleComparePanes);
        }
        right_x = close_rect.left() - 8.0;

        let to_sync = only_count + newer_count;
        if to_sync > 0 {
            let btn_label = format!("Sync {to_sync} →");
            let btn_w = 82.0;
            let sync_rect = egui::Rect::from_min_size(
                egui::pos2(right_x - btn_w, rect.center().y - 10.0),
                egui::vec2(btn_w, 20.0),
            );
            let sync_resp = ui.interact(sync_rect, ui.id().with(("compare-sync", src_group)), egui::Sense::click());
            painter.rect_filled(
                sync_rect,
                egui::CornerRadius::same(theme::RADIUS_CONTROL),
                if sync_resp.hovered() { p.accent } else { p.accent.gamma_multiply(0.85) },
            );
            painter.text(
                sync_rect.center(),
                egui::Align2::CENTER_CENTER,
                btn_label,
                theme::strong(ui.ctx(), 10.5),
                egui::Color32::WHITE,
            );
            if sync_resp.clicked() {
                actions.push(Action::SyncPaneToOther {
                    src: src_group,
                    dst: dst_group,
                });
            }
        }
    }

    pub(super) fn sync_pane_to_other(&mut self, src_group: GroupId, dst_group: GroupId, ctx: &egui::Context) {
        let Some(src_tab_id) = self.workspace.groups.get(&src_group).and_then(|g| g.active_tab()) else {
            return;
        };
        let Some(dst_tab_id) = self.workspace.groups.get(&dst_group).and_then(|g| g.active_tab()) else {
            return;
        };
        let (Some(src_tab), Some(dst_tab)) = (
            self.workspace.tabs.get(&src_tab_id),
            self.workspace.tabs.get(&dst_tab_id),
        ) else {
            return;
        };

        let diff = crate::diff::compare_lists(&src_tab.list, &dst_tab.list);
        let idxs = crate::diff::sync_candidate_indices(&src_tab.list, &diff.left_tags);
        if idxs.is_empty() {
            self.notices.info("Nothing to sync — destination already has all missing/newer items");
            return;
        }

        let src_loc = src_tab.location().clone();
        let dst_loc = dst_tab.location().clone();
        let names_kinds: Vec<(String, neutron_core::EntryKind)> = idxs
            .iter()
            .map(|&i| (src_tab.list.name(i).to_owned(), src_tab.list.kind(i)))
            .collect();

        match (&src_loc, &dst_loc) {
            (NodeId::Path(src_dir), NodeId::Path(dst_dir)) => {
                let paths: Vec<PathBuf> = names_kinds.iter().map(|(n, _)| src_dir.join(n)).collect();
                self.start_transfer(
                    crate::transfers::Request {
                        tab: dst_tab_id,
                        paths,
                        destination: dst_dir.clone(),
                        how: neutron_shell::fileops::Transfer::Copy,
                    },
                    neutron_shell::transfer::Clash::Replace,
                );
            }
            (NodeId::Path(src_dir), NodeId::Remote { site: dst_site_id, path: _ }) => {
                let paths: Vec<PathBuf> = names_kinds.iter().map(|(n, _)| src_dir.join(n)).collect();
                let label = format!("Syncing {} items to {}", paths.len(), self.server_name(dst_tab_id));
                self.start_remote(dst_tab_id, label, None, move |site, dir, progress| {
                    crate::remote_ops::run(site, dir, &crate::remote_ops::Op::Upload { sources: paths }, progress)
                });
                let _ = dst_site_id;
            }
            (NodeId::Remote { site: src_site_id, path: src_dir }, NodeId::Path(dst_dir)) => {
                let Some(site) = self.sites.get(src_site_id).cloned() else {
                    self.notices.error(format!("No saved server called {:?}", src_site_id));
                    return;
                };
                let dst_dir = dst_dir.clone();
                let src_dir = src_dir.clone();
                let label = format!("Syncing {} items from {}", names_kinds.len(), site.display());
                self.enqueue_remote_job(dst_tab_id, None, label, None, move |progress| {
                    crate::remote_ops::download_entries_sync(&site, &src_dir, &names_kinds, &dst_dir, progress)
                });
            }
            (NodeId::Remote { site: src_site_id, path: src_dir }, NodeId::Remote { site: dst_site_id, path: dst_dir }) => {
                let Some(src_site) = self.sites.get(src_site_id).cloned() else {
                    self.notices.error(format!("No saved server called {:?}", src_site_id));
                    return;
                };
                let Some(dst_site) = self.sites.get(dst_site_id).cloned() else {
                    self.notices.error(format!("No saved server called {:?}", dst_site_id));
                    return;
                };
                let src_dir = src_dir.clone();
                let dst_dir = dst_dir.clone();
                let label = format!("Syncing {} → {}", src_site.display(), dst_site.display());
                self.enqueue_remote_job(dst_tab_id, None, label, None, move |progress| {
                    crate::remote_ops::transfer_between_servers_sync(
                        &src_site,
                        &src_dir,
                        &names_kinds,
                        &dst_site,
                        &dst_dir,
                        progress,
                    )
                });
            }
            _ => {
                self.notices.error("Smart Sync supports local and saved server folders");
            }
        }
        let _ = ctx;
    }

    pub(super) fn remote_upload(&mut self, tab: TabId, sources: Vec<PathBuf>) {
        let taken = self.listed_names(tab);
        let label = format!("Uploading to {}", self.server_name(tab));
        self.start_remote(tab, label, None, move |site, dir, progress| {
            upload_new(site, dir, sources, &taken, progress)
        });
    }
}
