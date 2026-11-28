use super::*;

impl eframe::App for NeutronApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let mut snapshot = self.workspace.persist();
        snapshot.preview = self.preview.open;
        // The theme belongs to the window, not to the pane tree, so the
        // workspace cannot fill it in itself.
        snapshot.prefs.theme = Some(self.theme_mode.id().to_owned());
        snapshot.prefs.light_theme = self.theme_mode == ThemeMode::Light;
        snapshot.prefs.all_sizes = self.all_sizes;
        snapshot.prefs.hide_sidebar = self.hide_sidebar;
        snapshot.prefs.hide_toolbar = self.hide_toolbar;
        if let Ok(json) = serde_json::to_string(&snapshot) {
            storage.set_string(STORAGE_KEY, json);
        }

        // Written after eframe's own copy, which it makes only while the
        // window still exists: once it has closed and the application
        // carries on, a later save would otherwise keep the size from before.
        if let Some(frame) = &self.root_frame {
            storage.set_string("window", frame.clone());
        }

        // The other windows, where they are now and at their current size.
        let windows: Vec<SavedWindow> = self
            .windows
            .iter()
            .map(|w| {
                let (outer, inner) = self.ctx.input_for(w.viewport, |i| (i.viewport().outer_rect, i.viewport().inner_rect));
                SavedWindow {
                    workspace: w.workspace.persist(),
                    preview: w.preview.open,
                    at: outer.or(w.origin.map(|o| egui::Rect::from_min_size(o, egui::Vec2::ZERO))).map(|r| [r.min.x, r.min.y]),
                    size: inner.map(|r| [r.width(), r.height()]).or(w.size.map(|s| [s.x, s.y])),
                }
            })
            .collect();
        if let Ok(json) = serde_json::to_string(&windows) {
            storage.set_string(WINDOWS_KEY, json);
        }
    }

    /// What the window is cleared to before anything is drawn. On the Mac,
    /// nothing: the window is transparent, so the desktop shows between the
    /// cards. Elsewhere eframe's own default.
    #[cfg(target_os = "macos")]
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let frame_started = Instant::now();
        let ctx = ui.ctx().clone();

        // Once a frame, for every window.
        self.shared_frame(&ctx);
        if let Some(frame) = root_window_settings(&ctx) {
            self.root_frame = Some(frame);
        }

        // The first window, then the others, each with its own state.
        self.window_frame(ui);
        // Closing the first window while others are open hands one of them
        // the first window's place, rather than ending the application.
        let hand_over = !self.windows.is_empty() && ctx.input(|i| i.viewport().close_requested());
        if hand_over {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.other_windows(&ctx);
        if hand_over {
            self.hand_over_first_window();
        }
        // Anything no window took — work for a tab closed meanwhile.
        self.mail = Mail::default();

        self.startup.mark_frame(frame_started.elapsed());
    }
}

/// The first window's place and size in the form eframe saves them under
/// "window" and reads back at launch — positions in pixels, the size in
/// points — or `None` while the window is not there to measure.
pub(super) fn root_window_settings(ctx: &egui::Context) -> Option<String> {
    let (inner, outer, ppp, fullscreen, maximized, minimized) = ctx.input_for(egui::ViewportId::ROOT, |i| {
        let v = i.viewport();
        (v.inner_rect, v.outer_rect, v.native_pixels_per_point, v.fullscreen, v.maximized, v.minimized)
    });
    let (inner, outer, ppp) = (inner?, outer?, ppp?);
    // A minimised window reports a size it will not come back at.
    if minimized == Some(true) || inner.width() < 1.0 {
        return None;
    }
    Some(format!(
        "(inner_position_pixels:Some((x:{:.1},y:{:.1})),outer_position_pixels:Some((x:{:.1},y:{:.1})),\
         fullscreen:{},maximized:{},inner_size_points:Some((x:{:.1},y:{:.1})))",
        inner.min.x * ppp,
        inner.min.y * ppp,
        outer.min.x * ppp,
        outer.min.y * ppp,
        fullscreen.unwrap_or(false),
        maximized.unwrap_or(false),
        inner.width(),
        inner.height(),
    ))
}

