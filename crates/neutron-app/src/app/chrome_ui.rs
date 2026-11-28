use super::*;

// --- chrome ----------------------------------------------------------------

impl NeutronApp {
    /// Runs the Google Drive consent flow on a worker.
    ///
    /// Opens a browser and blocks until the user finishes, so it cannot be on
    /// the paint thread — and the STA pool is where every other blocking,
    /// user-facing operation already lives.
    pub(super) fn connect_drive(&mut self) {
        if matches!(self.drive_state, neutron_cloud::google::DriveState::NotConfigured) {
            tracing::warn!("no Google client id configured; not starting the flow");
            return;
        }
        // One sign-in at a time. Each waits on a worker for the browser, and a
        // second one would only compete for the same loopback redirect.
        if self.drive_signing_in {
            self.notices.info("Already waiting for Google sign-in in the browser");
            return;
        }
        self.drive_signing_in = true;

        self.drive_state = neutron_cloud::google::DriveState::Error("Connecting…".to_owned());
        let wake = self.ctx.clone();
        let done = self.drive_tx.clone();

        self.sta.submit(move || {
            let drive = neutron_cloud::google::GoogleDrive::new();
            let state = match drive.sign_in() {
                Ok(()) => neutron_cloud::google::DriveState::SignedIn,
                Err(e) => {
                    // At error level: this is the only record of why a sign-in
                    // the user actively started did not work, and it must show
                    // under the default log filter.
                    tracing::error!("Google Drive sign-in failed: {e}");
                    neutron_cloud::google::DriveState::Error(e.to_string())
                }
            };
            let _ = done.send(state);
            wake.request_repaint();
        });
    }



    /// Pinned folders as sidebar places.
    ///
    /// Built per frame rather than kept: it is a handful of paths, and a cached
    /// copy is one more thing that can disagree with `self.pins` after a
    /// toggle.
    pub(super) fn pinned_places(&self) -> Vec<Place> {
        let local = self.pins.folders.iter().map(|path| Place {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            id: NodeId::Path(path.clone()),
            kind: neutron_core::places::PlaceKind::KnownFolder,
            capacity: None,
        });
        let remote = self.pins.remote.iter().map(|rp| {
            let name = if rp.path == "/" {
                self.sites
                    .get(&rp.site)
                    .map(|s| format!("{} /", s.display()))
                    .unwrap_or_else(|| format!("{} /", rp.site))
            } else {
                neutron_remote::path::file_name(&rp.path)
            };
            Place {
                name,
                id: NodeId::remote(rp.site.as_str(), rp.path.as_str()),
                kind: neutron_core::places::PlaceKind::KnownFolder,
                capacity: None,
            }
        });
        local.chain(remote).collect()
    }

    /// What Quick access holds: the known folders, then anything pinned.
    ///
    /// Known folders first because they are always the same four and the eye
    /// learns where they are; pins after, newest first, because that order is
    /// the point of them.
    pub(super) fn quick_access(&self) -> Vec<Place> {
        let mut all = self.known_folders.clone();
        // A pin that is already a known folder is not listed twice. Pinning
        // Downloads is a reasonable thing to try, and two identical tiles side
        // by side looks like a bug rather than a preference.
        all.extend(
            self.pinned_places()
                .into_iter()
                .filter(|pinned| !self.known_folders.iter().any(|k| k.id == pinned.id)),
        );
        all
    }




    /// Opens `location` in a new tab of the focused pane.
    ///
    /// The sidebar's menu route. `Action::NewTab` deliberately opens at the
    /// pane's *current* location — "new tab here" is what Ctrl+T means — so it
    /// is the wrong thing to reuse when the point is to go somewhere else.
    pub(super) fn open_in_new_tab(&mut self, location: NodeId) {
        let Some(group) = self.workspace.focused_group().map(|g| g.id) else {
            return;
        };
        let view = self.workspace.view_for_new_tab();
        let tab = self.workspace.open_tab(group, location.clone(), view);
        self.workspace.focused = group;
        self.reload(tab, location);
    }



