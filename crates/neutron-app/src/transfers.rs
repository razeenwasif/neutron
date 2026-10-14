//! Copies and moves the app has started: the question about clashing names,
//! the job while it runs, and what is said when it ends.
//!
//! A paste or a drop goes through two steps, both off the UI thread:
//!
//! 1. [`plan`] checks the destination for names already taken. With none, the
//!    transfer is ready; with some, the user is asked first, and nothing has
//!    changed on disk while they decide.
//! 2. [`Job::start`] runs it on a thread of its own — not the shell pool, which
//!    icons and menus share and a ten-minute copy would starve — reporting into
//!    a [`Progress`] the status line reads from.

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use neutron_shell::fileops::Transfer;
use neutron_shell::transfer::{Clash, Progress, Report, Snapshot};

use crate::loader::TabId;

/// A transfer someone asked for.
#[derive(Debug, Clone)]
pub struct Request {
    /// The tab to refresh when it ends.
    pub tab: TabId,
    pub paths: Vec<PathBuf>,
    pub destination: PathBuf,
    pub how: Transfer,
}

impl Request {
    fn destination_name(&self) -> String {
        self.destination
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.destination.display().to_string())
    }
}

/// What checking the destination found.
pub enum Planned {
    /// Nothing in the way.
    Clear(Request),
    /// These names are taken; ask before going on.
    Clashes(Request, Vec<String>),
    /// Cannot be done at all — a folder into itself, a destination gone.
    Refused(String),
}

/// Checks `request`'s destination for clashes. Blocks on the disk, so call it
/// from a worker.
pub fn plan(request: Request) -> Planned {
    match neutron_shell::transfer::clashes(&request.paths, &request.destination, request.how) {
        Ok(names) if names.is_empty() => Planned::Clear(request),
        Ok(names) => Planned::Clashes(request, names),
        Err(why) => Planned::Refused(why),
    }
}

/// The question put when names clash.
pub struct Question {
    pub request: Request,
    pub names: Vec<String>,
}

impl Question {
    pub fn title(&self) -> String {
        match self.names.as_slice() {
            [one] => format!("“{one}” already exists in “{}”", self.request.destination_name()),
            names => format!("{} items already exist in “{}”", names.len(), self.request.destination_name()),
        }
    }

    pub fn body(&self) -> String {
        if self.names.len() == 1 {
            "Keep Both brings it in under a new name. Replace moves the one that is there to the Trash, \
             so it can be put back."
                .to_owned()
        } else {
            "Keep Both brings them in under new names. Replace moves the ones that are there to the Trash, \
             so they can be put back."
                .to_owned()
        }
    }
}

/// A transfer under way.
pub struct Job {
    pub id: u64,
    pub request: Request,
    progress: Arc<Progress>,
    finished: Receiver<Result<Report, String>>,
}

impl Job {
    /// Starts `request` on a thread of its own, settling clashes by `clash`.
    pub fn start(id: u64, request: Request, clash: Clash, ctx: egui::Context) -> Job {
        let progress = Arc::new(Progress::default());
        let (tx, finished): (Sender<_>, _) = crossbeam_channel::bounded(1);
        let work = request.clone();
        let shared = progress.clone();
        let spawned = std::thread::Builder::new().name("neutron-transfer".into()).spawn(move || {
            tracing::info!(count = work.paths.len(), how = ?work.how, dest = %work.destination.display(), "transfer");
            let result = neutron_shell::transfer::run(&work.paths, &work.destination, work.how, clash, &shared);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "could not start a transfer thread");
        }
        Job { id, request, progress, finished }
    }

    pub fn stop(&self) {
        self.progress.stop();
    }

    /// The outcome, once there is one.
    pub fn poll(&self) -> Option<Result<Report, String>> {
        self.finished.try_recv().ok()
    }

    /// One line for the status pill: what, how far.
    pub fn status(&self) -> String {
        describe_progress(self.request.how, &self.progress.snapshot(), self.progress.stopped())
    }

    /// How far, from 0 to 1, for a bar.
    pub fn fraction(&self) -> f32 {
        fraction(&self.progress.snapshot())
    }
}

