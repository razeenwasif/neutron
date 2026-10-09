//! Running one plugin command.
//!
//! Spawn, write the request, read what comes back until the plugin exits or the
//! deadline passes, and hand the result to the caller as plain data. Blocking
//! throughout — this is called on a worker, never on the UI thread.
//!
//! # Why three threads for one child process
//!
//! Each of the child's three pipes can block this side of it, and a plugin that
//! blocks the host is the failure the whole design exists to prevent.
//!
//! - **stdin** blocks the writer if the plugin never reads it and the request
//!   is larger than the pipe buffer, which a selection of a few thousand files
//!   is.
//! - **stdout** blocks the reader while the plugin thinks, which is exactly when
//!   the deadline has to be able to fire.
//! - **stderr** blocks the *plugin* if nobody drains it — a plugin that logs
//!   heavily and is never read deadlocks against a full pipe, and looks from
//!   here like a plugin that hung.
//!
//! So each pipe gets a thread, and the invoking thread does nothing but wait on
//! a channel with a deadline. When that deadline passes, killing the child
//! closes all three pipes and every thread ends on its own.
//!
//! # Limits, and why each one exists
//!
//! A plugin is not assumed hostile, but it is assumed to be capable of a bug,
//! and every one of these has a plausible bug behind it: a loop printing status
//! lines forever, a line with no newline growing without end, a crash dump on
//! stderr, a selection of every file on the volume.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::discover::Installed;
use crate::manifest::CommandDef;
use crate::protocol::{Request, Response};
use crate::sanitize;

/// Most responses acted on. A plugin printing in a loop stops being listened to
/// rather than filling memory.
pub const MAX_RESPONSES: usize = 64;
/// Longest single response line. Beyond this the plugin is not speaking the
/// protocol.
pub const MAX_LINE: usize = 64 * 1024;
/// Most paths sent in one request. A plugin command over a hundred thousand
/// files is a mistake, and one worth naming rather than serialising.
pub const MAX_SELECTION: usize = 1024;
/// Most `open` requests honoured from one run, so a bug cannot launch a hundred
/// windows.
pub const MAX_OPENS: usize = 8;
/// How much of a plugin's stderr is held while it runs: the most recent this
/// many bytes. Far more than the status bar can show, so the shown tail is
/// cut from flattened text rather than from raw bytes that may be mostly
/// whitespace and escape sequences.
const STDERR_KEEP: usize = 8 * 1024;
/// How often the wait wakes to check for a Stop.
const POLL: Duration = Duration::from_millis(50);

/// Everything one run of a plugin asked for.
///
/// Absent rather than defaulted, so the caller can tell "the plugin said
/// nothing" from "the plugin said nothing was wrong".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Last status line the plugin printed.
    pub status: Option<String>,
    /// Folder to show. Last one wins.
    pub navigate: Option<PathBuf>,
    /// Paths to hand to the shell.
    pub open: Vec<PathBuf>,
    /// Whether the folder needs re-listing.
    pub refresh: bool,
    /// Why the run failed — the plugin's own message, or the host's account of
    /// how it went wrong.
    pub error: Option<String>,
}

impl Outcome {
    fn failed(message: impl Into<String>) -> Outcome {
        Outcome { error: Some(sanitize::status(&message.into())), ..Outcome::default() }
    }

    /// Whether anything at all came back. A run that produced no output and no
    /// error still needs *something* on screen, or choosing the command looks
    /// like it did nothing.
    pub fn is_silent(&self) -> bool {
        self.status.is_none()
            && self.navigate.is_none()
            && self.open.is_empty()
            && !self.refresh
            && self.error.is_none()
    }
}

/// Runs `command` from `plugin` against the current folder and selection.
///
/// Never panics and never returns an error type: every way this can fail is
/// something to tell the user, so it all comes back in [`Outcome::error`].
pub fn invoke(
    plugin: &Installed,
    command: &CommandDef,
    folder: &Path,
    selection: &[PathBuf],
) -> Outcome {
    invoke_with_cancel(plugin, command, folder, selection, &AtomicBool::new(false))
}