/// What belongs to one window: its panes and tabs, and whatever is open in it.
///
/// The window being drawn has its state in the app's own fields, where the
/// code that draws and acts has always found it. The others wait here, and
/// each is swapped in for its own frame — so every function that reads
/// `self.workspace` reads the workspace of the window it is running for,
/// without being told there is more than one.
pub(super) struct WindowState {
    pub(super) viewport: egui::ViewportId,
    /// Where a new window first appears; the OS keeps it after that.
    pub(super) origin: Option<egui::Pos2>,
    /// Its size when it first appears, when restored from the last session.
    pub(super) size: Option<egui::Vec2>,
    pub(super) workspace: Workspace,
    pub(super) confirm: Option<(crate::confirm::Confirm, Box<Action>, Option<Subject>)>,
    pub(super) address: Option<(GroupId, String)>,
    pub(super) type_ahead: String,
    pub(super) type_ahead_at: Option<Instant>,
    pub(super) quick_look_shown: Option<QuickLookKey>,
    pub(super) menu: Option<neutron_ui::MenuState>,
    pub(super) menu_reply: Option<crossbeam_channel::Sender<Option<u32>>>,
    pub(super) menu_actions: Vec<(u32, Action)>,
    pub(super) pending_menu_actions: std::cell::RefCell<Vec<(u32, Action)>>,
    pub(super) pin_target: Option<NodeId>,
    pub(super) tab_target: Option<NodeId>,
    pub(super) edit_site_target: Option<String>,
    pub(super) tab_drag: Option<TabDrag>,
    pub(super) last_title: String,
    pub(super) finder: Finder,
    pub(super) connect: crate::connect::ConnectDialog,
    pub(super) preview: crate::preview::PreviewState,
    pub(super) previews: crate::preview::PreviewLoader,
    pub(super) rename: Option<RenameEdit>,
    pub(super) pending_rename: Option<(TabId, String)>,
    pub(super) tag_editor: Option<TagEditor>,
    pub(super) batch_rename: Option<BatchRenameDialog>,
    pub(super) compare_panes: bool,
}

/// The Rename Items dialog: the folder, each item's name and whether it is a
/// folder, and the rule as typed so far.
pub(super) struct BatchRenameDialog {
    pub(super) dir: PathBuf,
    pub(super) items: Vec<(String, bool)>,
    /// 0 replaces text, 1 adds text, 2 formats.
    pub(super) mode: u8,
    pub(super) find: String,
    pub(super) with: String,
    pub(super) text: String,
    pub(super) after: bool,
    pub(super) base: String,
    pub(super) start: String,
    pub(super) focused: bool,
}

impl BatchRenameDialog {
    pub(super) fn rule(&self) -> crate::batch_rename::Rule {
        use crate::batch_rename::Rule;
        match self.mode {
            0 => Rule::Replace { find: self.find.clone(), with: self.with.clone() },
            1 => Rule::Add { text: self.text.clone(), after: self.after },
            _ => Rule::Format { base: self.base.clone(), start: self.start.trim().parse().unwrap_or(1) },
        }
    }

    /// Each item's name and the name it would get.
    pub(super) fn pairs(&self) -> Vec<(String, String)> {
        let rule = self.rule();
        self.items
            .iter()
            .enumerate()
            .map(|(i, (name, is_dir))| (name.clone(), crate::batch_rename::apply(&rule, name, i, *is_dir)))
            .collect()
    }
}

/// The Edit Tags dialog: the items, and the names typed so far.
pub(super) struct TagEditor {
    pub(super) paths: Vec<PathBuf>,
    pub(super) text: String,
    /// Whether the field has been given the caret yet.
    pub(super) focused: bool,
}

impl WindowState {
    pub(super) fn new(ctx: &egui::Context, viewport: egui::ViewportId, origin: Option<egui::Pos2>, location: NodeId) -> Self {
        Self {
            viewport,
            origin,
            size: None,
            workspace: Workspace::new(location),
            confirm: None,
            address: None,
            type_ahead: String::new(),
            type_ahead_at: None,
            quick_look_shown: None,
            menu: None,
            menu_reply: None,
            menu_actions: Vec::new(),
            pending_menu_actions: std::cell::RefCell::new(Vec::new()),
            pin_target: None,
            tab_target: None,
            edit_site_target: None,
            tab_drag: None,
            last_title: String::new(),
            finder: Finder::default(),
            connect: crate::connect::ConnectDialog::default(),
            preview: crate::preview::PreviewState::default(),
            previews: crate::preview::PreviewLoader::spawn(ctx.clone()),
            rename: None,
            pending_rename: None,
            tag_editor: None,
            batch_rename: None,
            compare_panes: false,
        }
    }
}