    /// Opens the address bar on the focused pane, or closes it.
    ///
    /// Opens holding where the pane already is, selected, so `Ctrl+L` followed
    /// by typing replaces the path rather than appending to it — which is what
    /// every browser and Explorer do, and what muscle memory expects.
    pub(super) fn toggle_address(&mut self, ctx: &egui::Context) {
        if self.address.is_some() {
            self.address = None;
            return;
        }
        let Some(group) = self.workspace.focused_group().map(|g| g.id) else {
            return;
        };
        let Some(text) = self
            .workspace
            .active_tab()
            .and_then(|t| crate::address::text_for(t.location()))
        else {
            // The dashboard and a Drive folder have no path to put in the
            // field, and an empty address bar is a worse answer than none.
            self.notices
                .info("There is no path here to type over");
            return;
        };

        let id = crate::header::address_id(group);
        let mut state = egui::text_edit::TextEditState::default();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(text.chars().count()),
        )));
        egui::TextEdit::store_state(ctx, id, state);
        ctx.memory_mut(|m| m.request_focus(id));
        self.address = Some((group, text));
        ctx.request_repaint();
    }

    /// Goes where the address bar says.
    pub(super) fn submit_address(&mut self) {
        let Some((_, text)) = self.address.take() else {
            return;
        };
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };
        let current = self
            .workspace
            .tabs
            .get(&tab)
            .map(|t| t.location().clone())
            .unwrap_or(NodeId::Home);

        match crate::address::resolve(&text, &current, neutron_shell::places::home().as_deref()) {
            Some(target) => self.navigate(tab, target),
            // Nothing resolvable. Named rather than silent: the commonest cause
            // is a variable that is not set, and the path the user typed is not
            // the path an error would otherwise report.
            None => self
                .notices
                .error(format!("Cannot work out where {text:?} is")),
        }
    }

    /// Runs an action that has already been decided on.
    ///
    /// The context is the one held on `self`, so a confirmed action can be run
    /// from the dialog's own frame rather than being queued for the next one.
    pub(super) fn apply_now(&mut self, action: Action) {
        let ctx = self.ctx.clone();
        self.apply(&ctx, action);
    }

    /// Puts a question in front of an action, choosing its wording from what
    /// the action is about to do.
    ///
    /// The wording is built here rather than at each call site so the count and
    /// the place named in the button are the ones the action will actually act
    /// on — a dialog that says "3 files" while the code deletes 4 is worse than
    /// no dialog.
    pub(super) fn ask(&mut self, then: Action) {
        let Some(question) = self.question_for(&then) else {
            // Nothing about this needs asking. Doing it rather than inventing a
            // dialog keeps the list of things that get one short, which is what
            // makes people read them.
            self.apply_now(then);
            return;
        };
        let subject = self.subject();
        self.confirm = Some((question, Box::new(then), subject));
    }

    /// What a question asked now would be about. See [`Subject`].
    pub(super) fn subject(&self) -> Option<Subject> {
        let tab = self.workspace.active_tab_id()?;
        let t = self.workspace.tabs.get(&tab)?;
        Some(Subject {
            tab,
            location: t.location().clone(),
            names: visible_selection(t)
                .into_iter()
                .map(|i| t.list.name(i).to_owned())
                .collect(),
        })
    }

    /// The wording for `action`, or `None` if it does not warrant asking.
    pub(super) fn question_for(&self, action: &Action) -> Option<crate::confirm::Confirm> {
        match action {
            #[cfg(target_os = "macos")]
            Action::Delete { permanent: true } if self.workspace.active_tab()?.location().is_filesystem() => {
                let count = visible_selection(self.workspace.active_tab()?).len();
                Some(crate::confirm::Confirm {
                    title: "Delete permanently?".into(),
                    body: match count {
                        1 => "The selected item will be permanently deleted. This cannot be undone.".to_owned(),
                        n => format!("{n} selected items will be permanently deleted. This cannot be undone."),
                    },
                    proceed: "Delete permanently".into(),
                })
            }
            // Only on a server. A local delete goes to the Recycle Bin and is
            // undone from there; asking about it would train people to dismiss
            // the dialog that matters.
            Action::DeleteOnServer => {
                let place = self.workspace.active_tab()?.location();
                let (site, _) = place.as_remote()?;
                let name = self
                    .sites
                    .get(site)
                    .map(|s| s.display().to_owned())
                    .unwrap_or_else(|| "that server".to_owned());

                let tab = self.workspace.active_tab()?;
                let selected = visible_selection(tab);
                let what = match selected.len() {
                    1 => "1 item".to_owned(),
                    n => format!("{n} items"),
                };
                // Said whenever it is true, because it is the part that is
                // easy not to think about: one row can be a thousand files.
                let folders = if selected.iter().any(|&i| tab.list.kind(i) == neutron_core::EntryKind::Directory) {
                    " Folders are deleted with everything inside them."
                } else {
                    ""
                };
                Some(crate::confirm::Confirm {
                    title: format!("Delete from {name}?"),
                    body: format!(
                        "{what} will be deleted from {name}.{folders} A server \
                         has no {bin}, so this cannot be undone.",
                        bin = if cfg!(target_os = "macos") { "Trash" } else { "Recycle Bin" }
                    ),
                    proceed: format!("Delete {what}"),
                })
            }

            Action::EmptyTrash => Some(crate::confirm::Confirm {
                title: "Empty the Trash?".into(),
                body: "Everything in the Trash will be deleted permanently. This cannot be undone.".into(),
                proceed: "Empty Trash".into(),
            }),

            Action::ForgetServer => {
                let (site, _) = self.workspace.active_tab()?.location().as_remote()?;
                let name = self
                    .sites
                    .get(site)
                    .map(|s| s.display().to_owned())
                    .unwrap_or_else(|| "that server".to_owned());
                Some(crate::confirm::Confirm {
                    title: format!("Forget {name}?"),
                    body: format!(
                        "Its saved password is removed from {}. \
                         If you do not know the password, this cannot be undone.",
                        if cfg!(target_os = "macos") { "your keychain" } else { "Credential Manager" }
                    ),
                    proceed: "Forget it".to_owned(),
                })
            }

            _ => None,
        }
    }

    /// The confirmation dialog, drawn over everything.
    pub(super) fn confirm_overlay(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let Some((question, ..)) = self.confirm.as_ref() else {
            return;
        };
        match crate::confirm::show(ui, p, question) {
            crate::confirm::Answer::Waiting => {}
            crate::confirm::Answer::Cancelled => self.confirm = None,
            crate::confirm::Answer::Confirmed => {
                let Some((_, then, asked_about)) = self.confirm.take() else {
                    return;
                };
                // Input is blocked while the dialog is up, but a refresh can
                // still change the listing underneath it. A yes to one set of
                // files is not a yes to another.
                if asked_about != self.subject() {
                    self.notices
                        .error("The selection changed while the question was open, so nothing was done");
                    return;
                }
                self.apply_now(*then);
            }
        }
    }

    /// Neutron's own menu for a sidebar row.
    ///
    /// Not the shell's: a sidebar row is a destination rather than a file, and
    /// half of them — the dashboard, a server, a Drive root — have no shell
    /// item behind them at all. So this menu is built here and answered here,
    /// and never reaches `IContextMenu`.
    ///
    /// It exists mostly for one thing. Pinned folders appear in this list, and
    /// until now the only way to remove one was to navigate into it and use the
    /// keystroke — which is backwards, because the sidebar is exactly where a
    /// stale pin is noticed.
    pub(super) fn sidebar_menu(&mut self, id: NodeId, at: egui::Pos2) {
        let mut items = Vec::new();

        items.push(neutron_core::MenuItem {
            id: MENU_ID_NEW_TAB,
            label: "Open in a new tab".to_owned(),
            enabled: true,
            default: true,
            ..Default::default()
        });
        self.tab_target = Some(id.clone());

        self.pin_target = match &id {
            NodeId::Path(_) | NodeId::Remote { .. } => Some(id.clone()),
            _ => None,
        };
        if let Some(target) = self.pin_target.clone() {
            let pinned = match &target {
                NodeId::Path(dir) => self.pins.folders.contains(dir),
                NodeId::Remote { site, path } => self.pins.contains_remote(site, path),
                _ => false,
            };
            items.push(neutron_core::MenuItem::separator());
            items.push(neutron_core::MenuItem {
                id: MENU_ID_PIN,
                label: pin_label(pinned, None),
                accel: neutron_ui::format::shortcut(if cfg!(target_os = "macos") { "Control+Cmd+T" } else { "Ctrl+Shift+D" }),
                enabled: true,
                ..Default::default()
            });
        }

        // Neutron's own verbs for particular rows.
        self.menu_actions.clear();
        if let Some(path) = id.as_path() {
            if self.ejectable.contains(path) {
                items.push(neutron_core::MenuItem::separator());
                items.push(neutron_core::MenuItem {
                    id: MENU_ID_EJECT,
                    label: format!("Eject {}", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()),
                    accel: neutron_ui::format::shortcut("Cmd+E"),
                    enabled: true,
                    ..Default::default()
                });
                self.menu_actions.push((MENU_ID_EJECT, Action::Eject(path.to_path_buf())));
            }
            if Some(path) == trash_folder().as_deref() {
                items.push(neutron_core::MenuItem::separator());
                items.push(neutron_core::MenuItem {
                    id: MENU_ID_EMPTY_TRASH,
                    label: "Empty Trash…".to_owned(),
                    enabled: true,
                    ..Default::default()
                });
                self.menu_actions.push((MENU_ID_EMPTY_TRASH, Action::Ask(Box::new(Action::EmptyTrash))));
            }
        }

        // A saved search can be changed or forgotten from its own row.
        if let NodeId::Search { name, .. } = &id {
            if !name.is_empty() {
                items.push(neutron_core::MenuItem::separator());
                for (menu_id, label, action) in [
                    (MENU_ID_EDIT_SEARCH, "Edit Search…", Action::EditSearch(name.to_string())),
                    (MENU_ID_REMOVE_SEARCH, "Remove from Sidebar", Action::RemoveSearch(name.to_string())),
                ] {
                    items.push(neutron_core::MenuItem { id: menu_id, label: label.to_owned(), enabled: true, ..Default::default() });
                    self.menu_actions.push((menu_id, action));
                }
            }
        }

        // A saved server can be edited from its own row: its password, how it
        // signs in, a certificate to trust.
        self.edit_site_target = id.as_remote().map(|(site, _)| site.to_owned());
        if self.edit_site_target.is_some() {
            items.push(neutron_core::MenuItem::separator());
            items.push(neutron_core::MenuItem {
                id: MENU_ID_EDIT_SITE,
                label: "Edit server…".to_owned(),
                enabled: true,
                ..Default::default()
            });
        }

        // No reply sender: nothing on an apartment thread is waiting on this
        // one, unlike a menu the shell produced.
        self.menu = Some(neutron_ui::MenuState::new(items, at));
        self.menu_reply = None;
    }

    /// The pin row Neutron adds to the shell's menu, and the folder it acts on.
    ///
    /// The target is worked out when the menu is *built*, not when a row is
    /// chosen: by then the selection may have moved, and a pin that lands on a
    /// different folder than the one right-clicked is worse than no pin at all.
    ///
    /// Right-clicking a folder pins that folder; right-clicking anywhere else
    /// pins the one being shown, which is what `Ctrl+Shift+D` does.
    pub(super) fn menu_pin_row(&self) -> Option<(neutron_core::MenuItem, NodeId)> {
        let tab = self.workspace.active_tab()?;
        let selected_dir = tab
            .selection
            .cursor()
            .filter(|&i| i < tab.list.len())
            .filter(|&i| tab.list.kind(i).is_container())
            .filter(|&i| tab.selection.is_selected(i))
            .map(|i| tab.list.name(i));

        let (target, pinned, name) = match tab.location() {
            NodeId::Path(dir) => {
                let target_path = selected_dir
                    .map(|name| dir.join(name))
                    .unwrap_or_else(|| dir.to_path_buf());
                let pinned = self.pins.folders.contains(&target_path);
                let name = target_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| target_path.to_string_lossy().into_owned());
                (NodeId::Path(target_path), pinned, name)
            }
            NodeId::Remote { site, path } => {
                let target_path = match selected_dir {
                    Some(child) => neutron_remote::path::join(path, child).ok()?,
                    None => path.to_string(),
                };
                let pinned = self.pins.contains_remote(site, &target_path);
                let name = if target_path == "/" {
                    self.sites
                        .get(site)
                        .map(|s| s.display().to_owned())
                        .unwrap_or_else(|| "/".to_owned())
                } else {
                    neutron_remote::path::file_name(&target_path)
                };
                (NodeId::remote(site.as_ref(), target_path), pinned, name)
            }
            _ => return None,
        };

        Some((
            neutron_core::MenuItem {
                id: MENU_ID_PIN,
                label: pin_label(pinned, Some(&name)),
                accel: neutron_ui::format::shortcut(if cfg!(target_os = "macos") { "Control+Cmd+T" } else { "Ctrl+Shift+D" }),
                enabled: true,
                ..Default::default()
            },
            target,
        ))
    }

    /// Pins or unpins `target`, and persists it.
    ///
    /// Shared by the keyboard, the palette and the context menu, so all three
    /// give the same answer to the same question.
    pub(super) fn apply_pin(&mut self, target: NodeId) {
        let mut pins = self.pins.clone();
        let (outcome, name) = match target {
            NodeId::Path(dir) => {
                let outcome = pins.toggle(&dir);
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dir.to_string_lossy().into_owned());
                (outcome, name)
            }
            NodeId::Remote { site, path } => {
                let outcome = pins.toggle_remote(&site, &path);
                let name = if path.as_ref() == "/" {
                    self.sites
                        .get(&site)
                        .map(|s| s.display().to_owned())
                        .unwrap_or_else(|| "/".to_owned())
                } else {
                    neutron_remote::path::file_name(&path)
                };
                (outcome, name)
            }
            _ => {
                self.refuse_without_a_path("pin");
                return;
            }
        };

        if outcome == crate::pins::Toggled::Full {
            // Refused rather than dropping the oldest: that would lose a pin
            // the user made on purpose, and say nothing about it.
            self.notices.info(format!(
                "{} holds {} folders — remove one first",
                if cfg!(target_os = "macos") { "The sidebar" } else { "Quick access" },
                crate::pins::MAX
            ));
            return;
        }

        // Applied here so the sidebar changes on this frame; the write below
        // only makes it outlast the session.
        self.pins = pins.clone();
        self.notices.info(match outcome {
            crate::pins::Toggled::Pinned if cfg!(target_os = "macos") => format!("Added {name} to the sidebar"),
            crate::pins::Toggled::Pinned => format!("Pinned {name} to Quick access"),
            _ if cfg!(target_os = "macos") => format!("Removed {name} from the sidebar"),
            _ => format!("Unpinned {name}"),
        });

        let notices = self.notice_tx.clone();
        let wake = self.ctx.clone();
        self.sta.submit(move || {
            if let Err(e) = crate::pins::save(&pins) {
                report(&notices, format!("Could not save pins: {e}"));
            }
            wake.request_repaint();
        });
    }

    /// Pins the focused folder to Quick access, or unpins it if it is already
    /// there.
    ///
    /// One command for both, because which one applies is a property of where
    /// you are standing rather than something to choose from a menu.
    pub(super) fn toggle_pin(&mut self) {
        let Some(target) = self
            .workspace
            .active_tab()
            .map(|t| t.location().clone())
            .filter(|id| id.as_path().is_some() || id.as_remote().is_some())
        else {
            self.refuse_without_a_path("pin");
            return;
        };

        self.apply_pin(target);
    }

    /// Forgets the Google Drive credential.
    ///
    /// The counterpart to [`Self::connect_drive`]. `sign_out` has existed in
    /// `neutron-cloud` since M7 with nothing calling it, so the only way to
    /// revoke a Drive connection was to delete the entry from Credential
    /// Manager by hand — a credential the application would store and would not
    /// let go of.
    pub(super) fn sign_out_drive(&mut self) {
        use neutron_cloud::google::DriveState;
        if !matches!(self.drive_state, DriveState::SignedIn) {
            // The palette's entries are static, so this one is offered whether
            // or not Drive is connected. Saying so beats doing nothing.
            self.notices.info("Google Drive is not connected");
            return;
        }

        // Optimistic, and deliberately so: the sidebar changes on this frame.
        // A failure below replaces this with the reason.
        self.drive_state = DriveState::SignedOut;
        // The loader caches an access token that outlives the credential by
        // most of an hour. Told here rather than after the worker answers,
        // because a listing must not succeed once the user has disconnected.
        self.loader.sign_out_drive();

        let wake = self.ctx.clone();
        let done = self.drive_tx.clone();
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            let drive = neutron_cloud::google::GoogleDrive::new();
            if let Err(e) = drive.sign_out() {
                tracing::error!("Google Drive sign-out failed: {e}");
                report(&notices, format!("Could not disconnect Google Drive: {e}"));
                let _ = done.send(DriveState::Error(e.to_string()));
            } else {
                report(&notices, "Google Drive disconnected".to_owned());
                let _ = done.send(DriveState::SignedOut);
            }
            wake.request_repaint();
        });
    }

    /// Removes the focused tab's server from `sites.json`, password and all.
    ///
    /// The counterpart to **Connect to a server**. Until this existed a saved
    /// server could only be removed by editing the file by hand, and
    /// `forget_password` — which is what takes the password out of Credential
    /// Manager — had no callers at all, so the password outlived every other
    /// trace of the site.
    ///
    /// Scoped to the focused tab rather than offering a list, because
    /// navigating to a server is already the unambiguous way to say which one,
    /// and a palette row cannot ask a follow-up question.
    pub(super) fn forget_server(&mut self) {
        let Some(id) = self
            .workspace
            .active_tab()
            .and_then(|t| t.location().as_remote())
            .map(|(site, _)| site.to_owned())
        else {
            self.notices
                .info("Open a server first — this forgets the one you are looking at");
            return;
        };

        let Some(site) = self.sites.get(&id).cloned() else {
            self.notices.error(format!("{id} is no longer saved"));
            return;
        };
        let name = site.display().to_owned();

        let sites_tx = self.sites_tx.clone();
        let notices = self.notice_tx.clone();
        let wake = self.ctx.clone();
        self.sta.submit(move || {
            // The password goes first. If the file write fails afterwards the
            // site is still listed and can be re-entered; reversed, a failed
            // clear would leave a credential belonging to nothing, which
            // nothing would ever offer to remove again.
            if let Err(e) = neutron_remote::forget_password(&site) {
                // Not fatal to the removal — a site saved without a password
                // has nothing to clear, and Credential Manager reports that the
                // same way as a real failure.
                tracing::warn!("could not clear the password for {}: {e}", site.id);
            }

            // Re-read before writing, for the reason `connect::save` does it:
            // the loader records SSH host keys into this same file, and writing
            // a copy held in the UI over the top would silently un-learn one.
            match neutron_remote::site::load() {
                Ok(mut on_disk) => {
                    on_disk.sites.retain(|s| s.id != site.id);
                    match neutron_remote::site::save(&on_disk) {
                        Ok(_) => report_info(
                            &notices,
                            format!("Forgot {name} and its saved password"),
                        ),
                        Err(e) => {
                            report(&notices, format!("Could not update sites.json: {e}"))
                        }
                    }
                }
                Err(e) => report(&notices, format!("Could not read sites.json: {e}")),
            }

            // Sent whatever happened, so what is on screen is what is on disk
            // rather than the removal that may not have taken.
            let _ = sites_tx.send(SitesReply {
                sites: neutron_remote::site::load(),
                origin: SitesOrigin::Background,
            });
            wake.request_repaint();
        });
    }

    /// Recomputes the finder's rows for the current mode and needle.
    ///
    /// Commands are matched here and now — a few dozen entries, so a round trip
    /// to the indexer would cost more than the match. The two file modes send a
    /// request and fill in when it answers.
    pub(super) fn refresh_finder(&mut self) {
        if !self.finder.open {
            return;
        }

        match self.finder.mode {
            crate::finder::Mode::Commands => {
                self.remote_search_site = None;
                let matched = crate::finder::match_commands(&self.palette, &self.finder.needle);
                let status = format!("{} of {} commands", matched.len(), self.palette.len());
                self.finder.set_command_rows(matched, status);
            }

            crate::finder::Mode::Files => {
                if let Some((site_id, remote_dir)) = self
                    .workspace
                    .active_tab()
                    .and_then(|t| t.location().as_remote())
                    .map(|(s, p)| (s.to_owned(), p.to_owned()))
                {
                    let Some(site) = self.sites.get(&site_id).cloned() else {
                        self.remote_search_site = None;
                        self.finder.set_rows(Vec::new(), "Unknown remote server".into());
                        return;
                    };
                    if site.protocol != neutron_remote::Protocol::Sftp {
                        self.remote_search_site = None;
                        self.finder.set_rows(
                            Vec::new(),
                            "Server-side search is supported on SFTP servers only".into(),
                        );
                        return;
                    }
                    self.remote_search_site = Some(site_id.clone());
                    let needle = self.finder.needle.trim().to_owned();
                    if needle.is_empty() {
                        self.finder.set_rows(
                            Vec::new(),
                            format!("Type to search {}…", site.display()),
                        );
                        return;
                    }
                    self.remote_search_gen = self.remote_search_gen.wrapping_add(1);
                    let search_gen = self.remote_search_gen;
                    self.finder.set_rows(
                        Vec::new(),
                        format!("Searching {}…", site.display()),
                    );
                    let tx = self.remote_search_tx.clone();
                    let wake = self.ctx.clone();
                    let _ = std::thread::Builder::new()
                        .name("neutron-remote-search".into())
                        .spawn(move || {
                            let res = crate::remote::connect_checked(&site).and_then(|mut t| {
                                t.search(&remote_dir, &needle, 120).map_err(|e| e.to_string())
                            });
                            let _ = tx.send((search_gen, site_id, needle, res));
                            wake.request_repaint();
                        });
                    return;
                }

                self.remote_search_site = None;
                let Some(scope) = self
                    .workspace
                    .active_tab()
                    .and_then(|t| t.location().as_path())
                    .map(|p| p.display().to_string())
                else {
                    self.finder
                        .set_rows(Vec::new(), "This pane has no folder to search".into());
                    return;
                };

                match self.index.state() {
                    IndexState::Ready(_) => self.index.find(&self.finder.needle, &scope),
                    IndexState::Starting => {
                        self.finder.set_rows(Vec::new(), "Indexing…".into());
                    }
                    IndexState::Unavailable(why) => {
                        let why = why.clone();
                        self.finder.set_rows(Vec::new(), why);
                    }
                    IndexState::Idle => self.finder.set_rows(
                        Vec::new(),
                        "Needs the search index — press Enter to set it up".into(),
                    ),
                }
            }

            crate::finder::Mode::Everything => {
                self.remote_search_site = None;
                match self.index.state() {
                    IndexState::Ready(_) => self.index.search(&self.finder.needle),
                    IndexState::Unavailable(why) => {
                        let why = why.clone();
                        self.finder.set_rows(Vec::new(), why);
                    }
                    IndexState::Starting => {
                        self.finder
                            .set_rows(Vec::new(), "Indexing every volume — this happens once".into());
                    }
                    IndexState::Idle => self.finder.set_rows(
                        Vec::new(),
                        "Press Enter to start searching".into(),
                    ),
                }
            }
        }
    }

    /// Copies whatever the indexer last returned into the finder's rows.
    ///
    /// Called every frame rather than on arrival, because the client owns the
    /// staleness check — it drops answers to queries the user has moved past,
    /// and this must not reintroduce them.
    pub(super) fn sync_finder_results(&mut self) {
        if !self.finder.open || self.finder.mode == crate::finder::Mode::Commands {
            return;
        }

        match self.index.results() {
            Some(results) => {
                let rows: Vec<crate::finder::Row> = results
                    .hits
                    .iter()
                    .map(|hit| crate::finder::Row {
                        primary: hit.name.clone(),
                        secondary: hit.parent.clone(),
                        matched: hit.matched.clone(),
                        is_dir: hit.is_dir,
                    })
                    .collect();

                // A cached index is missing everything created since it was
                // written, and a search tool that quietly omits results is
                // worse than one that admits it might. Named where the count
                // is, because that is the number being qualified.
                let cached = match self.index.state() {
                    IndexState::Ready(status) if !status.fresh => format!(
                        "  ·  index is {} — \"Rebuild the search index\" to update it",
                        crate::index_client::describe_age(status.cached_age_secs),
                    ),
                    _ => String::new(),
                };

                let status = format!(
                    "{}{} results in {:.2} ms{cached}",
                    results.total,
                    if results.truncated { "+" } else { "" },
                    results.elapsed_micros as f64 / 1000.0,
                );
                if rows != self.finder.rows {
                    self.finder.set_rows(rows, status);
                } else {
                    self.finder.status = status;
                }
            }
            // A refused query says why in place of results — a helper from an
            // older build that does not know the request, or on the Mac a
            // folder that has gone. The next query is asked as normal.
            None => {
                if let Some(why) = self.index.rejection() {
                    if self.finder.status != why || !self.finder.rows.is_empty() {
                        self.finder.set_rows(Vec::new(), why.to_owned());
                    }
                }
            }
        }
    }

    /// Opens the selected finder row, or runs the selected command.
    pub(super) fn activate_finder_row(&mut self, index: usize) {
        let mode = self.finder.mode;
        let Some(row) = self.finder.rows.get(index).cloned() else {
            return;
        };

        if mode == crate::finder::Mode::Commands {
            // By position, through the invocation the row was built with. A
            // lookup by title would run the wrong command as soon as two
            // plugins offered the same one.
            let Some(invoke) = self.finder.invoke_at(index).cloned() else {
                return;
            };
            self.finder.close();
            let ctx = self.ctx.clone();
            match invoke {
                commands::Invoke::Builtin(id) => self.run_command(&ctx, id),
                commands::Invoke::Plugin(reference) => self.run_plugin(&ctx, &reference),
            }
            return;
        }

        self.finder.close();

        if let Some(site_id) = self.remote_search_site.take() {
            let target = if row.is_dir {
                neutron_remote::path::join(&row.secondary, &row.primary)
                    .unwrap_or_else(|_| row.secondary.clone())
            } else {
                row.secondary.clone()
            };
            if let Some(tab) = self.workspace.active_tab_id() {
                self.navigate(tab, NodeId::remote(site_id, target));
            }
            return;
        }

        // A folder result opens the folder itself; a file result opens the
        // folder containing it, since there is nowhere else to put the user.
        let target = if row.is_dir {
            PathBuf::from(&row.secondary).join(&row.primary)
        } else {
            PathBuf::from(&row.secondary)
        };
        if let Some(tab) = self.workspace.active_tab_id() {
            self.navigate(tab, NodeId::Path(target));
        }
    }

    pub(super) fn pump_remote_search(&mut self) {
        while let Ok((search_gen, site_id, needle, outcome)) = self.remote_search_rx.try_recv() {
            if search_gen != self.remote_search_gen || !self.finder.open {
                continue;
            }
            self.remote_search_site = Some(site_id);
            match outcome {
                Ok(hits) => {
                    let mut matcher = neutron_fuzzy::FuzzyMatcher::new();
                    let count = hits.len();
                    let rows: Vec<crate::finder::Row> = hits
                        .into_iter()
                        .map(|hit| {
                            let primary = neutron_remote::path::file_name(&hit.path);
                            let secondary = neutron_remote::path::parent(&hit.path)
                                .unwrap_or_else(|| "/".to_owned());
                            let matched = matcher
                                .score(&primary, &needle)
                                .map(|m| m.positions)
                                .unwrap_or_default();
                            crate::finder::Row {
                                primary,
                                secondary,
                                matched,
                                is_dir: hit.is_dir,
                            }
                        })
                        .collect();
                    let status = if count == 0 {
                        "No matching remote files".to_owned()
                    } else if count == 1 {
                        "1 remote match".to_owned()
                    } else {
                        format!("{count} remote matches")
                    };
                    self.finder.set_rows(rows, status);
                }
                Err(why) => {
                    self.finder.set_rows(Vec::new(), format!("Search failed: {why}"));
                }
            }
        }
    }

    /// Runs a palette command by turning it back into an ordinary action.
    ///
    /// Deliberately routed through the same actions the keyboard uses, so a
    /// command and its shortcut can never drift apart.
    pub(super) fn run_command(&mut self, ctx: &egui::Context, id: commands::CommandId) {
        use commands::CommandId as C;

        let known_folder = |name: &str| -> Option<NodeId> {
            self.known_folders
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.id.clone())
        };

        let action = match id {
            C::NewTab => self
                .workspace
                .focused_group()
                .map(|g| g.id)
                .map(Action::NewTab),
            C::CloseTab => Some(Action::CloseActiveTab),
            C::SplitRight => Some(Action::Split(Axis::Horizontal)),
            C::SplitDown => Some(Action::Split(Axis::Vertical)),
            C::FocusNextPane => Some(Action::FocusNextPane),

            C::GoUp => Some(Action::Up),
            C::GoBack => Some(Action::Back),
            C::GoForward => Some(Action::Forward),
            C::Refresh => Some(Action::Refresh),

            C::SelectAll => Some(Action::SelectAll),
            C::ClearSelection => Some(Action::ClearSelection),
            C::Copy => Some(Action::Clip { cut: false }),
            C::Cut => Some(Action::Clip { cut: true }),
            C::Paste => Some(Action::Paste),
            C::Rename => Some(Action::BeginRename),
            C::NewFolder => Some(Action::NewFolder),
            C::NewFile => Some(Action::NewFile(None)),
            C::Extract => Some(Action::ExtractSelection),
            C::Compress => Some(Action::CompressSelection),
            C::Duplicate => Some(Action::Duplicate),
            C::Share => Some(Action::Share),
            C::Undo => Some(Action::Undo),
            C::Redo => Some(Action::Redo),
            C::DeleteSelection => Some(Action::Delete { permanent: false }),

            C::ToggleHidden => Some(Action::ToggleHidden),
            C::ToggleView => Some(Action::ToggleView),
            C::ListView => Some(Action::SetView(file_list::ViewMode::List)),
            C::GridView => Some(Action::SetView(file_list::ViewMode::Grid)),
            C::ColumnView => Some(Action::SetView(file_list::ViewMode::Columns)),
            C::TogglePreview => Some(Action::TogglePreview),
            C::ToggleTheme => Some(Action::ToggleTheme),
            C::ThemeStratus => Some(Action::SetTheme(ThemeMode::Stratus)),
            C::ThemeObsidian => Some(Action::SetTheme(ThemeMode::Obsidian)),
            C::ThemeFlux => Some(Action::SetTheme(ThemeMode::Flux)),
            C::ThemeFairy => Some(Action::SetTheme(ThemeMode::Fairy)),
            C::ThemeAura => Some(Action::SetTheme(ThemeMode::Aura)),
            C::ThemeGruvbox => Some(Action::SetTheme(ThemeMode::Gruvbox)),
            C::ThemeLight => Some(Action::SetTheme(ThemeMode::Light)),

            C::FindInFolder => Some(Action::ToggleFinder(crate::finder::Mode::Files)),
            C::SearchEverything => Some(Action::ToggleFinder(crate::finder::Mode::Everything)),
            C::RefreshIndex => Some(Action::RefreshIndex),
            C::StopIndexer => {
                self.index.stop_server();
                None
            }

            C::OpenTerminal => Some(Action::OpenTerminal),
            C::AskClaude => Some(Action::AskAgent(crate::agent::Agent::Claude)),
            C::NewSmartFolder => Some(Action::NewSmartFolder),
            C::AskAntigravity => Some(Action::AskAgent(crate::agent::Agent::Antigravity)),
            C::NewWindow => Some(Action::NewWindow),
            C::CopyGitBranch => Some(Action::CopyGitBranch),
            C::RunNtrStatus => Some(Action::RunNtrStatus),
            C::InspectNtrCi => Some(Action::InspectNtrCi),
            C::ComparePanes => Some(Action::ToggleComparePanes),
            C::SaveWorkspaceLayout | C::SwitchWorkspaceLayout => Some(Action::OpenLayouts),
            C::SyncPanes => self.compare_pair().map(|(lg, _, rg, _)| {
                if self.workspace.focused == rg {
                    Action::SyncPaneToOther { src: rg, dst: lg }
                } else {
                    Action::SyncPaneToOther { src: lg, dst: rg }
                }
            }),

            C::ConnectServer => Some(Action::OpenConnect),
            C::ReloadSites => Some(Action::ReloadSites),
            // Asks first: it takes the password out of Credential Manager,
            // and the user may not know it any more.
            C::ForgetServer => Some(Action::Ask(Box::new(Action::ForgetServer))),
            C::TogglePin => Some(Action::TogglePin),

            C::SignOutDrive => Some(Action::SignOutDrive),

            C::ReloadPlugins => Some(Action::ReloadPlugins),
            C::OpenPluginsFolder => {
                self.open_plugins_folder();
                None
            }

            C::GoHome => known_folder("Home").map(Action::Navigate),
            C::GoDesktop => known_folder("Desktop").map(Action::Navigate),
            C::GoDownloads => known_folder("Downloads").map(Action::Navigate),
            C::GoDocuments => known_folder("Documents").map(Action::Navigate),
        };

        if let Some(action) = action {
            self.apply(ctx, action);
        }
    }

    /// The finder overlay, plus its text field and key handling.
    ///
    /// The field lives here rather than in `finder.rs` because the string it
    /// edits belongs to the app: drawing modules take `&self` like every other
    /// one, and a `TextEdit` needs `&mut String`.
    pub(super) fn finder_overlay(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        if !self.finder.open {
            return;
        }

        if let Some(action) = finder::show(ui, p, &self.finder) {
            actions.push(match action {
                FinderAction::Close => Action::CloseFinder,
                FinderAction::Activate(i) => Action::ActivateFinderRow(i),
            });
        }

        let screen = ui.ctx().content_rect();
        let field = finder::field_rect(screen, self.finder.rows.len());
        let id = egui::Id::new("neutron-finder-field");

        let mut text = self.finder.needle.clone();
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(field)
                .layer_id(egui::LayerId::new(egui::Order::Foreground, "finder".into())),
        );

        let output = egui::TextEdit::singleline(&mut text)
            .id(id)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 2)))
            .desired_width(field.width())
            .font(egui::FontId::proportional(16.0))
            .text_color(p.text)
            .hint_text(
                egui::RichText::new(self.finder.mode.placeholder())
                    .color(p.text_faint)
                    .size(16.0),
            )
            .show(&mut child);

        // The overlay opens on a keystroke, so the caret has to arrive without
        // a click. Requested every frame while open rather than once: egui
        // surrenders focus when the widget is rebuilt, which it is each frame.
        if !output.response.has_focus() {
            child.memory_mut(|m| m.request_focus(id));
        }

        if text != self.finder.needle {
            actions.push(Action::SetFinderNeedle(text));
        }

        // Handled here, not in the main key table: that table is skipped while
        // a text field has focus, which is the whole time the overlay is open.
        child.input(|i| {
            // The shortcuts that opened the overlay, so they still work once it
            // is open — pressing Ctrl+Shift+F again closes it, and the other
            // two switch mode. Without these they did nothing at all and the
            // keystroke's letter was typed into the needle instead, which is
            // how `neutron` became `neutronneutron-index` while testing.
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            if ctrl {
                if i.modifiers.shift && i.key_pressed(egui::Key::F) {
                    actions.push(Action::ToggleFinder(crate::finder::Mode::Everything));
                }
                if i.modifiers.shift && i.key_pressed(egui::Key::P) {
                    actions.push(Action::ToggleFinder(crate::finder::Mode::Commands));
                }
                if !i.modifiers.shift && i.key_pressed(egui::Key::P) {
                    actions.push(Action::ToggleFinder(crate::finder::Mode::Files));
                }
            }

            if i.key_pressed(egui::Key::Escape) {
                actions.push(Action::CloseFinder);
            }
            if i.key_pressed(egui::Key::ArrowDown) {
                actions.push(Action::MoveFinderCursor(1));
            }
            if i.key_pressed(egui::Key::ArrowUp) {
                actions.push(Action::MoveFinderCursor(-1));
            }
            if i.key_pressed(egui::Key::Enter) {
                // With no index and nothing to show, Enter is the one control
                // that can set search up — otherwise it would do nothing at all
                // on the screen that explains why there is nothing.
                if self.finder.rows.is_empty()
                    && matches!(self.index.state(), IndexState::Idle)
                {
                    actions.push(Action::StartIndexer);
                } else {
                    actions.push(Action::ActivateFinderRow(self.finder.cursor));
                }
            }
        });
    }

    /// Whether anything needs saying at the bottom of the window: a notice,
    /// a job in progress, or frames running over budget.
    pub(super) fn has_status(&self) -> bool {
        self.notices.current().is_some()
            || self.archive_job.is_some()
            || !self.transfers.is_empty()
            || self.plugins.running_label().is_some()
            || self.download.is_some()
            || !self.remote_jobs.is_empty()
            || self.startup.frame_percentiles().is_some_and(|(_, p99)| p99 > 8.0)
    }

    /// Notices and jobs, as a pill floating at the bottom of the window.
    ///
    /// Only there when there is something to say. What a folder holds is said
    /// in that folder's own pane — see `pane_footer` — so with nothing going on
    /// the window ends at the bottom of its panes.
    pub(super) fn status_bar(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        // In the window being worked in, not repeated in every one.
        if !self.has_status() || self.viewport != self.last_focused {
            return;
        }

        // A toast over the foot of the window, sized to what it says, rather
        // than a panel: a panel takes its height from every pane for as long
        // as a notice stands, and sits wherever the layout puts it.
        egui::Area::new(self.window_id("status_bar"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
            .show(ui.ctx(), |ui| {
                ui.set_max_width(560.0);
                egui::Frame::new()
                    .fill(p.elevated)
                    .corner_radius(egui::CornerRadius::same(theme::RADIUS_WELL))
                    .stroke(egui::Stroke::new(1.0, p.border_strong))
                    .inner_margin(egui::Margin::symmetric(14, 7))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 10],
                        blur: 28,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(160),
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some((p50, p99)) = self.startup.frame_percentiles() {
                                if p99 > 8.0 {
                                    ui.colored_label(p.warning, format!("{p99:.0}ms p99"))
                                        .on_hover_text(format!(
                                            "Frame build p50 {p50:.1}ms / p99 {p99:.1}ms — over the 8ms budget"
                                        ));
                                }
                            }
                            self.notice_bar(ui, p, actions);
                            self.download_status(ui, p, actions);
                            self.plugin_status(ui, p, actions);
                            self.archive_status(ui, p, actions);
                            self.transfer_status(ui, p, actions);
                        });
                    });
            });
    }

    /// The strip along the bottom of a folder pane: what the folder holds and
    /// what is selected on the left, the keys worth knowing on the right.
    pub(super) fn pane_footer(&self, ui: &mut egui::Ui, p: &Palette, rect: egui::Rect, tab: &crate::workspace::Tab, actions: &mut Vec<Action>) {
        let painter = ui.painter();
        painter.rect_filled(rect, egui::CornerRadius::ZERO, p.card);
        let font = egui::FontId::proportional(12.0);
        let y = rect.center().y;

        let text = match &tab.status {
            Status::Loading => "Loading…".to_owned(),
            Status::Error(_) => String::new(),
            Status::Loaded { entries, .. } => {
                let shown = tab.list.order().len();
                // Said explicitly when filtered, or a narrowed view looks like
                // a folder that lost files.
                let mut text = if shown == *entries {
                    format!("{shown} items")
                } else {
                    format!("{shown} of {entries} items")
                };
                let selected = visible_selection(tab);
                // Found from everywhere, rows of one name can be in many
                // folders: the one picked is named in full.
                if let ([only], NodeId::Tag { .. } | NodeId::Search { .. }) = (&selected[..], tab.location()) {
                    if let Some((target, _)) = tab.list.target(*only) {
                        text = format!("{text} · {target}");
                    }
                } else if !selected.is_empty() {
                    let bytes = tab.selection.total_size(&tab.list);
                    text.push_str(&format!(" · {} selected", selected.len()));
                    if bytes > 0 {
                        text.push_str(&format!(" · {}", neutron_ui::format::size(Some(bytes))));
                    }
                }
                text
            }
        };
        let counted = painter.text(pos(rect.left() + 12.0, y), egui::Align2::LEFT_CENTER, text, font.clone(), p.text_faint);
        if let Status::Loaded { elapsed, .. } = &tab.status {
            ui.interact(counted, ui.id().with("footer-count"), egui::Sense::hover())
                .on_hover_text(format!("Listed and sorted in {:.0} ms", elapsed.as_secs_f64() * 1000.0));
        }

        // In the grid, the tiles' size instead, as the Finder's icon view has
        // a slider in its status bar: a small tile, the slider, a large one.
        if tab.view.view == file_list::ViewMode::Grid {
            let right = rect.right() - 12.0;
            let small = egui::Rect::from_center_size(pos(right - 152.0, y), egui::vec2(9.0, 9.0));
            let large = egui::Rect::from_center_size(pos(right - 7.0, y), egui::vec2(14.0, 14.0));
            for r in [small, large] {
                ui.painter().rect_stroke(r, 2.5, egui::Stroke::new(1.2, p.text_faint), egui::StrokeKind::Inside);
            }
            let track = egui::Rect::from_min_max(pos(small.right() + 8.0, y - 8.0), pos(large.left() - 8.0, y + 8.0));
            // The theme's rail is the footer's own colour; this one shows.
            ui.painter().line_segment(
                [pos(track.left() + 4.0, y), pos(track.right() - 4.0, y)],
                egui::Stroke::new(3.0, theme::with_alpha(p.text_faint, 90)),
            );
            let mut scale = tab.view.tile_scale();
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(track).layout(egui::Layout::left_to_right(egui::Align::Center)));
            child.spacing_mut().slider_width = track.width();
            child.spacing_mut().interact_size.y = track.height();
            let slider = egui::Slider::new(&mut scale, file_list::TILE_SCALE_MIN..=file_list::TILE_SCALE_MAX)
                .show_value(false)
                .handle_shape(egui::style::HandleShape::Circle);
            if child.add(slider).on_hover_text("Tile size").changed() {
                actions.push(Action::SetTileScale(scale));
            }
            return;
        }

        // The keys, right-aligned: the two that open everything else.
        let mac = cfg!(target_os = "macos");
        let columns = tab.view.view == file_list::ViewMode::Columns;
        let hints: Vec<(&str, &str)> = if columns {
            vec![("←", ""), ("→", "move between columns")]
        } else {
            vec![
                (if mac { "⌘P" } else { "Ctrl+P" }, "find"),
                (if mac { "⌘⇧P" } else { "Ctrl+Shift+P" }, "commands"),
            ]
        };
        let mut x = rect.right() - 10.0;
        for (key, label) in hints.iter().rev() {
            if !label.is_empty() {
                let galley = painter.layout_no_wrap(label.to_string(), font.clone(), p.text_faint);
                x -= galley.size().x;
                painter.galley(pos(x, y - galley.size().y / 2.0), galley, p.text_faint);
                x -= 6.0;
            }
            let cap = painter.layout_no_wrap(key.to_string(), theme::strong(ui.ctx(), 11.0), p.text_muted);
            let size = cap.size() + egui::vec2(14.0, 4.0);
            let cap_rect = egui::Rect::from_min_size(pos(x - size.x, y - size.y / 2.0), size);
            painter.rect_filled(cap_rect, egui::CornerRadius::same(6), p.inset);
            painter.galley(pos(cap_rect.left() + 7.0, y - cap.size().y / 2.0), cap, p.text_muted);
            x = cap_rect.left() - if label.is_empty() { 4.0 } else { 14.0 };
        }
    }

    /// The running archive job, or what the last one did.
    ///
    /// Lives in the status bar rather than in a dialog. Extracting is not modal
    /// — there is no reason the user cannot carry on browsing while it runs —
    /// and a window that steals focus to say "43%" is the thing every other
    /// archiver gets wrong.
    pub(super) fn archive_status(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        if let Some(job) = self.archive_job.as_ref() {
            if ui
                .add(egui::Button::new(egui::RichText::new("Stop").size(11.0)).small())
                .on_hover_text("Stop this archive job. What has been written already stays.")
                .clicked()
            {
                actions.push(Action::CancelArchiveJob);
            }

            let progress = job.state.progress;
            let text = if progress.total > 0 {
                format!(
                    "{} — {:.0}%",
                    job.state.label,
                    (progress.done as f64 / progress.total as f64) * 100.0
                )
            } else {
                // A tar is a stream and a zip being written has no known total,
                // so there is no percentage to give. The byte count still moves,
                // which is what says it has not hung.
                format!(
                    "{} — {}",
                    job.state.label,
                    neutron_ui::format::size(Some(progress.done))
                )
            };
            ui.colored_label(p.accent, text);
        }
    }

    /// Unpacks the selected archive into a folder beside it.
    pub(super) fn extract_selection(&mut self) {
        let Some((_, dir, paths)) = self.selection_paths() else {
            return;
        };
        // One at a time, and the first selected archive is the one meant. A
        // selection of several archives is far more likely to be a stray
        // Ctrl+A than a request to unpack all of them at once.
        let Some(archive) = paths
            .iter()
            .find(|p| neutron_archive::format_of(p).is_some())
            .cloned()
        else {
            self.notices.error("That is not an archive Neutron can open");
            return;
        };
        let Some(format) = neutron_archive::format_of(&archive) else {
            return;
        };

        let _ = dir;
        let ctx = self.ctx.clone();
        self.start_archive_job(move || crate::archive_ops::extract(archive, format, ctx));
    }

    /// Zips the selection into a new archive beside it.
    pub(super) fn compress_selection(&mut self) {
        let Some((_, dir, paths)) = self.selection_paths() else {
            return;
        };

        let ctx = self.ctx.clone();
        self.start_archive_job(move || crate::archive_ops::compress(paths, dir, ctx));
    }

    pub(super) fn start_archive_job(&mut self, start: impl FnOnce() -> crate::archive_ops::ArchiveJob) {
        if self.archive_job.is_some() {
            // Refused rather than queued: archive work is disk-bound, so two at
            // once makes both slower, and the status bar has room for one line.
            self.notices
                .info("Already busy with an archive — wait for it to finish");
            return;
        }
        self.notices.dismiss_info();
        self.archive_job = Some(start());
    }

    /// Applies whatever the archive worker has reported.
    pub(super) fn pump_archive_job(&mut self) {
        let Some(job) = self.archive_job.as_mut() else {
            return;
        };
        let Some(finished) = job.poll() else {
            return;
        };

        let directory = job.directory.clone();
        self.archive_job = None;
        let (level, text) = match finished {
            crate::archive_ops::Finished::Done { label, summary } => {
                if !summary.refused.is_empty() {
                    // Logged in full, summarised on screen: the status bar has
                    // one line and a hostile archive can refuse hundreds.
                    for refusal in &summary.refused {
                        tracing::warn!("archive: {refusal}");
                    }
                }
                (
                    crate::notice::Level::Info,
                    format!("{label} — {}", crate::archive_ops::describe(&summary)),
                )
            }
            crate::archive_ops::Finished::Failed { label, error } => (
                crate::notice::Level::Error,
                format!("{label} failed: {error}"),
            ),
        };
        self.notices.raise_reported(level, text);

        // Refresh the originating folder even when no watcher is available or
        // the user has changed panes while the job was running.
        let _ = self.changed_tx.send(directory);
    }

    /// `%APPDATA%\\Neutron\\plugins`, created if it is not there yet.
    ///
    /// Created rather than reported missing, because the only reason to ask for
    /// it is to put a plugin in it, and "the folder does not exist" is not a
    /// useful answer to that.
    /// Creating it is a write, so it happens on a worker and the pane follows
    /// once the folder is there. `%APPDATA%` is an ordinary local directory for
    /// most people and a redirected network share for some, and this code
    /// cannot tell which it is looking at.
    pub(super) fn open_plugins_folder(&mut self) {
        let Some(dir) = neutron_plugin::discover::plugins_dir() else {
            self.notices
                .error(format!("There is no {} to keep plugins in", crate::persist::settings_folder_name()));
            return;
        };
        let Some(tab) = self.workspace.active_tab_id() else {
            return;
        };

        let goto = self.goto_tx.clone();
        let notices = self.notice_tx.clone();
        let wake = self.ctx.clone();
        self.sta.submit(move || {
            match std::fs::create_dir_all(&dir) {
                Ok(()) => {
                    let _ = goto.send((tab, NodeId::Path(dir)));
                }
                Err(e) => report(&notices, format!("Could not open the plugins folder: {e}")),
            }
            wake.request_repaint();
        });
    }

    /// Runs a plugin command against the focused tab.
    ///
    /// The folder and selection are read here, at the moment the command is
    /// chosen, rather than being carried in the action — a palette left open
    /// while another pane is clicked would otherwise act on the wrong one.
    pub(super) fn run_plugin(&mut self, ctx: &egui::Context, reference: &crate::plugins::Ref) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(folder) = self
            .workspace
            .tabs
            .get(&tab_id)
            .and_then(|t| t.location().as_path())
            .map(Path::to_path_buf)
        else {
            self.refuse_without_a_path("run plugins in");
            return;
        };

        let selection = self
            .selection_paths()
            .map(|(_, _, paths)| paths)
            .unwrap_or_default();

        self.plugins
            .start(reference, tab_id, folder, selection, ctx);
    }

    /// Collects failures reported from workers, and clears a notice that has
    /// run its course.
    pub(super) fn drain_notices(&mut self, ctx: &egui::Context) {
        while let Ok((level, text)) = self.notice_rx.try_recv() {
            match level {
                crate::notice::Level::Error => {
                    // Already logged where it was raised; raising it here would
                    // log it twice.
                    self.notices.raise_reported(level, text);
                }
                crate::notice::Level::Info => self.notices.info(text),
            }
        }

        // The plugin runtime keeps its own pending message rather than holding
        // a reference to the surface, so it is collected here.
        if let Some((level, text)) = self.plugins.note.take() {
            self.notices.raise_reported(level, text);
        }

        // Without this an information notice would sit there until something
        // else happened to cause a repaint.
        if let Some(remaining) = self.notices.tick() {
            ctx.request_repaint_after(remaining);
        }
    }

    /// Applies whatever the plugin worker reported.
    pub(super) fn pump_plugin(&mut self, ctx: &egui::Context) {
        let Some((tab, outcome)) = self.plugins.poll() else {
            return;
        };

        if let Some(error) = &outcome.error {
            tracing::warn!("plugin: {error}");
        }

        // Navigation before the refresh, since navigating re-lists anyway.
        if let Some(path) = outcome.navigate {
            self.navigate(tab, NodeId::Path(path));
        } else if outcome.refresh {
            self.refresh_in_place(tab);
        }

        for path in outcome.open {
            self.launch(path, neutron_shell::open::Verb::Open);
        }

        ctx.request_repaint();
    }

    /// Opens a terminal at the focused tab's folder.
    ///
    /// Runs on an apartment thread like every other launch: creating a process
    /// reads the registry and can block, and the frame must not.
    pub(super) fn open_terminal(&mut self) {
        if let Some((site_id, remote_dir)) = self
            .workspace
            .active_tab()
            .and_then(|t| t.location().as_remote())
            .map(|(s, p)| (s.to_owned(), p.to_owned()))
        {
            let Some(site) = self.sites.get(&site_id).cloned() else {
                self.notices.error(format!("No saved server called {site_id:?}"));
                return;
            };
            if site.protocol != neutron_remote::Protocol::Sftp {
                self.notices.info("SSH terminals work with SFTP servers only");
                return;
            }
            let local_cwd = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/"));
            let label = format!(
                "{} — {}",
                site.display(),
                neutron_remote::path::file_name(&remote_dir),
            );
            let program = crate::term::Program::ssh(&site, &remote_dir);
            self.open_program(local_cwd, program, &label);
            return;
        }
        let Some(dir) = self.focused_folder("open a terminal in") else {
            return;
        };
        self.open_program(dir, crate::term::Program::shell(), "Terminal");
    }

    /// Opens an agent on the focused folder, told what is selected there.
    pub(super) fn ask_agent(&mut self, agent: crate::agent::Agent) {
        if let Some((site_id, remote_dir)) = self
            .workspace
            .active_tab()
            .and_then(|t| t.location().as_remote())
            .map(|(s, p)| (s.to_owned(), p.to_owned()))
        {
            let Some(site) = self.sites.get(&site_id).cloned() else {
                self.notices.error(format!("No saved server called {site_id:?}"));
                return;
            };
            if site.protocol != neutron_remote::Protocol::Sftp {
                self.notices.info("Remote agents require an SFTP server");
                return;
            }
            let selected: Vec<String> = self
                .workspace
                .active_tab()
                .map(|t| {
                    visible_selection(t)
                        .into_iter()
                        .map(|i| t.list.name(i).to_owned())
                        .collect()
                })
                .unwrap_or_default();
            let local_cwd = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/"));
            let label = format!("{} · {}", agent.label(), site.display());
            let program = crate::agent::remote_program(agent, &site, &remote_dir, &selected);
            self.open_program(local_cwd, program, &label);
            return;
        }
        let Some(dir) = self.focused_folder("ask an agent about") else { return };
        let selected = self.chosen_paths().unwrap_or_default();
        let program = crate::agent::program(agent, &dir, &selected);
        self.open_program(dir, program, agent.label());
    }

    pub(super) fn copy_git_branch(&mut self, ctx: &egui::Context) {
        let Some(tab) = self.workspace.active_tab() else { return };
        let Some(branch) = self.vcs.peek(tab.location()).and_then(|v| v.branch.clone()) else {
            self.notices.info("This folder is not inside a Git repository");
            return;
        };
        ctx.copy_text(branch.clone());
        self.notices.info(format!("Copied branch “{branch}”"));
    }

    pub(super) fn run_ntr_terminal(&mut self, ci: bool) {
        let Some(tab) = self.workspace.active_tab() else { return };
        let loc = tab.location().clone();
        let slug = self.vcs.peek(&loc).and_then(|v| v.ntr.as_ref().map(|n| n.slug.clone()));
        let run_id = self
            .vcs
            .peek(&loc)
            .and_then(|v| v.ntr.as_ref().and_then(|n| n.ci.as_ref().map(|(id, _)| id.clone())));

        let snippet = match (ci, slug.as_deref(), run_id.as_deref()) {
            (true, Some(s), Some(id)) => {
                format!("ntr -r {s} runs && echo '' && ntr -r {s} run {id}")
            }
            (true, Some(s), None) => format!("ntr -r {s} runs"),
            (false, Some(s), _) => format!("git status -sb && echo '' && ntr -r {s} runs | head -n 5"),
            (_, None, _) => "git status -sb".to_owned(),
        };
        let label = if ci { "Neutrino CI" } else { "Neutrino Status" };

        if let Some((site_id, remote_dir)) = loc.as_remote() {
            let Some(site) = self.sites.get(site_id).cloned() else { return };
            let dir_q = neutron_remote::sftp::shell_quote_posix(&neutron_remote::path::normalize(remote_dir));
            let inner = format!("cd {dir_q} && export PATH=\"$HOME/.neutrino/bin:$PATH\"; {snippet}; exec ${{SHELL:-/bin/sh}} -l");
            let mut argv = crate::term::ssh_base_args(&site);
            argv.push(inner);
            let local_cwd = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/"));
            self.open_program(
                local_cwd,
                crate::term::Program { command: Some(argv), env: Vec::new() },
                label,
            );
            return;
        }
        if let Some(dir) = loc.as_path().map(Path::to_path_buf) {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let inner = format!("export PATH=\"$HOME/.neutrino/bin:$PATH\"; {snippet}; exec \"$0\" -l");
            let argv = vec![shell.clone(), "-l".into(), "-c".into(), inner, shell];
            self.open_program(
                dir,
                crate::term::Program { command: Some(argv), env: Vec::new() },
                label,
            );
        }
    }

    /// Opens a terminal tab in the focused pane running `program` in `dir`.
    pub(super) fn open_program(&mut self, dir: PathBuf, program: crate::term::Program, label: &str) {
        let Some(group) = self.workspace.focused_group().map(|g| g.id) else { return };
        let location = NodeId::terminal(&dir);
        let NodeId::Terminal { id, .. } = location else { return };
        match crate::term::Session::spawn(&dir, &program, label, self.ctx.clone()) {
            Ok(session) => {
                self.terminals.borrow_mut().insert(id, session);
                self.terminal_focus.borrow_mut().insert(id);
                let view = self.workspace.view_for_new_tab();
                self.workspace.open_tab(group, location, view);
                self.workspace.focused = group;
            }
            Err(e) => self.notices.error(e),
        }
    }

    /// Writes the saved searches down, on a worker.
    pub(super) fn save_searches(&self) {
        let saved = self.saved_searches.clone();
        let notices = self.notice_tx.clone();
        self.sta.submit(move || {
            if let Err(e) = crate::searches::save(&saved) {
                report(&notices, format!("The saved searches could not be written: {e}"));
            }
        });
    }

    /// Ends the terminals no tab in any window shows any more.
    pub(super) fn reap_terminals(&mut self) {
        let shown: std::collections::HashSet<u64> = std::iter::once(&self.workspace)
            .chain(self.windows.iter().map(|w| &w.workspace))
            .flat_map(|ws| ws.tabs.values())
            .filter_map(|t| match t.location() {
                NodeId::Terminal { id, .. } => Some(*id),
                _ => None,
            })
            .collect();
        self.terminals.get_mut().retain(|id, _| shown.contains(id));
        let viewed: std::collections::HashSet<PathBuf> = std::iter::once(&self.workspace)
            .chain(self.windows.iter().map(|w| &w.workspace))
            .flat_map(|ws| ws.tabs.values())
            .filter_map(|t| match t.location() {
                NodeId::Viewer { path } => Some(path.clone()),
                _ => None,
            })
            .collect();
        let viewers = self.viewers.get_mut();
        viewers.retain(&viewed);
        viewers.pump(&self.ctx);
    }

    /// Opens a second Neutron window on the focused tab's folder.
    /// Opens another window on the focused tab's location, in this process:
    /// its own panes and tabs, sharing everything else — the icon cache, the
    /// places, the jobs, the undo history.
    pub(super) fn new_window(&mut self) {
        let location = self.workspace.active_tab().map(|t| t.location().clone()).unwrap_or(NodeId::Home);
        let view = self.workspace.view_for_new_tab();
        self.windows_opened += 1;
        let viewport = egui::ViewportId::from_hash_of(("neutron-window", self.windows_opened));
        // A step down and across from the window it was opened from, as each
        // new document window steps from the last.
        let step = 28.0 * (self.windows.len() + 1) as f32;
        let origin = self.ctx.input(|i| i.viewport().outer_rect).map(|r| r.min + egui::vec2(step, step));
        let mut window = WindowState::new(&self.ctx, viewport, origin, location.clone());
        if let Some(tab) = window.workspace.active_tab_mut() {
            tab.view = view;
        }
        // Loaded with the new window's state in place, so the load is for its
        // tab and lands there.
        self.swap_window(&mut window);
        if let Some(tab) = self.workspace.active_tab_id() {
            self.reload(tab, location);
        }
        self.swap_window(&mut window);
        self.windows.push(window);
    }

    /// The focused tab's folder, or a notice saying why there isn't one.
    ///
    /// "This PC", a server and a Drive folder are all real places with no path
    /// a process can be started in, and both callers need exactly that answer.
    pub(super) fn focused_folder(&mut self, what: &str) -> Option<PathBuf> {
        let location = self.workspace.active_tab()?.location().clone();
        if let Some(path) = location.as_path() {
            return Some(path.to_path_buf());
        }
        let place = crate::place::pathless(&location, &self.sites)?;
        self.notices.info(crate::place::cannot(what, &place));
        None
    }

    /// Refuses an operation that needs a filesystem path, and says where the
    /// pane is pointing instead.
    ///
    /// Every operation guarded by this reaches for `location().as_path()` and
    /// finds nothing in a shell folder, a cloud folder or a server. Without
    /// this they are silent no-ops — the user presses Delete and nothing at all
    /// happens, which reads as a bug in the key handling rather than as a
    /// missing feature.
    ///
    /// This used to check only for a server, so a zip and a Drive folder — the
    /// other two pathless cases — went on failing silently, which is the exact
    /// thing `notice.rs` exists to end. See [`crate::place`] for why the
    /// sentence differs between them.
    pub(super) fn refuse_without_a_path(&mut self, what: &str) -> bool {
        let Some(place) = self
            .workspace
            .active_tab()
            .and_then(|t| crate::place::pathless(t.location(), &self.sites))
        else {
            return false;
        };
        // Information, not an error: nothing failed. The user asked for
        // something Neutron does not do here, and a receipt that clears itself
        // is the right weight for that.
        self.notices.info(crate::place::cannot(what, &place));
        true
    }

    /// Saves the dialog's server, then opens it in the focused tab.
    pub(super) fn submit_connect(&mut self) {
        let site = match self.connect.to_site(&self.sites) {
            Ok(site) => site,
            Err(reason) => {
                self.connect.error = Some(reason);
                return;
            }
        };

        // Writing the site is a write to `%APPDATA%` and a call into
        // Credential Manager, so it happens on a worker. The dialog stays open
        // until the reply arrives — it is the only place the password exists,
        // and closing it before the save is known to have worked would lose it.
        let password = self.connect.password().to_owned();
        let tab = self.workspace.active_tab_id();
        let start = site.start_path();
        let target = NodeId::remote(site.id.as_str(), start.as_str());

        self.notices.info(format!("Connecting to {}…", site.address()));

        let sites_tx = self.sites_tx.clone();
        let goto = self.goto_tx.clone();
        let wake = self.ctx.clone();
        self.sta.submit(move || {
            let result = crate::connect::save(&site, &password);
            let ok = result.is_ok();
            // Re-read rather than pushing the new site onto a list held in the
            // UI: the file is the truth, and this is also how the sidebar picks
            // up an edit made by hand.
            let sites = neutron_remote::site::load();
            let _ = sites_tx.send(SitesReply {
                sites,
                origin: SitesOrigin::Dialog(result),
            });
            if ok {
                if let Some(tab) = tab {
                    let _ = goto.send((tab, target));
                }
            }
            wake.request_repaint();
        });
    }

    /// Re-reads `sites.json` on a worker, and applies it when it arrives.
    ///
    /// Reading it here would be a read of `%APPDATA%`, which is an ordinary
    /// local directory for most people and a redirected network share for some
    /// — and this code cannot tell which it is looking at.
    pub(super) fn refresh_sites(&mut self, origin: SitesOrigin) {
        let sites_tx = self.sites_tx.clone();
        let wake = self.ctx.clone();
        self.sta.submit(move || {
            let _ = sites_tx.send(SitesReply {
                sites: neutron_remote::site::load(),
                origin,
            });
            wake.request_repaint();
        });
    }

    /// Fetches a remote file into the cache, then opens it with the shell.
    ///
    /// A copy, deliberately: editing it does not write back. Saying so is
    /// better than pretending to a two-way sync that this protocol layer does
    /// not do — see the note in the status bar when it lands.
    pub(super) fn start_download(&mut self, site_id: &str, remote_path: String, name: String) {
        if self.download.is_some() {
            self.notices.info("Already fetching a file — wait for it to finish");
            return;
        }

        let Some(site) = self.sites.get(site_id).cloned() else {
            self.notices.error(format!("No saved server called {site_id:?}"));
            return;
        };
        let Some(destination) = crate::remote::cache_path(site_id, &remote_path) else {
            self.notices.error(format!("{name} cannot be saved under that name"));
            return;
        };

        let bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::bounded(1);

        let worker_bytes = std::sync::Arc::clone(&bytes);
        let worker_cancel = std::sync::Arc::clone(&cancel);
        let wake = self.ctx.clone();
        let target = destination.clone();
        let source = (site_id.to_owned(), remote_path.clone(), name.clone());

        let spawned = std::thread::Builder::new()
            .name("neutron-download".into())
            .spawn(move || {
                let outcome = crate::remote::download(
                    &site,
                    &remote_path,
                    &target,
                    &mut |done| {
                        worker_bytes.store(done, std::sync::atomic::Ordering::Relaxed);
                        // Repainting per chunk would cost more than the
                        // transfer on a fast link; the status bar is already
                        // repainting for the drifting ground.
                        if worker_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            neutron_remote::Continue::Stop
                        } else {
                            neutron_remote::Continue::Yes
                        }
                    },
                );
                let _ = tx.send(outcome.map(|_| target));
                wake.request_repaint();
            });

        match spawned {
            Ok(_) => {
                self.notices.dismiss_info();
                self.download = Some(DownloadJob {
                    label: format!("Fetching {name}"),
                    bytes,
                    cancel,
                    done: rx,
                    remote_source: Some(source),
                });
            }
            Err(e) => self.notices.error(format!("Could not start the download: {e}")),
        }
    }

    /// Fetches a Drive object into the cache, then opens it with the shell.
    ///
    /// Shares [`DownloadJob`] with the server download — same progress, same
    /// Stop, same "opened a local copy" receipt — because from the user's side
    /// it is the same thing happening.
    ///
    /// The metadata request comes first and on the worker. Which of the three
    /// ways to fetch an object applies depends on its MIME type, and the
    /// listing deliberately does not keep that: it is one field the row never
    /// renders, and one request before a transfer costs nothing next to the
    /// transfer.
    pub(super) fn start_drive_download(&mut self, file_id: String, name: String) {
        if self.download.is_some() {
            self.notices.info("Already fetching a file — wait for it to finish");
            return;
        }

        let Some(cache) = crate::cloud::cache_dir() else {
            self.notices.error("There is no temp directory to fetch into");
            return;
        };

        let bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::bounded(1);

        let worker_bytes = std::sync::Arc::clone(&bytes);
        let worker_cancel = std::sync::Arc::clone(&cancel);
        let wake = self.ctx.clone();

        let spawned = std::thread::Builder::new()
            .name("neutron-drive-download".into())
            .spawn(move || {
                let outcome = crate::cloud::fetch(
                    &file_id,
                    &cache,
                    &mut |done| {
                        worker_bytes.store(done, std::sync::atomic::Ordering::Relaxed);
                        if worker_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            neutron_cloud::drive::Continue::Stop
                        } else {
                            neutron_cloud::drive::Continue::Yes
                        }
                    },
                );
                let _ = tx.send(outcome);
                wake.request_repaint();
            });

        match spawned {
            Ok(_) => {
                self.notices.dismiss_info();
                self.download = Some(DownloadJob {
                    label: format!("Fetching {name}"),
                    bytes,
                    cancel,
                    done: rx,
                    remote_source: None,
                });
            }
            Err(e) => self.notices.error(format!("Could not start the download: {e}")),
        }
    }

    /// Applies a finished download.
    pub(super) fn pump_download(&mut self) {
        let Some(job) = self.download.as_ref() else {
            return;
        };
        let Ok(outcome) = job.done.try_recv() else {
            return;
        };
        let label = job.label.clone();
        let remote_source = job.remote_source.clone();
        self.download = None;

        match outcome {
            Ok(path) => {
                if let Some((site_id, remote_path, name)) = remote_source {
                    let meta = std::fs::metadata(&path).ok();
                    let last_mtime = meta.as_ref().and_then(|m| m.modified().ok());
                    let last_len = meta.as_ref().map_or(0, |m| m.len());
                    self.remote_edits.retain(|e| e.local_path != path);
                    if self.remote_edits.len() >= 32 {
                        self.remote_edits.remove(0);
                    }
                    let server_name = self
                        .sites
                        .get(&site_id)
                        .map_or_else(|| "the server".to_owned(), |s| s.display().to_owned());
                    self.remote_edits.push(WatchedRemoteEdit {
                        site_id,
                        remote_path,
                        name: name.clone(),
                        local_path: path.clone(),
                        last_mtime,
                        last_len,
                        syncing: false,
                    });
                    self.notices.info(format!("Opened {name} — saves sync back to {server_name}"));
                } else {
                    self.notices.info(format!("{label} — opened a local copy"));
                }
                self.launch(path, neutron_shell::open::Verb::Open);
            }
            Err(reason) => self.notices.error(format!("{label} — {reason}")),
        }
    }

    /// The running download, or what the last remote operation said.
    pub(super) fn download_status(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        for (idx, job) in self.remote_jobs.iter().enumerate() {
            if idx >= 2 {
                let more = self.remote_jobs.len() - idx;
                ui.colored_label(p.text_faint, format!("+{more} queued"));
                break;
            }
            if ui
                .add(egui::Button::new(egui::RichText::new("Stop").size(11.0)).small())
                .on_hover_text("Stop after the item in progress. Any partial file is removed.")
                .clicked()
            {
                actions.push(Action::CancelRemoteJob(job.id));
            }
            let sent = job.progress.bytes.load(std::sync::atomic::Ordering::Relaxed);
            let bps = job.progress.throughput_bps();
            let label = if sent > 0 && bps >= 1.0 {
                format!(
                    "{} — {} ({}/s)",
                    job.label,
                    neutron_ui::format::size(Some(sent)),
                    neutron_ui::format::size(Some(bps as u64)),
                )
            } else if sent > 0 {
                format!("{} — {}", job.label, neutron_ui::format::size(Some(sent)))
            } else {
                format!("{}…", job.label)
            };
            ui.colored_label(p.accent, label);
        }
        if let Some(job) = self.download.as_ref() {
            if ui
                .add(egui::Button::new(egui::RichText::new("Stop").size(11.0)).small())
                .on_hover_text("Stop this download. The partial file is removed.")
                .clicked()
            {
                actions.push(Action::CancelDownload);
            }
            // Bytes rather than a percentage: neither protocol reliably reports
            // a size up front, and a percentage that jumps or stalls at 99 is
            // worse than a number that is plainly still moving.
            let done = job.bytes.load(std::sync::atomic::Ordering::Relaxed);
            ui.colored_label(
                p.accent,
                format!("{} — {}", job.label, neutron_ui::format::size(Some(done))),
            );
        }
    }

    /// The connect dialog, and the text fields it cannot draw itself.
    pub(super) fn connect_overlay(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        if !self.connect.open {
            return;
        }

        if let Some(action) = crate::connect::show(ui, p, &self.connect) {
            actions.push(match action {
                crate::connect::ConnectAction::Close => Action::CloseConnect,
                crate::connect::ConnectAction::Submit => Action::SubmitConnect,
                crate::connect::ConnectAction::SetProtocol(protocol) => {
                    Action::SetConnectProtocol(protocol)
                }
            });
        }

        let screen = ui.ctx().content_rect();

        // Where the caret should be when nothing in the dialog has it: the
        // first field still empty, so opening the dialog starts at the top and
        // clicking a protocol chip — which takes focus away, being an ordinary
        // clickable — returns it to where there is still work to do rather than
        // dropping it into the file list behind.
        let field_id = crate::connect::field_id;
        let focused = ui.memory(|m| m.focused());
        let ours = crate::connect::FIELDS
            .iter()
            .any(|&f| focused == Some(field_id(f)));
        let wants_caret = crate::connect::FIELDS
            .iter()
            .copied()
            .find(|&f| self.connect.value(f).is_empty())
            .unwrap_or(crate::connect::FIELDS[0]);

        for &field in crate::connect::FIELDS {
            let rect = crate::connect::field_rect(screen, field);
            let id = field_id(field);

            let mut text = self.connect.value(field).to_owned();
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layer_id(egui::LayerId::new(
                        egui::Order::Foreground,
                        "neutron-connect".into(),
                    )),
            );

            let output = egui::TextEdit::singleline(&mut text)
                .id(id)
                .password(crate::connect::is_secret(field))
                .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 2)))
                .desired_width(rect.width())
                .font(egui::FontId::proportional(13.0))
                .text_color(p.text)
                .hint_text(
                    egui::RichText::new(self.connect.hint(field))
                        .color(p.text_faint)
                        .size(13.0),
                )
                .show(&mut child);

            if !ours && field == wants_caret {
                child.memory_mut(|m| m.request_focus(id));
            }

            if text != self.connect.value(field) {
                actions.push(Action::SetConnectField(field, text));
            }

            let _ = output;
        }

        // Handled here rather than in the main key table, which is skipped
        // while a text field has focus — which is the whole time this is open.
        ui.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                actions.push(Action::CloseConnect);
            }
            if i.key_pressed(egui::Key::Enter) {
                actions.push(Action::SubmitConnect);
            }
        });
    }

    /// The one line that says what just happened.
    ///
    /// Drawn last in the right-to-left row, so it takes whatever width is left
    /// after the running-job indicators rather than pushing them off the edge.
    /// Clicking it dismisses it — which is the only way to clear an error, and
    /// is why an error is drawn as something you can click.
    pub(super) fn notice_bar(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        let Some(notice) = self.notices.current() else {
            return;
        };

        let error = notice.level == crate::notice::Level::Error;
        let text = if error {
            // The glyph survives being read out of the corner of an eye, which
            // a sentence in a slightly different colour does not.
            format!("⚠ {}", notice.text)
        } else {
            notice.text.clone()
        };

        let response = ui
            .add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .color(if error { p.danger } else { p.text_soft() }),
                )
                .truncate()
                .sense(egui::Sense::click()),
            )
            .on_hover_text(if error {
                "Click to dismiss. The full error is in the log."
            } else {
                "Click to dismiss"
            });

        if response.clicked() {
            actions.push(Action::DismissNotice);
        }
    }

    /// The running plugin command, or what the last one said.
    pub(super) fn plugin_status(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        if let Some(label) = self.plugins.running_label() {
            if ui
                .add(egui::Button::new(egui::RichText::new("Stop").size(11.0)).small())
                .on_hover_text("Stop this plugin command")
                .clicked()
            {
                actions.push(Action::CancelPluginCommand);
            }
            // No percentage: the protocol has no progress, and inventing one
            // would be a lie. A named command and a Stop is the honest amount.
            ui.colored_label(p.accent, format!("{label}…"));
        }
    }

    /// Starts and stops watchers so they match the folders tabs are showing.
    ///
    /// Run every frame and diffed rather than driven by navigation events,
    /// because there are a dozen ways a pane's folder can change — navigate,
    /// back, a tab closing, a split — and one of them will always be missed.
    /// The set of visible folders is the truth; this makes the watchers agree
    /// with it.
    pub(super) fn sync_watchers(&mut self, ctx: &egui::Context) {
        use std::collections::HashSet;

        // Every tab's folder, not only the visible ones. A background tab
        // that is kept current is right the moment it is switched to; watching
        // only the active tab would leave it showing whatever was true when it
        // was last on screen. The cost is a blocked thread per open tab, and
        // tabs are counted in ones.
        // Every window's tabs: a watcher dropped because the window being
        // drawn does not show its folder would leave another window stale.
        let wanted: HashSet<PathBuf> = self
            .workspaces()
            .flat_map(|w| w.tabs.values())
            .filter_map(|t| t.location().as_path().map(Path::to_path_buf))
            .collect();

        // Dropping a watcher cancels its blocking read and joins nothing — the
        // thread notices and exits on its own.
        self.watchers.retain(|path, _| wanted.contains(path));

        for path in wanted {
            if self.watchers.contains_key(&path) {
                continue;
            }
            let tx = self.changed_tx.clone();
            let wake = ctx.clone();
            let reported = path.clone();

            // A folder that cannot be watched is not an error: the pane just
            // keeps updating only when asked, exactly as it did before.
            if let Some(watcher) = neutron_shell::watch::watch(&path, move || {
                let _ = tx.send(reported.clone());
                wake.request_repaint();
            }) {
                tracing::debug!(path = %path.display(), "watching");
                self.watchers.insert(path, watcher);
            }
        }
    }

    /// Re-lists any tab showing a folder that changed on disk.
    pub(super) fn drain_changes(&mut self) {
        // Every window looks at every change: a folder can be open in several.
        let changed = self.mail.changes.clone();
        if changed.is_empty() {
            return;
        }

        // Every tab on that folder, not only the visible one: a background tab
        // that is re-listed now is correct the moment it is switched to,
        // instead of showing a stale listing until something else happens.
        let stale: Vec<TabId> = self
            .workspace
            .tabs
            .values()
            .filter(|t| {
                t.location()
                    .as_path()
                    .is_some_and(|p| changed.contains(p))
            })
            .map(|t| t.id)
            .collect();

        for tab in stale {
            if let Some(edit) = self.rename.as_mut().filter(|edit| edit.tab == tab) {
                edit.refresh_pending = true;
                continue;
            }
            self.refresh_in_place(tab);
        }
    }

    /// Keeps the preview in step with the selection, and uploads a decoded
    /// image once it arrives.
    ///
    /// Called before drawing, so the pane paints this frame's answer rather
    /// than last frame's.
    pub(super) fn pump_preview(&mut self, ctx: &egui::Context) {
        // Not while a band sweeps the rows: the cursor moves with it. The
        // gallery shows the same preview, large, so it wants one too.
        let gallery = self.workspace.active_tab().is_some_and(|t| t.view.view == file_list::ViewMode::Gallery);
        if !(self.preview.open || gallery) || file_list::sweeping(ctx) {
            return;
        }

        // What the pane should be showing: the cursor row of the focused tab.
        // The cursor rather than the selection, because with several files
        // selected there is no single thing to preview, and the cursor is the
        // one the user last touched.
        let wanted = self.workspace.active_tab().and_then(|tab| {
            let idx = tab.selection.cursor().filter(|&i| i < tab.list.len())?;
            let name = tab.list.name(idx);
            let source = match tab.location() {
                // Nothing is ever selected on the dashboard, so this is
                // unreachable in practice — listed rather than wildcarded so a
                // future location has to decide for itself.
                NodeId::Home | NodeId::Terminal { .. } | NodeId::Viewer { .. } => return None,
                NodeId::Path(dir) => crate::preview::Source::Local(dir.join(name)),
                // A server's rows are addressed by joining a name to a POSIX
                // path. The name went through `safe_name` on the way into the
                // listing, and joining is checked again here.
                NodeId::Remote { site, path } => {
                    let full = neutron_remote::path::join(path, name).ok()?;
                    let saved = self.sites.get(site)?;
                    crate::preview::Source::Remote {
                        site: Box::new(saved.clone()),
                        path: full,
                        name: name.to_owned(),
                    }
                }
                // Addressed by id, like everything else in Drive. A shortcut
                // resolves to its target, so previewing one shows what it
                // points at — which is what opening it would do.
                NodeId::Cloud { .. } => {
                    let (id, _) = tab.list.target(idx)?;
                    crate::preview::Source::Drive { id: id.to_owned() }
                }
                NodeId::Shell { .. } => crate::preview::Source::Shell,
                NodeId::Archive { archive, inner } => crate::preview::Source::Archive {
                    archive: archive.clone(),
                    inner: crate::archives::join(inner, name),
                },
                // Each row leads to its own file.
                NodeId::Tag { .. } | NodeId::Search { .. } => match tab.list.target(idx) {
                    Some((target, true)) => crate::preview::Source::Local(PathBuf::from(target)),
                    _ => return None,
                },
            };
            Some((source, tab.list.size(idx), tab.list.kind(idx).is_container()))
        });

        match &wanted {
            Some((source, size, is_dir)) => self.previews.request(
                source.clone(),
                *size,
                *is_dir,
                self.preview.showing.as_ref(),
            ),
            None => {
                if self.preview.showing.is_some() {
                    self.previews.clear();
                    self.preview.showing = None;
                    self.preview.content = crate::preview::Preview::Nothing;
                    self.preview.texture = None;
                }
            }
        }

        self.previews.tick();

        if let Some((source, content)) = self.previews.poll() {
            self.preview.texture = match &content {
                crate::preview::Preview::Image {
                    rgba,
                    width,
                    height,
                    ..
                } => {
                    // Uploaded here because only the UI thread may touch the
                    // graphics context. The worker hands over plain bytes,
                    // already downscaled, so this copies a few megabytes at
                    // most and does it once per file rather than per frame.
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [*width, *height],
                        rgba,
                    );
                    Some(ctx.load_texture("neutron-preview", image, egui::TextureOptions::LINEAR))
                }
                _ => None,
            };
            self.preview.showing = Some(source);
            self.preview.content = content;
        } else if self.previews.busy() && self.preview.showing != wanted.as_ref().map(|(s, _, _)| s.clone()) {
            // Something is on its way for a different file. Say so rather than
            // leaving the previous file's contents under the new name.
            self.preview.content = crate::preview::Preview::Loading;
            self.preview.texture = None;
        }
    }

    /// A large preview of entry `idx` of `tab`, at `path`, if it is the kind
    /// of file that has one — requested on first sight.
    pub(super) fn preview_of(&self, path: &Path, tab: &crate::workspace::Tab, idx: usize) -> Option<neutron_ui::thumb::Thumb> {
        let (name, kind) = (tab.list.name(idx), tab.list.kind(idx));
        // Not for a file still in the cloud: previewing it could fetch it.
        if !neutron_ui::thumb::wants_preview(name, kind) || tab.list.sync(idx) == neutron_core::entry::SyncState::CloudOnly {
            return None;
        }
        self.thumbs.get(path, tab.list.modified(idx), crate::thumbs::Grade::Large)
    }

    pub(super) fn preview_pane(&self, ui: &mut egui::Ui, p: &Palette) {
        if !self.preview.open {
            return;
        }

        let subject = self.workspace.active_tab().and_then(|tab| {
            let idx = tab.selection.cursor().filter(|&i| i < tab.list.len())?;
            Some(crate::preview::Subject {
                name: tab.list.name(idx),
                kind: neutron_ui::file_list::kind_label(tab.list.kind(idx), tab.list.name(idx)),
                size: Some(tab.list.size(idx)),
                modified: tab.list.modified(idx),
                is_dir: tab.list.kind(idx).is_container(),
                thumb: tab
                    .location()
                    .as_path()
                    .and_then(|dir| self.preview_of(&dir.join(tab.list.name(idx)), tab, idx)),
            })
        });

        egui::Panel::right(self.window_id("preview"))
            .show_separator_line(false)
            .default_size(300.0)
            .size_range(egui::Rangef::new(220.0, 600.0))
            .frame(
                theme::bare_card()
                    .outer_margin(egui::Margin::ZERO)
                    .inner_margin(egui::Margin::symmetric(12, 12)),
            )
            .show(ui, |ui| {
                paint_chrome_panel(ui, egui::Margin::symmetric(12, 12), ChromeEdge::Left, p);
                ui.set_min_width(ui.available_width());
                crate::preview::show(ui, p, &self.preview, subject);
            });
    }


    /// The action rail: a fixed strip of verbs between the sidebar and the
    /// panes. See [`crate::rail`] for why it exists in a keyboard-first
    /// application.
    pub(super) fn action_rail(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        let tab = self.workspace.active_tab();
        let state = crate::rail::RailState {
            local_folder: tab.is_some_and(|t| t.location().is_filesystem()),
            server_folder: tab.is_some_and(|t| t.location().as_remote().is_some()),
            archive_folder: tab.is_some_and(|t| t.location().as_archive().is_some()),
            selection_count: tab.map_or(0, |t| t.selection.iter().filter(|&i| t.list.rank(i).is_some()).count()),
            has_cursor: tab.is_some_and(|t| t.selection.cursor().is_some_and(|i| i < t.list.len())),
            selected_archive: tab.is_some_and(|t| t.selection.iter().any(|i| {
                i < t.list.len() && !t.list.kind(i).is_container()
                    && neutron_archive::format_of(Path::new(t.list.name(i))).is_some()
            })),
            archive_busy: self.archive_job.is_some(),
        };

        egui::Panel::left(self.window_id("action-rail"))
            .show_separator_line(false)
            .resizable(false)
            .exact_size(crate::rail::WIDTH)
            .frame(
                theme::bare_card()
                    .outer_margin(egui::Margin::ZERO)
                    .inner_margin(egui::Margin::symmetric(0, 10)),
            )
            .show(ui, |ui| {
                paint_chrome_panel(ui, egui::Margin::symmetric(0, 10), ChromeEdge::Right, p);
                if let Some(chosen) = crate::rail::show(ui, p, &state) {
                    actions.push(Action::RunCommand(chosen.command()));
                }
            });
    }

    /// The window's own title bar, above every other panel.
    ///
    /// Transparent: it sits on the ground, in the gutter the cards already
    /// leave around themselves, so the bar costs no vertical space that the
    /// layout was not spending anyway.
    pub(super) fn title_bar(&self, ui: &mut egui::Ui, p: &Palette) {
        let title = self
            .workspace
            .active_tab()
            .map(|t| format!("{} — Neutron", t.location().display_name()))
            .unwrap_or_else(|| "Neutron".to_owned());

        egui::Panel::top(self.window_id("title-bar"))
            .show_separator_line(false)
            .resizable(false)
            .exact_size(crate::chrome::HEIGHT)
            .frame(egui::Frame::new().outer_margin(egui::Margin::ZERO))
            .show(ui, |ui| {
                paint_chrome_panel(ui, egui::Margin::ZERO, ChromeEdge::Bottom, p);
                crate::chrome::show(ui, p, &title, &self.snap);
            });
    }

    pub(super) fn sidebar(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        let current = self.workspace.active_tab().map(|t| t.location().clone());

        egui::Panel::left(self.window_id("sidebar"))
            .show_separator_line(false)
            .default_size(224.0)
            .size_range(egui::Rangef::new(180.0, 400.0))
            .frame(
                theme::bare_card()
                    .outer_margin(egui::Margin::ZERO)
                    .inner_margin(egui::Margin { left: 10, right: 10, top: 12, bottom: 10 }),
            )
            .show(ui, |ui| {
                paint_chrome_panel(
                    ui,
                    egui::Margin { left: 10, right: 10, top: 12, bottom: 10 },
                    ChromeEdge::Right,
                    p,
                );
                ui.set_min_width(ui.available_width());

                sidebar::brand(ui, p);
                ui.add_space(10.0);
                if sidebar::find_pill(ui, p) {
                    actions.push(Action::ToggleFinder(Mode::Files));
                }
                ui.add_space(4.0);

                // The footer is carved out of the bottom of the card before the
                // list is drawn, so the storage panel stays pinned there rather
                // than scrolling away with the destinations above it.
                let body = ui.available_rect_before_wrap();
                let footer_top = body.bottom() - sidebar::footer_height();
                let scroll_area = egui::Rect::from_min_max(
                    body.min,
                    egui::pos2(body.right(), footer_top - 8.0),
                );

                let mut list = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(scroll_area)
                        .id_salt("sidebar-list"),
                );
                list.set_clip_rect(scroll_area);
                self.sidebar_places(&mut list, p, current.as_ref(), actions);

                let footer = egui::Rect::from_min_max(
                    egui::pos2(body.left(), footer_top),
                    body.max,
                );
                let footer_drives = self.sidebar_footer_drives();
                match sidebar::footer(ui, p, footer, &footer_drives, self.theme_mode) {
                    sidebar::FooterAction::Connect => actions.push(Action::OpenConnect),
                    sidebar::FooterAction::ToggleTheme => actions.push(Action::ToggleTheme),
                    sidebar::FooterAction::None => {}
                }
            });
    }

    /// The scrolling groups of destinations.
    pub(super) fn sidebar_places(
        &self,
        ui: &mut egui::Ui,
        p: &Palette,
        current: Option<&NodeId>,
        actions: &mut Vec<Action>,
    ) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("sidebar_scroll")
            .show(ui, |ui| {
                sidebar::section(ui, p, "Places");
                let dashboard = neutron_core::places::Place {
                    name: "Dashboard".to_owned(),
                    id: NodeId::Home,
                    kind: neutron_core::places::PlaceKind::KnownFolder,
                    capacity: None,
                };
                match sidebar::row(ui, p, &dashboard, current) {
                        sidebar::RowHit::Open => actions.push(Action::Navigate(NodeId::Home)),
                        sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(dashboard.id.clone(), at)),
                        sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                }
                for place in &self.known_folders {
                    match sidebar::row(ui, p, place, current) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                            sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                    }
                }
                // The Mac's own two: AirDrop, which is the Finder's window,
                // and the Trash, browsed here like any folder.
                if cfg!(target_os = "macos") {
                    let airdrop = Place {
                        name: "AirDrop".to_owned(),
                        id: NodeId::shell("neutron:airdrop", "AirDrop"),
                        kind: neutron_core::places::PlaceKind::KnownFolder,
                        capacity: None,
                    };
                    if matches!(sidebar::row(ui, p, &airdrop, current), sidebar::RowHit::Open) {
                        actions.push(Action::OpenAirDrop);
                    }
                    if let Some(trash) = trash_folder() {
                        let place = Place {
                            name: "Trash".to_owned(),
                            id: NodeId::path(&trash),
                            kind: neutron_core::places::PlaceKind::KnownFolder,
                            capacity: None,
                        };
                        match sidebar::row(ui, p, &place, current) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                            sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                        }
                    }
                }

                // Pins get a group of their own, and only when there are any:
                // they are the user's list, not the system's.
                let pinned: Vec<Place> = self
                    .pinned_places()
                    .into_iter()
                    .filter(|pin| !self.known_folders.iter().any(|k| k.id == pin.id))
                    .collect();
                if !pinned.is_empty() {
                    sidebar::section(ui, p, "Pinned");
                    for place in &pinned {
                        match sidebar::row(ui, p, place, current) {
                                sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                                sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                                sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                        }
                    }
                }

                if !self.shell_roots.is_empty() {
                    sidebar::section(ui, p, "System");
                    for place in &self.shell_roots {
                        match sidebar::row(ui, p, place, current) {
                                sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                                sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                                sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                        }
                    }
                }

                // Everywhere files live: the disks, the cloud, the servers.
                sidebar::section(ui, p, "Locations");
                for drive in &self.drives {
                    let path = drive.id.as_path();
                    let ejectable = path.is_some_and(|p| self.ejectable.contains(p));
                    match sidebar::drive_row(ui, p, drive, current, ejectable) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(drive.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(drive.id.clone(), at)),
                            sidebar::RowHit::Eject => {
                                if let Some(path) = path {
                                    actions.push(Action::Eject(path.to_path_buf()));
                                }
                            }
                            sidebar::RowHit::None => {}
                    }
                }
                // Below the drives found so far, not above them: the slow
                // devices are the ones still outstanding.
                if self.scanning_drives {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.add(egui::Spinner::new().size(10.0));
                        ui.colored_label(p.text_faint, "Scanning…");
                    });
                }
                for place in &self.cloud {
                    match sidebar::row(ui, p, place, current) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                            sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                    }
                }
                use neutron_cloud::google::DriveState;
                match &self.drive_state {
                    DriveState::SignedIn => {
                        let place = neutron_core::places::Place {
                            name: "Google Drive".to_owned(),
                            id: NodeId::cloud(
                                CloudProviderId::GoogleDrive,
                                neutron_cloud::drive::ROOT_ID,
                                "Google Drive",
                            ),
                            kind: neutron_core::places::PlaceKind::Cloud,
                            capacity: None,
                        };
                        match sidebar::row(ui, p, &place, current) {
                                sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                                sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                                sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                        }
                    }
                    DriveState::SignedOut => {
                        if sidebar::action_row(
                            ui,
                            p,
                            "Connect Google Drive",
                            neutron_ui::icons::Glyph::GoogleDrive,
                        ) {
                            actions.push(Action::ConnectDrive);
                        }
                    }
                    // A row that does something about it: an app opened from
                    // the Dock never sees an environment variable, so the
                    // file is the way, and this makes it and shows it.
                    DriveState::NotConfigured => {
                        if sidebar::action_row_with_hint(
                            ui,
                            p,
                            "Set up Google Drive",
                            neutron_ui::icons::Glyph::GoogleDrive,
                            "Drive needs a Google OAuth client of your own. This creates google.json \
                             in Neutron's settings folder for its client_id and client_secret.",
                        ) {
                            actions.push(Action::SetUpDrive);
                        }
                    }
                    // Retryable, not a dead row. A failed sign-in is usually
                    // something the user can act on — a declined consent, a
                    // dropped connection — and making them restart the
                    // application to try again is a poor answer to it.
                    DriveState::Error(why) => {
                        if sidebar::action_row_with_hint(
                            ui,
                            p,
                            "Retry Google Drive",
                            neutron_ui::icons::Glyph::GoogleDrive,
                            why,
                        ) {
                            actions.push(Action::ConnectDrive);
                        }
                    }
                }

                for site in &self.sites.sites {
                    let place = neutron_core::places::Place {
                        name: site.display().to_owned(),
                        id: NodeId::remote(site.id.as_str(), site.start_path().as_str()),
                        kind: neutron_core::places::PlaceKind::Server,
                        capacity: None,
                    };
                    match sidebar::row(ui, p, &place, current) {
                            sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                            sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                            sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                    }
                    if let Some(drives) = self.remote_drives.get(&site.id) {
                        for drive in drives {
                            match sidebar::remote_drive_row(ui, p, drive, current) {
                                sidebar::RowHit::Open => actions.push(Action::Navigate(drive.id.clone())),
                                sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(drive.id.clone(), at)),
                                sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                            }
                        }
                    }
                }
                if let Some(reason) = &self.sites_error {
                    sidebar::placeholder_row(
                        ui,
                        p,
                        "sites.json could not be read",
                        neutron_ui::icons::Glyph::Server,
                        reason,
                    );
                }
                if sidebar::action_row(ui, p, "Add a server…", neutron_ui::icons::Glyph::Server) {
                    actions.push(Action::OpenConnect);
                }

                // Only shown when WSL is actually installed. An empty "Linux"
                // heading on a machine without it is a permanent reminder of a
                // feature the user does not have.
                if !self.wsl.is_empty() {
                    sidebar::section(ui, p, "Linux");
                    for place in &self.wsl {
                        match sidebar::row(ui, p, place, current) {
                                sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                                sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                                sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                        }
                    }
                }

                // The Finder's colour tags, each showing every file that
                // carries it.
                if cfg!(target_os = "macos") {
                    if !self.saved_searches.is_empty() {
                        sidebar::section(ui, p, "Searches");
                        for saved in &self.saved_searches {
                            let place = neutron_core::places::Place {
                                name: saved.name.clone(),
                                id: saved.location(),
                                kind: neutron_core::places::PlaceKind::KnownFolder,
                                capacity: None,
                            };
                            match sidebar::row(ui, p, &place, current) {
                                sidebar::RowHit::Open => actions.push(Action::Navigate(place.id.clone())),
                                sidebar::RowHit::Menu(at) => actions.push(Action::SidebarMenu(place.id.clone(), at)),
                                sidebar::RowHit::None | sidebar::RowHit::Eject => {}
                            }
                        }
                    }
                    sidebar::section(ui, p, "Tags");
                    for &(_, color) in neutron_shell::tags::COLORS.iter() {
                        let name = neutron_shell::tags::color_name(color);
                        let selected = matches!(current, Some(NodeId::Tag { name: n }) if **n == *name);
                        if matches!(sidebar::tag_row(ui, p, &name, color, selected), sidebar::RowHit::Open) {
                            actions.push(Action::Navigate(NodeId::Tag { name: name.as_str().into() }));
                        }
                    }
                }

                ui.add_space(14.0);
            });
    }

    /// The pane tree fills whatever the chrome panels left over.
    pub(super) fn panes(&self, ui: &mut egui::Ui, p: &Palette, actions: &mut Vec<Action>) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
            .show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                let mut pane_actions = Vec::new();
                let mut group_actions = Vec::new();

                panes::set_multi_pane(ui.ctx(), self.workspace.layout.len() > 1);
                panes::show(
                    ui,
                    &self.workspace.layout,
                    rect,
                    p,
                    self.workspace.focused,
                    &mut pane_actions,
                    &mut |ui, group_id, is_focused| {
                        self.draw_group(ui, p, group_id, is_focused, &mut group_actions);
                    },
                );

                actions.append(&mut group_actions);
                for a in pane_actions {
                    actions.push(match a {
                        PaneAction::Focus(g) => Action::FocusGroup(g),
                        PaneAction::SetRatio { target, ratio } => Action::SetRatio { target, ratio },
                    });
                }
            });
    }

    /// One pane: tab strip, then header, then listing.
    /// A terminal tab's name: what the program calls itself, or what it is
    /// and where.
    pub(super) fn terminal_title(&self, id: u64, dir: &Path) -> String {
        let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
        match self.terminals.borrow().get(&id) {
            Some(session) => session.title().unwrap_or_else(|| format!("{} — {folder}", session.label)),
            None => format!("Terminal — {folder}"),
        }
    }

    /// A terminal tab: its session drawn to fill the pane, started now if the
    /// tab came back from a saved session and has none yet.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_terminal(
        &self,
        ui: &mut egui::Ui,
        p: &Palette,
        group_id: GroupId,
        index: usize,
        id: u64,
        dir: &Path,
        is_focused: bool,
        actions: &mut Vec<Action>,
    ) {
        let mut terminals = self.terminals.borrow_mut();
        if let std::collections::hash_map::Entry::Vacant(slot) = terminals.entry(id) {
            match crate::term::Session::spawn(dir, &crate::term::Program::shell(), "Terminal", self.ctx.clone()) {
                Ok(session) => {
                    slot.insert(session);
                }
                Err(e) => {
                    ui.label(egui::RichText::new(e).color(p.danger));
                    return;
                }
            }
        }
        let Some(session) = terminals.get_mut(&id) else { return };
        let focus = self.terminal_focus.borrow_mut().remove(&id);
        let exited = session.exited();
        let Some(out) = session.with_screen(|screen| {
            neutron_ui::term_view::show(ui, p, egui::Id::new(("neutron-terminal", id)), screen, exited, focus)
        }) else {
            return;
        };
        session.resize(out.grid.0, out.grid.1);
        if out.scroll != 0 {
            session.scroll(out.scroll);
        }
        if let Some(text) = out.copied {
            ui.ctx().copy_text(text);
        }
        if exited {
            // Return, or any key, closes a terminal whose program has ended.
            if !out.input.is_empty() {
                actions.push(Action::CloseTab { group: group_id, index });
            }
        } else {
            session.write(&out.input);
        }
        if !is_focused && ui.ctx().memory(|m| m.has_focus(egui::Id::new(("neutron-terminal", id)))) {
            actions.push(Action::FocusGroup(group_id));
        }
    }

    pub(super) fn draw_group(
        &self,
        ui: &mut egui::Ui,
        p: &Palette,
        group_id: GroupId,
        is_focused: bool,
        actions: &mut Vec<Action>,
    ) {
        let Some(group) = self.workspace.groups.get(&group_id) else {
            return;
        };

        let titles: Vec<String> = group
            .tabs
            .iter()
            .filter_map(|t| self.workspace.tabs.get(t))
            .map(|t| match t.location() {
                NodeId::Terminal { id, dir } => self.terminal_title(*id, dir),
                _ => t.title(),
            })
            .collect();

        let folders: Vec<Option<PathBuf>> = group
            .tabs
            .iter()
            .map(|t| self.workspace.tabs.get(t).and_then(|t| t.location().as_path().map(Path::to_path_buf)))
            .collect();
        if let Some(a) = panes::tab_strip(
            ui,
            group_id,
            &titles,
            &folders,
            group.active,
            is_focused,
            self.tab_drag,
            p,
        ) {
            actions.push(match a {
                TabAction::Select(index) => Action::SelectTab {
                    group: group_id,
                    index,
                },
                TabAction::Close(index) => Action::CloseTab {
                    group: group_id,
                    index,
                },
                TabAction::New => Action::NewTab(group_id),
                TabAction::BeginDrag(index) => Action::BeginTabDrag(TabDrag {
                    group: group_id,
                    index,
                }),
                TabAction::DropHere => Action::DropTab(group_id),
                TabAction::Reorder { from, to } => Action::ReorderTab { group: group_id, from, to },
            });
        }

        let Some(tab) = group.active_tab().and_then(|t| self.workspace.tabs.get(&t)) else {
            return;
        };

        if let NodeId::Terminal { id, dir } = tab.location() {
            self.draw_terminal(ui, p, group_id, group.active, *id, dir, is_focused, actions);
            return;
        }
        if let NodeId::Viewer { path } = tab.location() {
            let mut viewers = self.viewers.borrow_mut();
            viewers.view(path);
            let (view, pages) = viewers.parts(path);
            match crate::viewer_ui::show(ui, p, path, view, pages) {
                Some(crate::viewer_ui::Ask::Launch) => actions.push(Action::LaunchPath(path.clone())),
                Some(crate::viewer_ui::Ask::Copy(text)) => ui.ctx().copy_text(text),
                None => {}
            }
            if !is_focused && ui.ui_contains_pointer() && ui.input(|i| i.pointer.any_pressed()) {
                actions.push(Action::FocusGroup(group_id));
            }
            return;
        }

        // Header first, so navigation stays available even on a location that
        // failed to open — otherwise a bad path is a dead end with no way back.
        let (shown, total) = tab.counts();
        let head = header::Header {
            location: tab.location(),
            can_back: tab.history.can_go_back(),
            can_forward: tab.history.can_go_forward(),
            has_parent: tab.location().parent().is_some(),
            show_hidden: tab.view.show_hidden,
            view: tab.view.view,
            filter: &tab.view.filter,
            // Only in the pane it was opened on. A split showing two panes would
            // otherwise draw the same field over both breadcrumbs, bound to one
            // text id, and neither would behave.
            address: self
                .address
                .as_ref()
                .filter(|(g, _)| *g == group_id)
                .map(|(_, text)| text.as_str()),
            shown,
            total,
            vcs: self.vcs.peek(tab.location()),
        };
        if let Some(a) = header::show(ui, p, group_id, &head) {
            // Every header control acts on its own pane, so using one focuses
            // it — otherwise Back would apply to a different pane than the one
            // whose arrow was clicked.
            if group_id != self.workspace.focused {
                actions.push(Action::FocusGroup(group_id));
            }
            actions.push(match a {
                header::HeaderAction::Back => Action::Back,
                header::HeaderAction::Forward => Action::Forward,
                header::HeaderAction::Up => Action::Up,
                header::HeaderAction::Navigate(id) => Action::Navigate(id),
                header::HeaderAction::SetFilter(text) => Action::SetFilter(text),
                header::HeaderAction::ToggleHidden => Action::ToggleHidden,
                header::HeaderAction::SetView(mode) => Action::SetView(mode),
                header::HeaderAction::Split(axis) => Action::Split(axis),
                header::HeaderAction::SetAddress(text) => Action::SetAddress(text),
                header::HeaderAction::SubmitAddress => Action::SubmitAddress,
                header::HeaderAction::CancelAddress => Action::CancelAddress,
            });
        }

        // Files dragged over the window are seen to once it is all drawn, when
        // every folder a drop could go into has said where it is; see
        // `resolve_drop`. All this pane says is where it is.
        let pane_rect = ui.min_rect().union(ui.max_rect());
        self.drop_panes.borrow_mut().push((group_id, pane_rect));

        // A folder's footer is carved off the bottom of its pane before the
        // listing is laid out, so the rows end above it rather than under it.
        let mut content_ui;
        let ui = if matches!(tab.location(), NodeId::Home) {
            ui
        } else {
            let area = ui.available_rect_before_wrap();
            let footer = egui::Rect::from_min_max(egui::pos2(area.left(), area.bottom() - 34.0), area.max);
            self.pane_footer(ui, p, footer, tab, actions);
            content_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_max(area.min, egui::pos2(area.right(), footer.top())))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            &mut content_ui
        };

        if let Status::Error(e) = &tab.status {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.colored_label(p.danger, "Could not open this location");
                ui.colored_label(p.text_muted, e.as_str());
                // macOS keeps the Trash from apps until they are given Full
                // Disk Access, and the error alone does not say so.
                if tab.location().as_path() == trash_folder().as_deref() {
                    ui.add_space(10.0);
                    ui.colored_label(
                        p.text_muted,
                        "macOS shows the Trash only to apps with Full Disk Access. Turn it on for Neutron in \
                         System Settings, then come back here.",
                    );
                    ui.add_space(6.0);
                    if ui.button("Open Full Disk Access settings").clicked() {
                        actions.push(Action::OpenFullDiskAccess);
                    }
                }
                // On a server most of what goes wrong is fixed in its settings
                // — a password that was never saved, a certificate to trust,
                // a changed port — so the way there is right under the error.
                if let Some((site, _)) = tab.location().as_remote() {
                    ui.add_space(10.0);
                    if ui.button("Edit server…").clicked() {
                        actions.push(Action::EditSite(site.to_owned()));
                    }
                }
            });
            return;
        }

        // The dashboard is not a listing, so it does not go through the list
        // widget at all. It draws from what the sidebar scan already found.
        if matches!(tab.location(), NodeId::Home) {
            // Cloud storage and saved servers, beside the disks.
            let mut elsewhere: Vec<Place> = self.cloud.clone();
            elsewhere.extend(self.sites.sites.iter().map(|site| Place {
                name: site.display().to_owned(),
                id: NodeId::remote(site.id.as_str(), site.start_path().as_str()),
                kind: neutron_core::places::PlaceKind::Server,
                capacity: None,
            }));
            let remote_groups = self.remote_drive_groups();
            if let Some(hit) = crate::dashboard::show(
                ui,
                p,
                &self.drives,
                &remote_groups,
                &self.quick_access(),
                &self.recent.files,
                &elsewhere,
            ) {
                actions.push(match hit {
                    crate::dashboard::Chosen::Go(id) => Action::Navigate(id),
                    crate::dashboard::Chosen::Inspect(id) => Action::OpenStorageLens(id),
                    crate::dashboard::Chosen::Open(path) => Action::OpenRecent(path),
                    crate::dashboard::Chosen::Forget(path) => Action::ForgetRecent(path),
                });
            }
            return;
        }

        // A local folder or a saved server in the column view. Anything else —
        // Drive, This PC — has no path chain to build columns from and stays a list.
        if tab.view.view == file_list::ViewMode::Columns
            && (tab.location().as_path().is_some() || tab.location().as_remote().is_some())
        {
            self.draw_columns(ui, p, group_id, tab, tab.location(), actions);
            return;
        }

        // The list widget needs `&mut FileListState` to consume `scroll_to` and
        // record the scroll offset, but this method holds only `&self`. It is
        // drawn from a clone; the scroll offset is re-derived next frame and
        // `scroll_to` is re-issued by whichever action moved the cursor, so
        // nothing durable is lost.
        let mut view = tab.view.clone();
        // Always supplied, even before any icon exists. Gating this on the
        // texture being present deadlocks the whole service: rows only ask for
        // icons through the source, so no source means no requests, which means
        // no icons, which means no texture — forever. `uv_for` returns `None`
        // until a slot is genuinely ready, and a ready slot implies the atlas
        // is non-empty and was uploaded by `pump` earlier this frame.
        let source = RowIcons {
            service: &self.icons,
            thumbs: &self.thumbs,
            texture: self.icon_texture,
            dir: tab.location().as_path().map(|p| p.to_path_buf()),
            remote: tab
                .location()
                .as_remote()
                .and_then(|(s, p)| self.sites.get(s).cloned().map(|site| (site, p.to_owned()))),
        };

        // Only when the cut happened in the folder this pane is showing:
        // otherwise a file called `notes.txt` cut from somewhere else would
        // fade an unrelated `notes.txt` here.
        let cut = (self.cut_dir.as_deref() == tab.location().as_path())
            .then_some(&self.cut_names)
            .filter(|names| !names.is_empty());

        let pane_diff = self.pane_diff_for(group_id, tab);
        if let Some((_tags, other_group, only_count, newer_count, diff_count)) = &pane_diff {
            self.draw_compare_strip(ui, p, group_id, *other_group, *only_count, *newer_count, *diff_count, actions);
        }

        let vcs_entries = self.vcs.peek(tab.location()).map(|v| &v.entries);
        let marks = file_list::Marks {
            selection: &tab.selection,
            vcs: vcs_entries,
            diff: pane_diff.as_ref().map(|(tags, ..)| tags),
            cut,
            // Only in the pane that owns it. A split showing the same
            // folder twice would otherwise draw two boxes, both bound to
            // one text field id, and neither would behave.
            rename: self
                .rename
                .as_ref()
                .filter(|r| Some(r.tab) == group.active_tab())
                .map(|r| (r.idx, r.text.as_str(), r.generation)),
        };
        let icons = Some(&source as &dyn file_list::IconSource);
        // Scoped by tab and folder, so each keeps its own scroll position: a
        // folder just entered opens at the top rather than wherever the last
        // one was left, a second tab in the pane does not share the first
        // one's, and Back returns to where the folder was.
        let drawn = ui
            .push_id((tab.id, tab.location()), |ui| {
                file_list::show(ui, &mut view, &tab.list, &marks, p, icons)
            })
            .inner;

        // The grid works out its own column count from the pane width, and
        // that number is what Up and Down have to step by. It is computed on a
        // *clone* of the view state — this method holds only `&self` — so
        // without reporting it back the count stayed zero and the arrow keys
        // moved one tile at a time instead of one row.
        if view.columns != tab.view.columns {
            actions.push(Action::SetGridColumns {
                group: group_id,
                columns: view.columns,
            });
        }

        // The gallery's large preview, above its strip: the preview pane's
        // contents, for the item under the cursor.
        if let Some(top) = view.gallery_top.filter(|_| view.view == file_list::ViewMode::Gallery) {
            let subject = tab.selection.cursor().filter(|&i| i < tab.list.len()).map(|idx| crate::preview::Subject {
                name: tab.list.name(idx),
                kind: neutron_ui::file_list::kind_label(tab.list.kind(idx), tab.list.name(idx)),
                size: tab.list.size_known(idx).then(|| tab.list.size(idx)),
                modified: tab.list.modified(idx),
                is_dir: tab.list.kind(idx).is_container(),
                thumb: tab.location().as_path().and_then(|dir| self.preview_of(&dir.join(tab.list.name(idx)), tab, idx)),
            });
            let mut big = ui.new_child(egui::UiBuilder::new().max_rect(top.shrink2(egui::vec2(24.0, 12.0))).layout(egui::Layout::top_down(egui::Align::Min)));
            if subject.is_some() && is_focused {
                crate::preview::show(&mut big, p, &self.preview, subject);
            } else {
                big.label(egui::RichText::new("Select an item to see it here").color(p.text_faint));
            }
        }

        if let Some(a) = drawn {
            // Interacting with a pane also focuses it, so keystrokes go where
            // the user just clicked.
            if group_id != self.workspace.focused {
                actions.push(Action::FocusGroup(group_id));
            }
            actions.push(list_action(a));
        }
    }

    /// The column view of a folder: the folders on the way to it, then the
    /// folder itself, then what is inside the selected folder.
    pub(super) fn draw_columns(
        &self,
        ui: &mut egui::Ui,
        p: &Palette,
        group_id: GroupId,
        tab: &crate::workspace::Tab,
        location: &NodeId,
        actions: &mut Vec<Action>,
    ) {
        use neutron_ui::columns::{Column, ColumnAction, Contents, Role};

        let chain: Vec<NodeId> = match location {
            NodeId::Path(dir) => crate::columns::chain(dir, self.home.as_deref())
                .into_iter()
                .map(NodeId::Path)
                .collect(),
            NodeId::Remote { site, path } => {
                let root = self.sites.get(site).and_then(|s| s.path.as_deref());
                crate::columns::chain_remote(site, path, root)
            }
            _ => return,
        };
        // Not while a band sweeps: a folder it crosses first would open its
        // column for a moment.
        let peek = crate::columns::peek(tab).filter(|_| !file_list::sweeping(ui.ctx()));
        let arriving = crate::columns::arriving(tab);
        let current = chain.len() - 1;

        // The entry in each folder that leads to the next, owned here so the
        // columns can borrow it.
        let through: Vec<String> = chain
            .iter()
            .skip(1)
            .map(|d| match d {
                NodeId::Path(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                NodeId::Remote { path, .. } => neutron_remote::path::file_name(path),
                _ => String::new(),
            })
            .collect();
        let mut dirs: Vec<&NodeId> = chain.iter().collect();
        if let Some(peek) = &peek {
            dirs.push(peek);
        }
        // One per column: an executable's icon is keyed by its full path, so
        // each column's rows need their own folder to root it.
        let icons: Vec<RowIcons> = dirs
            .iter()
            .map(|d| RowIcons {
                service: &self.icons,
                thumbs: &self.thumbs,
                texture: self.icon_texture,
                dir: d.as_path().map(Path::to_path_buf),
                remote: d
                    .as_remote()
                    .and_then(|(s, p)| self.sites.get(s).cloned().map(|site| (site, p.to_owned()))),
            })
            .collect();

        let columns: Vec<Column> = dirs
            .iter()
            .enumerate()
            .map(|(i, &d)| {
                let (contents, role) = if i < current {
                    (self.column_contents(d), Role::Named { highlight: Some(through[i].as_str()) })
                } else if i == current && !arriving {
                    (
                        Contents::Ready(&tab.list),
                        Role::Current {
                            selection: &tab.selection,
                            rename: self
                                .rename
                                .as_ref()
                                .filter(|r| r.tab == tab.id)
                                .map(|r| (r.idx, r.text.as_str(), r.generation)),
                            scroll_to: tab.view.scroll_to,
                        },
                    )
                } else if i == current {
                    // Still loading: drawn from the listing the column view
                    // already had, with what will be selected marked.
                    let pending = tab.reselect.as_ref().and_then(|r| r.cursor.as_deref());
                    (self.column_contents(d), Role::Named { highlight: pending })
                } else {
                    (self.column_contents(d), Role::Named { highlight: None })
                };
                Column {
                    dir: d,
                    contents,
                    role,
                    cut: d
                        .as_path()
                        .filter(|p| self.cut_dir.as_deref() == Some(*p))
                        .map(|_| &self.cut_names)
                        .filter(|names| !names.is_empty()),
                    icons: Some(&icons[i] as &dyn file_list::IconSource),
                }
            })
            .collect();

        // The selected file, described in a column at the end — when exactly
        // one thing is selected and it is not a folder, whose contents take
        // that place instead.
        let described = (!arriving && peek.is_none() && !file_list::sweeping(ui.ctx()))
            .then(|| visible_selection(tab))
            .filter(|selected| selected.len() == 1)
            .map(|selected| selected[0])
            .filter(|&idx| {
                !tab.list.kind(idx).is_container()
                    || location
                        .as_path()
                        .is_some_and(|dir| crate::columns::is_package(&dir.join(tab.list.name(idx))))
            });
        let info = described.map(|idx| match location {
            NodeId::Path(dir) => neutron_ui::columns::Info {
                name: tab.list.name(idx),
                kind: tab.list.kind(idx),
                kind_label: file_list::kind_label(tab.list.kind(idx), tab.list.name(idx)),
                size: Some(tab.list.size(idx)),
                modified: tab.list.modified(idx),
                location: tidy_location(dir),
                quick_look: cfg!(target_os = "macos"),
                preview: self.preview_of(&dir.join(tab.list.name(idx)), tab, idx),
            },
            NodeId::Remote { site, path } => {
                let site_display = self
                    .sites
                    .get(site)
                    .map_or(site.as_ref(), |s| s.display());
                neutron_ui::columns::Info {
                    name: tab.list.name(idx),
                    kind: tab.list.kind(idx),
                    kind_label: file_list::kind_label(tab.list.kind(idx), tab.list.name(idx)),
                    size: Some(tab.list.size(idx)),
                    modified: tab.list.modified(idx),
                    location: format!("{site_display}: {path}"),
                    quick_look: false,
                    preview: None,
                }
            }
            _ => unreachable!(),
        });

        if let Some(a) = neutron_ui::columns::show(ui, p, &columns, info.as_ref()) {
            if group_id != self.workspace.focused {
                actions.push(Action::FocusGroup(group_id));
            }
            match a {
                ColumnAction::Current(a) => actions.push(list_action(a)),
                ColumnAction::Pick { dir, entry, double } => {
                    actions.push(Action::ColumnPick { dir, entry, double })
                }
                ColumnAction::DragOut(path) => actions.push(Action::DragPaths(vec![path])),
                ColumnAction::Open => {
                    if let Some(idx) = described {
                        actions.push(Action::Activate(idx));
                    }
                }
                ColumnAction::QuickLook => {
                    if let (Some(idx), Some(dir)) = (described, location.as_path()) {
                        actions.push(Action::QuickLook(dir.join(tab.list.name(idx))));
                    }
                }
            }
        }
    }

    /// A column's listing as the cache has it.
    pub(super) fn column_contents(&self, dir: &NodeId) -> neutron_ui::columns::Contents<'_> {
        use crate::columns::Slot;
        use neutron_ui::columns::Contents;
        match self.columns.get(dir) {
            Some(Slot::Ready { list, .. }) => Contents::Ready(list.as_ref()),
            Some(Slot::Failed(e)) => Contents::Failed(e),
            Some(Slot::Loading) | None => Contents::Loading,
        }
    }

    /// Whether the focused tab is showing a folder or server in the column view.
    pub(super) fn in_columns(&self) -> bool {
        self.workspace.active_tab().is_some_and(|t| {
            t.view.view == file_list::ViewMode::Columns
                && (t.location().as_path().is_some() || t.location().as_remote().is_some())
        })
    }

    /// Takes the column listings that have arrived, and tells the cache which
    /// folders the column views on screen need next.
    pub(super) fn pump_columns(&mut self) {
        self.columns.pump();
        // Folder sizes for the other columns, as for the folder a tab shows.
        let arrived = self.columns.take_arrived();
        if self.all_sizes {
            let folders: Vec<PathBuf> = arrived
                .iter()
                .filter_map(|id| {
                    let dir = id.as_path()?;
                    match self.columns.get(id) {
                        Some(crate::columns::Slot::Ready { list, .. }) => Some(
                            (0..list.len())
                                .filter(|&i| list.kind(i).is_container())
                                .map(|i| dir.join(list.name(i)))
                                .collect::<Vec<_>>(),
                        ),
                        _ => None,
                    }
                })
                .flatten()
                .collect();
            self.sizer.request(folders);
        }
        let sizer = &self.sizer;
        self.columns.apply_sizes(sizer.version(), !arrived.is_empty(), |p| sizer.get(p));

        let mut wanted = Vec::new();
        // Every pane of every window is on screen, so every pane's active tab
        // counts — the cache keeps only what is asked for.
        let panes = self.workspaces().flat_map(|w| w.groups.values().map(move |g| (w, g)));
        for (workspace, group) in panes {
            let Some(tab) = group.active_tab().and_then(|t| workspace.tabs.get(&t)) else {
                continue;
            };
            if tab.view.view != file_list::ViewMode::Columns {
                continue;
            }
            let (sort, show_hidden) = (tab.view.sort, tab.view.show_hidden);
            let want = |dir: NodeId| crate::columns::Want { dir, sort, show_hidden };

            let chain: Vec<NodeId> = match tab.location() {
                NodeId::Path(dir) => crate::columns::chain(dir, self.home.as_deref())
                    .into_iter()
                    .map(NodeId::Path)
                    .collect(),
                NodeId::Remote { site, path } => {
                    let root = self.sites.get(site).and_then(|s| s.path.as_deref());
                    crate::columns::chain_remote(site, path, root)
                }
                _ => continue,
            };
            // The folders before the current one; and the current one as well
            // while the tab is still arriving, because its column is drawn
            // from here until the tab's own listing lands.
            let upto = if crate::columns::arriving(tab) { chain.len() } else { chain.len() - 1 };
            wanted.extend(chain.into_iter().take(upto).map(want));
            if let Some(peek) = crate::columns::peek(tab).filter(|_| !file_list::sweeping(&self.ctx)) {
                wanted.push(want(peek));
            }
        }
        self.columns.want(&wanted);
    }

    /// Right and Left in the column view: into the folder under the cursor, or
    /// back out to the folder that contains this one.
    ///
    /// Out selects the folder just left, so Left then Right is a round trip.
    /// In selects the first entry when its listing is already in the column to
    /// the right — which it is once the cursor has rested on the folder.
    pub(super) fn column_step(&mut self, delta: isize) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };
        let Some(t) = self.workspace.tabs.get(&tab_id) else {
            return;
        };

        let (target, select) = match t.location() {
            NodeId::Path(dir) => {
                if delta > 0 {
                    if crate::columns::arriving(t) {
                        return;
                    }
                    let Some(idx) = t.selection.cursor().filter(|&i| t.list.rank(i).is_some()) else {
                        return;
                    };
                    let full = dir.join(t.list.name(idx));
                    if !t.list.kind(idx).is_container() || crate::columns::is_package(&full) {
                        return;
                    }
                    let target = NodeId::Path(full);
                    let first = crate::columns::first_name(self.columns.get(&target));
                    (target, first)
                } else {
                    let Some(parent) = dir.parent() else {
                        // A drive root has no parent folder, but it does have a place
                        // above it — This PC — and Up has always gone there.
                        if let Some(above) = t.location().parent() {
                            self.navigate(tab_id, above);
                        }
                        return;
                    };
                    let left = dir.file_name().map(|n| n.to_string_lossy().into_owned());
                    (NodeId::Path(parent.to_path_buf()), left)
                }
            }
            NodeId::Remote { site, path } => {
                if delta > 0 {
                    if crate::columns::arriving(t) {
                        return;
                    }
                    let Some(idx) = t.selection.cursor().filter(|&i| t.list.rank(i).is_some()) else {
                        return;
                    };
                    if !t.list.kind(idx).is_container() {
                        return;
                    }
                    let Ok(joined) = neutron_remote::path::join(path, t.list.name(idx)) else {
                        return;
                    };
                    let target = NodeId::remote(site.as_ref(), joined);
                    let first = crate::columns::first_name(self.columns.get(&target));
                    (target, first)
                } else {
                    let Some(parent) = neutron_remote::path::parent(path) else {
                        return;
                    };
                    let left = Some(neutron_remote::path::file_name(path));
                    (NodeId::remote(site.as_ref(), parent), left)
                }
            }
            _ => return,
        };

        self.navigate(tab_id, target);
        if let Some(t) = self.workspace.tabs.get_mut(&tab_id) {
            t.reselect = select.map(|name| crate::workspace::Reselect {
                names: vec![name.clone()],
                cursor: Some(name),
            });
        }
    }

    /// A click in a column other than the current one: that folder becomes
    /// the current one, with the entry clicked selected.
    pub(super) fn column_pick(
        &mut self,
        dir: NodeId,
        entry: Option<(String, neutron_core::EntryKind)>,
        double: bool,
    ) {
        let Some(tab_id) = self.workspace.active_tab_id() else {
            return;
        };

        // A double-click opens, as Enter does: a folder becomes the current
        // one and anything else goes to its application. The first click of
        // the pair has already selected it.
        if double {
            if let Some((name, kind)) = entry {
                match dir {
                    NodeId::Path(dir) => {
                        let full = dir.join(&name);
                        if kind.is_container() && !crate::columns::is_package(&full) {
                            self.navigate(tab_id, NodeId::Path(full));
                        } else {
                            self.open_local(full);
                        }
                    }
                    NodeId::Remote { site, path } => {
                        let Ok(full) = neutron_remote::path::join(&path, &name) else {
                            self.notices.error(format!("{name} is not a name this can open"));
                            return;
                        };
                        if kind.is_container() {
                            self.navigate(tab_id, NodeId::remote(site.as_ref(), full));
                        } else {
                            self.start_download(&site, full, name);
                        }
                    }
                    _ => {}
                }
            }
            return;
        }

        let Some(t) = self.workspace.tabs.get_mut(&tab_id) else {
            return;
        };
        // Already there and loaded: select by name now rather than reloading.
        if t.location() == &dir && !crate::columns::arriving(t) {
            let found = entry
                .as_ref()
                .and_then(|(name, _)| (0..t.list.len()).find(|&i| t.list.name(i) == name));
            match found {
                Some(idx) => {
                    t.selection.apply(&t.list, idx, SelectMode::Replace);
                    t.view.scroll_to = Some(idx);
                }
                None => t.selection.clear(),
            }
            return;
        }

        // A no-op when the tab is already on its way there; the selection is
        // then applied when that load lands.
        self.navigate(tab_id, dir);
        if let Some(t) = self.workspace.tabs.get_mut(&tab_id) {
            t.reselect = entry.map(|(name, _)| crate::workspace::Reselect {
                names: vec![name.clone()],
                cursor: Some(name),
            });
        }
    }
}