/// [`invoke`], but stoppable.
///
/// Setting `cancel` kills the plugin at the next poll. The deadline already
/// bounds every run, but a command allowed two minutes needs a way out that is
/// not waiting two minutes — the same reason the archive jobs have a Stop.
pub fn invoke_with_cancel(
    plugin: &Installed,
    command: &CommandDef,
    folder: &Path,
    selection: &[PathBuf],
    cancel: &AtomicBool,
) -> Outcome {
    if selection.len() > MAX_SELECTION {
        return Outcome::failed(format!(
            "{} files selected; a plugin command takes at most {MAX_SELECTION}",
            selection.len()
        ));
    }

    // Paths go over the wire as UTF-8. A Windows path that is not valid UTF-8
    // holds an unpaired surrogate, which cannot be encoded without changing it
    // — and handing a plugin a *corrupted* path is worse than refusing, since
    // the plugin would then write to the wrong file.
    let Some(folder_text) = folder.to_str().map(str::to_owned) else {
        return Outcome::failed(format!("{} is not a valid Unicode path", folder.display()));
    };
    let mut paths = Vec::with_capacity(selection.len());
    for path in selection {
        match path.to_str() {
            Some(text) => paths.push(text.to_owned()),
            None => {
                return Outcome::failed(format!(
                    "{} is not a valid Unicode path",
                    path.display()
                ));
            }
        }
    }

    let request = Request::new(&command.id, folder_text, paths);
    let mut line = match serde_json::to_string(&request) {
        Ok(line) => line,
        Err(e) => return Outcome::failed(format!("request could not be encoded: {e}")),
    };
    line.push('\n');

    let program = plugin.program();
    let mut child = match spawn(&program, &plugin.manifest.args, &plugin.dir) {
        Ok(child) => child,
        Err(e) => {
            return Outcome::failed(format!("{} could not be started: {e}", plugin.manifest.run));
        }
    };

    let deadline = Instant::now() + command.timeout();
    let (lines_tx, lines_rx) = std::sync::mpsc::channel::<Result<String, LineError>>();

    // stdin: its own thread, because `write_all` on a full pipe blocks until
    // the plugin reads, and a plugin that never reads must not hold this
    // invocation. A broken pipe here is not an error — a plugin that answers
    // without reading its request is unusual but legitimate.
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(line.as_bytes());
            let _ = stdin.flush();
        });
    }

    if let Some(stdout) = child.stdout.take() {
        let tx = lines_tx.clone();
        std::thread::spawn(move || read_lines(stdout, &tx));
    }
    drop(lines_tx);

    // stderr is drained unconditionally, even when nothing goes wrong: an
    // undrained pipe eventually blocks the plugin writing to it.
    let stderr = child
        .stderr
        .take()
        .map(|stderr| std::thread::spawn(move || drain_tail(stderr, STDERR_KEEP)));

    let mut outcome = Outcome::default();
    let mut seen = 0usize;
    let mut unparsed = 0usize;

    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            outcome.error = Some(format!("{} was stopped", plugin.manifest.name));
            return outcome;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = child.kill();
            let _ = child.wait();
            outcome.error = Some(sanitize::status(&format!(
                "{} did not answer within {:.0}s",
                plugin.manifest.name,
                command.timeout().as_secs_f32()
            )));
            return outcome;
        }

        // Never wait the whole remaining time in one go, or a Stop would not be
        // noticed until the deadline it exists to pre-empt.
        match lines_rx.recv_timeout(remaining.min(POLL)) {
            Ok(Ok(text)) => {
                seen += 1;
                if seen > MAX_RESPONSES {
                    tracing::warn!(
                        plugin = %plugin.id(),
                        "more than {MAX_RESPONSES} responses; ignoring the rest"
                    );
                    let _ = child.kill();
                    break;
                }
                match serde_json::from_str::<Response>(&text) {
                    Ok(response) => apply(&mut outcome, response),
                    Err(e) => {
                        // Not fatal on its own: a plugin printing a stray line
                        // is a nuisance, not a failure. But if it is *all* the
                        // plugin printed, the user is owed an explanation.
                        unparsed += 1;
                        tracing::warn!(plugin = %plugin.id(), "unreadable response: {e}");
                    }
                }
            }
            // The reader hit a line it could not deliver. Stop listening.
            Ok(Err(LineError::TooLong)) => {
                // Not a stray line but a plugin that has stopped speaking the
                // protocol, and one that may still be pouring out output
                // nobody will read. Killed rather than waited on: the reader
                // has already let go of stdout, so its next write fails
                // anyway, and a plugin ignoring that would otherwise run to
                // the deadline.
                tracing::warn!(plugin = %plugin.id(), "a response line exceeded {MAX_LINE} bytes");
                let _ = child.kill();
                let _ = child.wait();
                if outcome.error.is_none() {
                    outcome.error = Some(sanitize::status(&format!(
                        "{} wrote a response line longer than {} KB",
                        plugin.manifest.name,
                        MAX_LINE / 1024
                    )));
                }
                return outcome;
            }
            // Could not be read at all. The exit status is still worth
            // reporting.
            Ok(Err(LineError::Read(e))) => {
                tracing::warn!(plugin = %plugin.id(), "{e}");
                unparsed += 1;
                break;
            }
            // Disconnected: stdout closed, so the plugin is done writing.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
        }
    }

    let status = wait_until(&mut child, deadline);
    let stderr = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
    if !stderr.trim().is_empty() {
        tracing::debug!(plugin = %plugin.id(), "stderr: {}", stderr.trim());
    }

    // The plugin's own error message is better than anything inferred, so it is
    // never overwritten.
    if outcome.error.is_none() {
        outcome.error = failure(plugin, status, unparsed, &stderr);
    }

    outcome
}