/// What background work reported this frame, held until the window it is for
/// has had its turn.
///
/// The channels are shared by every window, so they are emptied once, here,
/// and each window takes what concerns its own tabs. Whatever is left when all
/// have drawn — a listing for a tab closed meanwhile — is dropped.
#[derive(Default)]
pub(super) struct Mail {
    pub(super) loads: Vec<(TabId, u64, LoadResult)>,
    pub(super) refreshes: Vec<TabId>,
    pub(super) open_as: Vec<(TabId, PathBuf, bool)>,
    pub(super) goto: Vec<(TabId, NodeId)>,
    pub(super) created: Vec<(TabId, String)>,
    /// Folders that changed on disk. Every window looks at all of them.
    pub(super) changes: std::collections::HashSet<PathBuf>,
    /// Files and folders the system asked Neutron to open.
    pub(super) opened: Vec<Vec<PathBuf>>,
    /// Context menus built by the shell, for the window that asked.
    pub(super) menus: Vec<PendingMenu>,
    /// Menu bar items chosen, for the window with focus.
    pub(super) chosen: Vec<u32>,
    /// Arrow keys pressed in Quick Look, for the window that opened it.
    pub(super) moves: Vec<isize>,
}

/// Takes the items `mine` claims out of `all`, leaving the rest.
pub(super) fn take_mine<T>(all: &mut Vec<T>, mine: impl Fn(&T) -> bool) -> Vec<T> {
    let (taken, rest): (Vec<T>, Vec<T>) = std::mem::take(all).into_iter().partition(|item| mine(item));
    *all = rest;
    taken
}

/// Files to share, and where to point the share menu — `None` for AirDrop.
pub(super) type ShareRequest = (Vec<PathBuf>, Option<(f32, f32)>);

/// The Group By choices, as the menus offer them.
pub(super) const GROUPS: [(&str, neutron_core::GroupBy); 6] = [
    ("None", neutron_core::GroupBy::None),
    ("Kind", neutron_core::GroupBy::Kind),
    ("Date Modified", neutron_core::GroupBy::Modified),
    ("Date Added", neutron_core::GroupBy::Added),
    ("Date Last Opened", neutron_core::GroupBy::Opened),
    ("Size", neutron_core::GroupBy::Size),
];

/// What a worker reports to the undo history.
pub(super) enum UndoEvent {
    /// A change was made.
    Did(crate::undo::Record),
    /// An undo (or, with `undo` false, a redo) finished.
    Reversed { undo: bool, record: crate::undo::Record, failed: Vec<String>, blocked: Vec<PathBuf> },
}

/// What the Quick Look panel was given: the tab, its cursor, and how many
/// items — enough to tell when the panel has to follow.
pub(super) type QuickLookKey = (TabId, Option<usize>, usize);

