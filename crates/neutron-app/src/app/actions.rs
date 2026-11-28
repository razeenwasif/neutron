use super::*;

impl NeutronApp {
    pub(super) fn apply(&mut self, ctx: &egui::Context, action: Action) {
        let active = self.workspace.active_tab_id();

        match action {
            Action::RunCommand(id) => self.run_command(ctx, id),
            Action::Navigate(id) => {
                if let Some(tab) = active {
                    self.navigate(tab, id);
                }
            }
            Action::Back => {
                if let Some(tab) = active {
                    let target = self
                        .workspace
                        .tabs
                        .get_mut(&tab)
                        .and_then(|t| t.history.back().cloned());
                    if let Some(id) = target {
                        self.reload(tab, id);
                    }
                }
            }
            Action::Forward => {
                if let Some(tab) = active {
                    let target = self
                        .workspace
                        .tabs
                        .get_mut(&tab)
                        .and_then(|t| t.history.forward().cloned());
                    if let Some(id) = target {
                        self.reload(tab, id);
                    }
                }
            }
            Action::Up if self.in_columns() => self.column_step(-1),
            Action::Up => {
                if let Some(tab) = active {
                    let parent = self
                        .workspace
                        .tabs
                        .get(&tab)
                        .and_then(|t| t.location().parent());
                    if let Some(p) = parent {
                        self.navigate(tab, p);
                    }
                }
            }
            Action::Refresh => {
                if let Some(tab) = active {
                    if let Some(loc) = self.workspace.tabs.get(&tab).map(|t| t.location().clone()) {
                        self.vcs.invalidate(&loc);
                    }
                    self.refresh_in_place(tab);
                }
                // The other columns are not the tab's listing, so a refresh
                // of the tab alone would leave them as they were.
                self.columns.clear();
                if !cfg!(test) {
                    let probe_needed = self.workspace.active_tab().is_some_and(|t| {
                        let loc = t.location();
                        *loc == NodeId::Home || loc.as_remote().is_some()
                    });
                    if probe_needed {
                        self.start_remote_volume_probe();
                    }
                }
            }

            Action::Activate(idx) => self.activate(idx),

            Action::Select { idx, mode } => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.apply(&t.list, idx, mode);
                }
            }
            Action::Marquee { from, to, additive } => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.select_span(&t.list, from, to, additive);
                }
            }

            Action::DragPaths(paths) => {
                if let Some(tab) = self.workspace.active_tab_id() {
                    self.drag_paths(tab, paths);
                }
            }
            Action::DragOut => {
                if !self.refuse_without_a_path("drag files out of") {
                    self.drag_out();
                }
            }
            Action::BeginRename if self.on_server() => self.begin_rename(ctx),
            Action::BeginRename => {
                if !self.refuse_without_a_path("rename anything in") && !self.begin_batch_rename() {
                    self.begin_rename(ctx);
                }
            }
            Action::BatchRename(dir, pairs) => self.batch_rename(dir, pairs),
            Action::RenameTyped(text) => {
                if let Some(r) = self.rename.as_mut() {
                    r.text = text;
                }
            }
            Action::RenameCommit(text) => {
                if let Some(edit) = self.rename.as_mut() { edit.text = text; }
                self.commit_rename(ctx);
            },
            Action::RenameCancel => {
                self.close_rename(ctx);
            }
            Action::NewFolder if self.on_server() => self.remote_new_folder(),
            Action::NewFolder => {
                if !self.refuse_without_a_path("create folders in") {
                    self.new_folder();
                }
            }
            Action::NewFile(template) if self.on_server() => self.remote_new_file(template),
            Action::NewFile(template) => {
                if !self.refuse_without_a_path("create files in") {
                    self.new_file(template);
                }
            }
            Action::OpenTemplates => {
                // Made by the first reading of it, at startup.
                if let (Some(dir), Some(tab)) = (crate::new_file::templates_dir(), self.workspace.active_tab_id()) {
                    self.navigate(tab, NodeId::path(&dir));
                }
            }
            Action::RefreshIndex => self.index.refresh_index(),
            Action::ExtractSelection => {
                if !self.refuse_without_a_path("extract archives in") {
                    self.extract_selection();
                }
            }
            Action::CompressSelection => {
                if !self.refuse_without_a_path("compress anything in") {
                    self.compress_selection();
                }
            }
            Action::Share | Action::AirDrop if self.in_archive() => {
                // Taken out into the cache first; shared when they are there.
                let Some((archive, entries)) = self.archive_selection() else { return };
                let at = matches!(action, Action::Share).then(|| {
                    let at = ctx.input(|i| i.pointer.latest_pos()).unwrap_or_else(|| ctx.content_rect().center());
                    (at.x, at.y)
                });
                let shares = self.share_tx.clone();
                let notices = self.notice_tx.clone();
                let wake = self.ctx.clone();
                self.sta.submit(move || match crate::archives::materialise(&archive, &entries) {
                    Ok(paths) => {
                        let _ = shares.send((paths, at));
                        wake.request_repaint();
                    }
                    Err(e) => report(&notices, format!("Could not share that: {e}")),
                });
            }
            Action::Share => {
                let Some(paths) = self.chosen_paths() else { return };
                // Where the pointer is — over the menu item just chosen — or
                // the middle of the window, for a keystroke.
                let at = ctx
                    .input(|i| i.pointer.latest_pos())
                    .unwrap_or_else(|| ctx.content_rect().center());
                if let Err(e) = neutron_shell::share::share(&paths, (at.x, at.y)) {
                    self.notices.error(e);
                }
            }
            Action::AirDrop => {
                let Some(paths) = self.chosen_paths() else { return };
                if let Err(e) = neutron_shell::share::airdrop(&paths) {
                    self.notices.error(e);
                }
            }
            Action::EditTags => {
                let Some(paths) = self.chosen_paths() else { return };
                // The first item's tags, to start from.
                let text = paths
                    .first()
                    .map(|p| neutron_shell::tags::read(p).into_iter().map(|t| t.name).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default();
                self.tag_editor = Some(TagEditor { paths, text, focused: false });
            }
            Action::SetTags(paths, names) => {
                let notices = self.notice_tx.clone();
                let done = self.refresh_tx.clone();
                let tab = self.workspace.active_tab_id();
                self.sta.submit(move || {
                    if let Err(e) = neutron_shell::tags::set_names(&paths, &names) {
                        report(&notices, format!("The tags could not be set: {e}"));
                    }
                    if let Some(tab) = tab {
                        let _ = done.send(tab);
                    }
                });
            }
            Action::ToggleTag { color, on } => {
                let Some(paths) = self.chosen_paths() else { return };
                let notices = self.notice_tx.clone();
                let done = self.refresh_tx.clone();
                let tab = self.workspace.active_tab_id();
                self.sta.submit(move || {
                    if let Err(e) = neutron_shell::tags::toggle_color(&paths, color, on) {
                        report(&notices, format!("The tag could not be changed: {e}"));
                    }
                    if let Some(tab) = tab {
                        let _ = done.send(tab);
                    }
                });
            }
            Action::ShowInFolder(path) => {
                let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else { return };
                let Some(tab) = self.workspace.active_tab_id() else { return };
                let name = name.to_string_lossy().into_owned();
                self.navigate(tab, NodeId::path(folder));
                if let Some(t) = self.workspace.tabs.get_mut(&tab) {
                    t.reselect = Some(crate::workspace::Reselect { names: vec![name.clone()], cursor: Some(name) });
                }
            }
            Action::Undo | Action::Redo => {
                let undo = matches!(action, Action::Undo);
                if self.history.busy {
                    self.notices.info("Still putting the last change back");
                    return;
                }
                let Some(record) = (if undo { self.history.take_undo() } else { self.history.take_redo() }) else {
                    self.notices.info(if undo { "Nothing to undo" } else { "Nothing to redo" });
                    return;
                };
                self.history.busy = true;
                let events = self.undo_tx.clone();
                let wake = self.ctx.clone();
                self.sta.submit(move || {
                    let done = crate::undo::reverse(&record.step);
                    let step = done.inverse.unwrap_or(crate::undo::Step::Created(Vec::new()));
                    let record = crate::undo::Record { label: record.label, step };
                    // What macOS would not let Neutron take out of the Trash,
                    // shown in the Finder, which can.
                    if !done.blocked.is_empty() {
                        let _ = neutron_shell::open::reveal(&done.blocked);
                    }
                    let _ = events.send(UndoEvent::Reversed { undo, record, failed: done.failed, blocked: done.blocked });
                    wake.request_repaint();
                });
            }
            Action::StopTransfer(id) => {
                if let Some(job) = self.transfers.iter().find(|j| j.id == id) {
                    job.stop();
                }
            }
            Action::CancelArchiveJob => {
                if let Some(job) = self.archive_job.as_ref() {
                    job.cancel();
                }
            }
            Action::CancelPluginCommand => self.plugins.cancel(),

            Action::OpenConnect => self.connect.open(),
            Action::CloseConnect => self.connect.close(),
            Action::SetConnectField(field, text) => self.connect.set(field, text),
            Action::SetConnectProtocol(protocol) => {
                self.connect.protocol = protocol;
                self.connect.error = None;
            }
            Action::SubmitConnect => self.submit_connect(),
            Action::ReloadSites => self.refresh_sites(SitesOrigin::Requested),
            Action::DismissNotice => self.notices.dismiss(),
            Action::OpenTerminal => self.open_terminal(),
            Action::AskAgent(agent) => self.ask_agent(agent),
            Action::NewWindow => self.new_window(),
            Action::CancelDownload => {
                if let Some(job) = self.download.as_ref() {
                    job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Action::CancelRemoteJob(id) => {
                if let Some(job) = self.remote_jobs.iter().find(|j| j.id == id) {
                    job.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Action::DownloadRemoteToDownloads => self.download_remote_to_downloads(),
            Action::CalculateRemoteSizes(site_id, dirs) => self.request_remote_sizes(&site_id, dirs),
            Action::CopyGitBranch => self.copy_git_branch(ctx),
            Action::RunNtrStatus => self.run_ntr_terminal(false),
            Action::InspectNtrCi => self.run_ntr_terminal(true),
            Action::OpenStorageLens(node) => {
                let label = node.display_name();
                self.storage_lens.open(node, label, &self.sites.sites, ctx);
            }
            Action::CloseStorageLens => self.storage_lens.close(),
            Action::DrillStorageLens(node, label) => {
                self.storage_lens.open(node, label, &self.sites.sites, ctx);
            }
            Action::UpStorageLens => {
                if let Some((parent, label)) = self.storage_lens.parent_root() {
                    self.storage_lens.open(parent, label, &self.sites.sites, ctx);
                }
            }
            Action::RescanStorageLens => self.storage_lens.rescan(&self.sites.sites, ctx),
            Action::TrashFromStorageLens(path) => {
                let target = [path.clone()];
                match neutron_shell::fileops::trash(&target) {
                    Ok(_) => {
                        self.notices.info(format!(
                            "Moved {} to Trash",
                            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
                        ));
                        self.storage_lens.rescan(&self.sites.sites, ctx);
                    }
                    Err(e) => self.notices.error(format!("Could not move to Trash: {e}")),
                }
            }
            Action::ToggleComparePanes => {
                if self.compare_panes {
                    self.compare_panes = false;
                } else {
                    if self.compare_pair().is_none() {
                        self.workspace.split(Axis::Horizontal);
                        self.notices.info("Compare mode on — open another folder in the second pane");
                    }
                    self.compare_panes = true;
                }
            }
            Action::SyncPaneToOther { src, dst } => self.sync_pane_to_other(src, dst, ctx),
            Action::OpenLayouts => {
                self.layout_dialog.open = true;
                self.layout_dialog.focus_input = true;
            }
            Action::CloseLayouts => self.layout_dialog.open = false,
            Action::SaveLayout(name) => {
                let saved = self.workspace.persist();
                self.layouts.upsert(name.clone(), saved);
                let snapshot = self.layouts.clone();
                let notices = self.notice_tx.clone();
                let wake = self.ctx.clone();
                self.sta.submit(move || {
                    if let Err(e) = crate::layouts::save(&snapshot) {
                        report(&notices, format!("Could not save layouts: {e}"));
                    }
                    wake.request_repaint();
                });
                self.notices.info(format!("Saved layout “{name}”"));
            }
            Action::ApplyLayout(name) => {
                if let Some(entry) = self.layouts.get(&name).cloned()
                    && let Some(ws) = Workspace::restore(&entry.workspace)
                {
                    self.layout_dialog.open = false;
                    self.compare_panes = false;
                    self.workspace = ws;
                    let tabs: Vec<(TabId, NodeId)> = self
                        .workspace
                        .tabs
                        .values()
                        .map(|t| (t.id, t.location().clone()))
                        .collect();
                    for (tab, location) in tabs {
                        self.reload(tab, location);
                    }
                    self.notices.info(format!("Switched to layout “{name}”"));
                }
            }
            Action::DeleteLayout(name) => {
                if self.layouts.remove(&name) {
                    let snapshot = self.layouts.clone();
                    let notices = self.notice_tx.clone();
                    let wake = self.ctx.clone();
                    self.sta.submit(move || {
                        if let Err(e) = crate::layouts::save(&snapshot) {
                            report(&notices, format!("Could not save layouts: {e}"));
                        }
                        wake.request_repaint();
                    });
                    self.notices.info(format!("Deleted layout “{name}”"));
                }
            }
            Action::ReloadPlugins => {
                self.plugins.reload();
                self.palette = commands::catalogue(&self.plugins.palette_entries());
                self.refresh_finder();
            }
            Action::TogglePreview => {
                self.preview.open = !self.preview.open;
                if !self.preview.open {
                    // Dropped rather than kept for next time: the texture is
                    // the largest thing the pane owns, and a hidden pane
                    // holding a few megabytes of an image nobody is looking at
                    // is the kind of thing that is never noticed and never
                    // freed.
                    self.previews.clear();
                    self.preview.showing = None;
                    self.preview.content = crate::preview::Preview::Nothing;
                    self.preview.texture = None;
                }
            }

            Action::Clip { cut } if self.on_server() => self.remote_clip(cut),
            Action::Clip { cut: false } if self.in_archive() => self.copy_out_of_archive(),
            Action::Clip { cut: false } if self.in_tag_view() => {
                self.remote_clipboard = None;
                if let Some(paths) = self.chosen_paths() {
                    let notices = self.notice_tx.clone();
                    self.sta.submit(move || {
                        if let Err(e) = neutron_shell::clipboard::write(&paths, neutron_shell::fileops::Transfer::Copy) {
                            report(&notices, format!("Could not copy: {e}"));
                        }
                    });
                }
            }
            Action::Clip { cut } => {
                if !self.refuse_without_a_path(if cut { "cut from" } else { "copy from" }) {
                    self.remote_clipboard = None;
                    self.clip_selection(cut);
                }
            }
            Action::Paste if self.on_server() => self.remote_paste(),
            Action::Paste if !self.on_server() && self.remote_clipboard.is_some() => {
                if !self.refuse_without_a_path("paste into") {
                    self.paste_remote_to_local();
                }
            }
            Action::Paste => {
                if !self.refuse_without_a_path("paste into") {
                    self.paste();
                }
            }

            Action::ClearSelection => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.clear();
                }
            }
            Action::SelectAll => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.select_all(&t.list);
                }
            }

            Action::SortBy(column) => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    // Clicking the active column flips direction; a different
                    // column starts ascending, as in Explorer.
                    if t.view.sort.column == column {
                        t.view.sort.order = t.view.sort.order.flipped();
                    } else {
                        t.view.sort.column = column;
                        t.view.sort.order = neutron_core::SortOrder::Ascending;
                    }
                    // Re-sort in place. The selection holds storage indices, so
                    // it survives untouched — no reload needed.
                    neutron_core::sort::sort(&mut t.list, t.view.sort);
                    t.view.scroll_to = t.selection.cursor();
                }
                // Last-opened dates are read only when asked for, so a first
                // sort by them reloads to fetch them.
                if column == neutron_core::SortColumn::Opened {
                    if let Some(tab) = active {
                        self.refresh_in_place(tab);
                    }
                }
            }
            Action::GroupBy(group) => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.view.sort.group = group;
                    neutron_core::sort::sort(&mut t.list, t.view.sort);
                    t.view.scroll_to = t.selection.cursor();
                }
                if group == neutron_core::GroupBy::Opened {
                    if let Some(tab) = active {
                        self.refresh_in_place(tab);
                    }
                }
            }

            Action::ToggleAllSizes => {
                self.all_sizes = !self.all_sizes;
                if self.all_sizes {
                    let mut folders = self.shown_folders(|_| true);
                    folders.extend(self.columns.folders());
                    self.sizer.request(folders);
                    self.request_remote_sizes_for_visible_tabs();
                } else {
                    // Forgotten, and each listing read again so its folders
                    // lose the sizes they were given.
                    self.sizer.clear();
                    self.remote_sizes.clear();
                    self.remote_sizes_pending.clear();
                    self.remote_sizes_version = self.remote_sizes_version.wrapping_add(1);
                    let tabs: Vec<TabId> = self.workspace.tabs.keys().copied().collect();
                    for tab in tabs {
                        self.refresh_in_place(tab);
                    }
                }
            }
            Action::CalculateSizes(folders) => self.sizer.request(folders),
            Action::ToggleSidebar => self.hide_sidebar = !self.hide_sidebar,
            Action::ToggleToolbar => self.hide_toolbar = !self.hide_toolbar,
            // Every tab, as Show Hidden Files is: how big icons are is a
            // preference, not a property of one folder.
            Action::SetTileScale(scale) => {
                for tab in self.workspace.tabs.values_mut() {
                    tab.view.tile_scale = scale;
                }
            }
            Action::ToggleHidden => {
                // Every tab, not just the focused one. "Show hidden files" is
                // an answer to "what do I want to see", not a property of one
                // folder — and with no per-tab indicator, having it on in one
                // tab and off in the next reads as tabs showing different
                // contents for no reason.
                let show = self
                    .workspace
                    .active_tab()
                    .is_some_and(|t| !t.view.show_hidden);
                for tab in self.workspace.tabs.values_mut() {
                    tab.view.show_hidden = show;
                    tab.refilter();
                }
            }

            Action::ToggleView => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.view.view = t.view.view.toggled();
                    // Bring the cursor back into view: a row that was on screen
                    // in the list is on a completely different line of a grid.
                    t.view.scroll_to = t.selection.cursor();
                }
            }
            Action::SetView(mode) => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.view.view = mode;
                    t.view.scroll_to = t.selection.cursor();
                }
            }
            Action::ColumnPick { dir, entry, double } => self.column_pick(dir, entry, double),

            Action::SetFilter(text) => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    if t.view.filter != text {
                        t.view.filter = text;
                        // Re-filtered in place from the listing already in
                        // memory: no reload, no I/O, so this stays instant
                        // while typing even in a 100k-entry directory.
                        t.refilter();
                        // Entries the filter hid are still selected by storage
                        // index but no longer reachable; dropping the selection
                        // avoids a status bar counting rows nobody can see.
                        t.selection.clear();
                    }
                }
            }

            // On a server the delete is permanent and asked about first; the
            // question names the server and the count. See `question_for`.
            Action::Delete { .. } if self.on_server() => self.ask(Action::DeleteOnServer),
            Action::DeleteOnServer => self.remote_delete(),
            Action::EditSite(id) => self.edit_site(&id),
            Action::QuickLook(path) => {
                if let Err(e) = neutron_shell::quicklook::toggle(std::slice::from_ref(&path), 0) {
                    self.notices.error(e);
                }
            }
            Action::Eject(volume) => {
                let name = volume.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let notices = self.notice_tx.clone();
                let wake = self.ctx.clone();
                self.notices.info(format!("Ejecting {name}…"));
                std::thread::spawn(move || {
                    match neutron_shell::places::eject(&volume) {
                        Ok(()) => report_info(&notices, format!("Ejected {name}")),
                        Err(why) => report(&notices, format!("{name} could not be ejected: {why}")),
                    }
                    wake.request_repaint();
                });
            }
            Action::EjectCurrent => {
                let here = self.workspace.active_tab().and_then(|t| t.location().as_path().map(Path::to_path_buf));
                let volume = here.and_then(|here| {
                    self.ejectable.iter().filter(|v| here.starts_with(v)).max_by_key(|v| v.as_os_str().len()).cloned()
                });
                match volume {
                    Some(volume) => self.apply(ctx, Action::Eject(volume)),
                    None => self.notices.info("This folder is not on a disk that can be ejected"),
                }
            }
            Action::OpenAirDrop => {
                let notices = self.notice_tx.clone();
                self.sta.submit(move || {
                    let opened = std::process::Command::new("/usr/bin/open")
                        .arg("/System/Library/CoreServices/Finder.app/Contents/Applications/AirDrop.app")
                        .status();
                    if !opened.is_ok_and(|s| s.success()) {
                        report(&notices, "AirDrop could not be opened".into());
                    }
                });
            }
            Action::OpenInNewTab(id) => self.open_in_new_tab(id),
            Action::OpenInViewer(path) => self.open_in_new_tab(NodeId::Viewer { path }),
            Action::LaunchPath(path) => self.launch(path, neutron_shell::open::Verb::Open),
            Action::ShowOriginal(path) => {
                let found = self.originals_tx.clone();
                let notices = self.notice_tx.clone();
                let wake = self.ctx.clone();
                self.sta.submit(move || {
                    match neutron_shell::fs::original(&path) {
                        Ok(target) => {
                            let _ = found.send(target);
                        }
                        Err(e) => report(&notices, format!("The original cannot be found: {e}")),
                    }
                    wake.request_repaint();
                });
            }
            Action::ExtractEntries => {
                // Out to the cache, then copied beside the archive by an
                // ordinary transfer — so a name already taken there is asked
                // about, and a large entry shows its progress.
                let Some((archive, entries)) = self.archive_selection() else { return };
                let Some(folder) = archive.parent().map(Path::to_path_buf) else { return };
                let Some(tab) = self.workspace.active_tab_id() else { return };
                let planned = self.planned_tx.clone();
                let notices = self.notice_tx.clone();
                let wake = self.ctx.clone();
                self.sta.submit(move || {
                    match crate::archives::materialise(&archive, &entries) {
                        Ok(paths) => {
                            let request = crate::transfers::Request {
                                tab,
                                paths,
                                destination: folder,
                                how: neutron_shell::fileops::Transfer::Copy,
                            };
                            let _ = planned.send(crate::transfers::plan(request));
                        }
                        Err(e) => report(&notices, format!("Could not extract that: {e}")),
                    }
                    wake.request_repaint();
                });
            }
            Action::ExtractWholeArchive => {
                if let Some((archive, _)) = self.workspace.active_tab().and_then(|t| t.location().as_archive()) {
                    let archive = archive.to_path_buf();
                    if let Some(format) = neutron_archive::format_of(&archive) {
                        let ctx = self.ctx.clone();
                        self.start_archive_job(move || crate::archive_ops::extract(archive, format, ctx));
                    }
                }
            }
            Action::SetUpDrive => {
                let notices = self.notice_tx.clone();
                let done = self.drive_tx.clone();
                let wake = self.ctx.clone();
                self.sta.submit(move || {
                    use neutron_cloud::google::{DriveState, GoogleDrive};
                    // Filled in since the last look: Drive is ready to sign in.
                    let state = GoogleDrive::new().state();
                    if state != DriveState::NotConfigured {
                        let _ = done.send(state);
                        wake.request_repaint();
                        return;
                    }
                    let Some(file) = neutron_cloud::config::config_path() else {
                        report(&notices, "There is no settings folder to keep google.json in".into());
                        return;
                    };
                    if !file.exists() {
                        let template = "{\n  \"client_id\": \"\",\n  \"client_secret\": \"\"\n}\n";
                        let made = file.parent().map_or(Ok(()), std::fs::create_dir_all)
                            .and_then(|()| std::fs::write(&file, template));
                        if let Err(e) = made {
                            report(&notices, format!("google.json could not be created: {e}"));
                            return;
                        }
                    }
                    let _ = neutron_shell::open::reveal(std::slice::from_ref(&file));
                    report_info(
                        &notices,
                        "Put your Google client's client_id and client_secret in google.json, \
                         then choose Set up Google Drive again"
                            .into(),
                    );
                    wake.request_repaint();
                });
            }
            Action::CopyPath => {
                let paths = match self.selection_paths() {
                    Some((_, _, paths)) => paths,
                    None => self
                        .workspace
                        .active_tab()
                        .and_then(|t| t.location().as_path().map(|p| vec![p.to_path_buf()]))
                        .unwrap_or_default(),
                };
                if !paths.is_empty() {
                    let text = paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
                    let notices = self.notice_tx.clone();
                    let count = paths.len();
                    self.sta.submit(move || match neutron_shell::clipboard::write_text(&text) {
                        Ok(()) => report_info(&notices, if count == 1 { "Copied the path".into() } else { format!("Copied {count} paths") }),
                        Err(e) => report(&notices, format!("The path could not be copied: {e}")),
                    });
                }
            }
            Action::OpenTerminalAt(dir) => self.open_program(dir, crate::term::Program::shell(), "Terminal"),
            Action::GetInfoSelection => {
                if let Some(paths) = self.chosen_paths() {
                    self.apply(ctx, Action::GetInfo(paths));
                }
            }
            Action::GetInfo(paths) => self.infos.open(paths, self.viewport, ctx),
            Action::PutBack(paths) => {
                let notices = self.notice_tx.clone();
                let undo = self.undo_tx.clone();
                let done_tx = self.refresh_tx.clone();
                let tab = self.workspace.active_tab_id();
                let wake = self.ctx.clone();
                self.sta.submit(move || {
                    let mut moved = Vec::new();
                    let mut failed = Vec::new();
                    let mut blocked = false;
                    for path in &paths {
                        match crate::trash::put_back(path) {
                            crate::trash::PutBack::Done(from, to) => moved.push((from, to)),
                            crate::trash::PutBack::Blocked => blocked = true,
                            crate::trash::PutBack::Failed(why) => failed.push(why),
                        }
                    }
                    if blocked {
                        report(&notices, "macOS keeps the Trash from apps without Full Disk Access. To put things back \
                            from Neutron, turn it on for Neutron in the System Settings just opened — or use \
                            File › Put Back in the Finder."
                            .to_owned());
                        let _ = std::process::Command::new("/usr/bin/open").arg(crate::trash::FULL_DISK_ACCESS).status();
                    } else if !failed.is_empty() {
                        report(&notices, format!("Not put back — {}", failed.join("; ")));
                    } else if !moved.is_empty() {
                        let n = moved.len();
                        report_info(&notices, format!("Put back {n} item{}", if n == 1 { "" } else { "s" }));
                    }
                    let _ = undo.send(UndoEvent::Did(crate::undo::Record {
                        label: "Put Back".into(),
                        step: crate::undo::Step::Relocated(moved),
                    }));
                    if let Some(tab) = tab {
                        let _ = done_tx.send(tab);
                    }
                    wake.request_repaint();
                });
            }
            Action::NewSmartFolder => {
                let editing = self.workspace.active_tab().and_then(|t| match t.location() {
                    NodeId::Search { name, query } => {
                        Some(crate::searches::Dialog::editing(name, &neutron_core::query::Query::decode(query)))
                    }
                    _ => None,
                });
                let folder = self.workspace.active_tab().and_then(|t| t.location().as_path().map(Path::to_path_buf));
                self.search_dialog = Some(editing.unwrap_or_else(|| crate::searches::Dialog::new(folder)));
            }
            Action::EditSearch(name) => {
                if let Some(saved) = self.saved_searches.iter().find(|s| s.name == name) {
                    let query = neutron_core::query::Query::decode(&saved.query);
                    self.search_dialog = Some(crate::searches::Dialog::editing(&saved.name, &query));
                }
            }
            Action::RemoveSearch(name) => {
                self.saved_searches.retain(|s| s.name != name);
                self.save_searches();
            }
            Action::RunSearch { query, save_as, replacing } => {
                let name = save_as.clone().unwrap_or_default();
                if let Some(name) = save_as {
                    let saved = crate::searches::Saved { name: name.clone(), query: query.encode() };
                    let at = self
                        .saved_searches
                        .iter()
                        .position(|s| Some(&s.name) == replacing.as_ref() || s.name == name);
                    match at {
                        Some(i) => self.saved_searches[i] = saved,
                        None => self.saved_searches.push(saved),
                    }
                    self.save_searches();
                }
                if let Some(tab) = active {
                    self.navigate(tab, NodeId::Search { name: name.as_str().into(), query: query.encode().into() });
                }
            }
            Action::FinderInfo(paths) => {
                // The Finder's own window, for what Neutron's leaves out:
                // "Open with" for all files of a kind, comments, locking.
                self.finder_script(
                    paths,
                    &["open information window of f"],
                    "Get Info could not be opened",
                    true,
                );
            }
            Action::MakeAlias(paths) => {
                self.finder_script(
                    paths,
                    &["make new alias file at (container of f) to f"],
                    "The alias could not be made",
                    false,
                );
            }
            Action::OpenFullDiskAccess => {
                self.sta.submit(|| {
                    let _ = std::process::Command::new("/usr/bin/open")
                        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
                        .status();
                });
            }
            Action::EmptyTrash => {
                let notices = self.notice_tx.clone();
                let done = self.refresh_tx.clone();
                let tab = self.workspace.active_tab_id();
                self.sta.submit(move || {
                    // The Finder owns the Trash, including its other volumes'
                    // parts; asking it is the only complete way to empty it.
                    let output = std::process::Command::new("/usr/bin/osascript")
                        .args(["-e", "tell application \"Finder\" to empty trash"])
                        .output();
                    match output {
                        Ok(o) if o.status.success() => report_info(&notices, "Emptied the Trash".into()),
                        Ok(o) => report(&notices, format!(
                            "The Trash could not be emptied: {}",
                            String::from_utf8_lossy(&o.stderr).trim()
                        )),
                        Err(e) => report(&notices, format!("The Trash could not be emptied: {e}")),
                    }
                    if let Some(tab) = tab {
                        let _ = done.send(tab);
                    }
                });
            }
            Action::QuickLookSelection if self.in_archive() && !neutron_shell::quicklook::is_open() => {
                // Taken out first; the panel opens when the files are there.
                if let Some((archive, entries)) = self.archive_selection() {
                    self.look_window = Some(self.viewport);
                    let notices = self.notice_tx.clone();
                    self.sta.submit(move || match crate::archives::materialise(&archive, &entries) {
                        Ok(paths) => neutron_shell::quicklook::request(paths),
                        Err(e) => report(&notices, format!("Quick Look could not open that: {e}")),
                    });
                }
            }
            Action::QuickLookSelection => {
                if neutron_shell::quicklook::is_open() {
                    let _ = neutron_shell::quicklook::toggle(&[], 0);
                    self.quick_look_shown = None;
                } else if let Some((paths, index, key)) = self.quick_look_subject() {
                    self.look_window = Some(self.viewport);
                    match neutron_shell::quicklook::toggle(&paths, index) {
                        Ok(()) => self.quick_look_shown = Some(key),
                        Err(e) => self.notices.error(e),
                    }
                }
            }
            Action::Delete { permanent } if self.in_tag_view() => self.delete_selection(permanent),
            Action::Delete { permanent } => {
                if !self.refuse_without_a_path("delete from") {
                    self.delete_selection(permanent);
                }
            }

            Action::SetGridColumns { group, columns } => {
                if let Some(tab) = self
                    .workspace
                    .groups
                    .get(&group)
                    .and_then(|g| g.active_tab())
                {
                    if let Some(t) = self.workspace.tabs.get_mut(&tab) {
                        t.view.columns = columns;
                    }
                }
            }

            Action::ContextMenu { idx, pos } => self.context_menu(idx, pos),

            Action::DropFiles { group, paths, .. } if self.on_server() => {
                if let Some(tab) = self.workspace.groups.get(&group).and_then(|g| g.active_tab()) {
                    self.remote_upload(tab, paths);
                }
            }
            Action::DropFiles { group, paths, copy } => {
                if !self.refuse_without_a_path("copy files into") {
                    self.drop_files(group, paths, copy);
                }
            }
            Action::DropInto { dir, paths, copy } => self.drop_into(dir, paths, copy),

            Action::ToggleFinder(mode) => {
                self.finder.toggle(mode);
                self.refresh_finder();
            }
            Action::CloseFinder => {
                self.remote_search_site = None;
                self.finder.close();
            }
            Action::SetFinderNeedle(text) => {
                self.finder.needle = text;
                // Reset rather than clamp: the previous highlight referred to a
                // different result set entirely.
                self.finder.cursor = 0;
                self.refresh_finder();
            }
            Action::MoveFinderCursor(delta) => self.finder.move_cursor(delta),
            Action::ActivateFinderRow(index) => self.activate_finder_row(index),
            Action::StartIndexer => self.index.start_helper(),

            Action::ConnectDrive => self.connect_drive(),
            Action::TogglePin => self.toggle_pin(),
            Action::ToggleAddress => self.toggle_address(ctx),
            Action::OpenRecent(path) => self.open_local(path),
            Action::ForgetRecent(path) => self.forget_recent(path),
            Action::SetAddress(text) => {
                if let Some((_, current)) = self.address.as_mut() {
                    *current = text;
                }
            }
            Action::SubmitAddress => self.submit_address(),
            Action::CancelAddress => self.address = None,
            Action::Ask(then) => self.ask(*then),
            Action::SidebarMenu(id, at) => self.sidebar_menu(id, at),
            Action::SignOutDrive => self.sign_out_drive(),
            Action::ForgetServer => self.forget_server(),

            Action::FocusFilter => {
                if let Some(group) = self.workspace.focused_group().map(|g| g.id) {
                    ctx.memory_mut(|m| m.request_focus(crate::header::filter_id(group)));
                }
            }

            Action::ToggleTheme => {
                let next = self.theme_mode.next();
                self.set_theme(ctx, next);
            }
            Action::SetTheme(mode) => {
                self.set_theme(ctx, mode);
            }

            Action::MoveCursor { delta, extend } => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    // In a grid, Up and Down move by a whole row. The layout is
                    // the only thing that knows how wide a row is, so it
                    // records the column count during paint.
                    let step = if t.view.view == neutron_ui::file_list::ViewMode::Grid {
                        (t.view.columns.max(1)) as isize
                    } else {
                        1
                    };
                    t.selection.move_cursor(&t.list, delta * step, extend);
                    t.view.scroll_to = t.selection.cursor();
                }
            }
            Action::MoveWithinRow { delta, .. } if self.in_columns() => self.column_step(delta),
            Action::MoveWithinRow { delta, extend } => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    if matches!(t.view.view, neutron_ui::file_list::ViewMode::Grid | neutron_ui::file_list::ViewMode::Gallery) {
                        t.selection.move_cursor(&t.list, delta, extend);
                        t.view.scroll_to = t.selection.cursor();
                    }
                }
            }

            Action::MoveTo { pos, extend } => {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.move_to(&t.list, pos, extend);
                    t.view.scroll_to = t.selection.cursor();
                }
            }

            Action::TypeAhead(c) => self.type_ahead(c),

            // --- workspace ---
            Action::FocusGroup(g) => self.workspace.focused = g,
            Action::ReorderTab { group, from, to } => {
                self.workspace.reorder_tab(group, from, to);
                // The drag is over; without this the next release anywhere
                // would read as a drop.
                self.tab_drag = None;
            }
            Action::FocusNextPane => self.workspace.focus_next_pane(),
            Action::SetRatio { target, ratio } => {
                self.workspace.layout.set_divider_ratio(target, ratio);
            }
            Action::Split(axis) => {
                tracing::debug!(?axis, panes = self.workspace.layout.len(), "split pane");
                if let Some(tab) = self.workspace.split(axis) {
                    if let Some(loc) = self.workspace.tabs.get(&tab).map(|t| t.location().clone()) {
                        self.reload(tab, loc);
                    }
                }
            }
            Action::NewTab(group) => {
                // Opens at the current pane's location rather than at home:
                // "new tab here" is what a file manager wants, and going home
                // would discard the context the user is working in.
                let location = self
                    .workspace
                    .groups
                    .get(&group)
                    .and_then(|g| g.active_tab())
                    .and_then(|t| self.workspace.tabs.get(&t))
                    .map(|t| t.location().clone())
                    .unwrap_or_else(|| {
                        NodeId::Path(
                            neutron_shell::places::home().unwrap_or_else(|| PathBuf::from(r"C:\")),
                        )
                    });
                let view = self.workspace.view_for_new_tab();
                let tab = self.workspace.open_tab(group, location.clone(), view);
                self.workspace.focused = group;
                tracing::debug!(?group, ?tab, tabs = self.workspace.tabs.len(), "new tab");
                self.reload(tab, location);
            }
            Action::SelectTab { group, index } => {
                if let Some(g) = self.workspace.groups.get_mut(&group) {
                    if index < g.tabs.len() {
                        g.active = index;
                    }
                }
                self.workspace.focused = group;
            }
            Action::CloseTab { group, index } => {
                let tab = self
                    .workspace
                    .groups
                    .get(&group)
                    .and_then(|g| g.tabs.get(index).copied());
                if let Some(tab) = tab {
                    self.close_tab(ctx, tab);
                }
            }
            // The last tab of a window closes the window, as Command-W does in
            // the Finder — unless it is the only window, which stays open
            // rather than quitting the application on a tab shortcut.
            Action::CloseActiveTab
                if self.workspace.tabs.len() == 1
                    && (self.viewport != egui::ViewportId::ROOT || !self.windows.is_empty()) =>
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Action::CloseActiveTab => {
                if let Some(tab) = active {
                    self.close_tab(ctx, tab);
                }
            }
            Action::FocusTabIndex(i) => self.workspace.focus_tab_index(i),
            Action::CycleTab(delta) => self.workspace.cycle_tab(delta),
            Action::GoKnown(name) => {
                match self.known_folders.iter().find(|p| p.name == name).map(|p| p.id.clone()) {
                    Some(id) => self.apply(ctx, Action::Navigate(id)),
                    None => self.notices.info(format!("There is no {name} folder here")),
                }
            }
            Action::Duplicate => {
                if !self.refuse_without_a_path("duplicate anything in") {
                    if let Some((tab, dir, paths)) = self.selection_paths() {
                        let request = crate::transfers::Request {
                            tab,
                            paths,
                            destination: dir,
                            how: neutron_shell::fileops::Transfer::Copy,
                        };
                        // Beside itself, so never a clash: each is named as a copy.
                        self.start_transfer(request, neutron_shell::transfer::Clash::KeepBoth);
                    }
                }
            }

            Action::BeginTabDrag(d) => self.tab_drag = Some(d),
            Action::EndTabDrag => self.tab_drag = None,
            Action::DropTab(target) => {
                if let Some(drag) = self.tab_drag.take() {
                    let tab = self
                        .workspace
                        .groups
                        .get(&drag.group)
                        .and_then(|g| g.tabs.get(drag.index).copied());
                    if let Some(tab) = tab {
                        self.workspace.move_tab(tab, target);
                    }
                }
            }
        }
    }

    pub(super) fn close_tab(&mut self, ctx: &egui::Context, tab: TabId) {
        if !self.workspace.close_tab(tab, &mut self.loader) {
            // That was the last tab anywhere — closing it closes the window,
            // matching every other tabbed application.
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub(super) fn activate(&mut self, idx: usize) {
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(t) = self.workspace.tabs.get(&tab) else {
            return;
        };
        if idx >= t.list.len() {
            return;
        }

        let kind = t.list.kind(idx);
        let name = t.list.name(idx).to_owned();

        // A cloud listing's target is the provider's own object id. It has to
        // be read *before* the generic target branch below, which treats a
        // non-path target as a shell parsing name — so a Drive folder id went
        // to `SHParseDisplayName`, which is why no folder inside Drive would
        // open and why activating a Drive file navigated to nothing. Present
        // since M7; there was never a cloud branch here at all.
        if let NodeId::Cloud { provider, .. } = t.location() {
            let provider = *provider;
            // Every cloud row carries one. A listing without targets is a bug
            // in the provider, not something to guess a path from.
            let Some((target, _)) = t.list.target(idx) else {
                self.notices
                    .error(format!("{name} has no id Neutron can open"));
                return;
            };
            let target = target.to_owned();
            if kind.is_container() {
                // The name travels with the id: it is what titles the tab, and
                // asking the provider for it later would be an API call on the
                // paint thread.
                self.navigate(tab, NodeId::cloud(provider, target, name));
            } else {
                match provider {
                    CloudProviderId::GoogleDrive => self.start_drive_download(target, name),
                    // OneDrive syncs to a real directory, so it is a `Path`
                    // node and never reaches here. Named rather than
                    // wildcarded so that changing it is a compile error, not a
                    // OneDrive file quietly fetched through the Drive API.
                    CloudProviderId::OneDrive => self
                        .notices
                        .error(format!("Neutron cannot open {name} from OneDrive this way")),
                }
            }
            return;
        }

        // A shell listing tells us where each row leads, because its children
        // cannot be addressed by joining a name to the parent — "This PC"
        // contains `C:\`, and a Control Panel item has no path at all.
        if let Some((target, is_path)) = t.list.target(idx) {
            let target = target.to_owned();
            if !kind.is_container() && is_path {
                self.open_local(PathBuf::from(target));
                return;
            }
            let node = if is_path {
                NodeId::Path(PathBuf::from(target))
            } else {
                NodeId::shell(target, name)
            };
            self.navigate(tab, node);
            return;
        }

        // A server's rows are addressed by joining a name to a POSIX path,
        // and the name has already been through `safe_name` on the way into
        // the listing — but this is where it becomes a path, so it is checked
        // again here rather than trusted.
        if let Some((site, dir)) = t.location().as_remote() {
            let (site, dir) = (site.to_owned(), dir.to_owned());
            let container = kind.is_container();
            let Ok(full) = neutron_remote::path::join(&dir, &name) else {
                self.notices.error(format!("{name} is not a name this can open"));
                return;
            };
            if container {
                self.navigate(tab, NodeId::remote(site.as_str(), full.as_str()));
            } else {
                self.start_download(&site, full, name);
            }
            return;
        }

        // Inside an archive: a folder is gone into, a file is taken out into
        // the cache and opened from there, as a file on a server is.
        if let Some((archive, inner)) = t.location().as_archive() {
            let (archive, entry) = (archive.to_path_buf(), crate::archives::join(inner, &name));
            if kind.is_container() {
                self.navigate(tab, NodeId::archive(archive, entry));
            } else {
                let notices = self.notice_tx.clone();
                self.sta.submit(move || {
                    let opened = crate::archives::materialise(&archive, std::slice::from_ref(&entry))
                        .and_then(|paths| match crate::openers::app_for(&paths[0]) {
                            Some(app) => crate::openers::open_with(&app, &paths[0]),
                            None => neutron_shell::open::shell_execute(&paths[0], neutron_shell::open::Verb::Open, 0),
                        });
                    if let Err(e) = opened {
                        report(&notices, format!("{name} could not be opened: {e}"));
                    }
                });
            }
            return;
        }

        let Some(base) = t.location().as_path() else {
            return;
        };
        let full = base.join(&name);
        // An archive opens as a folder: browsed in place, as in Explorer,
        // rather than unpacked beside itself. Extract is in its menu for that.
        if !kind.is_container() && neutron_archive::format_of(&full).is_some() {
            self.navigate(tab, NodeId::archive(full, ""));
            return;
        }
        #[cfg(target_os = "macos")]
        if full.extension().is_some_and(|ext| ext == "app") && kind.is_container() {
            self.open_local(full);
            return;
        }

        if kind.is_container() {
            self.navigate(tab, NodeId::Path(full));
            return;
        }

        // A zip is an ordinary file to the filesystem and a folder to the
        // shell. Only the shell knows which extensions have a namespace handler
        // registered, so it is asked rather than guessed at from a list of
        // extensions that would go stale.
        let probe = full.clone();
        let wake = self.ctx.clone();
        let open_as = self.open_as_tx.clone();
        let tab_id = tab;
        self.sta.submit(move || {
            let as_folder =
                neutron_shell::shell_ns::is_shell_container(&probe.to_string_lossy());
            let _ = open_as.send((tab_id, probe, as_folder));
            wake.request_repaint();
        });
    }

    /// Applies whatever a worker made of `sites.json`.
    pub(super) fn drain_sites(&mut self) {
        while let Ok(reply) = self.sites_rx.try_recv() {
            match reply.sites {
                Ok(sites) => {
                    self.sites = sites;
                    self.sites_error = None;
                }
                Err(e) => {
                    self.sites_error = Some(e.clone());
                    self.notices.error(format!("sites.json: {e}"));
                }
            }
            // The loader holds the connections, so it has to hear about this
            // too — a site whose host or port changed must not keep answering
            // from the session opened against the old one.
            self.loader.reload_sites();
            self.columns.reload_sites();
            if !cfg!(test) {
                self.start_remote_volume_probe();
            }

            match reply.origin {
                SitesOrigin::Background => {}
                SitesOrigin::Requested => {
                    let count = self.sites.sites.len();
                    self.notices.info(match count {
                        0 => "No servers saved".to_owned(),
                        1 => "1 server saved".to_owned(),
                        n => format!("{n} servers saved"),
                    });
                }
                SitesOrigin::Dialog(Ok(())) => self.connect.close(),
                // Kept open: the password is only in the dialog, and closing it
                // would make the user type it again to find out what went
                // wrong.
                SitesOrigin::Dialog(Err(reason)) => self.connect.error = Some(reason),
            }
        }
    }

    /// Probes SFTP servers in the background for their internal/external SSDs.
    pub(super) fn start_remote_volume_probe(&mut self) {
        self.last_remote_probe = Instant::now();
        let sftp_sites: Vec<neutron_remote::Site> = self
            .sites
            .sites
            .iter()
            .filter(|s| s.protocol == neutron_remote::Protocol::Sftp)
            .cloned()
            .collect();
        self.remote_drives.retain(|id, _| sftp_sites.iter().any(|s| s.id == *id));
        if sftp_sites.is_empty() {
            return;
        }
        for site in sftp_sites {
            if !self.probing_sites.insert(site.id.clone()) {
                continue;
            }
            let tx = self.remote_drives_tx.clone();
            let wake = self.ctx.clone();
            let _ = std::thread::Builder::new()
                .name(format!("neutron-vols-{}", site.id))
                .spawn(move || {
                    let places: Vec<Place> = match crate::remote::connect_checked(&site)
                        .and_then(|mut transport| transport.volumes().map_err(|e| e.to_string()))
                    {
                        Ok(vols) => vols
                            .into_iter()
                            .map(|v| Place {
                                name: v.name,
                                id: NodeId::remote(site.id.as_str(), v.path.as_str()),
                                kind: neutron_core::places::PlaceKind::Drive,
                                capacity: Some(neutron_core::places::Capacity {
                                    total: v.total,
                                    free: v.free,
                                }),
                            })
                            .collect(),
                        Err(_) => Vec::new(),
                    };
                    let _ = tx.send((site.id, places));
                    wake.request_repaint();
                });
        }
    }

    pub(super) fn drain_remote_drives(&mut self) {
        while let Ok((site_id, places)) = self.remote_drives_rx.try_recv() {
            self.probing_sites.remove(&site_id);
            if places.is_empty() {
                self.remote_drives.remove(&site_id);
            } else {
                self.remote_drives.insert(site_id, places);
            }
        }
    }

    pub(super) fn remote_drive_groups(&self) -> Vec<(String, Vec<Place>)> {
        self.sites
            .sites
            .iter()
            .filter_map(|site| {
                let vols = self.remote_drives.get(&site.id)?;
                (!vols.is_empty()).then(|| (site.display().to_owned(), vols.clone()))
            })
            .collect()
    }

    pub(super) fn sidebar_footer_drives(&self) -> std::borrow::Cow<'_, [Place]> {
        if let Some((site, path)) = self
            .workspace
            .active_tab()
            .and_then(|t| t.location().as_remote())
        {
            if let Some(vols) = self.remote_drives.get(site) {
                if !vols.is_empty() {
                    let norm_path = path.trim_end_matches('/');
                    let best = vols
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| {
                            v.id.as_remote().is_some_and(|(_, vp)| {
                                let vp = vp.trim_end_matches('/');
                                vp.is_empty()
                                    || norm_path == vp
                                    || norm_path.starts_with(&format!("{vp}/"))
                            })
                        })
                        .max_by_key(|(_, v)| v.id.as_remote().map_or(0, |(_, vp)| vp.len()))
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    return std::borrow::Cow::Owned(vec![vols[best].clone()]);
                }
            }
        }
        std::borrow::Cow::Borrowed(&self.drives)
    }

    /// Applies navigations a worker asked for once its work was done.
    pub(super) fn drain_goto(&mut self) {
        let owned = &self.workspace.tabs;
        let wanted = take_mine(&mut self.mail.goto, |(tab, _)| owned.contains_key(tab));
        for (tab, id) in wanted {
            self.navigate(tab, id);
        }
    }

    /// Applies decisions made on an apartment thread about how to open a file.
    pub(super) fn drain_open_as(&mut self) {
        let owned = &self.workspace.tabs;
        let decisions = take_mine(&mut self.mail.open_as, |(tab, ..)| owned.contains_key(tab));
        for (tab, path, as_folder) in decisions {
            if as_folder {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.navigate(tab, NodeId::shell(path.to_string_lossy().into_owned(), name));
            } else {
                self.open_local(path);
            }
        }
    }

    /// Hands the selection to the system as a drag.
    ///
    /// Returns at once. The drag itself blocks an apartment thread until the
    /// drop finishes, which is exactly why it is not on this one — the window
    /// keeps painting for the whole gesture, so the ground goes on drifting and
    /// hover still responds behind the drag image.
    pub(super) fn drag_out(&mut self) {
        let Some((tab_id, _, paths)) = self.selection_paths() else {
            return;
        };
        self.drag_paths(tab_id, paths);
    }

    /// Drags `paths` out of the application, re-listing `tab_id` after a drop
    /// where the system cannot say whether they left.
    pub(super) fn drag_paths(&mut self, tab_id: TabId, paths: Vec<PathBuf>) {

        // On the Mac a drag is started on the UI thread and does not block:
        // AppKit runs it from the event loop this thread already owns. On
        // Windows `DoDragDrop` is a modal loop of its own, hence the worker.
        #[cfg(target_os = "macos")]
        {
            let _ = tab_id;
            if let Err(e) = neutron_shell::dragout::drag(&paths, 0) {
                self.notices.error(format!("Drag failed: {e}"));
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let owner = self.hwnd.unwrap_or(0);
            let wake = self.ctx.clone();
            let done = self.refresh_tx.clone();

            tracing::info!(count = paths.len(), "dragging out");
            let notices = self.notice_tx.clone();
            self.sta.submit(move || {
                match neutron_shell::dragout::drag(&paths, owner) {
                    // Re-listed after any completed drop, not only after a move.
                    // The drag cannot report reliably whether the files left —
                    // Explorer moves them itself and then says it did nothing —
                    // so the listing is the only account that is true.
                    Ok(neutron_shell::dragout::Dropped::Completed) => {
                        let _ = done.send(tab_id);
                    }
                    Ok(neutron_shell::dragout::Dropped::Cancelled) => {}
                    Err(e) => report(&notices, format!("Drag failed: {e}")),
                }
                wake.request_repaint();
            });
        }
    }

    /// Opens the rename box on the focused tab's cursor row.
    pub(super) fn begin_rename(&mut self, ctx: &egui::Context) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(tab) = self.workspace.tabs.get(&tab_id) else {
            return;
        };
        // The cursor rather than the selection: renaming is a single-file
        // operation, and with several selected the cursor is the one the user
        // last touched.
        let Some(idx) = tab.selection.cursor().filter(|&i| i < tab.list.len()) else {
            return;
        };

        let name = tab.list.name(idx).to_owned();
        self.open_rename(ctx, tab_id, idx, name);
    }

    /// Puts a row into a rename box and focuses it.
    pub(super) fn open_rename(&mut self, ctx: &egui::Context, tab: TabId, idx: usize, name: String) {
        // A folder on disk or on a server: the two places a name can be changed.
        let Some(place) = self
            .workspace
            .tabs
            .get(&tab)
            .map(|t| t.location().clone())
            .filter(|l| l.as_path().is_some() || l.as_remote().is_some())
        else {
            return;
        };
        if let Some(tab) = self.workspace.tabs.get_mut(&tab) {
            tab.selection.apply(&tab.list, idx, SelectMode::Replace);
            tab.view.scroll_to = Some(idx);
        }
        self.renames_started += 1;
        let generation = self.renames_started;
        self.rename = Some(RenameEdit {
            tab,
            generation,
            idx,
            place,
            original: name.clone(),
            text: name,
            refresh_pending: false,
        });

        // Focused from here rather than from the widget: the box has not been
        // drawn yet, and without focus the first keystroke would go to the key
        // table instead — where it would be a navigation shortcut.
        ctx.memory_mut(|m| m.request_focus(file_list::rename_field_id(generation)));
        ctx.request_repaint();
    }

    /// Ends the rename and gives the keyboard back.
    ///
    /// Without surrendering focus the field keeps it after the box is gone, and
    /// `egui_wants_keyboard_input` stays true — which switches off every
    /// shortcut in the application until something else is clicked.
    pub(super) fn close_rename(&mut self, ctx: &egui::Context) -> Option<RenameEdit> {
        let edit = self.rename.take()?;
        ctx.memory_mut(|m| m.surrender_focus(file_list::rename_field_id(edit.generation)));
        if edit.refresh_pending {
            self.refresh_in_place(edit.tab);
        }
        Some(edit)
    }

    /// Applies the typed name.
    pub(super) fn commit_rename(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.close_rename(ctx) else {
            return;
        };

        let text = edit.text.trim().to_owned();
        // An unchanged name is not a failure; it is what pressing Enter without
        // typing means, and it should not put a file operation on a worker.
        if text == edit.original || text.is_empty() {
            return;
        }
        if let Some(reason) = neutron_core::naming::rejection(&text) {
            self.notices.error(format!("Cannot rename to {text:?}: {reason}"));
            return;
        }

        // The folder the rename was started in, not the one the tab shows
        // now. Clicking away commits, and a click on Up or a sidebar row
        // navigates in the same frame — reading the tab here would rename a
        // file of the same name in the folder just arrived at.
        if edit.place.as_remote().is_some() {
            self.remote_rename(edit.tab, &edit.place, edit.original, text);
            return;
        }
        let Some(dir) = edit.place.as_path().map(Path::to_path_buf) else {
            return;
        };
        let from = dir.join(&edit.original);

        // The commonest rename mistake, answered here rather than by the shell.
        // `IFileOperation` reports a collision as `0x80070057: The parameter is
        // incorrect` in a modal dialog, which tells the user nothing about what
        // they did or how to fix it.
        //
        // Only when the *name* differs by more than case: `README.md` to
        // `readme.md` is a real rename that Windows reports as already
        // existing, because on this filesystem it is the same file.
        // `to_lowercase` rather than `eq_ignore_ascii_case`: a name differing
        // only in the case of a non-ASCII letter is the same rename, and
        // refusing it would be a bug nobody could work around.
        let same_file_different_case = text.to_lowercase() == edit.original.to_lowercase();

        let owner = self.hwnd.unwrap_or(0);
        let wake = self.ctx.clone();
        let done = self.refresh_tx.clone();
        let tab = edit.tab;

        tracing::info!(from = %from.display(), to = %text, "rename");
        let notices = self.notice_tx.clone();
        let undo = self.undo_tx.clone();
        self.sta.submit(move || {
            // The collision check runs here, not before the submit. It is a
            // `stat`, and a stat is the blocking call the paint thread must
            // never make — on a sleeping drive or a disconnected share it is the
            // stall this architecture exists to refuse.
            if !same_file_different_case && dir.join(&text).exists() {
                report(
                    &notices,
                    format!("{text} is already here — pick another name"),
                );
            } else {
                if let Err(e) = neutron_shell::fileops::rename(&from, &text, owner) {
                    report(&notices, format!("Could not rename to {text:?}: {e}"));
                } else {
                    report_info(&notices, format!("Renamed to {text}"));
                    let _ = undo.send(UndoEvent::Did(crate::undo::Record {
                        label: "Rename".into(),
                        step: crate::undo::Step::Relocated(vec![(from.clone(), dir.join(&text))]),
                    }));
                }
                let _ = done.send(tab);
            }
            wake.request_repaint();
        });
    }

    /// Creates a folder in the focused tab and opens its rename box.
    pub(super) fn new_folder(&mut self) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(dir) = self
            .workspace
            .tabs
            .get(&tab_id)
            .and_then(|t| t.location().as_path())
            .map(|p| p.to_path_buf())
        else {
            return;
        };

        let owner = self.hwnd.unwrap_or(0);
        let wake = self.ctx.clone();
        let done = self.refresh_tx.clone();
        let created = self.created_tx.clone();
        let undo = self.undo_tx.clone();

        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            match neutron_shell::fileops::create_folder(&dir, owner) {
                // The name is reported so the row can be found once the listing
                // is re-read: a new folder appears with its name selected, and
                // that cannot be done without knowing which row it is.
                Ok(name) => {
                    let _ = undo.send(UndoEvent::Did(crate::undo::Record {
                        label: "New Folder".into(),
                        step: crate::undo::Step::Created(vec![dir.join(&name)]),
                    }));
                    let _ = created.send((tab_id, name));
                }
                Err(e) => report(&notices, format!("Could not create a folder here: {e}")),
            }
            let _ = done.send(tab_id);
            wake.request_repaint();
        });
    }

    /// Creates a file in the focused tab and opens its rename box.
    pub(super) fn new_file(&mut self, template: Option<String>) {
        let Some(tab_id) = self.workspace.active_tab_id() else { return };
        let Some(dir) = self.workspace.tabs.get(&tab_id).and_then(|t| t.location().as_path()).map(Path::to_path_buf)
        else {
            return;
        };
        let template = template.and_then(|name| crate::new_file::templates_dir().map(|d| d.join(name)));
        let wake = self.ctx.clone();
        let done = self.refresh_tx.clone();
        let created = self.created_tx.clone();
        let undo = self.undo_tx.clone();
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            match crate::new_file::create(&dir, template.as_deref()) {
                Ok(name) => {
                    let _ = undo.send(UndoEvent::Did(crate::undo::Record {
                        label: "New File".into(),
                        step: crate::undo::Step::Created(vec![dir.join(&name)]),
                    }));
                    let _ = created.send((tab_id, name));
                }
                Err(e) => report(&notices, format!("Could not create a file here: {e}")),
            }
            let _ = done.send(tab_id);
            wake.request_repaint();
        });
    }

    /// Reads the Templates folder again on a worker, for the next menu.
    pub(super) fn refresh_templates(&self) {
        let templates = self.templates.clone();
        self.sta.submit(move || {
            let names = crate::new_file::templates();
            if let Ok(mut held) = templates.lock() {
                *held = names;
            }
        });
    }

    /// The folders listed in this window's tabs, of those `pick` accepts.
    pub(super) fn shown_folders(&self, pick: impl Fn(&crate::workspace::Tab) -> bool) -> Vec<PathBuf> {
        let mut folders = Vec::new();
        for tab in self.workspace.tabs.values().filter(|t| pick(t)) {
            if let Some(dir) = tab.location().as_path() {
                folders.extend(
                    (0..tab.list.len()).filter(|&i| tab.list.kind(i).is_container()).map(|i| dir.join(tab.list.name(i))),
                );
            }
        }
        folders
    }

    /// Takes in folder sizes, and while any are being added up, tells the
    /// worker which folders every window still shows.
    pub(super) fn pump_sizes(&mut self) {
        self.sizer.pump();
        if self.sizer.busy() {
            let shown = std::iter::once(&self.workspace)
                .chain(self.windows.iter().map(|w| &w.workspace))
                .flat_map(|ws| ws.tabs.values())
                .filter_map(|t| t.location().as_path().map(Path::to_path_buf))
                .collect();
            self.sizer.set_shown(shown);
        }
    }

    /// Gives this window's folders the sizes added up since they last looked.
    pub(super) fn apply_folder_sizes(&mut self) {
        let version = self.sizer.version().wrapping_add(self.remote_sizes_version);
        for (id, tab) in self.workspace.tabs.iter_mut() {
            if self.sizes_seen.get(id) == Some(&version) {
                continue;
            }
            self.sizes_seen.insert(*id, version);
            let mut changed = false;
            if let Some(dir) = tab.location().as_path().map(Path::to_path_buf) {
                for i in 0..tab.list.len() {
                    if !tab.list.kind(i).is_container() {
                        continue;
                    }
                    if let Some(size) = self.sizer.get(&dir.join(tab.list.name(i)))
                        && !(tab.list.size_known(i) && tab.list.size(i) == size)
                    {
                        tab.list.set_folder_size(i, size);
                        changed = true;
                    }
                }
            } else if let Some((site, dir)) = tab
                .location()
                .as_remote()
                .map(|(s, d)| (s.to_owned(), d.to_owned()))
            {
                for i in 0..tab.list.len() {
                    if !tab.list.kind(i).is_container() {
                        continue;
                    }
                    if let Ok(full) = neutron_remote::path::join(&dir, tab.list.name(i))
                        && let Some(&size) = self.remote_sizes.get(&(site.clone(), full))
                        && !(tab.list.size_known(i) && tab.list.size(i) == size)
                    {
                        tab.list.set_folder_size(i, size);
                        changed = true;
                    }
                }
            } else {
                continue;
            }
            let by_size = tab.view.sort.column == SortColumn::Size || tab.view.sort.group == neutron_core::GroupBy::Size;
            if changed && by_size {
                neutron_core::sort::sort(&mut tab.list, tab.view.sort);
            }
        }
    }

    pub(super) fn request_remote_sizes(&mut self, site_id: &str, dirs: Vec<String>) {
        let Some(site) = self.sites.get(site_id).cloned() else {
            return;
        };
        if site.protocol != neutron_remote::Protocol::Sftp {
            return;
        }
        for dir in dirs {
            let norm = neutron_remote::path::normalize(&dir);
            let key = (site_id.to_owned(), norm.clone());
            if self.remote_sizes.contains_key(&key) || !self.remote_sizes_pending.insert(key.clone()) {
                continue;
            }
            let site_clone = site.clone();
            let site_key = site_id.to_owned();
            let tx = self.remote_sizes_tx.clone();
            let wake = self.ctx.clone();
            let _ = std::thread::Builder::new()
                .name("neutron-remote-size".into())
                .spawn(move || {
                    let size = crate::remote::connect_checked(&site_clone)
                        .ok()
                        .and_then(|mut t| t.dir_size(&norm).ok());
                    let _ = tx.send((site_key, norm, size));
                    wake.request_repaint();
                });
        }
    }

    pub(super) fn request_remote_sizes_for_visible_tabs(&mut self) {
        let requests: Vec<(String, Vec<String>)> = self
            .workspace
            .tabs
            .values()
            .filter_map(|t| {
                let (site, dir) = t.location().as_remote()?;
                let dirs: Vec<String> = (0..t.list.len())
                    .filter(|&i| t.list.kind(i).is_container())
                    .filter_map(|i| neutron_remote::path::join(dir, t.list.name(i)).ok())
                    .collect();
                (!dirs.is_empty()).then(|| (site.to_owned(), dirs))
            })
            .collect();
        for (site_id, dirs) in requests {
            self.request_remote_sizes(&site_id, dirs);
        }
    }

    pub(super) fn pump_remote_sizes(&mut self) {
        while let Ok((site_id, dir, size)) = self.remote_sizes_rx.try_recv() {
            self.remote_sizes_pending.remove(&(site_id.clone(), dir.clone()));
            if let Some(size) = size {
                self.remote_sizes.insert((site_id, dir), size);
                self.remote_sizes_version = self.remote_sizes_version.wrapping_add(1);
            }
        }
    }

    /// Puts a newly created folder into its rename box, once its row exists.
    pub(super) fn open_pending_rename(&mut self, ctx: &egui::Context) {
        let Some((tab_id, name)) = self.pending_rename.clone() else {
            return;
        };
        let Some(tab) = self.workspace.tabs.get(&tab_id) else {
            self.pending_rename = None;
            return;
        };
        // The listing may not have caught up yet. Left pending, it is retried
        // next frame; the entry appears as soon as the reload lands.
        let Some(idx) = (0..tab.list.len()).find(|&i| tab.list.name(i) == name) else {
            return;
        };

        self.pending_rename = None;
        self.open_rename(ctx, tab_id, idx, name);
    }

    /// The focused tab, the folder it shows, and the paths of its selection.
    ///
    /// Every command that acts on files needs exactly this triple, and each one
    /// used to rebuild it — including the `i < len` guard, which is what stops
    /// a selection left over from a previous listing indexing into the new one.
    pub(super) fn selection_paths(&self) -> Option<(TabId, PathBuf, Vec<PathBuf>)> {
        let tab_id = self.workspace.active_tab_id()?;
        let tab = self.workspace.tabs.get(&tab_id)?;
        let dir = tab.location().as_path()?.to_path_buf();

        // Sorted into the order the rows appear in, because the selection is a
        // set and iterating it directly gives a different order every run. That
        // order is visible: it decides the order files go into a zip, onto the
        // clipboard, and into a plugin's request — and a plugin writing a file
        // per selected item would produce a differently-ordered file each time
        // from the same selection.
        let paths: Vec<PathBuf> = visible_selection(tab)
            .into_iter()
            .map(|i| dir.join(tab.list.name(i)))
            .collect();

        (!paths.is_empty()).then_some((tab_id, dir, paths))
    }

    /// Puts the selection on the clipboard.
    ///
    /// Nothing is copied or moved here — that happens on paste, possibly in
    /// another application, possibly never. All this does is publish the list.
    pub(super) fn clip_selection(&mut self, cut: bool) {
        let Some((_, dir, paths)) = self.selection_paths() else {
            return;
        };

        use neutron_shell::fileops::Transfer;
        let how = if cut { Transfer::Move } else { Transfer::Copy };

        // Marked before the worker runs, not after: the fade is feedback for a
        // keystroke and has to land on the next frame, and a failed clipboard
        // write clears it again below.
        if cut {
            self.cut_dir = Some(dir);
            self.cut_names = paths
                .iter()
                .filter_map(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect();
        } else {
            self.clear_cut_marks();
        }

        let wake = self.ctx.clone();
        let failed = self.clip_fail_tx.clone();

        tracing::info!(count = paths.len(), ?how, "clipboard");
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            if let Err(e) = neutron_shell::clipboard::write(&paths, how) {
                report(&notices, format!("Could not put that on the clipboard: {e}"));
                let _ = failed.send(());
            } else {
                report_info(&notices, if cut {
                    format!("Cut {} items — choose a destination and Paste", paths.len())
                } else { format!("Copied {} items to the clipboard", paths.len()) });
            }
            wake.request_repaint();
        });
    }

    /// Turns the clipboard chords into actions.
    ///
    /// `Ctrl+C` and `Ctrl+X` arrive as `Event::Copy` and `Event::Cut`, because
    /// egui-winit converts them before they become key presses — so there is no
    /// `Key::C` to bind and these have to be read from the event stream.
    ///
    /// `Ctrl+V` does not arrive at all: egui-winit turns it into
    /// `Event::Paste(text)` and drops it when the clipboard holds no text,
    /// which is exactly what a copied *file* looks like. It comes in through
    /// the window subclass instead.
    pub(super) fn clipboard_keys(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        // A menu or a text field is using the keyboard; its own copy and paste
        // win.
        if self.menu.is_some() || self.confirm.is_some() || !self.clash_questions.is_empty() || ctx.egui_wants_keyboard_input() {
            // Still drained, so a press held while a field had focus does not
            // fire the moment focus leaves.
            while self.paste_rx.try_recv().is_ok() {}
            return;
        }

        ctx.input(|i| {
            for event in &i.events {
                match event {
                    egui::Event::Copy => actions.push(Action::Clip { cut: false }),
                    egui::Event::Cut => actions.push(Action::Clip { cut: true }),
                    _ => {}
                }
            }
        });

        // Collapsed to one: several presses between frames still mean one
        // paste, and a repeated file operation is not a harmless duplicate.
        let mut pasted = false;
        while self.paste_rx.try_recv().is_ok() {
            pasted = true;
        }
        if pasted {
            actions.push(Action::Paste);
        }
    }

    /// Starts checked transfers, queues questions about clashes, and reports
    /// transfers that have ended.
    pub(super) fn pump_transfers(&mut self) {
        while let Ok(event) = self.undo_rx.try_recv() {
            match event {
                UndoEvent::Did(record) => self.history.record(record),
                UndoEvent::Reversed { undo, record, failed, blocked } => {
                    self.history.busy = false;
                    let label = record.label.clone();
                    if undo {
                        self.history.undone(record);
                    } else {
                        self.history.redone(record);
                    }
                    let verb = if undo { "Undid" } else { "Redid" };
                    if !blocked.is_empty() {
                        self.notices.error(format!(
                            "macOS lets only apps with Full Disk Access take things out of the Trash. {} selected \
                             in the Finder — choose File › Put Back there, or turn on Full Disk Access for Neutron \
                             in System Settings.",
                            if blocked.len() == 1 { "It is" } else { "They are" }
                        ));
                    } else if failed.is_empty() {
                        self.notices.info(format!("{verb} {label}"));
                    } else {
                        for line in &failed {
                            tracing::warn!("undo: {line}");
                        }
                        self.notices.error(format!("{verb} {label}, except: {}", failed.join("; ")));
                    }
                    if let Some(tab) = self.workspace.active_tab_id() {
                        let _ = self.refresh_tx.send(tab);
                    }
                }
            }
        }

        while let Ok(planned) = self.planned_rx.try_recv() {
            match planned {
                crate::transfers::Planned::Clear(request) => {
                    // Nothing is in the way, so how clashes are settled does
                    // not matter; Keep Both is the one that can never lose a
                    // file to a name that appears in the meantime.
                    self.start_transfer(request, neutron_shell::transfer::Clash::KeepBoth);
                }
                crate::transfers::Planned::Clashes(request, names) => {
                    self.clash_questions.push_back(crate::transfers::Question { request, names });
                }
                crate::transfers::Planned::Refused(why) => self.notices.error(why),
            }
        }

        let mut finished = Vec::new();
        self.transfers.retain(|job| match job.poll() {
            Some(outcome) => {
                finished.push((job.request.clone(), outcome));
                false
            }
            None => true,
        });
        for (request, outcome) in finished {
            if let Ok(report) = &outcome {
                let beside = request.paths.iter().all(|p| p.parent() == Some(request.destination.as_path()));
                let mut record = match request.how {
                    neutron_shell::fileops::Transfer::Move => crate::undo::Record {
                        label: "Move".into(),
                        step: crate::undo::Step::Relocated(report.moved.clone()),
                    },
                    neutron_shell::fileops::Transfer::Copy => crate::undo::Record {
                        label: if beside { "Duplicate".into() } else { "Copy".into() },
                        step: crate::undo::Step::Created(report.moved.iter().map(|(_, to)| to.clone()).collect()),
                    },
                };
                // What it replaced went to the Trash first; undoing puts it
                // back after the transfer is undone.
                if !report.replaced.is_empty() {
                    record.step = crate::undo::Step::Sequence(vec![
                        crate::undo::Step::Trashed(report.replaced.clone()),
                        record.step,
                    ]);
                }
                self.history.record(record);
            }
            let (error, text) = crate::transfers::summary(&request, &outcome);
            if error {
                self.notices.error(text);
            } else {
                self.notices.info(text);
            }
            // The folder watcher sees the change too; this makes it certain.
            let _ = self.refresh_tx.send(request.tab);
        }

        if !self.transfers.is_empty() {
            // The bar moves without input; nothing else would redraw it.
            self.ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    pub(super) fn start_transfer(&mut self, request: crate::transfers::Request, clash: neutron_shell::transfer::Clash) {
        self.next_transfer += 1;
        self.notices.dismiss_info();
        let job = crate::transfers::Job::start(self.next_transfer, request, clash, self.ctx.clone());
        self.transfers.push(job);
    }

    /// Opens Rename Items when more than one item is selected in a folder.
    /// Whether it did.
    pub(super) fn begin_batch_rename(&mut self) -> bool {
        let Some(tab) = self.workspace.active_tab() else { return false };
        let Some(dir) = tab.location().as_path().map(Path::to_path_buf) else { return false };
        let selected = visible_selection(tab);
        if selected.len() < 2 {
            return false;
        }
        let items = selected
            .into_iter()
            .map(|i| {
                let name = tab.list.name(i).to_owned();
                let is_dir = tab.list.kind(i).is_container() && !crate::columns::is_package(Path::new(&name));
                (name, is_dir)
            })
            .collect();
        self.batch_rename = Some(BatchRenameDialog {
            dir,
            items,
            mode: 0,
            find: String::new(),
            with: String::new(),
            text: String::new(),
            after: true,
            base: String::new(),
            start: "1".into(),
            focused: false,
        });
        true
    }

    /// Renames the items on a worker and records the change for undo.
    pub(super) fn batch_rename(&mut self, dir: PathBuf, pairs: Vec<(String, String)>) {
        let notices = self.notice_tx.clone();
        let undo = self.undo_tx.clone();
        let done_tx = self.refresh_tx.clone();
        let tab = self.workspace.active_tab_id();
        let wake = self.ctx.clone();
        tracing::info!(dir = %dir.display(), count = pairs.len(), "batch rename");
        self.sta.submit(move || {
            let (done, failed) = crate::batch_rename::carry_out(&dir, &pairs);
            if !failed.is_empty() {
                report(&notices, format!("Not renamed — {}", failed.join("; ")));
            } else if !done.is_empty() {
                let n = done.len();
                report_info(&notices, format!("Renamed {n} item{}", if n == 1 { "" } else { "s" }));
            }
            let _ = undo.send(UndoEvent::Did(crate::undo::Record {
                label: "Rename".into(),
                step: crate::undo::Step::Relocated(done),
            }));
            if let Some(tab) = tab {
                let _ = done_tx.send(tab);
            }
            wake.request_repaint();
        });
    }

    /// The Rename Items dialog, when it is open: a rule, and what it would
    /// make of the first few names.
    pub(super) fn batch_rename_overlay(&mut self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        let Some(dialog) = self.batch_rename.as_mut() else { return };
        let screen = ui.ctx().content_rect();
        ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(90));
        let _ = ui.allocate_rect(screen, egui::Sense::click());
        let mut outcome: Option<bool> = None;
        let pairs = dialog.pairs();
        let changes = pairs.iter().any(|(old, new)| old != new);
        let problem = crate::batch_rename::problem(&pairs.iter().map(|(_, new)| new.clone()).collect::<Vec<_>>());
        let ready = changes && problem.is_none();
        egui::Area::new(egui::Id::new("neutron-batch-rename"))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                theme::popover(p)
                    .corner_radius(egui::CornerRadius::same(theme::RADIUS_CARD))
                    .inner_margin(egui::Margin::symmetric(20, 18))
                    .show(ui, |ui| {
                        ui.set_width(420.0);
                        let title = format!("Rename {} Items", dialog.items.len());
                        ui.label(egui::RichText::new(title).font(theme::strong(ui.ctx(), 15.0)).color(p.text));
                        ui.add_space(10.0);
                        let before = dialog.mode;
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut dialog.mode, 0, "Replace Text");
                            ui.selectable_value(&mut dialog.mode, 1, "Add Text");
                            ui.selectable_value(&mut dialog.mode, 2, "Format");
                        });
                        if dialog.mode != before {
                            dialog.focused = false;
                        }
                        ui.add_space(8.0);
                        let first = match dialog.mode {
                            0 => {
                                let find = ui.add(egui::TextEdit::singleline(&mut dialog.find).hint_text("Find").desired_width(f32::INFINITY));
                                ui.add(egui::TextEdit::singleline(&mut dialog.with).hint_text("Replace with").desired_width(f32::INFINITY));
                                find
                            }
                            1 => {
                                let text = ui.add(egui::TextEdit::singleline(&mut dialog.text).hint_text("Text to add").desired_width(f32::INFINITY));
                                ui.horizontal(|ui| {
                                    ui.radio_value(&mut dialog.after, false, "Before name");
                                    ui.radio_value(&mut dialog.after, true, "After name");
                                });
                                text
                            }
                            _ => {
                                let base = ui.add(egui::TextEdit::singleline(&mut dialog.base).hint_text("Name, as in \"Holiday\"").desired_width(f32::INFINITY));
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("Start numbers at").color(p.text_muted));
                                    ui.add(egui::TextEdit::singleline(&mut dialog.start).desired_width(60.0));
                                });
                                base
                            }
                        };
                        if !dialog.focused {
                            first.request_focus();
                            dialog.focused = true;
                        }
                        ui.add_space(10.0);
                        for (old, new) in pairs.iter().take(4) {
                            ui.label(egui::RichText::new(format!("{old}  →  {new}")).size(12.0).color(p.text_muted));
                        }
                        if pairs.len() > 4 {
                            ui.label(egui::RichText::new(format!("and {} more", pairs.len() - 4)).size(12.0).color(p.text_muted));
                        }
                        if let Some(problem) = &problem {
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(problem).size(12.0).color(p.danger));
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) && ready {
                            outcome = Some(true);
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            outcome = Some(false);
                        }
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                outcome = Some(false);
                            }
                            if ui.add_enabled(ready, egui::Button::new("Rename")).clicked() {
                                outcome = Some(true);
                            }
                        });
                    });
            });
        match outcome {
            Some(true) => {
                if let Some(dialog) = self.batch_rename.take() {
                    let pairs = dialog.pairs();
                    actions.push(Action::BatchRename(dialog.dir, pairs));
                }
            }
            Some(false) => self.batch_rename = None,
            None => {}
        }
    }

    /// The Edit Tags dialog, when it is open: tag names, separated by commas.
    pub(super) fn tag_editor_overlay(&mut self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        let Some(editor) = self.tag_editor.as_mut() else { return };
        let screen = ui.ctx().content_rect();
        ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(90));
        let _ = ui.allocate_rect(screen, egui::Sense::click());
        let mut outcome: Option<bool> = None;
        let count = editor.paths.len();
        egui::Area::new(egui::Id::new("neutron-tag-editor"))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                theme::popover(p)
                    .corner_radius(egui::CornerRadius::same(theme::RADIUS_CARD))
                    .inner_margin(egui::Margin::symmetric(20, 18))
                    .show(ui, |ui| {
                        ui.set_width(380.0);
                        let title = if count == 1 { "Tags".to_owned() } else { format!("Tags for {count} items") };
                        ui.label(egui::RichText::new(title).font(theme::strong(ui.ctx(), 15.0)).color(p.text));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Separate names with commas. A name of a colour tag gets its colour.")
                                .size(12.0)
                                .color(p.text_muted),
                        );
                        ui.add_space(10.0);
                        let field = egui::TextEdit::singleline(&mut editor.text)
                            .hint_text("Red, Work, Receipts")
                            .desired_width(f32::INFINITY)
                            .show(ui);
                        if !editor.focused {
                            field.response.request_focus();
                            editor.focused = true;
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            outcome = Some(true);
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            outcome = Some(false);
                        }
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                outcome = Some(false);
                            }
                            if ui.button("Set Tags").clicked() {
                                outcome = Some(true);
                            }
                        });
                    });
            });
        match outcome {
            Some(true) => {
                if let Some(editor) = self.tag_editor.take() {
                    let names: Vec<String> = editor
                        .text
                        .split(',')
                        .map(|n| n.trim().to_owned())
                        .filter(|n| !n.is_empty())
                        .collect();
                    actions.push(Action::SetTags(editor.paths, names));
                }
            }
            Some(false) => self.tag_editor = None,
            None => {}
        }
    }

    /// The question about clashing names, when there is one.
    pub(super) fn clash_overlay(&mut self, ui: &mut egui::Ui, p: &Palette) {
        // One question at a time, and never over the delete confirmation.
        if self.confirm.is_some() {
            return;
        }
        let Some(question) = self.clash_questions.front() else {
            return;
        };
        match crate::confirm::ask_clash(ui, p, &question.title(), &question.body()) {
            crate::confirm::ClashAnswer::Waiting => {}
            crate::confirm::ClashAnswer::Cancelled => {
                self.clash_questions.pop_front();
            }
            crate::confirm::ClashAnswer::Chose(clash) => {
                if let Some(question) = self.clash_questions.pop_front() {
                    self.start_transfer(question.request, clash);
                }
            }
        }
    }

    /// Each copy or move under way: what it is doing, a bar, and Stop.
    pub(super) fn transfer_status(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        for job in &self.transfers {
            if ui
                .add(egui::Button::new(egui::RichText::new("Stop").size(11.0)).small())
                .on_hover_text("Stop. The item being copied is removed; what is already done stays done.")
                .clicked()
            {
                actions.push(Action::StopTransfer(job.id));
            }
            ui.label(egui::RichText::new(job.status()).color(p.text_soft()));
            let (rect, _) = ui.allocate_exact_size(egui::vec2(90.0, 6.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, theme::pill(6.0), p.inset);
            let fill = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * job.fraction(), rect.height()));
            if fill.width() >= 1.0 {
                ui.painter().rect_filled(fill, theme::pill(6.0), p.accent);
            }
        }
    }

    /// Sends any tab showing a disk that is no longer mounted to the
    /// dashboard. Run when a rescan of the drives finishes: a disk ejected,
    /// from here or anywhere else, leaves its tabs pointing at nothing.
    pub(super) fn leave_missing_volumes(&mut self) {
        let mounted: Vec<PathBuf> = self.drives.iter().filter_map(|d| d.id.as_path().map(Path::to_path_buf)).collect();
        let stranded: Vec<TabId> = self
            .workspace
            .tabs
            .iter()
            .filter_map(|(id, tab)| {
                let path = tab.location().as_path()?;
                let on_volume = path.starts_with("/Volumes/") && path.components().count() >= 3;
                let gone = on_volume && !mounted.iter().any(|m| m != Path::new("/") && path.starts_with(m));
                gone.then_some(*id)
            })
            .collect();
        for tab in stranded {
            self.navigate(tab, NodeId::Home);
        }
    }

    /// What Quick Look should show: the selection when there are several, or
    /// the item under the cursor, with where in it the cursor is.
    pub(super) fn quick_look_subject(&self) -> Option<(Vec<PathBuf>, usize, QuickLookKey)> {
        let tab = self.workspace.active_tab()?;
        let dir = tab.location().as_path().map(Path::to_path_buf).unwrap_or_default();
        // Somewhere with neither a folder nor rows that lead to files has
        // nothing for Quick Look.
        if tab.location().as_path().is_none() && !self.in_tag_view() {
            return None;
        }
        let cursor = tab.selection.cursor().filter(|&i| i < tab.list.len());
        let selected = visible_selection(tab);
        let (items, index) = if selected.len() > 1 {
            let at = cursor.and_then(|c| selected.iter().position(|&i| i == c)).unwrap_or(0);
            (selected, at)
        } else {
            (vec![cursor?], 0)
        };
        let paths: Vec<PathBuf> = items.iter().map(|&i| row_path(tab, &dir, i)).collect();
        let key = (tab.id, cursor, paths.len());
        Some((paths, index, key))
    }

    /// Keeps an open Quick Look panel on the cursor as it moves.
    pub(super) fn quick_look_follow(&mut self) {
        if self.quick_look_shown.is_none() || !neutron_shell::quicklook::is_open() {
            self.quick_look_shown = None;
            return;
        }
        if let Some((paths, index, key)) = self.quick_look_subject() {
            if self.quick_look_shown != Some(key) {
                neutron_shell::quicklook::follow(&paths, index);
                self.quick_look_shown = Some(key);
            }
        }
    }

    /// Runs `lines` in the Finder for each of `paths`, bound to `f`.
    ///
    /// Paths are passed as arguments, never pasted into the script, so a name
    /// with a quote in it cannot change what the script does. The first use
    /// asks the user to let Neutron control the Finder, which is the system's
    /// own consent and the only way to these.
    pub(super) fn finder_script(&self, paths: Vec<PathBuf>, lines: &[&str], failed: &'static str, bring_forward: bool) {
        if paths.is_empty() {
            return;
        }
        let mut script = vec![
            "on run argv".to_owned(),
            "tell application \"Finder\"".to_owned(),
            "repeat with p in argv".to_owned(),
            "set f to (POSIX file (p as text)) as alias".to_owned(),
        ];
        script.extend(lines.iter().map(|l| (*l).to_owned()));
        script.push("end repeat".to_owned());
        if bring_forward {
            script.push("activate".to_owned());
        }
        script.extend(["end tell".to_owned(), "end run".to_owned()]);
        let notices = self.notice_tx.clone();
        let done = self.refresh_tx.clone();
        let tab = self.workspace.active_tab_id();
        self.sta.submit(move || {
            let mut command = std::process::Command::new("/usr/bin/osascript");
            for line in &script {
                command.arg("-e").arg(line);
            }
            command.arg("--").args(&paths);
            match command.output() {
                Ok(o) if o.status.success() => {}
                Ok(o) => report(&notices, format!("{failed}: {}", String::from_utf8_lossy(&o.stderr).trim())),
                Err(e) => report(&notices, format!("{failed}: {e}")),
            }
            if let Some(tab) = tab {
                let _ = done.send(tab);
            }
        });
    }

    /// Neutron's rows for a file's menu, or for the folder's own when nothing
    /// is selected, as labels and what each does.
    pub(super) fn menu_rows(&self) -> Vec<neutron_core::MenuItem> {
        use neutron_core::MenuItem;
        let mut rows: Vec<MenuItem> = Vec::new();
        let mut actions: Vec<(u32, Action)> = Vec::new();
        let mut next = MENU_ID_OURS;
        let mut row = |label: &str, action: Action, actions: &mut Vec<(u32, Action)>| -> MenuItem {
            next += 1;
            actions.push((next, action));
            MenuItem { id: next, label: label.to_owned(), enabled: true, ..Default::default() }
        };

        let Some(tab) = self.workspace.active_tab() else { return rows };
        let Some(dir) = tab.location().as_path() else { return rows };
        let selected = visible_selection(tab);

        if selected.is_empty() {
            // Moved up beside the system's New Folder; see `context_menu_overlay`.
            rows.push(row("New File", Action::NewFile(None), &mut actions));
            // As last read: reading them again now, for the next time, keeps
            // the disk off the paint thread.
            self.refresh_templates();
            let templates = self.templates.lock().map(|t| t.clone()).unwrap_or_default();
            let mut children: Vec<MenuItem> =
                templates.into_iter().map(|name| row(&name, Action::NewFile(Some(name.clone())), &mut actions)).collect();
            if !children.is_empty() {
                children.push(MenuItem::separator());
            }
            children.push(row("Open Templates Folder", Action::OpenTemplates, &mut actions));
            rows.push(MenuItem { label: "New File from Template".into(), enabled: true, children, ..Default::default() });
            rows.push(MenuItem::separator());
            rows.push(row("Ask Claude Code", Action::AskAgent(crate::agent::Agent::Claude), &mut actions));
            rows.push(row("Ask Antigravity", Action::AskAgent(crate::agent::Agent::Antigravity), &mut actions));
            if let Some(vcs) = self.vcs.peek(tab.location()) {
                if vcs.branch.is_some() {
                    rows.push(row("Copy Git Branch", Action::CopyGitBranch, &mut actions));
                }
                if vcs.ntr.is_some() {
                    rows.push(row("Run Neutrino Status", Action::RunNtrStatus, &mut actions));
                    rows.push(row("Inspect Neutrino CI", Action::InspectNtrCi, &mut actions));
                }
            }
            rows.push(MenuItem::separator());
            let views = [
                ("List", file_list::ViewMode::List),
                ("Grid", file_list::ViewMode::Grid),
                ("Columns", file_list::ViewMode::Columns),
            ];
            let children = views
                .into_iter()
                .map(|(label, mode)| MenuItem { checked: tab.view.view == mode, ..row(label, Action::SetView(mode), &mut actions) })
                .collect();
            rows.push(MenuItem { label: "View as".into(), enabled: true, children, ..Default::default() });
            let sorts = [
                ("Name", neutron_core::SortColumn::Name),
                ("Kind", neutron_core::SortColumn::Kind),
                ("Date Modified", neutron_core::SortColumn::Modified),
                ("Date Created", neutron_core::SortColumn::Created),
                ("Date Added", neutron_core::SortColumn::Added),
                ("Date Last Opened", neutron_core::SortColumn::Opened),
                ("Size", neutron_core::SortColumn::Size),
            ];
            let children = sorts
                .into_iter()
                .map(|(label, column)| MenuItem {
                    checked: tab.view.sort.column == column,
                    ..row(label, Action::SortBy(column), &mut actions)
                })
                .collect();
            rows.push(MenuItem { label: "Sort By".into(), enabled: true, children, ..Default::default() });
            let children = GROUPS
                .into_iter()
                .map(|(label, group)| MenuItem {
                    checked: tab.view.sort.group == group,
                    ..row(label, Action::GroupBy(group), &mut actions)
                })
                .collect();
            rows.push(MenuItem { label: "Group By".into(), enabled: true, children, ..Default::default() });
            let hidden = if tab.view.show_hidden { "Hide Hidden Files" } else { "Show Hidden Files" };
            rows.push(row(hidden, Action::ToggleHidden, &mut actions));
        } else {
            let paths: Vec<PathBuf> = selected.iter().map(|&i| dir.join(tab.list.name(i))).collect();
            if selected.len() > 1 {
                rows.push(row(&format!("Rename {} Items…", selected.len()), Action::BeginRename, &mut actions));
            }
            if let [only] = selected.as_slice() {
                let path = dir.join(tab.list.name(*only));
                rows.push(row("Rename", Action::BeginRename, &mut actions));
                if tab.list.is_alias(*only) {
                    rows.push(MenuItem {
                        accel: neutron_ui::format::shortcut("Cmd+R"),
                        ..row("Show Original", Action::ShowOriginal(path.clone()), &mut actions)
                    });
                }
                if !tab.list.kind(*only).is_container() && crate::viewer::can_show(tab.list.name(*only)) {
                    rows.push(MenuItem {
                        accel: neutron_ui::format::shortcut("Cmd+Alt+O"),
                        ..row("Open in Viewer", Action::OpenInViewer(path.clone()), &mut actions)
                    });
                }
                if crate::columns::is_package(&path) {
                    rows.push(row("Show Package Contents", Action::Navigate(NodeId::path(&path)), &mut actions));
                } else if tab.list.kind(*only).is_container() {
                    rows.push(row("Open in New Tab", Action::OpenInNewTab(NodeId::path(&path)), &mut actions));
                    rows.push(row("Open in Terminal", Action::OpenTerminalAt(path.clone()), &mut actions));
                }
            }
            if paths.iter().any(|p| neutron_archive::format_of(p).is_some()) {
                rows.push(row("Extract", Action::ExtractSelection, &mut actions));
            }
            let folders: Vec<PathBuf> =
                selected.iter().filter(|&&i| tab.list.kind(i).is_container()).map(|&i| dir.join(tab.list.name(i))).collect();
            if !folders.is_empty() && !self.all_sizes {
                rows.push(row("Calculate Size", Action::CalculateSizes(folders), &mut actions));
            }
            if Some(dir) == trash_folder().as_deref() {
                rows.push(row("Put Back", Action::PutBack(paths.clone()), &mut actions));
            }
            rows.push(row("Ask Claude Code", Action::AskAgent(crate::agent::Agent::Claude), &mut actions));
            rows.push(row("Ask Antigravity", Action::AskAgent(crate::agent::Agent::Antigravity), &mut actions));
            if cfg!(target_os = "macos") {
                rows.push(row("Share…", Action::Share, &mut actions));
                rows.push(row("AirDrop…", Action::AirDrop, &mut actions));
                rows.push(MenuItem { accel: neutron_ui::format::shortcut("Cmd+I"), ..row("Get Info", Action::GetInfo(paths.clone()), &mut actions) });
                rows.push(row("Make Alias", Action::MakeAlias(paths), &mut actions));
                // Ticked where every selected item has the tag.
                let bits: Vec<u8> = selected.iter().map(|&i| tab.list.tag_colors(i)).collect();
                let mut children: Vec<MenuItem> = neutron_shell::tags::COLORS
                    .iter()
                    .map(|&(_, color)| {
                        let all = bits.iter().all(|b| b & (1 << (color - 1)) != 0);
                        let name = neutron_shell::tags::color_name(color);
                        MenuItem { checked: all, ..row(&name, Action::ToggleTag { color, on: !all }, &mut actions) }
                    })
                    .collect();
                children.push(MenuItem::separator());
                children.push(row("Edit Tags…", Action::EditTags, &mut actions));
                rows.push(MenuItem { label: "Tags".into(), enabled: true, children, ..Default::default() });
            }
        }
        // Remembered for the answer; see `context_menu_overlay`.
        self.pending_menu_actions.replace(actions);
        rows
    }

    /// The menu for a row inside an archive, or for the archive folder itself.
    pub(super) fn archive_menu(&mut self, on_row: bool, at: egui::Pos2) {
        use neutron_core::MenuItem;
        self.menu_actions.clear();
        let mut items = Vec::new();
        let mut next = MENU_ID_OURS;
        let mut add = |label: &str, action: Action, actions: &mut Vec<(u32, Action)>, default: bool| {
            next += 1;
            actions.push((next, action));
            MenuItem { id: next, label: label.to_owned(), enabled: true, default, ..Default::default() }
        };
        if on_row {
            if let Some(cursor) = self.workspace.active_tab().and_then(|t| t.selection.cursor()) {
                items.push(add("Open", Action::Activate(cursor), &mut self.menu_actions, true));
            }
            items.push(add("Quick Look", Action::QuickLookSelection, &mut self.menu_actions, false));
            items.push(MenuItem::separator());
            items.push(add("Copy", Action::Clip { cut: false }, &mut self.menu_actions, false));
            items.push(add("Extract to the Archive's Folder", Action::ExtractEntries, &mut self.menu_actions, false));
            if cfg!(target_os = "macos") {
                items.push(MenuItem::separator());
                items.push(add("Share…", Action::Share, &mut self.menu_actions, false));
                items.push(add("AirDrop…", Action::AirDrop, &mut self.menu_actions, false));
            }
        } else {
            items.push(add("Extract All", Action::ExtractWholeArchive, &mut self.menu_actions, false));
        }
        self.menu = Some(neutron_ui::MenuState::new(items, at));
        self.menu_reply = None;
    }

    /// Whether the focused tab is showing the Trash.
    pub(super) fn in_trash(&self) -> bool {
        self.workspace.active_tab().and_then(|t| t.location().as_path()) == trash_folder().as_deref()
            && trash_folder().is_some()
    }

    /// Whether the focused tab is showing the files with a tag.
    pub(super) fn in_tag_view(&self) -> bool {
        self.workspace.active_tab().is_some_and(|t| matches!(t.location(), NodeId::Tag { .. } | NodeId::Search { .. }))
    }

    /// The real paths of what is selected: joined to the folder, or — in a
    /// list of tagged files, which come from everywhere — each row's own.
    pub(super) fn chosen_paths(&self) -> Option<Vec<PathBuf>> {
        if let Some((_, _, paths)) = self.selection_paths() {
            return Some(paths);
        }
        let tab = self.workspace.active_tab()?;
        if !matches!(tab.location(), NodeId::Tag { .. } | NodeId::Search { .. }) {
            return None;
        }
        let paths: Vec<PathBuf> = visible_selection(tab)
            .into_iter()
            .filter_map(|i| match tab.list.target(i) {
                Some((target, true)) => Some(PathBuf::from(target)),
                _ => None,
            })
            .collect();
        (!paths.is_empty()).then_some(paths)
    }

    /// The menu for a list of tagged files: what works on files wherever they
    /// are, and the way to the folder one is in.
    /// The context menu for a saved server tab, in list or column view.
    pub(super) fn remote_menu(&mut self, on_row: bool, at: egui::Pos2) {
        use neutron_core::MenuItem;
        fn add(
            next: &mut u32,
            label: &str,
            action: Action,
            actions: &mut Vec<(u32, Action)>,
            default: bool,
        ) -> MenuItem {
            *next += 1;
            actions.push((*next, action));
            MenuItem { id: *next, label: label.to_owned(), enabled: true, default, ..Default::default() }
        }
        self.menu_actions.clear();
        let mut next = MENU_ID_OURS;
        let mut items = Vec::new();
        if on_row {
            if let Some(cursor) = self.workspace.active_tab().and_then(|t| t.selection.cursor()) {
                items.push(add(&mut next, "Open", Action::Activate(cursor), &mut self.menu_actions, true));
                items.push(MenuItem::separator());
            }
            self.pin_target = self.menu_pin_row().map(|(row, target)| {
                items.push(row);
                items.push(MenuItem::separator());
                target
            });
            items.push(add(&mut next, "Copy", Action::Clip { cut: false }, &mut self.menu_actions, false));
            items.push(add(&mut next, "Cut", Action::Clip { cut: true }, &mut self.menu_actions, false));
            items.push(add(
                &mut next,
                "Download to Downloads",
                Action::DownloadRemoteToDownloads,
                &mut self.menu_actions,
                false,
            ));
            items.push(MenuItem::separator());
            items.push(add(&mut next, "Rename", Action::BeginRename, &mut self.menu_actions, false));
            items.push(add(
                &mut next,
                "Delete…",
                Action::Ask(Box::new(Action::DeleteOnServer)),
                &mut self.menu_actions,
                false,
            ));
            if let Some((site_id, dir, rfolders)) = self.workspace.active_tab().and_then(|t| {
                let (s, d) = t.location().as_remote()?;
                let dirs: Vec<String> = visible_selection(t)
                    .into_iter()
                    .filter(|&i| t.list.kind(i).is_container())
                    .filter_map(|i| neutron_remote::path::join(d, t.list.name(i)).ok())
                    .collect();
                (!dirs.is_empty()).then(|| (s.to_owned(), d.to_owned(), dirs))
            }) {
                let _ = dir;
                items.push(add(
                    &mut next,
                    "Calculate Size",
                    Action::CalculateRemoteSizes(site_id, rfolders),
                    &mut self.menu_actions,
                    false,
                ));
            }
            items.push(MenuItem::separator());
            items.push(add(&mut next, "New Folder", Action::NewFolder, &mut self.menu_actions, false));
            items.push(add(&mut next, "New File", Action::NewFile(None), &mut self.menu_actions, false));
            items.push(add(&mut next, "Paste", Action::Paste, &mut self.menu_actions, false));
            items.push(MenuItem::separator());
            items.push(add(&mut next, "Open in Terminal", Action::OpenTerminal, &mut self.menu_actions, false));
            items.push(add(&mut next, "Ask Claude Code", Action::AskAgent(crate::agent::Agent::Claude), &mut self.menu_actions, false));
            items.push(add(&mut next, "Ask Antigravity", Action::AskAgent(crate::agent::Agent::Antigravity), &mut self.menu_actions, false));
        } else {
            items.push(add(&mut next, "New Folder", Action::NewFolder, &mut self.menu_actions, false));
            items.push(add(&mut next, "New File", Action::NewFile(None), &mut self.menu_actions, false));
            items.push(add(&mut next, "Paste", Action::Paste, &mut self.menu_actions, false));
            self.pin_target = self.menu_pin_row().map(|(row, target)| {
                items.push(MenuItem::separator());
                items.push(row);
                target
            });
            items.push(MenuItem::separator());
            items.push(add(&mut next, "Open in Terminal", Action::OpenTerminal, &mut self.menu_actions, false));
            items.push(add(&mut next, "Ask Claude Code", Action::AskAgent(crate::agent::Agent::Claude), &mut self.menu_actions, false));
            items.push(add(&mut next, "Ask Antigravity", Action::AskAgent(crate::agent::Agent::Antigravity), &mut self.menu_actions, false));
        }
        self.menu = Some(neutron_ui::MenuState::new(items, at));
        self.menu_reply = None;
    }

    pub(super) fn tag_menu(&mut self, on_row: bool, at: egui::Pos2) {
        use neutron_core::MenuItem;
        fn add(next: &mut u32, label: &str, action: Action, actions: &mut Vec<(u32, Action)>, default: bool) -> MenuItem {
            *next += 1;
            actions.push((*next, action));
            MenuItem { id: *next, label: label.to_owned(), enabled: true, default, ..Default::default() }
        }
        self.menu_actions.clear();
        let paths = self.chosen_paths().unwrap_or_default();
        if !on_row || paths.is_empty() {
            // The search itself can be changed from its empty space.
            if self.workspace.active_tab().is_some_and(|t| matches!(t.location(), NodeId::Search { .. })) {
                let mut next = MENU_ID_OURS;
                let items = vec![add(&mut next, "Edit Search…", Action::NewSmartFolder, &mut self.menu_actions, true)];
                self.menu = Some(neutron_ui::MenuState::new(items, at));
                self.menu_reply = None;
            }
            return;
        }
        let mut next = MENU_ID_OURS;
        let mut items = Vec::new();
        if let Some(cursor) = self.workspace.active_tab().and_then(|t| t.selection.cursor()) {
            items.push(add(&mut next, "Open", Action::Activate(cursor), &mut self.menu_actions, true));
        }
        items.push(add(&mut next, "Quick Look", Action::QuickLookSelection, &mut self.menu_actions, false));
        if let [only] = paths.as_slice() {
            items.push(add(&mut next, "Show in Enclosing Folder", Action::ShowInFolder(only.clone()), &mut self.menu_actions, false));
        }
        items.push(MenuItem::separator());
        items.push(add(&mut next, "Copy", Action::Clip { cut: false }, &mut self.menu_actions, false));
        items.push(add(&mut next, "Get Info", Action::GetInfo(paths.clone()), &mut self.menu_actions, false));
        items.push(add(&mut next, "Share…", Action::Share, &mut self.menu_actions, false));
        let tags = self.tags_submenu(&mut next);
        items.push(tags);
        items.push(MenuItem::separator());
        items.push(add(&mut next, "Move to Trash", Action::Delete { permanent: false }, &mut self.menu_actions, false));
        self.menu = Some(neutron_ui::MenuState::new(items, at));
        self.menu_reply = None;
    }

    /// The Tags submenu for the selection: each colour, ticked when every
    /// selected item has it.
    pub(super) fn tags_submenu(&mut self, next: &mut u32) -> neutron_core::MenuItem {
        use neutron_core::MenuItem;
        let bits: Vec<u8> = self
            .workspace
            .active_tab()
            .map(|t| visible_selection(t).into_iter().map(|i| t.list.tag_colors(i)).collect())
            .unwrap_or_default();
        let mut children: Vec<MenuItem> = neutron_shell::tags::COLORS
            .iter()
            .map(|&(_, color)| {
                let all = !bits.is_empty() && bits.iter().all(|b| b & (1 << (color - 1)) != 0);
                *next += 1;
                self.menu_actions.push((*next, Action::ToggleTag { color, on: !all }));
                MenuItem { id: *next, label: neutron_shell::tags::color_name(color), enabled: true, checked: all, ..Default::default() }
            })
            .collect();
        *next += 1;
        self.menu_actions.push((*next, Action::EditTags));
        children.push(MenuItem::separator());
        children.push(MenuItem { id: *next, label: "Edit Tags…".into(), enabled: true, ..Default::default() });
        MenuItem { label: "Tags".into(), enabled: true, children, ..Default::default() }
    }

    /// Whether the focused tab is inside an archive.
    pub(super) fn in_archive(&self) -> bool {
        self.workspace.active_tab().is_some_and(|t| t.location().as_archive().is_some())
    }

    /// The archive the focused tab is inside, and the selected entries' paths
    /// within it — or the cursor's, with nothing selected.
    pub(super) fn archive_selection(&self) -> Option<(PathBuf, Vec<String>)> {
        let tab = self.workspace.active_tab()?;
        let (archive, inner) = tab.location().as_archive()?;
        let mut rows = visible_selection(tab);
        if rows.is_empty() {
            rows.extend(tab.selection.cursor().filter(|&i| i < tab.list.len()));
        }
        if rows.is_empty() {
            return None;
        }
        let entries = rows.iter().map(|&i| crate::archives::join(inner, tab.list.name(i))).collect();
        Some((archive.to_path_buf(), entries))
    }

    /// Copy inside an archive: the entries are taken out into the cache and
    /// those files go on the clipboard, so a paste anywhere copies them out.
    pub(super) fn copy_out_of_archive(&mut self) {
        let Some((archive, entries)) = self.archive_selection() else {
            return;
        };
        let notices = self.notice_tx.clone();
        let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.sta.submit(move || {
            let copied = crate::archives::materialise(&archive, &entries).and_then(|paths| {
                neutron_shell::clipboard::write(&paths, neutron_shell::fileops::Transfer::Copy).map(|()| paths.len())
            });
            match copied {
                Ok(1) => report_info(&notices, format!("Copied 1 item out of {name} — paste to put it somewhere")),
                Ok(n) => report_info(&notices, format!("Copied {n} items out of {name} — paste to put them somewhere")),
                Err(e) => report(&notices, format!("Could not copy out of {name}: {e}")),
            }
        });
    }

    /// Whether a question or a menu has the window: keys and menu commands
    /// wait until it is answered.
    pub(super) fn modal(&self) -> bool {
        self.confirm.is_some()
            || !self.clash_questions.is_empty()
            || self.menu.is_some()
            || self.connect.open
            || self.tag_editor.is_some()
            || self.batch_rename.is_some()
            || self.search_dialog.is_some()
            || self.storage_lens.is_open()
            || self.layout_dialog.open
    }

    /// What a menu bar item does, now.
    pub(super) fn menu_command(&self, cmd: crate::menubar::Cmd) -> Option<Action> {
        use crate::menubar::Cmd as C;
        use neutron_core::SortColumn;
        let selection = || self.selection_paths().map(|(_, _, paths)| paths);
        Some(match cmd {
            C::NewWindow => Action::NewWindow,
            C::NewTab => Action::NewTab(self.workspace.focused_group()?.id),
            C::NewFolder => Action::NewFolder,
            C::NewFile => Action::NewFile(None),
            C::ShowOriginal => {
                let tab = self.workspace.active_tab()?;
                let dir = tab.location().as_path()?;
                let [only] = visible_selection(tab)[..] else { return None };
                Action::ShowOriginal(dir.join(tab.list.name(only)))
            }
            C::NewSmartFolder => Action::NewSmartFolder,
            C::OpenInViewer => {
                let (_, _, paths) = self.selection_paths()?;
                Action::OpenInViewer(paths.into_iter().next()?)
            }
            C::Open => Action::Activate(self.workspace.active_tab()?.selection.cursor()?),
            C::CloseTab => Action::CloseActiveTab,
            C::GetInfo => Action::GetInfoSelection,
            C::Rename => Action::BeginRename,
            C::Duplicate => Action::Duplicate,
            C::MakeAlias => Action::MakeAlias(selection()?),
            C::QuickLook => Action::QuickLookSelection,
            C::Share => Action::Share,
            C::Compress => Action::CompressSelection,
            C::Extract => Action::ExtractSelection,
            // In the Trash, Command-Delete puts back, as in the Finder.
            C::MoveToTrash if self.in_trash() => Action::PutBack(self.chosen_paths()?),
            C::MoveToTrash => Action::Delete { permanent: false },
            C::DeleteImmediately => Action::Ask(Box::new(Action::Delete { permanent: true })),
            C::Eject => Action::EjectCurrent,
            C::AddToSidebar => Action::TogglePin,
            C::Undo => Action::Undo,
            C::Redo => Action::Redo,
            C::Cut => Action::Clip { cut: true },
            C::Copy => Action::Clip { cut: false },
            C::Paste => Action::Paste,
            C::SelectAll => Action::SelectAll,
            C::CopyPath => Action::CopyPath,
            C::Filter => Action::FocusFilter,
            C::SearchEverywhere => Action::ToggleFinder(Mode::Everything),
            C::FindFile => Action::ToggleFinder(Mode::Files),
            C::CommandPalette => Action::ToggleFinder(Mode::Commands),
            C::AsGrid => Action::SetView(file_list::ViewMode::Grid),
            C::AsList => Action::SetView(file_list::ViewMode::List),
            C::AsColumns => Action::SetView(file_list::ViewMode::Columns),
            C::AsGallery => Action::SetView(file_list::ViewMode::Gallery),
            C::SortName => Action::SortBy(SortColumn::Name),
            C::SortKind => Action::SortBy(SortColumn::Kind),
            C::SortModified => Action::SortBy(SortColumn::Modified),
            C::SortCreated => Action::SortBy(SortColumn::Created),
            C::SortSize => Action::SortBy(SortColumn::Size),
            C::SortAdded => Action::SortBy(SortColumn::Added),
            C::SortOpened => Action::SortBy(SortColumn::Opened),
            C::GroupNone => Action::GroupBy(neutron_core::GroupBy::None),
            C::GroupKind => Action::GroupBy(neutron_core::GroupBy::Kind),
            C::GroupModified => Action::GroupBy(neutron_core::GroupBy::Modified),
            C::GroupAdded => Action::GroupBy(neutron_core::GroupBy::Added),
            C::GroupOpened => Action::GroupBy(neutron_core::GroupBy::Opened),
            C::GroupSize => Action::GroupBy(neutron_core::GroupBy::Size),
            C::ShowPreview => Action::TogglePreview,
            C::ShowHidden => Action::ToggleHidden,
            C::AllSizes => Action::ToggleAllSizes,
            C::ToggleSidebar => Action::ToggleSidebar,
            C::ToggleToolbar => Action::ToggleToolbar,
            C::SplitRight => Action::Split(Axis::Horizontal),
            C::SplitDown => Action::Split(Axis::Vertical),
            C::AddressBar | C::GoToFolder => Action::ToggleAddress,
            C::ToggleTheme => Action::ToggleTheme,
            C::ThemeStratus => Action::SetTheme(ThemeMode::Stratus),
            C::ThemeObsidian => Action::SetTheme(ThemeMode::Obsidian),
            C::ThemeFlux => Action::SetTheme(ThemeMode::Flux),
            C::ThemeFairy => Action::SetTheme(ThemeMode::Fairy),
            C::ThemeAura => Action::SetTheme(ThemeMode::Aura),
            C::ThemeGruvbox => Action::SetTheme(ThemeMode::Gruvbox),
            C::ThemeLight => Action::SetTheme(ThemeMode::Light),
            C::Back => Action::Back,
            C::Forward => Action::Forward,
            C::Enclosing => Action::Up,
            C::Dashboard => Action::Navigate(NodeId::Home),
            C::Home => Action::GoKnown("Home"),
            C::Desktop => Action::GoKnown("Desktop"),
            C::Documents => Action::GoKnown("Documents"),
            C::Downloads => Action::GoKnown("Downloads"),
            C::Applications => Action::GoKnown("Applications"),
            C::Trash => Action::Navigate(NodeId::path(&trash_folder()?)),
            C::Connect => Action::OpenConnect,
            C::AirDrop => Action::OpenAirDrop,
            C::Terminal => Action::OpenTerminal,
            C::AskClaude => Action::AskAgent(crate::agent::Agent::Claude),
            C::AskAntigravity => Action::AskAgent(crate::agent::Agent::Antigravity),
            C::NextTab => Action::CycleTab(1),
            C::PreviousTab => Action::CycleTab(-1),
            C::NextPane => Action::FocusNextPane,
        })
    }

    /// Tells the menu bar which items are available and which are ticked.
    pub(super) fn publish_menu_state(&self, ctx: &egui::Context) {
        if !cfg!(target_os = "macos") {
            return;
        }
        use crate::menubar::Cmd as C;
        let mut disabled = std::collections::HashSet::new();
        let mut checked = std::collections::HashSet::new();
        if self.modal() {
            disabled.extend(C::all().map(C::tag));
            neutron_shell::menubar::set_state(disabled, checked);
            return;
        }
        // A text field has the keyboard: its editing keys must reach it.
        let typing = ctx.egui_wants_keyboard_input();
        let tab = self.workspace.active_tab();
        let selected = tab.map(visible_selection).unwrap_or_default();
        let dir = tab.and_then(|t| t.location().as_path());
        let on_server = tab.is_some_and(|t| t.location().as_remote().is_some());
        let archive = match (tab, dir) {
            (Some(t), Some(d)) => selected.iter().any(|&i| neutron_archive::format_of(&d.join(t.list.name(i))).is_some()),
            _ => false,
        };
        let ejectable = dir.is_some_and(|d| self.ejectable.iter().any(|v| d.starts_with(v)));
        let tabs = self.workspace.focused_group().map_or(0, |g| g.tabs.len());
        for cmd in C::all() {
            let off = match cmd {
                c if typing && c.edits_text() => true,
                C::Undo => self.history.busy || self.history.undo_label().is_none(),
                C::Redo => self.history.busy || self.history.redo_label().is_none(),
                C::Open | C::OpenInViewer | C::GetInfo | C::Rename | C::Duplicate | C::MakeAlias | C::QuickLook | C::Share | C::Compress
                | C::MoveToTrash | C::DeleteImmediately | C::Cut | C::Copy => selected.is_empty(),
                C::Extract => !archive,
                // Off unless one alias is selected, so Command-R falls through
                // to refreshing the folder.
                C::ShowOriginal => !(selected.len() == 1 && tab.is_some_and(|t| t.list.is_alias(selected[0]))),
                C::NewFolder | C::NewFile | C::Paste | C::AddToSidebar => dir.is_none() && !on_server,
                C::Terminal | C::AskClaude | C::AskAntigravity => dir.is_none() && !on_server,
                C::Back => !tab.is_some_and(|t| t.history.can_go_back()),
                C::Forward => !tab.is_some_and(|t| t.history.can_go_forward()),
                C::Eject => !ejectable,
                C::NextTab | C::PreviousTab => tabs < 2,
                _ => false,
            };
            if off {
                disabled.insert(cmd.tag());
            }
        }
        if let Some(t) = tab {
            checked.insert(match t.view.view {
                file_list::ViewMode::Grid => C::AsGrid,
                file_list::ViewMode::List => C::AsList,
                file_list::ViewMode::Columns => C::AsColumns,
                file_list::ViewMode::Gallery => C::AsGallery,
            }.tag());
            checked.insert(match t.view.sort.column {
                neutron_core::SortColumn::Name => C::SortName,
                neutron_core::SortColumn::Kind => C::SortKind,
                neutron_core::SortColumn::Modified => C::SortModified,
                neutron_core::SortColumn::Created => C::SortCreated,
                neutron_core::SortColumn::Size => C::SortSize,
                neutron_core::SortColumn::Added => C::SortAdded,
                neutron_core::SortColumn::Opened => C::SortOpened,
            }.tag());
            checked.insert(match t.view.sort.group {
                neutron_core::GroupBy::None => C::GroupNone,
                neutron_core::GroupBy::Kind => C::GroupKind,
                neutron_core::GroupBy::Modified => C::GroupModified,
                neutron_core::GroupBy::Added => C::GroupAdded,
                neutron_core::GroupBy::Opened => C::GroupOpened,
                neutron_core::GroupBy::Size => C::GroupSize,
            }.tag());
            if t.view.show_hidden {
                checked.insert(C::ShowHidden.tag());
            }
        }
        if self.preview.open {
            checked.insert(C::ShowPreview.tag());
        }
        if self.all_sizes {
            checked.insert(C::AllSizes.tag());
        }
        checked.insert(match self.theme_mode {
            ThemeMode::Stratus => C::ThemeStratus,
            ThemeMode::Obsidian => C::ThemeObsidian,
            ThemeMode::Flux => C::ThemeFlux,
            ThemeMode::Fairy => C::ThemeFairy,
            ThemeMode::Aura => C::ThemeAura,
            ThemeMode::Gruvbox => C::ThemeGruvbox,
            ThemeMode::Light => C::ThemeLight,
        }.tag());
        neutron_shell::menubar::set_state(disabled, checked);
        // "Undo Move to Trash", as the Finder names it.
        let mut titles = std::collections::HashMap::new();
        titles.insert(C::Undo.tag(), self.history.undo_label().map_or("Undo".to_owned(), |l| format!("Undo {l}")));
        titles.insert(C::Redo.tag(), self.history.redo_label().map_or("Redo".to_owned(), |l| format!("Redo {l}")));
        if self.in_trash() {
            titles.insert(C::MoveToTrash.tag(), "Put Back".to_owned());
        } else {
            titles.insert(C::MoveToTrash.tag(), "Move to Trash".to_owned());
        }
        titles.insert(C::ToggleSidebar.tag(), if self.hide_sidebar { "Show Sidebar" } else { "Hide Sidebar" }.to_owned());
        titles.insert(C::ToggleToolbar.tag(), if self.hide_toolbar { "Show Toolbar" } else { "Hide Toolbar" }.to_owned());
        neutron_shell::menubar::set_titles(titles);
    }

    pub(super) fn clear_cut_marks(&mut self) {
        self.cut_dir = None;
        self.cut_names.clear();
    }

    /// Pastes the clipboard into the focused tab's folder.
    ///
    /// Reading the clipboard is itself a blocking call — another process may
    /// hold it — so reading it and checking the destination happen on the
    /// worker. What comes back is a checked transfer, or a question about
    /// clashing names; see `pump_transfers`.
    pub(super) fn paste(&mut self) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(dir) = self
            .workspace
            .tabs
            .get(&tab_id)
            .and_then(|t| t.location().as_path())
            .map(|p| p.to_path_buf())
        else {
            return;
        };

        // A cut is spent by the paste that consumes it, whatever the outcome:
        // the files have either moved or the paste failed, and in both cases
        // leaving rows faded would be a lie about a clipboard we no longer own.
        self.clear_cut_marks();

        let wake = self.ctx.clone();
        let notices = self.notice_tx.clone();
        let planned = self.planned_tx.clone();
        self.sta.submit(move || {
            let Some((paths, how)) = neutron_shell::clipboard::read() else {
                report_info(&notices, "No files on the clipboard — copy or cut files first".into());
                wake.request_repaint();
                return;
            };
            // On the worker, so a stat is fine here. A cut that has already
            // been pasted is the usual case: the clipboard still names the
            // files, but they have moved, and "into itself" was the wrong
            // thing to say about it.
            if !paths.iter().any(|p| p.exists()) {
                report_info(
                    &notices,
                    "The files on the clipboard are no longer there — a cut can only be pasted once".into(),
                );
                wake.request_repaint();
                return;
            }
            if !neutron_shell::clipboard::can_paste_into(&paths, &dir) {
                report(
                    &notices,
                    "Cannot paste a folder into itself".to_owned(),
                );
                wake.request_repaint();
                return;
            }

            // Checked here, carried out by a job of its own: see
            // `transfers`. The listing is refreshed when the job ends.
            let request = crate::transfers::Request { tab: tab_id, paths, destination: dir, how };
            let _ = planned.send(crate::transfers::plan(request));
            wake.request_repaint();
        });
    }

    /// Sends the focused tab's selection to the Recycle Bin.
    ///
    /// The listing is refreshed when the operation finishes rather than
    /// optimistically: the shell may have been refused, the user may have
    /// cancelled at the confirmation prompt, and some of a batch can fail while
    /// the rest succeeds. Re-reading the directory is the only account of what
    /// actually happened that is guaranteed to be true.
    pub(super) fn delete_selection(&mut self, permanent: bool) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(paths) = self.chosen_paths() else {
            return;
        };
        self.dispose(tab_id, paths, permanent);
    }

    /// Moves `paths` to the Trash, or deletes them for good, then re-lists
    /// `tab_id`.
    pub(super) fn dispose(&mut self, tab_id: TabId, paths: Vec<PathBuf>, permanent: bool) {
        let disposal = if permanent {
            neutron_shell::fileops::Disposal::Permanent
        } else {
            neutron_shell::fileops::Disposal::Recycle
        };
        let owner = self.hwnd.unwrap_or(0);
        let ctx = self.ctx.clone();
        let done = self.refresh_tx.clone();

        tracing::info!(count = paths.len(), ?disposal, "deleting");
        let notices = self.notice_tx.clone();
        let undo = self.undo_tx.clone();
        self.sta.submit(move || {
            // To the Trash, the way that says where each item went, so Undo
            // can put them back. A permanent delete has nothing to undo.
            let outcome = if permanent {
                neutron_shell::fileops::delete(&paths, disposal, owner)
            } else {
                neutron_shell::fileops::trash(&paths).map(|went| {
                    let items: Vec<(PathBuf, PathBuf)> = paths
                        .iter()
                        .cloned()
                        .zip(went)
                        .filter_map(|(was, went)| went.map(|w| (was, w)))
                        .collect();
                    // For Put Back, which the system's record does not serve.
                    crate::trash::remember(&items);
                    let _ = undo.send(UndoEvent::Did(crate::undo::Record {
                        label: "Move to Trash".into(),
                        step: crate::undo::Step::Trashed(items),
                    }));
                })
            };
            if let Err(e) = outcome {
                report(&notices, format!("Delete failed: {e}"));
            } else {
                report_info(&notices, format!("{} {} items",
                    if permanent { "Permanently deleted" } else if cfg!(target_os = "macos") { "Moved to Trash:" } else { "Recycled" }, paths.len()));
            }
            // Refresh regardless of the outcome, for the reason above.
            let _ = done.send(tab_id);
            ctx.request_repaint();
        });
    }

    /// A drag over this window: lights the folder or pane it would land in,
    /// and on the drop sends the files there. A folder under the pointer — a
    /// row, a place in the sidebar — wins over the pane around it.
    pub(super) fn resolve_drop(&mut self, ctx: &egui::Context, p: &Palette, actions: &mut Vec<Action>) {
        let targets = neutron_ui::drop::take(ctx);
        #[cfg(test)]
        {
            self.drop_debug = targets.clone();
        }
        if !neutron_ui::drop::active(ctx) || self.modal() {
            self.spring = None;
            return;
        }
        let Some(at) = self.drop_at else { return };
        let pane = self.drop_panes.borrow().iter().find(|(_, r)| r.contains(at)).copied();
        // Later offers are drawn on top.
        let hit = targets.iter().rev().find(|t| t.rect.contains(at));
        // A name in a list is inside the folder its pane shows.
        let dir = hit.and_then(|t| {
            if t.path.is_absolute() {
                return Some(t.path.clone());
            }
            let (group, _) = pane?;
            let tab = self.workspace.groups.get(&group)?.active_tab()?;
            Some(self.workspace.tabs.get(&tab)?.location().as_path()?.join(&t.path))
        });

        // Spring-loading: a drag held on a folder opens it, so files can be
        // carried several folders down in one drag. The Trash is somewhere to
        // drop things, not to go.
        let held = dir.as_ref().filter(|d| trash_folder().as_ref() != Some(*d));
        let rested = match (&mut self.spring, held) {
            (Some((was, since, _)), Some(now)) if was == now => since.elapsed(),
            (spring, Some(now)) => {
                *spring = Some((now.clone(), Instant::now(), false));
                Duration::ZERO
            }
            (spring, None) => {
                *spring = None;
                Duration::ZERO
            }
        };
        if rested >= SPRING_DELAY
            && let Some((dir, _, sprung @ false)) = &mut self.spring
        {
            *sprung = true;
            // In the pane it was rested in; a place in the sidebar opens in the
            // focused one.
            // A tab shows itself; anything else opens in the pane it was
            // rested in, or, from the sidebar, the focused one.
            if let Some((group, index)) = hit.and_then(|t| t.tab) {
                actions.push(Action::SelectTab { group: GroupId(group), index });
            } else {
                let group = pane.filter(|(_, r)| hit.is_some_and(|t| r.contains_rect(t.rect))).map(|(g, _)| g);
                if let Some(group) = group.filter(|g| *g != self.workspace.focused) {
                    actions.push(Action::FocusGroup(group));
                }
                actions.push(Action::Navigate(NodeId::path(dir.clone())));
            }
        }
        // It blinks just before it opens, as the Finder's does.
        let blink = rested >= SPRING_DELAY.saturating_sub(SPRING_BLINK)
            && rested < SPRING_DELAY
            && (rested.as_millis() / 80) % 2 == 0;

        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("neutron-drop")));
        match (hit, &dir, pane) {
            (Some(t), Some(_), _) => {
                painter.rect_filled(t.rect, t.radius, theme::with_alpha(p.accent, if blink { 110 } else { 40 }));
                painter.rect_stroke(t.rect, t.radius, egui::Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
            }
            // The only place the accent outlines a whole pane, which is what
            // makes "this one" unmistakable while dragging.
            (_, _, Some((_, rect))) => {
                painter.rect_stroke(
                    rect.shrink(2.0),
                    theme::RADIUS_CARD as f32,
                    egui::Stroke::new(2.0, p.accent),
                    egui::StrokeKind::Inside,
                );
            }
            _ => {}
        }
        ctx.request_repaint();

        let (dropped, copy) = ctx.input(|i| {
            (
                i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>(),
                if cfg!(target_os = "macos") { i.modifiers.alt } else { i.modifiers.ctrl },
            )
        });
        if dropped.is_empty() {
            return;
        }
        match (dir, pane) {
            (Some(dir), _) => actions.push(Action::DropInto { dir, paths: dropped, copy }),
            (None, Some((group, _))) => {
                // The drop lands here, and the checks that run on it — is this
                // a folder with a path at all? — read the focused pane.
                if group != self.workspace.focused {
                    actions.push(Action::FocusGroup(group));
                }
                actions.push(Action::DropFiles { group, paths: dropped, copy });
            }
            _ => {}
        }
    }

    /// Copies or moves files dropped from another application into `group`.
    pub(super) fn drop_files(&mut self, group: GroupId, paths: Vec<PathBuf>, copy: bool) {
        let Some(tab_id) = self
            .workspace
            .groups
            .get(&group)
            .and_then(|g| g.active_tab())
        else {
            return;
        };
        let Some(dir) = self
            .workspace
            .tabs
            .get(&tab_id)
            .and_then(|t| t.location().as_path())
            .map(|p| p.to_path_buf())
        else {
            return;
        };
        self.transfer_dropped(tab_id, dir, paths, copy);
    }

    /// Puts files dropped on a folder inside it — or, dropped on the Trash,
    /// in the Trash, as the Finder does.
    pub(super) fn drop_into(&mut self, dir: PathBuf, paths: Vec<PathBuf>, copy: bool) {
        let Some(tab_id) = self.workspace.active_tab_id() else { return };
        if trash_folder().is_some_and(|t| t == dir) {
            self.dispose(tab_id, paths, false);
            return;
        }
        // Not into itself, nor into a folder inside it; and what is already
        // in the folder stays where it is.
        if let Some(inside) = paths.iter().find(|p| dir.starts_with(p)) {
            let name = inside.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.notices.error(format!("{name} cannot be put inside itself"));
            return;
        }
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.parent() != Some(dir.as_path()) || copy).collect();
        if !paths.is_empty() {
            self.transfer_dropped(tab_id, dir, paths, copy);
        }
    }

    /// Plans the copy or move of dropped files into `dir`, on a worker.
    pub(super) fn transfer_dropped(&mut self, tab_id: TabId, dir: PathBuf, paths: Vec<PathBuf>, copy: bool) {
        use neutron_shell::fileops::Transfer;
        let wake = self.ctx.clone();
        let planned = self.planned_tx.clone();
        self.sta.submit(move || {
            // Option forces a copy, as in the Finder. Without it the volumes
            // decide: a move within one disk, a copy onto another. Worked out
            // here because it stats both ends.
            let how = if copy {
                Transfer::Copy
            } else {
                neutron_shell::fileops::default_transfer(&paths[0], &dir)
            };
            tracing::info!(count = paths.len(), ?how, dest = %dir.display(), "drop");
            let request = crate::transfers::Request { tab: tab_id, paths, destination: dir, how };
            let _ = planned.send(crate::transfers::plan(request));
            wake.request_repaint();
        });
    }

    /// Asks the shell for the context menu of the selection.
    ///
    /// Right-clicking a row that is not selected selects it first, as every
    /// file manager does — acting on a hidden selection is how people delete
    /// the wrong thing.
    ///
    /// Returns immediately; the menu appears once the apartment thread has
    /// built it, which is a frame or two later on a folder with slow handlers
    /// installed.
    pub(super) fn context_menu(&mut self, idx: Option<usize>, pos: egui::Pos2) {
        if let Some(idx) = idx {
            let outside = self
                .workspace
                .active_tab()
                .is_some_and(|t| !t.selection.is_selected(idx));
            if outside {
                if let Some(t) = self.workspace.active_tab_mut() {
                    t.selection.apply(&t.list, idx, SelectMode::Replace);
                }
            }
        }

        // Inside an archive the shell has nothing to say — its menu is for
        // files on disk — so the menu is Neutron's own.
        if self.in_archive() {
            self.archive_menu(idx.is_some(), pos);
            return;
        }
        if self.in_tag_view() {
            self.tag_menu(idx.is_some(), pos);
            return;
        }
        if self.on_server() {
            self.remote_menu(idx.is_some(), pos);
            return;
        }

        // Nothing selected means the click was on empty space, and the menu is
        // the folder's own — New, Paste, Sort by. A different COM object, so it
        // is fetched differently below.
        let background = self.selection_paths();
        let (tab_id, dir, paths) = match background {
            Some(found) => found,
            None => {
                let Some(tab_id) = self.workspace.active_tab_id() else {
                    return;
                };
                let Some(dir) = self
                    .workspace
                    .tabs
                    .get(&tab_id)
                    .and_then(|t| t.location().as_path())
                    .map(|p| p.to_path_buf())
                else {
                    // The folder's own menu comes from a shell view object, and
                    // there is no path to bind one to here. Silence read as a
                    // right-click the application had dropped.
                    self.refuse_without_a_path("show a menu for");
                    return;
                };
                (tab_id, dir, Vec::new())
            }
        };

        // A menu already up is replaced, not stacked. Dropping the reply
        // sender releases whichever apartment thread is parked on the old one.
        self.menu = None;
        self.menu_reply = None;
        self.menu_window = Some(self.viewport);

        let ctx_wake = self.ctx.clone();
        let done = self.refresh_tx.clone();
        let offer = self.menu_tx.clone();

        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            // Blocks this apartment for as long as the menu is open. The UI
            // thread keeps painting — that is the entire reason it is here.
            let present = |items: Vec<neutron_core::MenuItem>| {
                // The shell's menu, now plain data. Hand it to the UI and wait
                // for the answer; the COM object stays on this thread.
                let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                if offer.send(PendingMenu { items, pos, reply: reply_tx }).is_err() {
                    return None;
                }
                ctx_wake.request_repaint();
                // A disconnected channel means the UI went away — a closed
                // window, or a second right-click replacing this menu. Either
                // way there is nothing to invoke.
                reply_rx.recv().unwrap_or(None)
            };

            let result = if paths.is_empty() {
                neutron_shell::menu::open_background(&dir, present)
            } else {
                neutron_shell::menu::open(&paths, present)
            };
            if let Err(e) = result {
                report(&notices, format!("The context menu could not be built: {e}"));
            }
            // The command may have renamed, deleted, or created something.
            // There is no way to know which, so re-list unconditionally.
            let _ = done.send(tab_id);
            ctx_wake.request_repaint();
        });
    }

    /// Draws the open context menu and answers the thread waiting on it.
    pub(super) fn context_menu_overlay(&mut self, ctx: &egui::Context, p: &neutron_ui::Palette) {
        // Only in the window it was asked for in.
        let mine = self.menu_window.is_none_or(|w| w == self.viewport);
        let pending_menus = if mine { std::mem::take(&mut self.mail.menus) } else { Vec::new() };
        for pending in pending_menus {
            // Plugin rows are appended here rather than on the apartment
            // thread: this is the side that knows what is installed, and it is
            // also where the ids handed out have to be remembered.
            let mut items = pending.items;

            // The shell's Paste and Duplicate, taken over: Neutron's go
            // through the transfer job, with its progress, Stop, and question
            // about names already taken.
            self.menu_actions.clear();
            for item in items.iter_mut() {
                let ours = match item.label.as_str() {
                    "Paste" => Some((MENU_ID_OURS + 0xF0, Action::Paste)),
                    "Duplicate" => Some((MENU_ID_OURS + 0xF1, Action::Duplicate)),
                    _ => None,
                };
                if let Some((id, action)) = ours {
                    item.id = id;
                    self.menu_actions.push((id, action));
                }
            }
            let mut rows = self.menu_rows();
            // New File and its templates go just after the system's New
            // Folder, where they are looked for.
            if let Some(at) = items.iter().position(|i| i.label == "New Folder")
                && rows.first().is_some_and(|r| r.label == "New File")
            {
                let new: Vec<_> = rows.drain(..2).collect();
                rows.drain(..usize::from(rows.first().is_some_and(|r| r.separator)));
                items.splice(at + 1..at + 1, new);
            }
            if !rows.is_empty() {
                items.push(neutron_core::MenuItem::separator());
                items.extend(rows);
                let built = std::mem::take(&mut *self.pending_menu_actions.borrow_mut());
                self.menu_actions.extend(built);
            }

            // Neutron's own rows come before the plugins', and after the
            // shell's: they are part of the application rather than an
            // extension of it.
            self.pin_target = self.menu_pin_row().map(|(row, target)| {
                items.push(neutron_core::MenuItem::separator());
                items.push(row);
                target
            });

            let has_selection = self.selection_paths().is_some();
            let rows = self.plugins.menu_rows(has_selection);
            if !rows.is_empty() {
                items.push(neutron_core::MenuItem::separator());
                items.extend(rows);
            }

            self.menu = Some(neutron_ui::MenuState::new(items, pending.pos));
            // Replaces any previous reply sender, which releases the thread
            // parked on it as dismissed.
            self.menu_reply = Some(pending.reply);
        }

        let Some(state) = self.menu.as_mut() else {
            return;
        };

        let answer = match neutron_ui::context_menu::show(ctx, p, state) {
            neutron_ui::MenuOutcome::Open => return,
            neutron_ui::MenuOutcome::Chosen(id) => Some(id),
            neutron_ui::MenuOutcome::Dismissed => None,
        };

        self.menu = None;

        // A plugin row's id means nothing to the shell — handing it to
        // `InvokeCommand` would invoke an unrelated verb. So the apartment
        // thread is told the menu was dismissed, and the command runs here.
        // Ours, like a plugin's, mean nothing to the shell.
        let pin = (answer == Some(MENU_ID_PIN))
            .then(|| self.pin_target.take())
            .flatten();
        let new_tab = (answer == Some(MENU_ID_NEW_TAB))
            .then(|| self.tab_target.take())
            .flatten();

        let edit_site = (answer == Some(MENU_ID_EDIT_SITE))
            .then(|| self.edit_site_target.take())
            .flatten();

        let plugin = answer.and_then(|id| self.plugins.menu_choice(id));
        let ours = answer.and_then(|id| {
            let found = self.menu_actions.iter().position(|(i, _)| *i == id);
            found.map(|at| self.menu_actions.swap_remove(at).1)
        });
        self.menu_actions.clear();
        let for_shell = match (&pin, &new_tab, &plugin, &edit_site, &ours) {
            (None, None, None, None, None) => answer,
            _ => None,
        };

        if let Some(reply) = self.menu_reply.take() {
            let _ = reply.send(for_shell);
        }

        if let Some(dir) = pin {
            self.apply_pin(dir);
        }

        if let Some(id) = new_tab {
            self.open_in_new_tab(id);
        }

        if let Some(site) = edit_site {
            self.edit_site(&site);
        }

        if let Some(reference) = plugin {
            let ctx = ctx.clone();
            self.run_plugin(&ctx, &reference);
        }

        if let Some(action) = ours {
            let ctx = ctx.clone();
            self.apply(&ctx, action);
        }
    }

    /// Opens the connect dialog on a saved server, to change it in place.
    pub(super) fn edit_site(&mut self, id: &str) {
        let Some(site) = self.sites.get(id).cloned() else {
            self.notices.error(format!("No saved server called {id:?}"));
            return;
        };
        self.connect.edit(&site);
        // Opened from a refusal over an unrecognised certificate, the dialog
        // arrives with that certificate's fingerprint filled in. Trusting it
        // still takes the user's own Save; this only spares them copying a
        // ninety-five character string out of an error message.
        let refused = self
            .workspace
            .active_tab()
            .filter(|t| t.location().as_remote().is_some_and(|(s, _)| s == id))
            .and_then(|t| match &t.status {
                Status::Error(e) => certificate_in(e),
                _ => None,
            });
        if let Some(fingerprint) = refused {
            self.connect.trust_certificate(&fingerprint);
        }
    }

    /// Drops a file from the recent list.
    pub(super) fn forget_recent(&mut self, path: PathBuf) {
        if !self.recent.forget(&path) {
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        self.notices.info(format!("Removed {name} from Recent files"));

        let recent = self.recent.clone();
        self.sta.submit(move || {
            if let Err(e) = crate::recent::save(&recent) {
                tracing::warn!("could not save recent files: {e}");
            }
        });
    }

    /// Opens a local file and remembers it.
    ///
    /// The two call sites are a double-click and the shell's answer about what
    /// a row leads to. Deliberately not used for a plugin's `open` responses —
    /// one command can ask for eight, and a recent list flooded by one plugin
    /// run is no longer a record of what *you* were doing — nor for a file
    /// fetched from a server, which is a copy under `%TEMP%` that will not be
    /// there later.
    /// Opens what the system handed over: a folder in a new tab, a file in a
    /// new tab on its folder with the file selected.
    pub(super) fn drain_opened(&mut self) {
        for paths in std::mem::take(&mut self.mail.opened) {
            let Some(group) = self.workspace.focused_group().map(|g| g.id) else {
                return;
            };
            for path in paths {
                let (folder, select) = if path.is_dir() {
                    (path, None)
                } else {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                    match path.parent() {
                        Some(parent) => (parent.to_path_buf(), name),
                        None => continue,
                    }
                };
                let view = self.workspace.view_for_new_tab();
                let tab = self.workspace.open_tab(group, NodeId::Path(folder.clone()), view);
                self.reload(tab, NodeId::Path(folder));
                if let Some(t) = self.workspace.tabs.get_mut(&tab) {
                    t.reselect = select.map(|n| crate::workspace::Reselect { names: vec![n.clone()], cursor: Some(n) });
                }
            }
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    pub(super) fn open_local(&mut self, path: PathBuf) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        if self.recent.record(&path, now) {
            let recent = self.recent.clone();
            let notices = self.notice_tx.clone();
            self.sta.submit(move || {
                if let Err(e) = crate::recent::save(&recent) {
                    // A warning, not a notice: failing to remember a file is
                    // not a failure to open it, and the open is what the user
                    // asked for.
                    tracing::warn!("could not save recent files: {e}");
                    let _ = &notices;
                }
            });
        }
        // A disk image is mounted and its disk shown here; one that cannot be
        // — it wants a password, say — goes to the system, which can ask.
        if crate::disk_image::is_disk_image(&path) {
            if let Some(tab) = self.workspace.active_tab_id() {
                let goto = self.goto_tx.clone();
                let owner = self.hwnd.unwrap_or(0);
                let wake = self.ctx.clone();
                self.sta.submit(move || {
                    match crate::disk_image::mount(&path) {
                        Ok(volume) => {
                            let _ = goto.send((tab, NodeId::path(volume)));
                        }
                        Err(e) => {
                            tracing::info!("mounting {} here failed ({e}); the system will", path.display());
                            let _ = neutron_shell::open::shell_execute(&path, neutron_shell::open::Verb::Open, owner);
                        }
                    }
                    wake.request_repaint();
                });
                return;
            }
        }
        self.launch(path, neutron_shell::open::Verb::Open);
    }

    /// Opens `path` with the shell, on an apartment thread.
    ///
    /// Fire and forget. There is nothing useful to wait for — the launched
    /// process is not ours — and waiting is exactly what must not happen: the
    /// association lookup reads the registry, may load a shell extension, and
    /// on a network or cloud path can block for many seconds.
    pub(super) fn launch(&self, path: PathBuf, verb: neutron_shell::open::Verb) {
        // Recording happens in `open_local`, not here: `launch` is also how a
        // fetched *copy* of a remote file is opened, and a path under %TEMP%
        // is not somewhere anyone can go back to.
        let owner = self.hwnd.unwrap_or(0);
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            // An app chosen for this kind of file in `openers.json` wins over
            // the system's choice.
            let chosen = matches!(verb, neutron_shell::open::Verb::Open).then(|| crate::openers::app_for(&path)).flatten();
            let opened = match chosen {
                Some(app) => crate::openers::open_with(&app, &path),
                None => neutron_shell::open::shell_execute(&path, verb, owner),
            };
            if let Err(e) = opened {
                report(
                    &notices,
                    format!("Could not open {}: {e}", path.display()),
                );
            }
        });
    }

    /// Jumps to the next entry whose name starts with the accumulated prefix.
    ///
    /// Repeating a single character cycles through matches for that letter,
    /// which is what a file list is expected to do; typing distinct characters
    /// within the timeout extends the prefix instead.
    pub(super) fn type_ahead(&mut self, c: char) {
        let now = Instant::now();
        let expired = self
            .type_ahead_at
            .is_none_or(|t| now.duration_since(t) > TYPE_AHEAD_TIMEOUT);

        if expired {
            self.type_ahead.clear();
        }
        self.type_ahead.push(c.to_ascii_lowercase());
        self.type_ahead_at = Some(now);

        let first = self.type_ahead.chars().next().unwrap_or(c);
        let cycling = self.type_ahead.len() > 1 && self.type_ahead.chars().all(|ch| ch == first);
        let prefix: String = if cycling {
            first.to_string()
        } else {
            self.type_ahead.clone()
        };

        let Some(t) = self.workspace.active_tab_mut() else {
            return;
        };
        let visible = t.list.order().len();
        if visible == 0 {
            return;
        }

        // Start just past the cursor so repeats advance rather than sticking.
        let start = match t.selection.cursor().and_then(|c| t.list.rank(c)) {
            Some(pos) if cycling || expired => pos + 1,
            Some(pos) => pos,
            None => 0,
        };

        for step in 0..visible {
            let pos = (start + step) % visible;
            let idx = t.list.at(pos);
            if t.list.name(idx).to_lowercase().starts_with(&prefix) {
                t.selection.apply(&t.list, idx, SelectMode::Replace);
                t.view.scroll_to = t.selection.cursor();
                return;
            }
        }
    }
}
