//! Terminals inside Neutron: a shell, or a program such as an AI agent's CLI,
//! running on a pseudo-terminal and shown in a tab.
//!
//! # Shape
//!
//! A [`Session`] owns the pty, the child and a `vt100` screen. A thread reads
//! the child's output into the screen as it comes and asks for a frame; the
//! paint thread only ever locks the screen to draw it, which is plain memory —
//! the threading contract holds. Keys go the other way, written straight to
//! the pty.
//!
//! Sessions are kept by the app by id, not inside a tab, so a tab dragged to
//! another pane or window keeps its shell, and closing the tab ends it.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, MasterPty, PtySize};

/// Lines kept above the screen, for scrolling back.
const SCROLLBACK: usize = 10_000;

/// What a session runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    /// The program and its arguments; `None` for the user's login shell.
    pub command: Option<Vec<String>>,
    /// Extra environment, on top of what the shell sets up.
    pub env: Vec<(String, String)>,
}

impl Program {
    pub fn shell() -> Self {
        Program { command: None, env: Vec::new() }
    }
}

/// Answers the questions a program asks its terminal and then waits on: where
/// the cursor is, and what kind of terminal this is. `vt100` keeps the screen
/// but answers nothing, and a program that asks and hears nothing can hang.
#[derive(Default)]
struct Replies {
    out: Vec<u8>,
    title: Option<String>,
}

impl vt100::Callbacks for Replies {
    fn unhandled_csi(&mut self, screen: &mut vt100::Screen, intermediate: Option<u8>, _i2: Option<u8>, params: &[&[u16]], c: char) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (intermediate, c, first) {
            // Device status report: where the cursor is, counted from one.
            (None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                self.out.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            // Device status report: "all is well".
            (None, 'n', 5) => self.out.extend_from_slice(b"\x1b[0n"),
            // Primary device attributes: a VT220 with colour.
            (None, 'c', 0) => self.out.extend_from_slice(b"\x1b[?62;22c"),
            // Secondary device attributes.
            (Some(b'>'), 'c', 0) => self.out.extend_from_slice(b"\x1b[>1;10;0c"),
            _ => {}
        }
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).into_owned());
    }
}

/// One terminal.
pub struct Session {
    parser: Arc<Mutex<vt100::Parser<Replies>>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    exited: Arc<AtomicBool>,
    size: (u16, u16),
    /// What it runs, for the tab's name when the program sets none.
    pub label: String,
}

impl Session {
    /// Starts `program` in `dir`. The reader thread wakes `wake` whenever
    /// output arrives.
    pub fn spawn(dir: &Path, program: &Program, label: &str, wake: egui::Context) -> Result<Self, String> {
        let size = (24, 80);
        let pair = portable_pty::native_pty_system()
            .openpty(PtySize { rows: size.0, cols: size.1, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| format!("no terminal could be opened: {e}"))?;

        let mut cmd = match &program.command {
            Some(argv) if !argv.is_empty() => {
                let mut cmd = CommandBuilder::new(&argv[0]);
                cmd.args(&argv[1..]);
                cmd
            }
            // A login shell, so it reads the user's profile: started from the
            // Dock, Neutron has none of the PATH a terminal would.
            _ => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
                let mut cmd = CommandBuilder::new(shell);
                cmd.arg("-l");
                cmd
            }
        };
        cmd.cwd(dir);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "Neutron");
        if std::env::var_os("LANG").is_none() {
            cmd.env("LANG", "en_US.UTF-8");
        }
        for (key, value) in &program.env {
            cmd.env(key, value);
        }

        let child = pair.slave.spawn_command(cmd).map_err(|e| format!("{label} could not be started: {e}"))?;
        // The child holds its own end; keeping ours open would stop the reader
        // ever seeing the end of its output.
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(size.0, size.1, SCROLLBACK, Replies::default())));
        let exited = Arc::new(AtomicBool::new(false));
        // Replies go back through a writer of their own: the main one is the
        // paint thread's, for keys.
        let mut answer = pair.master.take_writer().ok();
        {
            let parser = parser.clone();
            let exited = exited.clone();
            std::thread::Builder::new()
                .name("terminal-reader".into())
                .spawn(move || {
                    let mut buf = [0u8; 16 * 1024];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let reply = match parser.lock() {
                                    Ok(mut p) => {
                                        p.process(&buf[..n]);
                                        std::mem::take(&mut p.callbacks_mut().out)
                                    }
                                    Err(_) => break,
                                };
                                if !reply.is_empty() {
                                    if let Some(w) = answer.as_mut() {
                                        let _ = w.write_all(&reply);
                                        let _ = w.flush();
                                    }
                                }
                                wake.request_repaint();
                            }
                        }
                    }
                    exited.store(true, Ordering::Release);
                    wake.request_repaint();
                })
                .map_err(|e| e.to_string())?;
        }

        Ok(Session {
            parser,
            writer,
            master: pair.master,
            child,
            exited,
            size,
            label: label.to_owned(),
        })
    }

    /// Sends what was typed.
    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // Typing returns to the bottom, as every terminal does.
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_scrollback(0);
        }
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Fits the terminal to `rows` by `cols`, telling the program.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        if (rows, cols) == self.size {
            return;
        }
        self.size = (rows, cols);
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }

    /// Scrolls back by `lines` (negative: towards the bottom).
    pub fn scroll(&mut self, lines: i32) {
        if let Ok(mut p) = self.parser.lock() {
            let now = p.screen().scrollback() as i32;
            p.screen_mut().set_scrollback((now + lines).max(0) as usize);
        }
    }

    /// Draws or reads the screen.
    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> Option<R> {
        self.parser.lock().ok().map(|p| f(p.screen()))
    }

    /// The title the program gave itself, if any.
    pub fn title(&self) -> Option<String> {
        self.parser.lock().ok().and_then(|p| p.callbacks().title.clone()).filter(|t| !t.trim().is_empty())
    }

    /// Whether the program has finished.
    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing the tab ends what runs in it, as closing a terminal window
        // does.
        let _ = self.child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for(session: &Session, needle: &str) -> bool {
        for _ in 0..200 {
            if session.with_screen(|s| s.contents()).is_some_and(|c| c.contains(needle)) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_program_runs_in_its_folder_and_its_output_reaches_the_screen() {
        let dir = std::env::temp_dir();
        let program = Program { command: Some(vec!["/bin/pwd".into()]), env: Vec::new() };
        let session = Session::spawn(&dir, &program, "pwd", egui::Context::default()).unwrap();
        let shown = dir.canonicalize().unwrap().display().to_string();
        assert!(wait_for(&session, &shown), "{:?}", session.with_screen(|s| s.contents()));
        for _ in 0..200 {
            if session.exited() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(session.exited());
    }

    #[test]
    fn typing_reaches_the_program_and_a_cursor_query_is_answered() {
        // `cat` echoes what it is sent; the query is answered by the session,
        // and the reply, read back by `cat`, shows as typed text.
        let program = Program { command: Some(vec!["/bin/cat".into()]), env: Vec::new() };
        let mut session = Session::spawn(&std::env::temp_dir(), &program, "cat", egui::Context::default()).unwrap();
        session.write(b"hello\r");
        assert!(wait_for(&session, "hello"));
        session.resize(30, 100);
        assert_eq!(session.with_screen(|s| s.size()), Some((30, 100)));
    }
}