impl NeutronApp {
    /// What is done once a frame however many windows are open: background
    /// work pumped, shared caches fed, and the channels emptied into `mail`.
    pub(super) fn shared_frame(&mut self, ctx: &egui::Context) {
        self.drain_places();
        self.drain_sites();
        self.drain_remote_drives();
        if !cfg!(test)
            && self.last_remote_probe.elapsed() >= std::time::Duration::from_secs(60)
            && self.sites.sites.iter().any(|s| s.protocol == neutron_remote::Protocol::Sftp)
        {
            self.start_remote_volume_probe();
        }
        while let Ok(state) = self.drive_rx.try_recv() {
            // An answer ends any sign-in in progress; the sign-in's own
            // result is the only thing sent while one runs.
            self.drive_signing_in = false;
            self.drive_state = state;
        }
        if self.clip_fail_rx.try_recv().is_ok() {
            self.clear_cut_marks();
        }
        self.pump_archive_job();
        self.pump_transfers();
        self.pump_plugin(ctx);
        self.pump_download();
        self.pump_remote_job();
        self.pump_remote_edits();
        self.pump_remote_search();
        self.pump_remote_sizes();
        if self.storage_lens.pump() {
            ctx.request_repaint();
        }
        if self.storage_lens.is_loading() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        self.vcs.pump();
        let vcs_targets: Vec<(NodeId, Option<neutron_remote::Site>)> = self
            .workspaces()
            .flat_map(|w| {
                w.groups
                    .values()
                    .filter_map(move |g| g.active_tab().and_then(|t| w.tabs.get(&t)))
            })
            .map(|tab| {
                let site = tab
                    .location()
                    .as_remote()
                    .and_then(|(s, _)| self.sites.get(s).cloned());
                (tab.location().clone(), site)
            })
            .collect();
        for (loc, site) in vcs_targets {
            self.vcs.request_if_needed(&loc, site.as_ref());
        }
        self.drain_notices(ctx);
        self.sync_watchers(ctx);
        self.pump_columns();
        // Before drawing: installs icons that arrived since the last frame, so
        // rows painted below pick them up immediately rather than a frame late.
        self.icons.pump();
        self.thumbs.pump();
        self.icon_texture = self.icons.texture(ctx);
        let previous_index_state = std::mem::discriminant(self.index.state());
        self.index.pump();
        if previous_index_state != std::mem::discriminant(self.index.state()) {
            self.refresh_finder();
        }
        neutron_shell::quicklook::pump();
        self.pump_sizes();
        self.reap_terminals();
        while let Ok((paths, at)) = self.share_rx.try_recv() {
            let shared = match at {
                Some(at) => neutron_shell::share::share(&paths, at),
                None => neutron_shell::share::airdrop(&paths),
            };
            if let Err(e) = shared {
                self.notices.error(e);
            }
        }
        self.collect_mail();
    }

    /// Empties the shared channels into `mail`, for each window to take its
    /// own from.
    pub(super) fn collect_mail(&mut self) {
        while let Some(load) = self.loader.poll() {
            self.mail.loads.push(load);
        }
        self.mail.refreshes.extend(self.refresh_rx.try_iter());
        self.mail.open_as.extend(self.open_as_rx.try_iter());
        self.mail.goto.extend(self.goto_rx.try_iter());
        self.mail.created.extend(self.created_rx.try_iter());
        self.mail.changes.extend(self.changed_rx.try_iter());
        self.mail.opened.extend(self.opened.try_iter());
        self.mail.menus.extend(self.menu_rx.try_iter());
        self.mail.chosen.extend(neutron_shell::menubar::take_chosen());
        self.mail.moves.extend(neutron_shell::quicklook::take_moves());
    }

    /// One window's frame: its share of the mail, its input, its drawing, and
    /// the actions taken in it.
    pub(super) fn window_frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let p = self.palette();
        let mut actions: Vec<Action> = Vec::new();

