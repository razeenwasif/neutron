//! Plugin commands inside the application: where they appear, and what happens
//! when one is chosen.
//!
//! The protocol and the process handling are in `neutron-plugin`, which knows
//! nothing about egui. This is the other half: turning a [`PluginSet`] into
//! palette entries and menu rows, running one on a worker, and carrying the
//! result back to the UI thread.
//!
//! # One at a time
//!
//! A second command while one is running is refused rather than queued, for the
//! same reason as archive jobs: one status line, one Stop, and no ambiguity
//! about which plugin a message came from.
//!
//! # The set is read once at startup
//!
//! Discovery is a directory listing and a few small file reads — a fraction of
//! a millisecond — but it is still I/O, and cold start is measured in this
//! project. It happens once, and the palette offers a way to read it again for
//! anyone editing a manifest.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use neutron_core::MenuItem;
use neutron_plugin::discover::PluginSet;
use neutron_plugin::host::Outcome;
use neutron_plugin::manifest::Needs;

use crate::notice::Level;

use crate::loader::TabId;

/// First menu id used for a plugin row.
///
/// The shell's own command ids run from 1 to `0x7FFF` (see
/// `neutron_shell::menu`), and an id at or above this can only be ours. Kept
/// far enough clear of that range that no arithmetic slip could overlap it.
pub const MENU_ID_BASE: u32 = 0x1000_0000;

/// The shell's ids stop at `0x7FFF`. If this range ever overlapped, choosing a
/// plugin row would invoke an unrelated shell verb — silently, and possibly
/// destructively. Checked at compile time so it cannot regress unnoticed.
const _: () = assert!(MENU_ID_BASE > 0x7FFF);

/// A command a plugin declared, referred to by name rather than by position.
///
/// Positions go stale: the set can be re-read while the palette is open, and an
/// index into the old list would then run a *different* command rather than
/// reporting that this one is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    pub plugin: String,
    pub command: String,
}

/// What is running now, and what the last one said.
pub struct PluginRuntime {
    set: PluginSet,
    running: Option<Running>,
    /// What to say in the status bar, and whether it is a failure.
    ///
    /// Drained by the app into the one notice surface rather than drawn here —
    /// see [`crate::notice`] for why there is only one line.
    pub note: Option<(crate::notice::Level, String)>,
    /// Menu ids handed out when the current context menu was built.
    menu_refs: Vec<Ref>,
}

struct Running {
    /// "Checksums: Copy SHA-256" — the plugin named as well as the command,
    /// because the command title is the plugin author's words and may not say
    /// whose they are.
    label: String,
    /// Where a `navigate` or `refresh` should land.
    tab: TabId,
    cancel: Arc<AtomicBool>,
    done: crossbeam_channel::Receiver<Outcome>,
}

impl PluginRuntime {
    /// Reads the plugins folder. Nothing is executed.
    pub fn load() -> PluginRuntime {
        let set = PluginSet::load();
        for error in &set.errors {
            tracing::warn!(dir = %error.dir.display(), "plugin not loaded: {}", error.reason);
        }
        for plugin in &set.plugins {
            tracing::info!(
                id = plugin.id(),
                commands = plugin.manifest.commands.len(),
                "plugin loaded"
            );
        }
        // A refusal reaches the status bar as well as the log. The whole point
        // of keeping the reason is that somebody sees it, and nobody reads a
        // log to find out why a menu entry is missing — they do not yet know it
        // is missing.
        let note = match set.errors.len() {
            0 => None,
            1 => Some(format!(
                "A plugin was not loaded: {} — {}",
                set.errors[0].dir.file_name().unwrap_or_default().to_string_lossy(),
                set.errors[0].reason
            )),
            n => Some(format!("{n} plugins were not loaded — run \"Reload plugins\" for why")),
        };

        let note = note.map(|text| (Level::Error, text));
        PluginRuntime { set, running: None, note, menu_refs: Vec::new() }
    }

