use super::*;

mod certificate_tests {
    use super::certificate_in;

    #[test]
    fn the_fingerprint_is_found_in_a_refusal() {
        let fp = vec!["AB"; 32].join(":");
        let message = format!("files.example.com presented a certificate nobody vouches for (SHA-256 {fp}). Edit the server to trust it.");
        assert_eq!(certificate_in(&message), Some(fp));
        assert_eq!(certificate_in("connection refused"), None);
    }
}

mod screens {
    //! Renders the real interface to PNG files, for looking at a visual change
    //! without driving the app by hand.
    //!
    //! Ignored by default: it needs a GPU and writes files. Run with
    //! `NEUTRON_SHOTS=<dir> cargo test -p neutron-app screens -- --ignored`;
    //! `NEUTRON_SHOTS_FOLDER` picks the folder shown (the repository otherwise).
    use super::*;

    fn settle(harness: &mut egui_kittest::Harness<'_, NeutronApp>) {
        // Listings arrive from worker threads, so frames alone are not enough:
        // give the workers wall-clock time between them.
        for _ in 0..30 {
            harness.run_steps(2);
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn shoot(harness: &mut egui_kittest::Harness<'_, NeutronApp>, out: &Path, name: &str) {
        let mut image = harness.render().expect("render");
        // The window is transparent on the Mac; lay it over something like a
        // dark desktop so the picture reads the way the app does.
        for pixel in image.pixels_mut() {
            let [r, g, b, a] = pixel.0;
            let a = a as f32 / 255.0;
            let over = |c: u8, bg: f32| (c as f32 + bg * (1.0 - a)).round().min(255.0) as u8;
            pixel.0 = [over(r, 22.0), over(g, 22.0), over(b, 28.0), 255];
        }
        image.save(out.join(name)).expect("save");
    }

    fn act(harness: &mut egui_kittest::Harness<'_, NeutronApp>, action: Action) {
        let ctx = harness.ctx.clone();
        harness.state_mut().apply(&ctx, action);
    }

    /// A file as the system hands over a drop of one.
    #[derive(Debug)]
    struct Dropped(PathBuf);

    impl egui::DroppedFile for Dropped {
        fn path(&self) -> &Path {
            &self.0
        }
        fn bytes(&self) -> Result<Vec<u8>, String> {
            std::fs::read(&self.0).map_err(|e| e.to_string())
        }
    }

    /// "Open in Terminal" opens a terminal tab there, and what is typed into it
    /// runs.
    #[test]
    #[ignore]
    fn a_terminal_tab_runs_what_is_typed() {
        let root = std::env::temp_dir().join(format!("neutron-term-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let (_keep, opened) = crossbeam_channel::unbounded();
        let folder = root.display().to_string();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .with_pixels_per_point(1.0)
            .wgpu()
            .build_eframe(|cc| NeutronApp::new(cc, StartupTimer::new(Instant::now()), Some(folder), opened));
        settle(&mut harness);
        act(&mut harness, Action::OpenTerminalAt(root.clone()));
        settle(&mut harness);
        assert!(harness.state().workspace.active_tab().unwrap().location().is_terminal());
        harness.input_mut().events.push(egui::Event::Text("echo neutron-$((6*7))".into()));
        harness.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(1);
        let mut seen = String::new();
        for _ in 0..100 {
            settle(&mut harness);
            seen = harness.state().terminals.borrow().values().next().and_then(|s| s.with_screen(|s| s.contents())).unwrap_or_default();
            if seen.contains("neutron-42") {
                break;
            }
        }
        assert!(seen.contains("neutron-42"), "{seen}");
        if let Some(out) = std::env::var_os("NEUTRON_SHOTS").map(PathBuf::from) {
            shoot(&mut harness, &out, "terminal.png");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Ask Claude Code opens it in a terminal tab on the folder, where its
    /// interface draws. Needs Claude Code installed; it is only started, and
    /// asked nothing.
    #[test]
    #[ignore]
    fn an_agent_opens_in_a_terminal_tab() {
        let root = std::env::temp_dir().join(format!("neutron-agent-tab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let (_keep, opened) = crossbeam_channel::unbounded();
        let folder = root.display().to_string();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .with_pixels_per_point(1.0)
            .wgpu()
            .build_eframe(|cc| NeutronApp::new(cc, StartupTimer::new(Instant::now()), Some(folder), opened));
        settle(&mut harness);
        act(&mut harness, Action::AskAgent(crate::agent::Agent::Claude));
        let mut seen = String::new();
        for _ in 0..40 {
            settle(&mut harness);
            seen = harness.state().terminals.borrow().values().next().and_then(|s| s.with_screen(|s| s.contents())).unwrap_or_default();
            if seen.to_lowercase().contains("claude") || seen.contains("trust") {
                break;
            }
        }
        if let Some(out) = std::env::var_os("NEUTRON_SHOTS").map(PathBuf::from) {
            shoot(&mut harness, &out, "agent.png");
        }
        assert!(seen.to_lowercase().contains("claude") || seen.contains("trust"), "{seen}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The viewer opens code with its lines coloured, and finds in it; and
    /// draws a PDF's pages. Shots of each with `NEUTRON_SHOTS`;
    /// `NEUTRON_SHOTS_PDF` names a PDF to show.
    #[test]
    #[ignore]
    fn the_viewer_shows_code_and_pdfs() {
        let code = Path::new(env!("CARGO_MANIFEST_DIR")).join("../neutron-ui/src/highlight.rs").canonicalize().unwrap();
        let folder = code.parent().unwrap().display().to_string();
        let (_keep, opened) = crossbeam_channel::unbounded();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .with_pixels_per_point(1.0)
            .wgpu()
            .build_eframe(|cc| NeutronApp::new(cc, StartupTimer::new(Instant::now()), Some(folder), opened));
        settle(&mut harness);
        act(&mut harness, Action::OpenInViewer(code.clone()));
        settle(&mut harness);
        {
            let state = harness.state();
            let mut viewers = state.viewers.borrow_mut();
            let view = viewers.view(&code);
            assert!(matches!(view.doc, crate::viewer::Doc::Text(_)));
            view.find = "carry".into();
            view.refind();
            assert!(!view.matches.is_empty());
        }
        settle(&mut harness);
        let out = std::env::var_os("NEUTRON_SHOTS").map(PathBuf::from);
        if let Some(out) = &out {
            shoot(&mut harness, out, "viewer-code.png");
        }
        if let Some(pdf) = std::env::var_os("NEUTRON_SHOTS_PDF").map(PathBuf::from) {
            act(&mut harness, Action::OpenInViewer(pdf.clone()));
            for _ in 0..4 {
                settle(&mut harness);
            }
            assert!(matches!(harness.state().viewers.borrow_mut().view(&pdf).doc, crate::viewer::Doc::Pdf(_)));
            if let Some(out) = &out {
                shoot(&mut harness, out, "viewer-pdf.png");
            }
        }
    }

    /// The gallery, the window without its sidebar and rail, sizes in the
    /// grid and columns, a Smart Folder and its dialog, and Get Info — as
    /// shots, with `NEUTRON_SHOTS`.
    #[test]
    #[ignore]
    fn the_finder_parity_views() {
        let Some(out) = std::env::var_os("NEUTRON_SHOTS").map(PathBuf::from) else { return };
        std::fs::create_dir_all(&out).unwrap();
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
        let (_keep, opened) = crossbeam_channel::unbounded();
        let start = folder.display().to_string();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .with_pixels_per_point(1.0)
            .wgpu()
            .build_eframe(|cc| NeutronApp::new(cc, StartupTimer::new(Instant::now()), Some(start), opened));
        settle(&mut harness);
        let select = |h: &mut egui_kittest::Harness<'_, NeutronApp>, name: &str| {
            let idx = {
                let t = h.state().workspace.active_tab().unwrap();
                (0..t.list.len()).find(|&i| t.list.name(i) == name).unwrap_or(0)
            };
            act(h, Action::Select { idx, mode: SelectMode::Replace });
        };
        act(&mut harness, Action::SetView(file_list::ViewMode::Gallery));
        select(&mut harness, "README.md");
        settle(&mut harness);
        shoot(&mut harness, &out, "gallery.png");

        act(&mut harness, Action::ToggleSidebar);
        act(&mut harness, Action::ToggleToolbar);
        act(&mut harness, Action::SetView(file_list::ViewMode::Grid));
        act(&mut harness, Action::ToggleAllSizes);
        for _ in 0..3 {
            settle(&mut harness);
        }
        shoot(&mut harness, &out, "bare-grid-sizes.png");
        act(&mut harness, Action::ToggleSidebar);
        act(&mut harness, Action::ToggleToolbar);

        act(&mut harness, Action::SetView(file_list::ViewMode::Columns));
        select(&mut harness, "crates");
        for _ in 0..3 {
            settle(&mut harness);
        }
        shoot(&mut harness, &out, "columns-sizes.png");
        act(&mut harness, Action::ToggleAllSizes);

        act(&mut harness, Action::SetView(file_list::ViewMode::List));
        let query = neutron_core::query::Query::typed("Cargo kind:text");
        act(&mut harness, Action::RunSearch { query: neutron_core::query::Query { scope: Some(folder.clone()), ..query }, save_as: None, replacing: None });
        for _ in 0..3 {
            settle(&mut harness);
        }
        shoot(&mut harness, &out, "smart-folder.png");
        act(&mut harness, Action::NewSmartFolder);
        settle(&mut harness);
        shoot(&mut harness, &out, "smart-folder-dialog.png");
        harness.state_mut().search_dialog = None;

        act(&mut harness, Action::GetInfo(vec![folder.join("crates")]));
        for _ in 0..3 {
            settle(&mut harness);
        }
        shoot(&mut harness, &out, "get-info.png");
    }

    /// A drag held on a folder's row opens the folder, in each view.
    #[test]
    #[ignore]
    fn a_drag_held_on_a_folder_opens_it() {
        let root = std::env::temp_dir().join(format!("neutron-spring-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("inbox")).unwrap();
        let file = root.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        for view in [file_list::ViewMode::List, file_list::ViewMode::Grid, file_list::ViewMode::Columns] {
            let (_keep, opened) = crossbeam_channel::unbounded();
            let folder = root.display().to_string();
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1440.0, 900.0))
                .with_pixels_per_point(1.0)
                .wgpu()
                .build_eframe(|cc| NeutronApp::new(cc, StartupTimer::new(Instant::now()), Some(folder), opened));
            act(&mut harness, Action::SetView(view));
            settle(&mut harness);
            let hover = |h: &mut egui_kittest::Harness<'_, NeutronApp>| {
                h.input_mut().hovered_files.push(egui::HoveredFile { path: Some(file.clone()), ..Default::default() });
                h.run_steps(1);
            };
            hover(&mut harness);
            let target = harness.state().drop_debug.clone().into_iter().find(|t| t.path.ends_with("inbox")).unwrap();
            harness.hover_at(target.rect.center());
            let started = Instant::now();
            while started.elapsed() < SPRING_DELAY + Duration::from_millis(300) {
                hover(&mut harness);
                std::thread::sleep(Duration::from_millis(30));
            }
            let at = harness.state().workspace.active_tab().unwrap().location().clone();
            assert_eq!(at, NodeId::path(root.join("inbox")), "{view:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A file dropped on a folder's row goes inside that folder, in each view.
    #[test]
    #[ignore]
    fn a_drop_on_a_folder_row_goes_inside_it() {
        let root = std::env::temp_dir().join(format!("neutron-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (here, away) = (root.join("here"), root.join("away"));
        std::fs::create_dir_all(here.join("inbox")).unwrap();
        std::fs::create_dir_all(&away).unwrap();
        for view in [file_list::ViewMode::List, file_list::ViewMode::Grid, file_list::ViewMode::Columns] {
            let file = away.join(format!("{view:?}.txt"));
            std::fs::write(&file, "x").unwrap();
            let (_keep, opened) = crossbeam_channel::unbounded();
            let folder = here.display().to_string();
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1440.0, 900.0))
                .with_pixels_per_point(1.0)
                .wgpu()
                .build_eframe(|cc| NeutronApp::new(cc, StartupTimer::new(Instant::now()), Some(folder), opened));
            act(&mut harness, Action::SetView(view));
            settle(&mut harness);
            // Where the inbox is drawn: the target it offers while a drag is on.
            harness.input_mut().hovered_files.push(egui::HoveredFile { path: Some(file.clone()), ..Default::default() });
            harness.run_steps(2);
            let targets = harness.state().drop_debug.clone();
            // The path bar's folders, and the pane's tab, take drops too.
            assert!(targets.iter().any(|t| t.path == root && t.tab.is_none()), "{view:?}: the path bar");
            assert!(targets.iter().any(|t| t.path == here && t.tab.is_some()), "{view:?}: the tab");
            let target = targets.into_iter().find(|t| t.path.ends_with("inbox")).unwrap();
            harness.hover_at(target.rect.center());
            harness.input_mut().hovered_files.push(egui::HoveredFile { path: Some(file.clone()), ..Default::default() });
            harness.run_steps(1);
            harness.input_mut().dropped_files.push(std::sync::Arc::new(Dropped(file.clone())));
            harness.run_steps(1);
            settle(&mut harness);
            assert!(here.join("inbox").join(file.file_name().unwrap()).exists(), "{view:?}: not moved into the folder");
            assert!(!file.exists(), "{view:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore]
    fn screens() {
        let Some(out) = std::env::var_os("NEUTRON_SHOTS").map(PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&out).unwrap();
        let folder = std::env::var("NEUTRON_SHOTS_FOLDER").unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap().display().to_string()
        });

        let (_keep, opened) = crossbeam_channel::unbounded();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .with_pixels_per_point(1.0)
            .wgpu()
            .build_eframe(|cc| NeutronApp::new(cc, StartupTimer::new(Instant::now()), Some(folder), opened));

        act(&mut harness, Action::SetView(file_list::ViewMode::List));
        settle(&mut harness);
        // A selected row, so the selection is in the picture.
        let idx = {
            let t = harness.state().workspace.active_tab().unwrap();
            (0..t.list.len()).find(|&i| t.list.name(i) == "Cargo.toml").unwrap_or(0)
        };
        act(&mut harness, Action::Select { idx, mode: SelectMode::Replace });
        settle(&mut harness);
        shoot(&mut harness, &out, "list.png");

        act(&mut harness, Action::TogglePreview);
        harness.state_mut().notices.info("Copied 1 item");
        settle(&mut harness);
        shoot(&mut harness, &out, "preview.png");
        act(&mut harness, Action::TogglePreview);
        act(&mut harness, Action::DismissNotice);

        act(&mut harness, Action::SetView(file_list::ViewMode::Columns));
        settle(&mut harness);
        shoot(&mut harness, &out, "columns.png");

        act(&mut harness, Action::SetView(file_list::ViewMode::Grid));
        settle(&mut harness);
        shoot(&mut harness, &out, "grid.png");
        act(&mut harness, Action::GroupBy(neutron_core::GroupBy::Kind));
        act(&mut harness, Action::SetTileScale(1.4));
        settle(&mut harness);
        shoot(&mut harness, &out, "grid-grouped.png");
        act(&mut harness, Action::GroupBy(neutron_core::GroupBy::None));
        act(&mut harness, Action::SetTileScale(1.0));

        // The overlays, over the list.
        act(&mut harness, Action::SetView(file_list::ViewMode::List));
        act(&mut harness, Action::ContextMenu { idx: Some(idx), pos: egui::pos2(760.0, 470.0) });
        settle(&mut harness);
        shoot(&mut harness, &out, "menu.png");
        // Dropping the reply releases the thread parked on the menu.
        harness.state_mut().menu = None;
        harness.state_mut().menu_reply = None;

        // The folder's own menu, with nothing selected.
        act(&mut harness, Action::ClearSelection);
        act(&mut harness, Action::ContextMenu { idx: None, pos: egui::pos2(760.0, 300.0) });
        settle(&mut harness);
        shoot(&mut harness, &out, "menu-folder.png");
        harness.state_mut().menu = None;
        harness.state_mut().menu_reply = None;
        act(&mut harness, Action::Select { idx, mode: SelectMode::Replace });

        act(&mut harness, Action::ToggleFinder(crate::finder::Mode::Files));
        settle(&mut harness);
        shoot(&mut harness, &out, "finder.png");
        act(&mut harness, Action::CloseFinder);

        // The question alone, set directly: `apply` would delete. What follows
        // a yes is made harmless in case anything answers it.
        {
            let app = harness.state_mut();
            let question = app.question_for(&Action::Delete { permanent: true });
            let subject = app.subject();
            app.confirm = question.map(|q| (q, Box::new(Action::DismissNotice), subject));
        }
        settle(&mut harness);
        shoot(&mut harness, &out, "confirm.png");
        harness.state_mut().confirm = None;

        // A question about clashing names. Nothing is transferred: the
        // request names no files, and the question is cleared unanswered.
        {
            let app = harness.state_mut();
            let tab = app.workspace.active_tab_id().unwrap();
            app.clash_questions.push_back(crate::transfers::Question {
                request: crate::transfers::Request {
                    tab,
                    paths: vec![],
                    destination: PathBuf::from("/Users/someone/Downloads"),
                    how: neutron_shell::fileops::Transfer::Copy,
                },
                names: vec!["report.pdf".into(), "notes.md".into(), "photo.heic".into()],
            });
        }
        settle(&mut harness);
        shoot(&mut harness, &out, "clash.png");
        harness.state_mut().clash_questions.clear();

        act(&mut harness, Action::OpenConnect);
        settle(&mut harness);
        shoot(&mut harness, &out, "connect.png");
        harness.state_mut().connect.close();

        // Inside an archive, when one is given.
        if let Some(archive) = std::env::var_os("NEUTRON_SHOTS_ARCHIVE").map(PathBuf::from) {
            act(&mut harness, Action::SetView(file_list::ViewMode::List));
            act(&mut harness, Action::Navigate(NodeId::archive(archive.clone(), "project")));
            settle(&mut harness);
            act(&mut harness, Action::Select { idx: 0, mode: SelectMode::Replace });
            settle(&mut harness);
            shoot(&mut harness, &out, "archive.png");
            act(&mut harness, Action::ContextMenu { idx: Some(0), pos: egui::pos2(700.0, 260.0) });
            settle(&mut harness);
            shoot(&mut harness, &out, "archive-menu.png");
            harness.state_mut().menu = None;
            act(&mut harness, Action::Back);
            settle(&mut harness);
        }

        // Tags, in a folder given for them.
        if let Some(folder) = std::env::var_os("NEUTRON_SHOTS_TAGS").map(PathBuf::from) {
            act(&mut harness, Action::SetView(file_list::ViewMode::List));
            act(&mut harness, Action::Navigate(NodeId::path(folder)));
            settle(&mut harness);
            act(&mut harness, Action::Select { idx: 1, mode: SelectMode::Replace });
            settle(&mut harness);
            shoot(&mut harness, &out, "tags.png");
            act(&mut harness, Action::SetView(file_list::ViewMode::Grid));
            settle(&mut harness);
            shoot(&mut harness, &out, "tags-grid.png");
            act(&mut harness, Action::SetView(file_list::ViewMode::List));
            act(&mut harness, Action::Back);
            settle(&mut harness);
        }

        // Grouped by kind, then by date added.
        act(&mut harness, Action::SetView(file_list::ViewMode::List));
        act(&mut harness, Action::GroupBy(neutron_core::GroupBy::Kind));
        settle(&mut harness);
        shoot(&mut harness, &out, "grouped.png");
        act(&mut harness, Action::SortBy(neutron_core::SortColumn::Added));
        act(&mut harness, Action::GroupBy(neutron_core::GroupBy::Added));
        settle(&mut harness);
        shoot(&mut harness, &out, "grouped-added.png");
        act(&mut harness, Action::GroupBy(neutron_core::GroupBy::None));
        act(&mut harness, Action::SortBy(neutron_core::SortColumn::Name));
        settle(&mut harness);

        // A second window: its own tabs, its listing routed to it.
        act(&mut harness, Action::NewWindow);
        settle(&mut harness);
        {
            let app = harness.state();
            let second = &app.windows[0].workspace;
            let tab = second.active_tab().expect("the new window has a tab");
            assert!(!tab.list.is_empty(), "the new window's listing did not reach it");
            assert!(
                app.workspace.active_tab_id() != second.active_tab_id(),
                "the windows share a tab id"
            );
        }
        shoot(&mut harness, &out, "windows.png");
        harness.state_mut().windows.clear();

        act(&mut harness, Action::Navigate(NodeId::Home));
        settle(&mut harness);
        shoot(&mut harness, &out, "dashboard.png");

        // The other theme, back where the list was. Flipped back afterwards:
        // the choice is saved, and a test has no business changing it.
        act(&mut harness, Action::Back);
        let original_theme = harness.state().theme_mode;
        act(&mut harness, Action::SetTheme(ThemeMode::Light));
        settle(&mut harness);
        shoot(&mut harness, &out, "light.png");
        act(&mut harness, Action::SetTheme(original_theme));
        settle(&mut harness);
    }
}

mod saved_window_tests {
    use super::*;

    #[test]
    fn a_saved_window_comes_back_where_it_was_with_its_tabs() {
        let mut workspace = Workspace::new(NodeId::path("/tmp"));
        let group = workspace.focused;
        let view = workspace.view_for_new_tab();
        workspace.open_tab(group, NodeId::path("/usr"), view);
        let saved = SavedWindow {
            workspace: workspace.persist(),
            preview: true,
            at: Some([40.0, 60.0]),
            size: Some([900.0, 640.0]),
        };
        let json = serde_json::to_string(&vec![saved]).unwrap();
        let back: Vec<SavedWindow> = serde_json::from_str(&json).unwrap();
        assert_eq!((back[0].at, back[0].size, back[0].preview), (Some([40.0, 60.0]), Some([900.0, 640.0]), true));
        let restored = Workspace::restore(&back[0].workspace).unwrap();
        let mut places: Vec<String> = restored.tabs.values().map(|t| t.location().to_string()).collect();
        places.sort();
        assert_eq!(places, vec!["/tmp".to_owned(), "/usr".to_owned()]);
    }
}