        // The ground the cards sit on. Painted onto a background layer so it
        // stays beneath every panel regardless of declaration order.
        let screen = ctx.content_rect();
        let bg = ui
            .painter()
            .clone()
            .with_layer_id(egui::LayerId::background());
        if cfg!(target_os = "macos") {
            // The window is transparent here only so that its corners can be
            // rounded: the ground is painted, solid, inside them. Under it, one
            // part in 255 of black across the whole window — the window server
            // sends a click on a pixel with no alpha at all to whatever is
            // behind, and the very corners would otherwise stop belonging to
            // Neutron.
            bg.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(1));
            theme::paint_ground(&bg, screen, &p);
        } else {
            neutron_ui::ambient::paint(&bg, screen, &p);
        }

        self.drop_at = drop_pointer(&ctx);
        self.drop_panes.borrow_mut().clear();
        neutron_ui::drop::take(&ctx);
        self.drain_loads(&ctx);
        self.apply_folder_sizes();
        self.drain_refreshes();
        self.drain_open_as();
        self.drain_goto();
        self.clipboard_keys(&ctx, &mut actions);
        let tabs = &self.workspace.tabs;
        for (tab, name) in take_mine(&mut self.mail.created, |(t, _)| tabs.contains_key(t)) {
            self.pending_rename = Some((tab, name));
        }
        self.open_pending_rename(&ctx);
        // Opened from the Finder or the Dock: into the first window.
        if self.viewport == egui::ViewportId::ROOT {
            self.drain_opened();
        }
        self.drain_changes();
        self.pump_preview(&ctx);
        self.sync_finder_results();
        self.context_menu_overlay(&ctx, &p);
        self.keyboard(&ctx, &mut actions);

        // The menu bar acts on the window with focus; Quick Look's arrows on
        // the window that opened it.
        let focused = ctx
            .input(|i| i.viewport().focused)
            .unwrap_or(self.viewport == egui::ViewportId::ROOT && self.windows.is_empty());
        if focused {
            self.last_focused = self.viewport;
        }
        let looks_here = self.look_window.map_or(focused, |w| w == self.viewport);
        if looks_here {
            for delta in std::mem::take(&mut self.mail.moves) {
                actions.push(Action::MoveCursor { delta, extend: false });
            }
        }
        if focused {
            actions.extend(self.originals_rx.try_iter().map(Action::ShowInFolder));
            for tag in std::mem::take(&mut self.mail.chosen) {
                if self.modal() {
                    continue;
                }
                if let Some(action) = crate::menubar::Cmd::from_tag(tag).and_then(|c| self.menu_command(c)) {
                    actions.push(action);
                }
            }
        }

        // Before every other panel: it claims the top strip, and the rest
        // lay out inside what is left.
        self.title_bar(ui, &p);

        self.status_bar(ui, &p, &mut actions);
        if !self.hide_sidebar {
            self.sidebar(ui, &p, &mut actions);
        }
        if !self.hide_toolbar {
            self.action_rail(ui, &p, &mut actions);
        }
        self.preview_pane(ui, &p);
        self.panes(ui, &p, &mut actions);
        self.workspace.consume_scroll_requests();

        crate::chrome::resize_edges(ui, screen);
        if cfg!(target_os = "macos") {
            let fg = ui.painter().clone().with_layer_id(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("neutron-window-border"),
            ));
            fg.rect_stroke(
                screen,
                egui::CornerRadius::same(theme::RADIUS_WINDOW),
                egui::Stroke::new(1.0, p.border),
                egui::StrokeKind::Inside,
            );
        }

        // Overlays draw after the panes so they sit on top of them, and the
        // overlay's own keys win while it is open.
        self.finder_overlay(ui, &p, &mut actions);
        self.connect_overlay(ui, &p, &mut actions);
        self.storage_lens_overlay(&ctx, &p, &mut actions);
        self.layout_dialog_overlay(&ctx, &p, &mut actions);

        // Last of all: a question is answered before anything underneath it is
        // touched, and its scrim has to cover the overlays too.
        self.confirm_overlay(ui, &p);
        self.clash_overlay(ui, &p);
        self.tag_editor_overlay(ui, &p, &mut actions);
        self.batch_rename_overlay(ui, &p, &mut actions);
        if let Some(dialog) = self.search_dialog.as_mut() {
            match dialog.show(ui, &p) {
                Some(crate::searches::Answer::Cancel) => self.search_dialog = None,
                Some(crate::searches::Answer::Search { query, save_as, replacing }) => {
                    self.search_dialog = None;
                    actions.push(Action::RunSearch { query, save_as, replacing });
                }
                None => {}
            }
        }
        self.infos.pump();
        match self.infos.show(&ctx, &p, self.viewport) {
            Some(crate::info::Ask::ShowInFolder(path)) => actions.push(Action::ShowInFolder(path)),
            Some(crate::info::Ask::ShowOriginal(path)) => actions.push(Action::ShowOriginal(path)),
            Some(crate::info::Ask::FinderInfo(path)) => actions.push(Action::FinderInfo(vec![path])),
            Some(crate::info::Ask::EditTags(path)) => {
                let text = neutron_shell::tags::read(&path).into_iter().map(|t| t.name).collect::<Vec<_>>().join(", ");
                self.tag_editor = Some(TagEditor { paths: vec![path], text, focused: false });
            }
            None => {}
        }
        self.resolve_drop(&ctx, &p, &mut actions);

        // Queued *after* drawing so a `DropTab` pushed by a tab strip this
        // frame is applied first. Pushed before it, this would clear the drag
        // and turn every drop into a no-op.
        if self.tab_drag.is_some() && ctx.input(|i| i.pointer.any_released()) {
            actions.push(Action::EndTabDrag);
        }

        // Actions update the model after the widgets have drawn. Present that
        // new state even if no further mouse or keyboard event arrives.
        if !actions.is_empty() {
            ctx.request_repaint();
        }
        for action in actions {
            self.apply(&ctx, action);
        }
        if looks_here {
            self.quick_look_follow();
        }
        if focused {
            self.publish_menu_state(&ctx);
        }
    }

    /// Draws every window but the first, each with its state swapped in.
    pub(super) fn other_windows(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.windows.len() {
            let mut window = self.windows.remove(i);
            let mut builder = crate::window_builder().with_inner_size(window.size.unwrap_or(egui::vec2(1240.0, 720.0)));
            if let Some(at) = window.origin {
                builder = builder.with_position(at);
            }
            self.swap_window(&mut window);
            let mut closing = false;
            ctx.show_viewport_immediate(self.viewport, builder, |ui, _class| {
                self.window_frame(ui);
                closing = ui.ctx().input(|i| i.viewport().close_requested());
            });
            self.swap_window(&mut window);
            if closing {
                self.forget_window(window);
            } else {
                self.windows.insert(i, window);
                i += 1;
            }
        }
    }

    /// The first window was closed while others are open: the first of those
    /// moves into it, and what the first window showed is let go.
    pub(super) fn hand_over_first_window(&mut self) {
        if self.windows.is_empty() {
            return;
        }
        let mut window = self.windows.remove(0);
        // The first window cannot close without ending the application, so it
        // takes the other's place as well as its tabs: moved and sized to
        // where the other is, it stands in for it as that one closes, and
        // what is seen is the window closed going and the other staying put.
        let (outer, inner) =
            self.ctx.input_for(window.viewport, |i| (i.viewport().outer_rect, i.viewport().inner_rect));
        if let Some(outer) = outer {
            self.ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::OuterPosition(outer.min));
        }
        if let Some(inner) = inner {
            self.ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::InnerSize(inner.size()));
        }
        self.swap_window(&mut window);
        // The state moves; the windows stay. The first window is still the
        // root viewport, and the one emptied is not drawn again, so it closes.
        std::mem::swap(&mut self.viewport, &mut window.viewport);
        self.origin = None;
        self.forget_window(window);
    }

    /// Lets go of a closed window's tabs.
    pub(super) fn forget_window(&mut self, window: WindowState) {
        for tab in window.workspace.tabs.keys() {
            self.loader.forget(*tab);
        }
        if self.look_window == Some(window.viewport) {
            self.look_window = None;
        }
        if self.menu_window == Some(window.viewport) {
            self.menu_window = None;
        }
        if self.last_focused == window.viewport {
            self.last_focused = egui::ViewportId::ROOT;
        }
    }

    /// Exchanges the window being drawn's state with `window`'s.
    pub(super) fn swap_window(&mut self, window: &mut WindowState) {
        use std::mem::swap;
        swap(&mut self.viewport, &mut window.viewport);
        swap(&mut self.origin, &mut window.origin);
        swap(&mut self.workspace, &mut window.workspace);
        swap(&mut self.confirm, &mut window.confirm);
        swap(&mut self.address, &mut window.address);
        swap(&mut self.type_ahead, &mut window.type_ahead);
        swap(&mut self.type_ahead_at, &mut window.type_ahead_at);
        swap(&mut self.quick_look_shown, &mut window.quick_look_shown);
        swap(&mut self.menu, &mut window.menu);
        swap(&mut self.menu_reply, &mut window.menu_reply);
        swap(&mut self.menu_actions, &mut window.menu_actions);
        swap(&mut self.pending_menu_actions, &mut window.pending_menu_actions);
        swap(&mut self.pin_target, &mut window.pin_target);
        swap(&mut self.tab_target, &mut window.tab_target);
        swap(&mut self.edit_site_target, &mut window.edit_site_target);
        swap(&mut self.tab_drag, &mut window.tab_drag);
        swap(&mut self.last_title, &mut window.last_title);
        swap(&mut self.finder, &mut window.finder);
        swap(&mut self.connect, &mut window.connect);
        swap(&mut self.preview, &mut window.preview);
        swap(&mut self.previews, &mut window.previews);
        swap(&mut self.rename, &mut window.rename);
        swap(&mut self.pending_rename, &mut window.pending_rename);
        swap(&mut self.tag_editor, &mut window.tag_editor);
        swap(&mut self.batch_rename, &mut window.batch_rename);
        swap(&mut self.compare_panes, &mut window.compare_panes);
    }

    /// An id for one of a window's own panels. The first window keeps the
    /// plain name, so the widths egui saved for its panels still apply; the
    /// others add their viewport, so two windows' sidebars are never taken
    /// for the same one where viewports share a pass.
    pub(super) fn window_id(&self, name: &str) -> egui::Id {
        if self.viewport == egui::ViewportId::ROOT {
            egui::Id::new(name)
        } else {
            egui::Id::new((name, self.viewport))
        }
    }

    /// Every window's workspace: the one being drawn, and the others.
    pub(super) fn workspaces(&self) -> impl Iterator<Item = &Workspace> {
        std::iter::once(&self.workspace).chain(self.windows.iter().map(|w| &w.workspace))
    }
}