    /// Re-reads the plugins folder, and says what it found.
    ///
    /// The reason this exists is plugin development: editing a manifest and
    /// restarting the file manager to see whether it parsed is a miserable loop.
    pub fn reload(&mut self) {
        let previous = self.set.plugins.len();
        *self = PluginRuntime {
            set: PluginSet::load(),
            running: self.running.take(),
            note: None,
            menu_refs: Vec::new(),
        };

        let found = self.set.plugins.len();
        // A refusal is a failure worth keeping on screen; a plain count is a
        // receipt that can clear itself.
        let level = if self.set.errors.is_empty() {
            Level::Info
        } else {
            Level::Error
        };
        let mut note = match found {
            0 => "No plugins installed".to_owned(),
            1 => "1 plugin loaded".to_owned(),
            n => format!("{n} plugins loaded"),
        };
        // The failures matter more than the successes here — this command is
        // almost always run because something did not appear.
        if !self.set.errors.is_empty() {
            note.push_str(&match self.set.errors.len() {
                1 => format!(", 1 folder refused: {}", self.set.errors[0].reason),
                n => format!(", {n} folders refused, first: {}", self.set.errors[0].reason),
            });
        } else if found == previous && found > 0 {
            note.push_str(" (unchanged)");
        }
        self.note = Some((level, note));
    }

    /// Every plugin command, for the command palette.
    ///
    /// Commands needing a selection are offered whether or not there is one:
    /// the palette is a catalogue, and an entry vanishing from it is more
    /// confusing than an entry that explains it needs a selection.
    pub fn palette_entries(&self) -> Vec<(Ref, String, String)> {
        self.set
            .commands()
            .map(|(plugin, command)| {
                (
                    Ref { plugin: plugin.id().to_owned(), command: command.id.clone() },
                    command.title.clone(),
                    plugin.manifest.name.clone(),
                )
            })
            .collect()
    }

    /// Rows to append to a context menu, and the ids they were given.
    ///
    /// `has_selection` decides what is offered, because a "hash the selection"
    /// row with nothing selected is not an error to report — it is a row that
    /// should not be there.
    pub fn menu_rows(&mut self, has_selection: bool) -> Vec<MenuItem> {
        self.menu_refs.clear();

        let mut rows = Vec::new();
        for (plugin, command) in self.set.commands() {
            if !command.menu {
                continue;
            }
            if command.needs == Needs::Selection && !has_selection {
                continue;
            }
            let id = MENU_ID_BASE + self.menu_refs.len() as u32;
            self.menu_refs.push(Ref {
                plugin: plugin.id().to_owned(),
                command: command.id.clone(),
            });
            rows.push(MenuItem {
                id,
                label: command.title.clone(),
                enabled: true,
                ..Default::default()
            });
        }
        rows
    }

    /// Resolves a chosen menu id, if it was one of ours.
    pub fn menu_choice(&self, id: u32) -> Option<Ref> {
        if id < MENU_ID_BASE {
            return None;
        }
        self.menu_refs.get((id - MENU_ID_BASE) as usize).cloned()
    }