fn fraction(now: &Snapshot) -> f32 {
    if now.bytes > 0 {
        (now.bytes_done as f64 / now.bytes as f64).clamp(0.0, 1.0) as f32
    } else if now.items > 0 {
        now.items_done as f32 / now.items as f32
    } else {
        0.0
    }
}

fn describe_progress(how: Transfer, now: &Snapshot, stopping: bool) -> String {
    let verb = if how == Transfer::Move { "Moving" } else { "Copying" };
    if stopping {
        return "Stopping…".to_owned();
    }
    if now.items == 0 {
        return format!("{verb}…");
    }
    let percent = (fraction(now) * 100.0).floor();
    if now.items == 1 {
        format!("{verb} “{}” — {percent:.0}%", now.current)
    } else {
        format!("{verb} {} of {} — {percent:.0}%", (now.items_done + 1).min(now.items), now.items)
    }
}

/// What to say when a transfer ends, and whether it is an error.
pub fn summary(request: &Request, outcome: &Result<Report, String>) -> (bool, String) {
    let past = if request.how == Transfer::Move { "Moved" } else { "Copied" };
    let verb = if request.how == Transfer::Move { "move" } else { "copy" };
    let into = request.destination_name();
    let items = |n: usize| if n == 1 { "1 item".to_owned() } else { format!("{n} items") };
    let report = match outcome {
        Ok(report) => report,
        Err(why) => return (true, format!("Could not {verb} those items: {why}")),
    };

    if report.stopped {
        return (false, format!("Stopped — {} {} to “{into}” before then", past.to_lowercase(), items(report.done)));
    }
    if !report.failed.is_empty() {
        // The first two named; the log has the rest.
        for line in &report.failed {
            tracing::warn!("transfer: {line}");
        }
        let shown: Vec<&str> = report.failed.iter().take(2).map(String::as_str).collect();
        let more = report.failed.len().saturating_sub(2);
        let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
        let total = report.done + report.failed.len();
        return (
            true,
            format!("Could not {verb} {} of {}: {}{tail}", report.failed.len(), items(total), shown.join("; ")),
        );
    }
    let mut text = format!("{past} {} to “{into}”", items(report.done));
    if report.skipped > 0 {
        text.push_str(&format!(" · {} skipped", report.skipped));
    }
    (false, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(how: Transfer) -> Request {
        Request { tab: TabId(1), paths: vec![], destination: PathBuf::from("/Users/a/Downloads"), how }
    }

    #[test]
    fn the_question_names_one_clash_and_counts_several() {
        let q = Question { request: request(Transfer::Copy), names: vec!["a.txt".into()] };
        assert_eq!(q.title(), "“a.txt” already exists in “Downloads”");
        let q = Question { request: request(Transfer::Copy), names: vec!["a".into(), "b".into()] };
        assert_eq!(q.title(), "2 items already exist in “Downloads”");
        assert!(q.body().contains("Trash"), "Replace must say where the old ones go");
    }

    #[test]
    fn progress_reads_in_items_and_bytes() {
        let now = Snapshot { bytes: 200, bytes_done: 50, items: 4, items_done: 1, current: "b".into() };
        assert_eq!(describe_progress(Transfer::Copy, &now, false), "Copying 2 of 4 — 25%");
        let one = Snapshot { bytes: 0, bytes_done: 0, items: 1, items_done: 0, current: "a.mov".into() };
        assert_eq!(describe_progress(Transfer::Move, &one, false), "Moving “a.mov” — 0%");
    }

    #[test]
    fn the_end_says_what_happened() {
        let done = Ok(Report { done: 3, skipped: 1, ..Report::default() });
        assert_eq!(summary(&request(Transfer::Copy), &done), (false, "Copied 3 items to “Downloads” · 1 skipped".into()));
        let failed = Ok(Report { done: 1, failed: vec!["a: denied".into()], ..Report::default() });
        assert_eq!(summary(&request(Transfer::Move), &failed), (true, "Could not move 1 of 2 items: a: denied".into()));
        let stopped = Ok(Report { done: 2, stopped: true, ..Report::default() });
        assert!(summary(&request(Transfer::Copy), &stopped).1.starts_with("Stopped"));
    }
}