/// Whether the preview pane was showing when the last session was saved.
///
/// Read separately from the workspace restore because the pane belongs to the
/// window rather than to the pane tree, and because a `--start-path` launch
/// skips the workspace restore entirely but should still remember it.
pub(super) fn saved_preview_open(cc: &eframe::CreationContext<'_>) -> bool {
    saved_session(cc).is_some_and(|saved| saved.preview)
}

/// The saved settings, or the defaults on a first run.
///
/// Read separately from the workspace restore because the theme has to be
/// applied before the first paint, which happens before the pane tree is built.
pub(super) fn saved_prefs(cc: &eframe::CreationContext<'_>) -> crate::workspace::PersistedPrefs {
    saved_session(cc).map(|saved| saved.prefs).unwrap_or_default()
}

pub(super) fn saved_session(
    cc: &eframe::CreationContext<'_>,
) -> Option<crate::workspace::PersistedWorkspace> {
    cc.storage
        .and_then(|s| s.get_string(STORAGE_KEY))
        .and_then(|json| serde_json::from_str::<crate::workspace::PersistedWorkspace>(&json).ok())
}

/// A rename in progress.
///
/// The typed text lives here rather than in the list widget because that widget
/// is drawn from a clone of the view state — anything it wrote would be thrown
/// away when the frame ended. It edits a copy and reports each keystroke back.
pub(super) struct RenameEdit {
    pub(super) tab: TabId,
    /// Which rename this is. Gives the text field a fresh id each time, so it
    /// cannot inherit the previous rename's caret position.
    pub(super) generation: u64,
    /// Storage index, so a re-sort under the box does not move the rename to a
    /// different file.
    pub(super) idx: usize,
    /// The folder the entry is in — on disk or on a server — fixed when the
    /// rename opens.
    pub(super) place: NodeId,
    /// What the file is called now, to tell an unchanged rename from a real one.
    pub(super) original: String,
    pub(super) text: String,
    /// Watcher changes wait until the editor closes so its row stays stable.
    pub(super) refresh_pending: bool,
}