    /// Starts a command on a worker thread.
    ///
    /// Every refusal sets [`PluginRuntime::note`] rather than being silent: the
    /// user picked an entry from a menu, and something has to answer.
    pub fn start(
        &mut self,
        reference: &Ref,
        tab: TabId,
        folder: PathBuf,
        selection: Vec<PathBuf>,
        ctx: &egui::Context,
    ) {
        if self.running.is_some() {
            self.note = Some((Level::Info, "A plugin command is already running".to_owned()));
            return;
        }

        let Some((plugin, command)) = self.set.find(&reference.plugin, &reference.command) else {
            // Reachable: the set was re-read while the palette was open, or the
            // folder was deleted from under it.
            self.note = Some((Level::Error, format!("{} is no longer installed", reference.plugin)));
            return;
        };

        if command.needs == Needs::Selection && selection.is_empty() {
            self.note = Some((Level::Info, format!("{} needs a selection", command.title)));
            return;
        }

        let label = format!("{}: {}", plugin.manifest.name, command.title);
        let cancel = Arc::new(AtomicBool::new(false));

        let (tx, rx) = crossbeam_channel::bounded(1);
        // Cloned rather than borrowed: the worker outlives this call, and
        // `PluginSet` is behind `&self` here.
        let plugin = plugin.clone();
        let command = command.clone();
        let worker_cancel = Arc::clone(&cancel);
        let wake = ctx.clone();

        let spawned = std::thread::Builder::new()
            .name("neutron-plugin".into())
            .spawn(move || {
                let outcome = neutron_plugin::host::invoke_with_cancel(
                    &plugin,
                    &command,
                    &folder,
                    &selection,
                    &worker_cancel,
                );
                let _ = tx.send(outcome);
                wake.request_repaint();
            });

        match spawned {
            Ok(_) => {
                self.note = None;
                self.running = Some(Running { label, tab, cancel, done: rx });
            }
            Err(e) => {
                // Out of threads. Worth saying rather than appearing to ignore
                // the click.
                self.note = Some((Level::Error, format!("could not start {label}: {e}")));
            }
        }
    }

    /// Stops the running command, if any.
    pub fn cancel(&mut self) {
        if let Some(running) = self.running.as_ref() {
            running.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// What is running, for the status bar.
    pub fn running_label(&self) -> Option<&str> {
        self.running.as_ref().map(|r| r.label.as_str())
    }

    /// Collects a finished command. Returns the tab it was run from and what it
    /// asked for.
    pub fn poll(&mut self) -> Option<(TabId, Outcome)> {
        let running = self.running.as_ref()?;
        let outcome = running.done.try_recv().ok()?;
        let Running { label, tab, .. } = self.running.take()?;

        self.note = Some(match (&outcome.error, &outcome.status) {
            (Some(error), _) => (Level::Error, format!("{label} — {error}")),
            (None, Some(status)) => (Level::Info, format!("{label} — {status}")),
            // Succeeded and said nothing. Silence reads as a broken menu
            // entry, so the host speaks for it.
            (None, None) => (Level::Info, format!("{label} — done")),
        });

        Some((tab, outcome))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> PluginRuntime {
        PluginRuntime {
            set: PluginSet::default(),
            running: None,
            note: None,
            menu_refs: Vec::new(),
        }
    }

    #[test]
    fn menu_rows_are_numbered_from_the_base_and_resolve_back() {
        let mut runtime = empty();
        // Stand in for what discovery would have produced.
        runtime.menu_refs = vec![
            Ref { plugin: "a".into(), command: "one".into() },
            Ref { plugin: "b".into(), command: "two".into() },
        ];

        assert_eq!(runtime.menu_choice(MENU_ID_BASE).unwrap().command, "one");
        assert_eq!(runtime.menu_choice(MENU_ID_BASE + 1).unwrap().command, "two");
        // One past the end: a row from a menu that has since been rebuilt.
        assert_eq!(runtime.menu_choice(MENU_ID_BASE + 2), None);
    }

    #[test]
    fn a_menu_id_from_a_previous_menu_resolves_to_nothing() {
        // Menu rows are rebuilt per right-click, so an id from a stale menu
        // must come back as None rather than as whatever now sits at that
        // index.
        let runtime = empty();
        assert_eq!(runtime.menu_choice(MENU_ID_BASE), None);
        assert_eq!(runtime.menu_choice(MENU_ID_BASE + 5), None);
    }

    #[test]
    fn a_shell_id_is_never_read_as_a_plugin_row() {
        let runtime = empty();
        assert_eq!(runtime.menu_choice(1), None);
        assert_eq!(runtime.menu_choice(0x7FFF), None);
    }
}
