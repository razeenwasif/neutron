//! Running archive work in the background, with progress and a way to stop.
//!
//! # Why this is not an `IFileOperation`
//!
//! Copying and deleting go through the shell, which supplies its own progress
//! dialog. Nothing supplies one for a zip we are writing ourselves, so this
//! carries progress back to the status bar and offers a cancel — which is the
//! entire reason for extracting archives natively rather than leaving it to
//! whatever the user has installed.
//!
//! # One at a time
//!
//! A second request while one is running is refused rather than queued.
//! Archive work is heavy and disk-bound; running two at once makes both slower
//! and turns one progress line into a list. Refusing says so plainly.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Receiver, Sender};
use neutron_archive::{Continue, Format, Progress, Summary};

/// What a running job is doing, for the status bar.
#[derive(Debug, Clone)]
pub struct JobState {
    /// Shown to the user: "Extracting photos.zip", "Compressing 12 items".
    pub label: String,
    pub progress: Progress,
}

/// How a job ended.
#[derive(Debug)]
pub enum Finished {
    Done { label: String, summary: Summary },
    Failed { label: String, error: String },
}

enum Message {
    Progress(Progress),
    Finished(Finished),
}

/// A background archive job.
pub struct ArchiveJob {
    pub state: JobState,
    cancel: Arc<AtomicBool>,
    messages: Receiver<Message>,
}

impl ArchiveJob {
    /// Asks the job to stop at the next chunk boundary.
    ///
    /// What has already been written stays: deleting a half-extracted folder or
    /// a partial archive is a destructive act nobody asked for, and the user
    /// can see what is there and decide.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Applies whatever the worker has sent. Returns the outcome once it ends.
    pub fn poll(&mut self) -> Option<Finished> {
        let mut finished = None;
        while let Ok(message) = self.messages.try_recv() {
            match message {
                Message::Progress(p) => self.state.progress = p,
                Message::Finished(f) => finished = Some(f),
            }
        }
        finished
    }
}

/// Starts extracting `archive` into `destination`.
pub fn extract(
    archive: PathBuf,
    destination: PathBuf,
    format: Format,
    ctx: egui::Context,
) -> ArchiveJob {
    let label = format!(
        "Extracting {}",
        archive.file_name().unwrap_or_default().to_string_lossy()
    );
    spawn(label.clone(), ctx, move |report| {
        neutron_archive::extract::extract(&archive, &destination, format, report)
    })
}

/// Starts writing `sources` into a new zip at `output`.
pub fn compress(
    sources: Vec<PathBuf>,
    base: PathBuf,
    output: PathBuf,
    ctx: egui::Context,
) -> ArchiveJob {
    let label = match sources.len() {
        1 => format!(
            "Compressing {}",
            sources[0].file_name().unwrap_or_default().to_string_lossy()
        ),
        n => format!("Compressing {n} items"),
    };
    spawn(label.clone(), ctx, move |report| {
        neutron_archive::create::zip(&sources, &base, &output, report)
    })
}

fn spawn<F>(label: String, ctx: egui::Context, work: F) -> ArchiveJob
where
    F: FnOnce(&mut dyn FnMut(Progress) -> Continue) -> Result<Summary, neutron_archive::ArchiveError>
        + Send
        + 'static,
{
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));

    let worker_cancel = Arc::clone(&cancel);
    let worker_label = label.clone();
    std::thread::Builder::new()
        .name("neutron-archive".into())
        .spawn(move || {
            let outcome = work(&mut |progress| {
                report(&tx, &ctx, progress);
                if worker_cancel.load(Ordering::Relaxed) {
                    Continue::Stop
                } else {
                    Continue::Yes
                }
            });

            let finished = match outcome {
                Ok(summary) => Finished::Done {
                    label: worker_label,
                    summary,
                },
                Err(e) => Finished::Failed {
                    label: worker_label,
                    error: e.to_string(),
                },
            };
            let _ = tx.send(Message::Finished(finished));
            ctx.request_repaint();
        })
        .expect("failed to spawn archive thread");

    ArchiveJob {
        state: JobState {
            label,
            progress: Progress::default(),
        },
        cancel,
        messages: rx,
    }
}

/// Sends progress, and wakes the UI at a rate a human can read.
///
/// The callback fires once per 64 KB chunk, which on a fast disk is thousands
/// of times a second. Repainting for each would cost more than the extraction.
fn report(tx: &Sender<Message>, ctx: &egui::Context, progress: Progress) {
    use std::sync::atomic::AtomicU64;
    use std::time::{SystemTime, UNIX_EPOCH};

    // Last wake, as milliseconds since the epoch. A plain atomic rather than a
    // captured `Instant` because this is called from a `FnMut` that has to stay
    // small enough to pass as `&mut dyn`.
    static LAST_WAKE_MS: AtomicU64 = AtomicU64::new(0);

    let _ = tx.send(Message::Progress(progress));

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let last = LAST_WAKE_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= 50 {
        LAST_WAKE_MS.store(now, Ordering::Relaxed);
        ctx.request_repaint();
    }
}

/// A short account of what a finished job did, for the status bar.
pub fn describe(summary: &Summary) -> String {
    let mut parts = Vec::new();
    parts.push(match summary.files {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    });
    if summary.cancelled {
        parts.push("stopped early".to_owned());
    }
    if !summary.refused.is_empty() {
        parts.push(match summary.refused.len() {
            1 => "1 entry skipped".to_owned(),
            n => format!("{n} entries skipped"),
        });
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(files: u64, refused: usize, cancelled: bool) -> Summary {
        Summary {
            files,
            bytes: 0,
            refused: (0..refused).map(|i| format!("bad {i}")).collect(),
            cancelled,
        }
    }

    #[test]
    fn a_plain_result_reports_the_count() {
        assert_eq!(describe(&summary(7, 0, false)), "7 files");
        assert_eq!(describe(&summary(1, 0, false)), "1 file");
    }

    #[test]
    fn skipped_entries_are_never_silent() {
        // An extraction that quietly drops files is worse than one that says
        // how many it dropped — the user cannot see what is missing.
        assert_eq!(describe(&summary(3, 2, false)), "3 files, 2 entries skipped");
    }

    #[test]
    fn cancelling_is_reported_as_well_as_the_count() {
        assert_eq!(describe(&summary(3, 0, true)), "3 files, stopped early");
    }
}