/// A shell menu waiting to be shown, from the apartment thread that built it.
pub(super) struct PendingMenu {
    pub(super) items: Vec<neutron_core::MenuItem>,
    /// Where to put it, in egui's coordinates — which is where the click was
    /// reported, so no conversion to physical screen pixels is needed any more.
    pub(super) pos: egui::Pos2,
    pub(super) reply: crossbeam_channel::Sender<Option<u32>>,
}

// --- background results ----------------------------------------------------

impl NeutronApp {
    pub(super) fn drain_places(&mut self) {
        // Drain everything queued: several drives can resolve between frames.
        while let Ok(update) = self.places_rx.try_recv() {
            match update {
                PlacesUpdate::KnownFolders(folders) => self.known_folders = folders,
                PlacesUpdate::Cloud(place) => self.cloud.push(place),
                PlacesUpdate::Wsl(distros) => self.wsl = distros,
                PlacesUpdate::Shell(roots) => self.shell_roots = roots,
                PlacesUpdate::Drive(place) => self.drives.push(place),
                PlacesUpdate::Ejectable(path) => {
                    self.ejectable.insert(path);
                }
                PlacesUpdate::DrivesReset => {
                    self.drives.clear();
                    self.ejectable.clear();
                    self.scanning_drives = true;
                }
                PlacesUpdate::Done => {
                    self.scanning_drives = false;
                    self.leave_missing_volumes();
                }
            }
        }
    }

