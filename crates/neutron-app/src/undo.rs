//! Undo and redo for what Neutron does to files: copies, moves, renames, new
//! folders and moves to the Trash.
//!
//! # Three kinds of step
//!
//! Everything undoable comes down to one of three things having happened, and
//! each has a plain reverse that is itself one of the three:
//!
//! - **Relocated** — items moved from one place to another: a move, a rename,
//!   a put-back. Reversed by moving them back, which relocates them again.
//! - **Created** — items that did not exist: copies, a new folder. Reversed by
//!   moving them to the Trash, never by deleting them, so even an undo can be
//!   recovered from. That trashes them.
//! - **Trashed** — items moved to the Trash, with where each went there.
//!   Reversed by putting them back, which creates them again.
//!
//! So reversing a step yields the step that would redo it, and undo and redo
//! are the same operation run from different stacks. A permanent delete is
//! none of these and is not recorded: there is nothing to put back.

use std::path::PathBuf;

/// Something that happened to files, in the terms that reverse it.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// `(where it was, where it is now)`, for each item.
    Relocated(Vec<(PathBuf, PathBuf)>),
    /// Items made: copies, a new folder.
    Created(Vec<PathBuf>),
    /// `(where it was, where it is in the Trash)`, for each item.
    Trashed(Vec<(PathBuf, PathBuf)>),
    /// Steps taken one after another, as one change: a paste that replaced
    /// what was there trashed it first, then transferred. Reversed last first.
    Sequence(Vec<Step>),
}

/// One undoable change, as the menu names it.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// "Copy", "Move to Trash" — what follows "Undo" in the menu.
    pub label: String,
    pub step: Step,
}

/// Most changes kept; the oldest go first.
const DEPTH: usize = 100;

/// The two stacks, and whether a reversal is under way.
#[derive(Default)]
pub struct History {
    undo: Vec<Record>,
    redo: Vec<Record>,
    /// Set while a reversal runs on a worker, so a second press of Command-Z
    /// does not reverse the next change before the first has finished.
    pub busy: bool,
}

impl History {
    /// A new change. Clears what could be redone: redoing past a fresh change
    /// would replay something on top of files it no longer describes.
    pub fn record(&mut self, record: Record) {
        if record_is_empty(&record) {
            return;
        }
        self.redo.clear();
        self.push(record);
    }

    fn push(&mut self, record: Record) {
        self.undo.push(record);
        if self.undo.len() > DEPTH {
            self.undo.remove(0);
        }
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|r| r.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|r| r.label.as_str())
    }

    pub fn take_undo(&mut self) -> Option<Record> {
        self.undo.pop()
    }

    pub fn take_redo(&mut self) -> Option<Record> {
        self.redo.pop()
    }

    /// An undo finished: what would redo it goes on the redo stack.
    pub fn undone(&mut self, inverse: Record) {
        if !record_is_empty(&inverse) {
            self.redo.push(inverse);
        }
    }

    /// A redo finished: what would undo it again goes back on the undo stack.
    pub fn redone(&mut self, inverse: Record) {
        if !record_is_empty(&inverse) {
            self.push(inverse);
        }
    }
}

fn record_is_empty(record: &Record) -> bool {
    step_is_empty(&record.step)
}

fn step_is_empty(step: &Step) -> bool {
    match step {
        Step::Relocated(items) | Step::Trashed(items) => items.is_empty(),
        Step::Created(items) => items.is_empty(),
        Step::Sequence(steps) => steps.iter().all(step_is_empty),
    }
}

/// What reversing a step did.
#[derive(Debug, Default)]
pub struct Reversed {
    /// The step that would carry it forwards again — covering only what was
    /// reversed.
    pub inverse: Option<Step>,
    /// A line for each item that could not be reversed.
    pub failed: Vec<String>,
    /// Items in the Trash that macOS would not let Neutron take out: it keeps
    /// the Trash from apps without Full Disk Access. The Finder can put them
    /// back, and these are where they are.
    pub blocked: Vec<PathBuf>,
}

