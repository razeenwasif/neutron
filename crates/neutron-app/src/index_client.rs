//! Talking to the elevated indexer.
//!
//! # The elevation dance
//!
//! `neutron.exe` is unelevated and must stay that way — an elevated window
//! cannot accept drag-and-drop from an unelevated Explorer, because UIPI drops
//! the messages. But reading a volume handle needs administrator rights.
//!
//! So the index lives in `neutron-indexer.exe`, launched once with the `runas`
//! verb, which is what raises the single UAC prompt. This module owns that
//! lifecycle:
//!
//! 1. Try to connect to the pipe. A helper from a previous session may already
//!    be running, in which case there is no prompt at all.
//! 2. If not, ask the user — a UAC prompt appearing unbidden because they typed
//!    into a search box is exactly the kind of thing that trains people to
//!    click Yes without reading.
//! 3. Launch, then poll for the pipe while the helper indexes.
//!
//! Declining is a supported outcome, not an error state. Search simply reports
//! that it is unavailable, and everything else in the application carries on.
//!
//! # Threading
//!
//! One worker thread owns the connection. The UI posts queries to it and reads
//! results back; a pipe read blocks, so none of it can be on the paint thread.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use neutron_index::protocol::{
    DEFAULT_PIPE, IndexStatus, Request, Response, SearchHit, sanitize_needle,
};

/// How many results to ask for. The overlay shows a scrolling list, but nobody
/// scrolls past a few hundred — and beyond that the count is the useful signal,
/// not the rows.
const RESULT_LIMIT: usize = 500;

/// How long to keep polling for the pipe after launching the helper.
///
/// Generous because it covers the UAC prompt *and* a full index of every
/// volume, and the helper only starts serving once indexing is done.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(180);

/// What the search subsystem is currently doing, for the UI to render.
#[derive(Debug, Clone, PartialEq)]
pub enum IndexState {
    /// No helper, and the user has not been asked.
    Idle,
    /// Waiting on the UAC prompt and the first index build.
    Starting,
    Ready(IndexStatus),
    /// The user declined, or the helper could not be started. Carries a
    /// human-readable reason, which the overlay shows in place of results.
    Unavailable(String),
    /// Connected, but the indexer refused the last request — typically a helper
    /// from an older build. Search still works in whatever modes it does know.
    Rejected(String),
}

/// Identifies the question a response answers.
///
/// Both the needle *and* the scope, because switching panes changes the scope
/// while the needle stays put — and a result computed under the old scope lists
/// files from a folder the user is no longer looking at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub needle: String,
    /// Empty for the global search, which has no scope.
    pub scope: String,
}

/// A query result, tagged with the query it answers.
///
/// Tagged because responses can arrive out of order relative to typing — a slow
/// full scan for `e` can land after a fast narrow for `error` — and rendering a
/// stale answer makes the search look broken.
pub struct SearchResults {
    pub query: Query,
    pub hits: Vec<SearchHit>,
    pub total: usize,
    pub truncated: bool,
    pub elapsed_micros: u64,
}

/// Why a query did not produce results.
///
/// The distinction matters: the indexer *answering* with an error means the
/// connection is healthy and only this request failed, while a transport
/// failure means there is nothing to talk to any more. Conflating them meant a
/// single rejected request tore down search completely — which is exactly what
/// a version-skewed helper produced, since it replies "unknown variant" to a
/// request it does not know rather than dying.
enum QueryError {
    /// The indexer replied, but refused. Keep the connection.
    Rejected(String),
    /// The pipe is gone. Drop the connection.
    Transport(String),
}

enum Command {
    Connect { launch: bool },
    /// Substring across everything.
    Search(Query),
    /// Fuzzy beneath one folder.
    Find(Query),
    /// Ask the helper to exit, then forget the connection.
    StopServer,
    Shutdown,
}

enum Event {
    State(IndexState),
    Results(Box<SearchResults>),
}

pub struct IndexClient {
    commands: Sender<Command>,
    events: Receiver<Event>,
    state: IndexState,
    latest: Option<SearchResults>,
    /// What the UI most recently asked for, so a stale response can be
    /// recognised and dropped.
    pending: Query,
}

impl IndexClient {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (evt_tx, evt_rx) = crossbeam_channel::unbounded();

        std::thread::Builder::new()
            .name("neutron-index-client".into())
            .spawn(move || worker(cmd_rx, evt_tx, ctx))
            .expect("failed to spawn index client thread");