    /// Re-lists tabs whose contents a finished shell operation may have changed.
    pub(super) fn drain_refreshes(&mut self) {
        // Collected first because refreshing borrows `self` mutably.
        let owned = &self.workspace.tabs;
        let tabs = take_mine(&mut self.mail.refreshes, |tab| owned.contains_key(tab));
        for tab in tabs {
            self.refresh_in_place(tab);
        }
    }

    pub(super) fn drain_loads(&mut self, ctx: &egui::Context) {
        // Set when a remote folder arrives, and applied after the loop — `tab`
        // borrows `self.workspace` mutably for the whole body.
        let mut cleared_remote_note = false;
        let mut pending_remote_size_batches: Vec<(String, Vec<String>)> = Vec::new();

        let tabs = &self.workspace.tabs;
        let mine = take_mine(&mut self.mail.loads, |(tab, ..)| tabs.contains_key(tab));
        for (tab_id, generation, result) in mine {
            let Some(tab) = self.workspace.tabs.get_mut(&tab_id) else {
                continue;
            };
            // Final staleness guard: a response can still be in the channel
            // from a navigation the user has already left.
            if tab.pending != Some(generation) {
                continue;
            }
            tab.pending = None;

            match result {
                LoadResult::Ready(loaded) => {
                    tab.status = Status::Loaded {
                        entries: loaded.list.len(),
                        elapsed: loaded.enumerate_time + loaded.sort_time,
                    };
                    tab.list = loaded.list;
                    self.sizes_seen.remove(&tab_id);
                    if self.all_sizes {
                        if let Some(dir) = tab.location().as_path() {
                            let folders = (0..tab.list.len())
                                .filter(|&i| tab.list.kind(i).is_container())
                                .map(|i| dir.join(tab.list.name(i)))
                                .collect();
                            self.sizer.request(folders);
                        } else if let Some((site, dir)) = tab
                            .location()
                            .as_remote()
                            .map(|(s, d)| (s.to_owned(), d.to_owned()))
                        {
                            let rfolders: Vec<String> = (0..tab.list.len())
                                .filter(|&i| tab.list.kind(i).is_container())
                                .filter_map(|i| neutron_remote::path::join(&dir, tab.list.name(i)).ok())
                                .collect();
                            if !rfolders.is_empty() {
                                pending_remote_size_batches.push((site, rfolders));
                            }
                        }
                    }
                    // The loader sorts and hides, but it never sees the filter
                    // field — a refresh keeps the filter text, so without this
                    // F5 or any change on disk listed the whole folder under a
                    // box still saying "doc". Only when there is a filter: the
                    // re-sort is the work the loader exists to keep off this
                    // thread.
                    if !tab.view.filter.trim().is_empty() {
                        tab.refilter();
                    }
                    match tab.reselect.take() {
                        Some(wanted) => tab.restore_selection(&wanted),
                        None => tab.selection.clear(),
                    }
                    // A folder that opened is its own proof. Leaving
                    // "Connecting to …" up beside a listing that plainly
                    // arrived is a status line telling a small lie.
                    if matches!(tab.location(), NodeId::Remote { .. }) {
                        cleared_remote_note = true;
                    }
                }
                LoadResult::Failed { id, error } => {
                    tab.status = Status::Error(format!("{id}: {error}"));
                    tab.list = EntryList::new();
                    tab.reselect = None;
                    tab.selection.clear();
                }
            }
        }

        if cleared_remote_note {
            // A folder that opened is its own proof, so "Connecting…" goes.
            // An error stays: it has still not been acknowledged.
            self.notices.dismiss_info();
        }
        for (site, rfolders) in pending_remote_size_batches {
            self.request_remote_sizes(&site, rfolders);
        }

        // Title tracks the focused tab, so the taskbar entry is identifiable
        // when several Neutron windows are open.
        //
        // Only sent when it actually changes. Issuing a viewport command every
        // frame keeps the event loop permanently awake — it held the app at
        // ~9% of a core while sitting completely idle.
        if let Some(t) = self.workspace.active_tab() {
            let title = format!("{} — Neutron", t.title());
            if title != self.last_title {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
                self.last_title = title;
            }
        }
    }
}