/// Carries `step` out backwards. Blocks on the disk: call it from a worker.
pub fn reverse(step: &Step) -> Reversed {
    let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut failed = Vec::new();
    let mut blocked = Vec::new();
    let inverse = match step {
        Step::Relocated(items) => {
            let mut back = Vec::new();
            for (was, now) in items {
                match neutron_shell::transfer::relocate(now, was) {
                    Ok(()) => back.push((now.clone(), was.clone())),
                    Err(why) => failed.push(format!("{}: {why}", name(now))),
                }
            }
            Step::Relocated(back)
        }
        Step::Created(items) => {
            // Only what is still there: an item since moved or deleted is
            // not this step's to touch.
            let present: Vec<PathBuf> = items.iter().filter(|p| p.symlink_metadata().is_ok()).cloned().collect();
            for gone in items.iter().filter(|p| p.symlink_metadata().is_err()) {
                failed.push(format!("{}: no longer there", name(gone)));
            }
            match neutron_shell::fileops::trash(&present) {
                Ok(went) => Step::Trashed(
                    present.into_iter().zip(went).filter_map(|(was, went)| went.map(|w| (was, w))).collect(),
                ),
                Err(why) => {
                    failed.push(why);
                    Step::Trashed(Vec::new())
                }
            }
        }
        // The last step taken is the first undone — the transfer before the
        // put-back, so the names it took are free again — and the inverse
        // lists them in the order they were just done, which is the order
        // redoing them reverses.
        Step::Sequence(steps) => {
            let mut inverses = Vec::new();
            for step in steps.iter().rev() {
                let done = reverse(step);
                failed.extend(done.failed);
                blocked.extend(done.blocked);
                if let Some(inverse) = done.inverse {
                    inverses.push(inverse);
                }
            }
            Step::Sequence(inverses)
        }
        Step::Trashed(items) => {
            let mut back = Vec::new();
            for (was, in_trash) in items {
                match neutron_shell::transfer::relocate(in_trash, was) {
                    Ok(()) => back.push(was.clone()),
                    Err(neutron_shell::transfer::RelocateError::NotPermitted(_)) => blocked.push(in_trash.clone()),
                    Err(why) => failed.push(format!("{}: {why}", name(was))),
                }
            }
            Step::Created(back)
        }
    };
    Reversed { inverse: Some(inverse), failed, blocked }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relocated(n: usize) -> Record {
        Record {
            label: "Move".into(),
            step: Step::Relocated((0..n).map(|i| (format!("/a/{i}").into(), format!("/b/{i}").into())).collect()),
        }
    }

    #[test]
    fn a_new_change_clears_what_could_be_redone() {
        let mut h = History::default();
        h.record(relocated(1));
        let r = h.take_undo().unwrap();
        h.undone(r);
        assert_eq!(h.redo_label(), Some("Move"));
        h.record(relocated(2));
        assert_eq!(h.redo_label(), None);
        assert_eq!(h.undo_label(), Some("Move"));
    }

    #[test]
    fn nothing_is_recorded_for_a_change_that_touched_nothing() {
        let mut h = History::default();
        h.record(relocated(0));
        assert_eq!(h.undo_label(), None);
    }

    #[test]
    fn the_history_keeps_only_so_many() {
        let mut h = History::default();
        for _ in 0..DEPTH + 5 {
            h.record(relocated(1));
        }
        let mut count = 0;
        while h.take_undo().is_some() {
            count += 1;
        }
        assert_eq!(count, DEPTH);
    }

    #[test]
    fn a_move_reversed_comes_back_and_its_inverse_moves_it_again() {
        let dir = std::env::temp_dir().join(format!("neutron-undo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("b")).unwrap();
        std::fs::write(dir.join("b/x.txt"), "x").unwrap();
        let step = Step::Relocated(vec![(dir.join("a/x.txt"), dir.join("b/x.txt"))]);

        let done = reverse(&step);
        assert!(done.failed.is_empty(), "{:?}", done.failed);
        assert!(dir.join("a/x.txt").exists() && !dir.join("b/x.txt").exists());
        let inverse = done.inverse.unwrap();
        assert_eq!(inverse, Step::Relocated(vec![(dir.join("b/x.txt"), dir.join("a/x.txt"))]));

        let again = reverse(&inverse).inverse.unwrap();
        assert!(dir.join("b/x.txt").exists());
        assert_eq!(again, step);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_move_back_onto_a_name_now_taken_is_refused_not_forced() {
        let dir = std::env::temp_dir().join(format!("neutron-undo-taken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        std::fs::write(dir.join("a/x.txt"), "newer").unwrap();
        std::fs::write(dir.join("b/x.txt"), "moved").unwrap();
        let done = reverse(&Step::Relocated(vec![(dir.join("a/x.txt"), dir.join("b/x.txt"))]));
        assert_eq!(done.failed.len(), 1);
        assert_eq!(done.inverse, Some(Step::Relocated(Vec::new())));
        assert_eq!(std::fs::read_to_string(dir.join("a/x.txt")).unwrap(), "newer");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sequence_is_undone_last_step_first_and_redone_in_order() {
        let dir = std::env::temp_dir().join(format!("neutron-undo-seq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("stash")).unwrap();
        std::fs::create_dir_all(dir.join("to")).unwrap();
        // As a replacing move leaves things: the old file set aside, the new
        // one in its place. (`stash` stands in for the Trash.)
        std::fs::write(dir.join("stash/old.txt"), "old").unwrap();
        std::fs::write(dir.join("to/a.txt"), "new").unwrap();
        let step = Step::Sequence(vec![
            Step::Trashed(vec![(dir.join("to/a.txt"), dir.join("stash/old.txt"))]),
            Step::Relocated(vec![(dir.join("from/a.txt"), dir.join("to/a.txt"))]),
        ]);

        let done = reverse(&step);
        assert!(done.failed.is_empty(), "{:?}", done.failed);
        assert_eq!(std::fs::read_to_string(dir.join("from/a.txt")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(dir.join("to/a.txt")).unwrap(), "old", "the replaced file is back");
        match done.inverse.unwrap() {
            Step::Sequence(inverses) => {
                assert!(matches!(inverses[0], Step::Relocated(_)), "the move was undone first");
                assert!(matches!(inverses[1], Step::Created(_)), "then the put-back");
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