/// Builds the host's account of a failed run, or `None` if it went fine.
fn failure(
    plugin: &Installed,
    status: Option<std::process::ExitStatus>,
    unparsed: usize,
    stderr: &str,
) -> Option<String> {
    let tail = || {
        // The last words, not the first: a traceback's useful end is at the
        // bottom, and a crash dump's beginning is boilerplate. Flattened
        // first and then cut from the *front* — `sanitize::status` on its own
        // keeps the start and drops the end, which turned "the end of stderr"
        // into whatever happened to come 8 KB before it.
        // Capped at its own length, which no text in characters exceeds, so
        // this flattens without truncating.
        let flat = sanitize::text(stderr, stderr.len());
        let count = flat.chars().count();
        if count <= sanitize::MAX_STATUS {
            return flat;
        }
        // One character fewer, for the ellipsis that says it was cut.
        let rest: String = flat.chars().skip(count - (sanitize::MAX_STATUS - 1)).collect();
        format!("…{}", rest.trim_start())
    };

    match status {
        Some(status) if status.success() => {
            if unparsed > 0 {
                Some(format!("{} printed something this build cannot read", plugin.manifest.name))
            } else {
                None
            }
        }
        Some(status) => {
            let code = status
                .code()
                .map(|c| format!("exit code {c}"))
                .unwrap_or_else(|| "was terminated".to_owned());
            let tail = tail();
            Some(if tail.is_empty() {
                format!("{} failed with {code}", plugin.manifest.name)
            } else {
                format!("{}: {tail}", plugin.manifest.name)
            })
        }
        // Killed on the deadline, or the wait itself timed out.
        None => Some(format!("{} had to be stopped", plugin.manifest.name)),
    }
}

/// Folds one response into the outcome.
fn apply(outcome: &mut Outcome, response: Response) {
    match response {
        Response::Status { text } => {
            let text = sanitize::status(&text);
            if !text.is_empty() {
                outcome.status = Some(text);
            }
        }
        Response::Navigate { path } => {
            if let Some(path) = usable_path(&path) {
                outcome.navigate = Some(path);
            }
        }
        Response::Open { path } => {
            if outcome.open.len() < MAX_OPENS {
                if let Some(path) = usable_path(&path) {
                    outcome.open.push(path);
                }
            }
        }
        Response::Refresh => outcome.refresh = true,
        Response::Error { message } => {
            let message = sanitize::status(&message);
            outcome.error = Some(if message.is_empty() {
                "the plugin reported an error with no message".to_owned()
            } else {
                message
            });
        }
    }
}

/// Checks a path a plugin asked the host to act on.
///
/// Only two rules, because a plugin can already reach anything the user can and
/// pretending otherwise would be theatre. What is refused is what would
/// misbehave rather than what is forbidden: an empty path, and a path carrying
/// characters that cannot appear in a real one.
fn usable_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\0') || trimmed.chars().any(|c| c.is_control()) {
        tracing::warn!("plugin asked for an unusable path: {raw:?}");
        return None;
    }
    Some(PathBuf::from(trimmed))
}

/// Waits for the child, but not past the deadline.
///
/// `None` means it had to be killed. Polling rather than a blocking wait
/// because `std` has no wait-with-timeout, and this only runs after the plugin
/// has already closed stdout — so in practice the first poll succeeds.
fn wait_until(child: &mut Child, deadline: Instant) -> Option<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return None,
        }
    }
}