        Self {
            commands: cmd_tx,
            events: evt_rx,
            state: IndexState::Idle,
            latest: None,
            pending: Query::default(),
        }
    }

    pub fn state(&self) -> &IndexState {
        &self.state
    }

    pub fn results(&self) -> Option<&SearchResults> {
        self.latest.as_ref()
    }

    /// Connects to an already-running helper, without prompting.
    ///
    /// Called once at startup: if a helper from an earlier session is still
    /// alive, search is available immediately and the user is never asked.
    pub fn try_attach(&self) {
        let _ = self.commands.send(Command::Connect { launch: false });
    }

    /// Starts the helper, raising the UAC prompt.
    pub fn start_helper(&mut self) {
        self.state = IndexState::Starting;
        let _ = self.commands.send(Command::Connect { launch: true });
    }

    /// Substring search across every volume.
    pub fn search(&mut self, needle: &str) {
        let query = Query {
            needle: sanitize_needle(needle),
            scope: String::new(),
        };
        if query == self.pending {
            return;
        }
        self.pending = query.clone();

        if query.needle.is_empty() {
            self.latest = None;
            return;
        }
        let _ = self.commands.send(Command::Search(query));
    }

    /// Fuzzy search beneath `scope`.
    ///
    /// An empty needle is still sent, unlike the global search: with a scope
    /// there is a bounded, useful answer — everything in this folder — whereas
    /// globally it would mean every file on the machine.
    pub fn find(&mut self, needle: &str, scope: &str) {
        let query = Query {
            needle: sanitize_needle(needle),
            scope: scope.to_owned(),
        };
        if query == self.pending {
            return;
        }
        self.pending = query.clone();
        let _ = self.commands.send(Command::Find(query));
    }

    /// Asks the helper to exit. The index is lost; the next search re-prompts.
    pub fn stop_server(&mut self) {
        self.state = IndexState::Idle;
        self.latest = None;
        self.pending = Query::default();
        let _ = self.commands.send(Command::StopServer);
    }

    /// Applies anything the worker has sent. Call once per frame.
    pub fn pump(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(Event::State(s)) => self.state = s,
                Ok(Event::Results(r)) => {
                    // Drop answers to queries the user has already moved past.
                    if r.query == self.pending {
                        self.latest = Some(*r);
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }
}

impl Drop for IndexClient {
    fn drop(&mut self) {
        // Leaves the helper running deliberately — a later session reconnects
        // to it without another UAC prompt, and re-indexing every volume on
        // each launch would waste far more than the memory it holds.
        let _ = self.commands.send(Command::Shutdown);
    }
}

// --- worker ----------------------------------------------------------------

fn worker(commands: Receiver<Command>, events: Sender<Event>, ctx: egui::Context) {
    let mut connection: Option<Connection> = None;
    // Commands pulled off the channel while coalescing queries but not consumed
    // — a Shutdown swallowed there would leave the thread running forever.
    let mut pending_back: Vec<Command> = Vec::new();

    loop {
        let command = match pending_back.pop() {
            Some(c) => c,
            None => match commands.recv() {
                Ok(c) => c,
                Err(_) => return,
            },
        };

        match command {
            Command::Shutdown => return,

            Command::Connect { launch } => {
                if connection.is_some() {
                    continue;
                }
                match connect(launch) {
                    Ok(mut conn) => {
                        let status = conn.status().unwrap_or_default();
                        connection = Some(conn);
                        send(&events, &ctx, Event::State(IndexState::Ready(status)));
                    }
                    Err(e) => {
                        // Silent when merely probing at startup: reporting "no
                        // helper" before the user has asked for search would be
                        // an error message about something nobody attempted.
                        if launch {
                            send(&events, &ctx, Event::State(IndexState::Unavailable(e)));
                        }
                    }
                }
            }

            Command::StopServer => {
                if let Some(conn) = connection.as_mut() {
                    // Best effort: the helper exits on receipt, so the write
                    // may well fail on the way out. That is success, not an
                    // error worth reporting.
                    let _ = conn.request(&Request::Shutdown);
                }
                connection = None;
                send(&events, &ctx, Event::State(IndexState::Idle));
            }

            Command::Search(query) | Command::Find(query) => {
                let scoped = !query.scope.is_empty();
                let Some(conn) = connection.as_mut() else {
                    continue;
                };

                // Coalesce: while a scan is running the user keeps typing, and
                // every intermediate query is already obsolete. Only the last
                // one is worth asking about.
                let mut query = query;
                let mut scoped = scoped;
                loop {
                    match commands.try_recv() {
                        Ok(Command::Search(newer)) => {
                            scoped = false;
                            query = newer;
                        }
                        Ok(Command::Find(newer)) => {
                            scoped = true;
                            query = newer;
                        }
                        // Anything else is not a query and must not be eaten.
                        Ok(other) => {
                            pending_back.push(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }

                let outcome = if scoped {
                    conn.find(&query)
                } else {
                    conn.search(&query)
                };

                match outcome {
                    Ok(results) => send(&events, &ctx, Event::Results(Box::new(results))),

                    // The indexer is fine, this request is not. Most likely a
                    // helper left running from an older build that does not
                    // know the request. Report it and keep the connection —
                    // the other modes still work.
                    Err(QueryError::Rejected(why)) => {
                        tracing::warn!("query rejected: {why}");
                        send(&events, &ctx, Event::State(IndexState::Rejected(why)));
                    }

                    Err(QueryError::Transport(why)) => {
                        tracing::warn!("indexer connection lost: {why}");
                        connection = None;
                        send(
                            &events,
                            &ctx,
                            Event::State(IndexState::Unavailable(
                                "the indexer stopped responding".to_owned(),
                            )),
                        );
                    }
                }
            }
        }
    }
}

fn send(events: &Sender<Event>, ctx: &egui::Context, event: Event) {
    if events.send(event).is_ok() {
        // The worker's channel send does not itself wake the event loop.
        ctx.request_repaint();
    }
}

#[cfg(windows)]
fn connect(launch: bool) -> Result<Connection, String> {
    match Connection::open() {
        Ok(conn) => return Ok(conn),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            // A helper is running but will not talk to us. Distinguished from
            // "not running" because the remedy is completely different, and
            // launching a second one would only fail to claim the pipe name.
            //
            // This is what a wrong DACL looks like from the client side, and
            // reporting it as "no indexer" cost real time to diagnose once.
            return Err(
                "an indexer is running but refused the connection — it may belong to another user"
                    .to_owned(),
            );
        }
        Err(_) => {}
    }

    if !launch {
        return Err("no indexer running".to_owned());
    }

    launch_helper()?;

    // The helper indexes before it starts serving, so this covers the UAC
    // prompt and the whole first build.
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(conn) = Connection::open() {
            return Ok(conn);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err("the indexer did not start — the prompt may have been declined".to_owned())
}

#[cfg(not(windows))]
fn connect(_launch: bool) -> Result<Connection, String> {
    Err("search is Windows-only".to_owned())
}

/// Launches the helper with the `runas` verb, raising the UAC prompt.
#[cfg(windows)]
fn launch_helper() -> Result<(), String> {
    let exe = helper_path()?;
    tracing::info!(path = %exe.display(), "launching the indexer (elevated)");

    // Reuses the shell-execute path built at M3, including the deliberate
    // choice to keep the consent dialog visible — suppressing UI on `runas`
    // would suppress the prompt the verb exists to raise.
    neutron_shell::open::shell_execute_with_args(
        &exe,
        &["--serve".to_owned(), DEFAULT_PIPE.to_owned()],
        neutron_shell::open::Verb::RunAs,
        0,
    )
}

/// The helper beside this executable.
#[cfg(windows)]
fn helper_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this executable: {e}"))?;
    let helper = exe
        .parent()
        .ok_or("this executable has no directory")?
        .join("neutron-indexer.exe");

    // Checked before launching so a missing helper reports itself plainly,
    // rather than as a UAC prompt for a file that does not exist.
    if !helper.is_file() {
        return Err(format!("{} is missing", helper.display()));
    }
    Ok(helper)
}

/// A live pipe connection.
#[cfg(windows)]
pub struct Connection {
    reader: BufReader<std::fs::File>,
    writer: std::fs::File,
}

#[cfg(windows)]
impl Connection {
    fn open() -> std::io::Result<Self> {
        use std::fs::OpenOptions;
        // A named pipe client is an ordinary file open, which means the
        // standard `File` read/write plumbing works and no extra Win32 is
        // needed on this side.
        let path = format!(r"\\.\pipe\{DEFAULT_PIPE}");
        let writer = OpenOptions::new().read(true).write(true).open(&path)?;
        let reader = BufReader::new(writer.try_clone()?);
        Ok(Self { reader, writer })
    }

    fn request(&mut self, request: &Request) -> Result<Response, String> {
        let mut line = serde_json::to_string(request).map_err(|e| e.to_string())?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())?;

        let mut response = String::new();
        if self
            .reader
            .read_line(&mut response)
            .map_err(|e| e.to_string())?
            == 0
        {
            return Err("the indexer closed the connection".to_owned());
        }
        serde_json::from_str(&response).map_err(|e| e.to_string())
    }

    /// Fuzzy search beneath a folder.
    fn find(&mut self, query: &Query) -> Result<SearchResults, QueryError> {
        let response = self
            .request(&Request::Find {
                needle: query.needle.clone(),
                scope: query.scope.clone(),
                limit: RESULT_LIMIT,
            })
            .map_err(QueryError::Transport)?;
        self.results_from(response, query)
    }

    /// Unwraps a `Results` response, or turns anything else into an error.
    fn results_from(
        &self,
        response: Response,
        query: &Query,
    ) -> Result<SearchResults, QueryError> {
        match response {
            Response::Results {
                hits,
                total,
                truncated,
                elapsed_micros,
            } => Ok(SearchResults {
                query: query.clone(),
                hits,
                total,
                truncated,
                elapsed_micros,
            }),
            // A reply, not a failure — see `QueryError`.
            Response::Error(e) => Err(QueryError::Rejected(e)),
            Response::Status(_) => {
                Err(QueryError::Rejected("unexpected status response".to_owned()))
            }
        }
    }

    /// Asks how much is indexed. Used once, right after connecting.
    fn status(&mut self) -> Option<IndexStatus> {
        match self.request(&Request::Status) {
            Ok(Response::Status(s)) => Some(s),
            Ok(other) => {
                tracing::warn!("unexpected reply to Status: {other:?}");
                None
            }
            Err(e) => {
                tracing::warn!("status request failed: {e}");
                None
            }
        }
    }

    fn search(&mut self, query: &Query) -> Result<SearchResults, QueryError> {
        let response = self
            .request(&Request::Search {
                needle: query.needle.clone(),
                limit: RESULT_LIMIT,
            })
            .map_err(QueryError::Transport)?;
        self.results_from(response, query)
    }
}

#[cfg(not(windows))]
pub struct Connection;

#[cfg(not(windows))]
impl Connection {
    fn status(&mut self) -> Option<IndexStatus> {
        None
    }
    fn request(&mut self, _request: &Request) -> Result<Response, String> {
        Err("search is Windows-only".to_owned())
    }
    fn search(&mut self, _query: &Query) -> Result<SearchResults, QueryError> {
        Err(QueryError::Transport("search is Windows-only".to_owned()))
    }
    fn find(&mut self, _query: &Query) -> Result<SearchResults, QueryError> {
        Err(QueryError::Transport("search is Windows-only".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_response_is_discarded() {
        // A slow full scan for "e" can land after a fast narrow for "error".
        // Rendering it would replace the right answer with an older one, which
        // looks like the search randomly forgetting what was typed.
        let ctx = egui::Context::default();
        let mut client = IndexClient::spawn(ctx);
        client.pending = Query {
            needle: "error".to_owned(),
            scope: String::new(),
        };

        let stale = SearchResults {
            query: Query {
                needle: "e".to_owned(),
                scope: String::new(),
            },
            hits: Vec::new(),
            total: 99,
            truncated: false,
            elapsed_micros: 0,
        };
        // Simulates what `pump` does on receipt.
        if stale.query == client.pending {
            client.latest = Some(stale);
        }
        assert!(client.results().is_none(), "a stale result was displayed");
    }

    #[test]
    fn clearing_the_needle_clears_the_results() {
        // Emptying the box must drop the results, or the overlay keeps showing
        // hits for a query that is no longer on screen.
        let ctx = egui::Context::default();
        let mut client = IndexClient::spawn(ctx);

        // Through the real sequence: a query, its answer, then a clear. Setting
        // `latest` without a matching `pending` is a state the client cannot
        // reach, since `pump` only stores results whose needle is current.
        client.search("report");
        client.latest = Some(SearchResults {
            query: client.pending.clone(),
            hits: Vec::new(),
            total: 1,
            truncated: false,
            elapsed_micros: 0,
        });
        assert!(client.results().is_some());

        client.search("");
        assert!(client.results().is_none());
    }

    #[test]
    fn repeating_a_query_does_not_resend_it() {
        // Every frame asks for the current needle; without this the worker gets
        // one request per frame at 60fps for a query already answered.
        let ctx = egui::Context::default();
        let mut client = IndexClient::spawn(ctx);

        client.search("report");
        let first = client.commands.len();
        client.search("report");
        assert_eq!(client.commands.len(), first, "the query was sent twice");
    }
}
