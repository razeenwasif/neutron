use super::*;

// --- input -----------------------------------------------------------------

impl NeutronApp {
    pub(super) fn keyboard(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        // Skip only while a *text field* has focus, or navigation keys would
        // fight with editing. Matters as soon as the rename box and search land.
        //
        // Not `memory().focused().is_some()`: that is true whenever any widget
        // holds focus, including a button the user merely clicked, which
        // silently disabled every shortcut in the application.
        if ctx.egui_wants_keyboard_input() {
            return;
        }

        // A context menu is modal to the pane behind it. Without this, Delete
        // while the menu is open would bin the selection *and* run whichever
        // command the cursor was on.
        if self.menu.is_some() {
            return;
        }

        // Nor while a question is open. A key that moved the selection would
        // change what the dialog's button acts on, after the dialog counted it.
        if self.confirm.is_some() || !self.clash_questions.is_empty() {
            return;
        }

        let page = file_list::rows_per_page(ctx.content_rect().height()) as isize;
        let visible = self
            .workspace
            .active_tab()
            .map(|t| t.list.order().len())
            .unwrap_or(0);

        ctx.input(|i| {
            let shift = i.modifiers.shift;
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            let alt = i.modifiers.alt;

            for event in &i.events {
                // Text events rather than key events, so keyboard layout and
                // modifiers are already resolved by the OS.
                if let egui::Event::Text(text) = event {
                    if !ctrl && !alt {
                        if let Some(c) = text.chars().next() {
                            // Space is Quick Look on the Mac — except partway
                            // through typing a name that has a space in it.
                            let typing = !self.type_ahead.is_empty()
                                && self.type_ahead_at.is_some_and(|t| t.elapsed() <= TYPE_AHEAD_TIMEOUT);
                            if c == ' ' && cfg!(target_os = "macos") && !typing {
                                actions.push(Action::QuickLookSelection);
                            } else if !c.is_control() {
                                actions.push(Action::TypeAhead(c));
                            }
                        }
                    }
                }
            }

            // Built as a table rather than dispatched through a closure: a
            // closure capturing `actions` would hold the mutable borrow for the
            // whole block, blocking the conditional pushes below it.
            let mut bindings: Vec<(egui::Key, Action)> = Vec::new();

            if alt {
                bindings.push((egui::Key::ArrowLeft, Action::Back));
                bindings.push((egui::Key::ArrowRight, Action::Forward));
                bindings.push((egui::Key::ArrowUp, Action::Up));
            } else {
                let last = visible.saturating_sub(1);
                bindings.extend([
                    (
                        egui::Key::ArrowDown,
                        Action::MoveCursor { delta: 1, extend: shift },
                    ),
                    (
                        egui::Key::ArrowUp,
                        Action::MoveCursor { delta: -1, extend: shift },
                    ),
                    // Horizontal movement only means anything in a grid; in the
                    // list it is a no-op, which is what `MoveWithin` reports.
                    (
                        egui::Key::ArrowRight,
                        Action::MoveWithinRow { delta: 1, extend: shift },
                    ),
                    (
                        egui::Key::ArrowLeft,
                        Action::MoveWithinRow { delta: -1, extend: shift },
                    ),
                    (
                        egui::Key::PageDown,
                        Action::MoveCursor { delta: page, extend: shift },
                    ),
                    (
                        egui::Key::PageUp,
                        Action::MoveCursor { delta: -page, extend: shift },
                    ),
                    (egui::Key::Home, Action::MoveTo { pos: 0, extend: shift }),
                    (egui::Key::End, Action::MoveTo { pos: last, extend: shift }),
                ]);
                // Backspace goes up a level, as in Explorer. Only without Alt,
                // so it does not double up with Alt+← above.
                if cfg!(target_os = "macos") && ctrl {
                    bindings.push((egui::Key::Backspace, Action::Delete { permanent: false }));
                } else {
                    bindings.push((egui::Key::Backspace, Action::Up));
                }
            }

            bindings.push((
                egui::Key::Delete,
                // Shift is the long-standing Windows convention for bypassing
                // the Recycle Bin, and the shell still confirms it.
                if cfg!(target_os = "macos") && shift {
                    Action::Ask(Box::new(Action::Delete { permanent: true }))
                } else { Action::Delete { permanent: shift } },
            ));
            // Alt+P, which is Explorer's binding for the same pane. Not
            // Ctrl+P: that is the file finder, and it is used far more often.
            if i.modifiers.alt && !ctrl && i.key_pressed(egui::Key::P) {
                actions.push(Action::TogglePreview);
            }

            bindings.push((egui::Key::F2, Action::BeginRename));
            bindings.push((egui::Key::F5, Action::Refresh));
            bindings.push((egui::Key::Escape, Action::ClearSelection));
            bindings.push((egui::Key::F6, Action::FocusNextPane));

            // Hidden files. Not Cmd+H on the Mac: the application menu owns
            // that as "Hide Neutron", so the window vanished instead. The
            // Finder's own binding, Cmd+Shift+., is the one Mac users know.
            if cfg!(target_os = "macos") {
                if i.modifiers.command && shift && i.key_pressed(egui::Key::Period) {
                    actions.push(Action::ToggleHidden);
                }
            } else if ctrl {
                bindings.push((egui::Key::H, Action::ToggleHidden));
            }

            // The Finder's layout, where it differs from the one above: the
            // views on Cmd+1–3, Duplicate on Cmd+D, the Go menu's folders, tabs
            // on Cmd+Shift+[ and ], Return to rename. Worked out first so the
            // shared table can stand aside for the keys it claims.
            let mac = cfg!(target_os = "macos") && i.modifiers.command;
            if mac {
                let control = i.modifiers.ctrl;
                let pressed = |k: egui::Key| i.key_pressed(k);
                let mut go = |action: Action| actions.push(action);
                if shift && !alt {
                    for (key, name) in [
                        (egui::Key::H, "Home"),
                        (egui::Key::D, "Desktop"),
                        (egui::Key::O, "Documents"),
                        (egui::Key::A, "Applications"),
                    ] {
                        if pressed(key) {
                            go(Action::GoKnown(name));
                        }
                    }
                    if pressed(egui::Key::G) {
                        go(Action::ToggleAddress);
                    }
                    if pressed(egui::Key::Z) {
                        go(Action::Redo);
                    }
                    // Shifted, a bracket arrives as a brace on most layouts.
                    if pressed(egui::Key::CloseBracket) || pressed(egui::Key::CloseCurlyBracket) {
                        go(Action::CycleTab(1));
                    }
                    if pressed(egui::Key::OpenBracket) || pressed(egui::Key::OpenCurlyBracket) {
                        go(Action::CycleTab(-1));
                    }
                }
                if alt && !shift && pressed(egui::Key::L) {
                    go(Action::GoKnown("Downloads"));
                }
                if alt && pressed(egui::Key::Backspace) {
                    go(Action::Ask(Box::new(Action::Delete { permanent: true })));
                }
                if !shift && !alt && !control {
                    for (key, view) in [
                        (egui::Key::Num1, file_list::ViewMode::Grid),
                        (egui::Key::Num2, file_list::ViewMode::List),
                        (egui::Key::Num3, file_list::ViewMode::Columns),
                        (egui::Key::Num4, file_list::ViewMode::Gallery),
                    ] {
                        if pressed(key) {
                            go(Action::SetView(view));
                        }
                    }
                    if pressed(egui::Key::D) {
                        go(Action::Duplicate);
                    }
                    if pressed(egui::Key::K) {
                        go(Action::OpenConnect);
                    }
                    if pressed(egui::Key::Y) {
                        go(Action::QuickLookSelection);
                    }
                    if pressed(egui::Key::E) {
                        go(Action::EjectCurrent);
                    }
                    if pressed(egui::Key::I) {
                        go(Action::GetInfoSelection);
                    }
                    if pressed(egui::Key::Z) {
                        go(Action::Undo);
                    }
                }
                // Control-Command-T: the Finder's "Add to Sidebar".
                if control && pressed(egui::Key::T) {
                    go(Action::TogglePin);
                }
            }
            // Control-Tab through the tabs, as in every tabbed Mac app.
            if cfg!(target_os = "macos") && i.modifiers.ctrl && !i.modifiers.command && i.key_pressed(egui::Key::Tab) {
                actions.push(Action::CycleTab(if shift { -1 } else { 1 }));
            }

            if i.modifiers.command && alt && !shift && !i.modifiers.ctrl && i.key_pressed(egui::Key::D) {
                actions.push(Action::ToggleComparePanes);
            }
            if mac && !(shift || alt || i.modifiers.ctrl) {
                bindings.extend([
                    (egui::Key::A, Action::SelectAll),
                    (egui::Key::R, Action::Refresh),
                    (egui::Key::W, Action::CloseActiveTab),
                ]);
            }

            // Everywhere but the Mac, whose layout is the block above.
            if ctrl && !cfg!(target_os = "macos") {
                bindings.extend([
                    (
                        // Ctrl+L is the address bar, as it is in every browser
                        // and in Explorer. Shift escalates to the view toggle,
                        // which had the plain binding first only because there
                        // was nothing else asking for it.
                        egui::Key::L,
                        if shift {
                            Action::ToggleView
                        } else {
                            Action::ToggleAddress
                        },
                    ),
                    // Filters the current listing. Index-wide search takes
                    // this binding at M4; until then narrowing what is already
                    // on screen is what Ctrl+F can honestly do.
                    (
                        // Shift escalates from filtering this listing to
                        // searching every volume.
                        egui::Key::F,
                        if shift {
                            Action::ToggleFinder(Mode::Everything)
                        } else {
                            Action::FocusFilter
                        },
                    ),
                    (
                        // Shift switches from finding a file to running a
                        // command, mirroring the editor convention.
                        egui::Key::P,
                        Action::ToggleFinder(if shift {
                            Mode::Commands
                        } else {
                            Mode::Files
                        }),
                    ),
                    (
                        // One binding, chosen by Shift, rather than two entries
                        // for the same key: both were in the table at once and
                        // Ctrl+Shift+D ran the pin *and* flipped the theme.
                        egui::Key::D,
                        if shift {
                            Action::TogglePin
                        } else {
                            Action::ToggleTheme
                        },
                    ),
                    (egui::Key::R, Action::Refresh),
                    (egui::Key::W, Action::CloseActiveTab),
                    (egui::Key::Z, if shift { Action::Redo } else { Action::Undo }),
                    (egui::Key::Y, Action::Redo),
                    // Backslash splits, mirroring the common terminal binding.
                    (
                        egui::Key::Backslash,
                        Action::Split(if shift { Axis::Vertical } else { Axis::Horizontal }),
                    ),
                ]);

                if shift {
                    bindings.push((egui::Key::N, Action::NewFolder));
                } else {
                    // Ctrl+N is a new window everywhere else; Ctrl+Shift+N is
                    // a new folder in every file manager. Both conventions are
                    // strong enough that neither can be given to the other.
                    bindings.push((egui::Key::N, Action::NewWindow));
                }

                // Ctrl+` for the terminal, as in the editor this is most often
                // sitting beside.
                bindings.push((egui::Key::Backtick, Action::OpenTerminal));

                if let Some(group) = self.workspace.focused_group().map(|g| g.id) {
                    bindings.push((egui::Key::T, Action::NewTab(group)));
                }

                // Ctrl+1..9 jumps to a tab within the focused pane.
                for (n, key) in [
                    egui::Key::Num1,
                    egui::Key::Num2,
                    egui::Key::Num3,
                    egui::Key::Num4,
                    egui::Key::Num5,
                    egui::Key::Num6,
                    egui::Key::Num7,
                    egui::Key::Num8,
                    egui::Key::Num9,
                ]
                .into_iter()
                .enumerate()
                {
                    bindings.push((key, Action::FocusTabIndex(n)));
                }
            }

            // What the Mac keeps of the table above, with Command.
            if mac && !alt && !i.modifiers.ctrl {
                bindings.extend([
                    (egui::Key::L, if shift { Action::ToggleView } else { Action::ToggleAddress }),
                    (egui::Key::F, if shift { Action::ToggleFinder(Mode::Everything) } else { Action::FocusFilter }),
                    (egui::Key::P, Action::ToggleFinder(if shift { Mode::Commands } else { Mode::Files })),
                    (egui::Key::Backslash, Action::Split(if shift { Axis::Vertical } else { Axis::Horizontal })),
                    (egui::Key::N, if shift { Action::NewFolder } else { Action::NewWindow }),
                ]);
                if !shift {
                    if let Some(group) = self.workspace.focused_group().map(|g| g.id) {
                        bindings.push((egui::Key::T, Action::NewTab(group)));
                    }
                }
            }
            // The real Control key for the terminal on the Mac, where Cmd+` is
            // the system's own "next window" and never arrives here.
            if cfg!(target_os = "macos") && i.modifiers.ctrl && !i.modifiers.command {
                bindings.push((egui::Key::Backtick, Action::OpenTerminal));
            }

            // The Finder's navigation keys: Cmd+Up to the enclosing folder,
            // Cmd+Down into the one under the cursor, Cmd+[ and Cmd+] through
            // history. They replace plain Up and Down while Command is held,
            // where the table above would otherwise also move the cursor.
            let finder_keys = cfg!(target_os = "macos") && i.modifiers.command && !alt;
            // On the Mac, Return renames and Command-O or Command-Down opens,
            // as in the Finder.
            let mut open_cursor = if cfg!(target_os = "macos") {
                if i.key_pressed(egui::Key::Enter) && !i.modifiers.command && !alt {
                    actions.push(Action::BeginRename);
                }
                false
            } else {
                i.key_pressed(egui::Key::Enter)
            };
            if finder_keys {
                bindings.retain(|(key, _)| !matches!(key, egui::Key::ArrowUp | egui::Key::ArrowDown));
                bindings.push((egui::Key::ArrowUp, Action::Up));
                if !shift {
                    bindings.push((egui::Key::OpenBracket, Action::Back));
                    bindings.push((egui::Key::CloseBracket, Action::Forward));
                }
                open_cursor |= i.key_pressed(egui::Key::ArrowDown);
                open_cursor |= !shift && i.key_pressed(egui::Key::O);
            }

            if open_cursor {
                if let Some(idx) = self
                    .workspace
                    .active_tab()
                    .and_then(|t| t.selection.cursor())
                {
                    actions.push(Action::Activate(idx));
                }
            }

            for (key, action) in bindings {
                if i.key_pressed(key) {
                    actions.push(action);
                }
            }
        });
    }
}