/// Why the stdout reader stopped short of the end.
#[derive(Debug)]
enum LineError {
    /// A line grew past [`MAX_LINE`] without a newline.
    TooLong,
    /// The pipe itself failed.
    Read(String),
}

/// Reads newline-delimited lines, refusing one that grows past [`MAX_LINE`].
///
/// The cap is enforced *while* reading, not checked afterwards: each line is
/// read through a `take` of one byte more than the limit, so a plugin that
/// writes gigabytes without a newline costs this side at most `MAX_LINE + 1`
/// bytes before it is refused. `read_until` on its own would buffer the lot
/// and only then notice, by which point the memory is already gone.
fn read_lines(stdout: impl Read, tx: &std::sync::mpsc::Sender<Result<String, LineError>>) {
    let mut reader = BufReader::new(stdout);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match (&mut reader).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buffer) {
            Ok(0) => return,
            Ok(_) => {
                if buffer.len() > MAX_LINE {
                    let _ = tx.send(Err(LineError::TooLong));
                    return;
                }
                let text = String::from_utf8_lossy(&buffer).trim().to_owned();
                if text.is_empty() {
                    continue;
                }
                if tx.send(Ok(text)).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = tx.send(Err(LineError::Read(format!("could not be read: {e}"))));
                return;
            }
        }
    }
}

/// Reads `stderr` until the plugin closes it, keeping only the last `keep`
/// bytes.
///
/// It has to read to the end. Stopping after the first few kilobytes — which
/// an earlier version did — dropped the pipe while the plugin was still
/// writing to it, so a chatty plugin died of `SIGPIPE` (or saw `EPIPE`) for
/// the crime of logging, and the "tail" shown in the error was really the
/// head. The last bytes are the ones worth keeping: a traceback's useful end
/// is at the bottom.
fn drain_tail(mut stderr: impl Read, keep: usize) -> String {
    let mut tail = std::collections::VecDeque::with_capacity(keep);
    let mut chunk = [0u8; 4096];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &chunk[..n];
                // Only the last `keep` of this chunk can survive anyway.
                let chunk = &chunk[chunk.len().saturating_sub(keep)..];
                let overflow = (tail.len() + chunk.len()).saturating_sub(keep);
                tail.drain(..overflow);
                tail.extend(chunk);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(tail.make_contiguous()).into_owned()
}

/// Starts the plugin process with all three pipes captured.
fn spawn(program: &Path, args: &[String], dir: &Path) -> std::io::Result<Child> {
    let mut command = Command::new(program);
    command
        .args(args)
        // The plugin's own folder, so its relative paths mean somewhere
        // predictable — and in particular not the folder being browsed, where a
        // plugin writing a temporary file would drop it among the user's files.
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Without this, a console-subsystem plugin flashes a black window over the
    // file manager every time a command is run — which is most of what makes
    // shell-extension plugins feel cheap.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_is_read_to_the_end_and_only_its_tail_is_kept() {
        let mut input = vec![b'x'; 100_000];
        input.extend_from_slice(b"the real reason");
        let tail = drain_tail(&input[..], 64);
        assert_eq!(tail.len(), 64);
        assert!(tail.ends_with("the real reason"), "{tail}");
    }

    #[test]
    fn short_stderr_is_kept_whole() {
        assert_eq!(drain_tail(&b"oops"[..], 64), "oops");
    }

    #[test]
    fn a_line_longer_than_the_cap_is_refused_without_reading_past_it() {
        // An endless source: if the reader buffered until a newline, this
        // would never return.
        let endless = std::io::repeat(b'a');
        let (tx, rx) = std::sync::mpsc::channel();
        read_lines(endless, &tx);
        assert!(matches!(rx.recv(), Ok(Err(LineError::TooLong))));
    }

    #[test]
    fn a_line_exactly_at_the_cap_is_still_delivered() {
        let mut input = vec![b'a'; MAX_LINE - 1];
        input.push(b'\n');
        input.extend_from_slice(b"next\n");
        let (tx, rx) = std::sync::mpsc::channel();
        read_lines(&input[..], &tx);
        drop(tx);
        let got: Vec<_> = rx.iter().collect();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(matches!(&got[1], Ok(line) if line == "next"));
    }
}
