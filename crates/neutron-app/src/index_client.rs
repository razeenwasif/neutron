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

#[cfg(windows)]
use std::io::{BufRead, BufReader, Write};
#[cfg(any(windows, target_os = "macos"))]
use std::path::PathBuf;
#[cfg(windows)]
use std::time::Duration;
#[cfg(any(windows, target_os = "macos"))]
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use neutron_index::protocol::{
    IndexStatus, Request, Response, SearchHit, sanitize_needle,
};

#[cfg(windows)]
use neutron_index::protocol::DEFAULT_PIPE;

/// How many results to ask for. The overlay shows a scrolling list, but nobody
/// scrolls past a few hundred — and beyond that the count is the useful signal,
/// not the rows.
#[cfg(any(windows, target_os = "macos"))]
const RESULT_LIMIT: usize = 500;

/// How long to give an unelevated helper to load its cache before deciding it
/// has nothing and asking for elevation.
///
/// Loading three million records off an SSD is well under a second; this is
/// generous enough to cover a cold file cache without making the fall back to
/// a prompt feel like a hang.
#[cfg(windows)]
const CACHE_LOAD_TIMEOUT: Duration = Duration::from_secs(8);

/// How long to keep polling for the pipe after launching the helper.
///
/// Generous because it covers the UAC prompt *and* a full index of every
/// volume, and the helper only starts serving once indexing is done.
#[cfg(windows)]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(180);

/// How old a cached index is, in words.
///
/// Rounded hard and deliberately vague past a day. The user is deciding whether
/// to spend a UAC prompt, and "6 days old" answers that as well as a timestamp
/// would while taking a quarter of the space.
pub fn describe_age(seconds: Option<u64>) -> String {
    let Some(secs) = seconds else {
        return "from an earlier session".to_owned();
    };
    match secs {
        s if s < 90 * 60 => "from earlier today".to_owned(),
        s if s < 36 * 3600 => format!("{} hours old", s / 3600),
        s if s < 14 * 86_400 => format!("{} days old", s / 86_400),
        s => format!("{} weeks old", s / (7 * 86_400)),
    }
}

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
    Connect { launch: Launch },
    /// Substring across everything.
    Search(Query),
    /// Fuzzy beneath one folder.
    Find(Query),
    /// Ask the helper to exit, then forget the connection.
    StopServer,
    Shutdown,
}

/// How far a connect attempt may go.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Launch {
    /// Connect if a helper is already there; otherwise give up quietly. Used at
    /// startup, where a prompt nobody asked for would be an ambush.
    Never,
    /// Start a helper if needed, unelevated first so a cached index costs no
    /// prompt at all.
    IfNeeded,
    /// Go straight to the elevated helper. This is the user explicitly asking
    /// for a fresh index, which is the one case where the prompt is the point.
    Elevated,
}

enum Event {
    State(IndexState),
    Results(Box<SearchResults>),
    /// The indexer refused one query. About that query only — see
    /// [`IndexClient::rejection`].
    Rejected { query: Query, why: String },
}

pub struct IndexClient {
    commands: Sender<Command>,
    events: Receiver<Event>,
    state: IndexState,
    latest: Option<SearchResults>,
    /// Why the indexer refused a query, and which one.
    ///
    /// Kept per query rather than as a state. A refusal used to replace
    /// `Ready`, nothing ever put `Ready` back, and no query is sent in any
    /// other state — so one refused search, which on the Mac is as ordinary as
    /// Ctrl+P in a folder that has since been deleted, turned search off for
    /// the rest of the session.
    rejected: Option<(Query, String)>,
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
            rejected: None,
            pending: Query::default(),
        }
    }

    pub fn state(&self) -> &IndexState {
        &self.state
    }

    /// The answer to the query being asked now, once it has arrived.
    ///
    /// Never the answer to an earlier one: until the new results land, a stale
    /// list would sit under the new query — global hits in the folder finder,
    /// say — and Enter would open a row that does not match what was typed.
    pub fn results(&self) -> Option<&SearchResults> {
        self.latest.as_ref().filter(|r| r.query == self.pending)
    }

    /// Why the query being asked now was refused, if it was.
    pub fn rejection(&self) -> Option<&str> {
        self.rejected
            .as_ref()
            .filter(|(query, _)| *query == self.pending)
            .map(|(_, why)| why.as_str())
    }

    /// Connects to an already-running helper, without prompting.
    ///
    /// Called once at startup: if a helper from an earlier session is still
    /// alive, search is available immediately and the user is never asked.
    pub fn try_attach(&self) {
        let _ = self.commands.send(Command::Connect { launch: Launch::Never });
    }

    /// Starts the helper, preferring a cached index over a UAC prompt.
    pub fn start_helper(&mut self) {
        self.state = IndexState::Starting;
        self.pending = Query::default();
        let _ = self.commands.send(Command::Connect { launch: Launch::IfNeeded });
    }

    /// Rebuilds the index from the volumes, which needs administrator rights.
    ///
    /// Stops whatever is serving first: a helper running on last session's
    /// cache holds the pipe name, and the fresh one cannot claim it until that
    /// one is gone.
    pub fn refresh_index(&mut self) {
        self.state = IndexState::Starting;
        self.pending = Query::default();
        self.latest = None;
        let _ = self.commands.send(Command::StopServer);
        let _ = self.commands.send(Command::Connect { launch: Launch::Elevated });
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
                Ok(Event::Rejected { query, why }) => {
                    if query == self.pending {
                        self.rejected = Some((query, why));
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
                        if launch != Launch::Never {
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
                        send(&events, &ctx, Event::Rejected { query, why });
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
fn connect(launch: Launch) -> Result<Connection, String> {
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

    if launch == Launch::Never {
        return Err("no indexer running".to_owned());
    }

    // Unelevated first, and this is the whole point of the cache.
    //
    // A helper with no rights can still load the index the last elevated run
    // wrote and answer from it. That takes no prompt and about a second, so the
    // common case — every launch after the first — asks the user for nothing.
    // Only when there is no cache to serve does the elevated attempt happen,
    // and then the prompt is buying something the user can see the need for.
    if launch == Launch::IfNeeded && launch_helper(Elevation::AsInvoker).is_ok() {
        if let Some(conn) = await_pipe(CACHE_LOAD_TIMEOUT) {
            return Ok(conn);
        }
        tracing::info!("the unelevated helper had nothing to serve; asking for elevation");
    }

    launch_helper(Elevation::Prompt)?;

    // The helper indexes before it starts serving, so this covers the UAC
    // prompt and the whole first build.
    await_pipe(CONNECT_TIMEOUT)
        .ok_or_else(|| "the indexer did not start — the prompt may have been declined".to_owned())
}

/// Polls for the pipe until `timeout`.
#[cfg(windows)]
fn await_pipe(timeout: Duration) -> Option<Connection> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(conn) = Connection::open() {
            return Some(conn);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

#[cfg(not(any(windows, target_os = "macos")))]
fn connect(_launch: Launch) -> Result<Connection, String> {
    Err("search is Windows-only".to_owned())
}

/// Whether to ask for administrator rights when starting the helper.
#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Elevation {
    /// No prompt. The helper can serve a cached index but cannot build one.
    AsInvoker,
    /// Raises the UAC prompt, which is what lets it read the volumes.
    Prompt,
}

/// Launches the helper.
#[cfg(windows)]
fn launch_helper(elevation: Elevation) -> Result<(), String> {
    let exe = helper_path()?;
    tracing::info!(path = %exe.display(), elevated = elevation == Elevation::Prompt, "launching the indexer");

    // Reuses the shell-execute path built at M3, including the deliberate
    // choice to keep the consent dialog visible — suppressing UI on `runas`
    // would suppress the prompt the verb exists to raise.
    neutron_shell::open::shell_execute_with_args(
        &exe,
        &["--serve".to_owned(), DEFAULT_PIPE.to_owned()],
        match elevation {
            Elevation::AsInvoker => neutron_shell::open::Verb::Open,
            Elevation::Prompt => neutron_shell::open::Verb::RunAs,
        },
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

#[cfg(not(any(windows, target_os = "macos")))]
pub struct Connection;

#[cfg(not(any(windows, target_os = "macos")))]
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
mod age_tests {
    use super::describe_age;

    #[test]
    fn an_unknown_age_says_so_rather_than_guessing() {
        assert_eq!(describe_age(None), "from an earlier session");
    }

    #[test]
    fn a_fresh_cache_reads_as_today() {
        assert_eq!(describe_age(Some(60)), "from earlier today");
        assert_eq!(describe_age(Some(80 * 60)), "from earlier today");
    }

    #[test]
    fn hours_are_reported_up_to_a_day_and_a_half() {
        assert_eq!(describe_age(Some(5 * 3600)), "5 hours old");
        assert_eq!(describe_age(Some(30 * 3600)), "30 hours old");
    }

    #[test]
    fn days_take_over_from_hours() {
        assert_eq!(describe_age(Some(3 * 86_400)), "3 days old");
    }

    #[test]
    fn a_long_neglected_index_is_reported_in_weeks() {
        // The case the note exists for: search has been quietly missing
        // everything created in the last month.
        assert_eq!(describe_age(Some(30 * 86_400)), "4 weeks old");
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
        // Counted on a channel nothing else reads. The worker drains the real
        // one as it goes, so its length could drop between the two looks and
        // pass — or fail — for reasons that have nothing to do with this.
        let (sent, received) = crossbeam_channel::unbounded();
        client.commands = sent;

        client.search("report");
        client.search("report");
        assert_eq!(received.len(), 1, "the query was sent twice");
    }
}

#[cfg(target_os = "macos")]
fn connect(_launch: Launch) -> Result<Connection, String> {
    Ok(Connection::default())
}

/// How long a walk of a folder is reused for Ctrl+P before it is taken again.
///
/// Long enough that typing a query is one walk rather than one per keystroke —
/// which is what it was, and on a large tree that was three seconds a letter —
/// and short enough that a file made a minute ago is found.
#[cfg(target_os = "macos")]
const WALK_TTL: std::time::Duration = std::time::Duration::from_secs(20);

/// Paths Spotlight is read for before the rest are only counted. Ranking needs
/// more than the 500 shown to put the best ones first.
#[cfg(target_os = "macos")]
const SPOTLIGHT_CANDIDATES: usize = 20_000;

/// Paths counted before giving up on an exact total. Past this the count is
/// shown as "at least".
#[cfg(target_os = "macos")]
const SPOTLIGHT_COUNT_LIMIT: usize = 200_000;

/// On the Mac there is no index process: global search asks Spotlight, and
/// Ctrl+P walks the folder. What persists between queries is the last walk.
#[cfg(target_os = "macos")]
#[derive(Default)]
pub struct Connection {
    walk: Option<Walk>,
}

/// Every entry under one folder, as of when it was walked.
#[cfg(target_os = "macos")]
struct Walk {
    root: PathBuf,
    taken: Instant,
    truncated: bool,
    entries: Vec<Walked>,
}

#[cfg(target_os = "macos")]
struct Walked {
    /// Relative to the walk's root, which is what Ctrl+P scores — so `src/ma`
    /// finds `src/main.rs` and not every `main.rs`.
    relative: String,
    /// Where in `relative`, in characters, the name begins. Match positions
    /// past it are the ones that can be highlighted in the name.
    name_from: usize,
    name: String,
    parent: String,
    is_dir: bool,
}

#[cfg(target_os = "macos")]
impl Walk {
    /// Walks breadth first, never through a link, within the same bounds the
    /// one-shot walk had: a hundred thousand entries or three seconds.
    fn take(root: &std::path::Path) -> Walk {
        let started = Instant::now();
        let mut entries = Vec::new();
        let mut truncated = false;
        let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
        'walk: while let Some(dir) = queue.pop_front() {
            let Ok(listing) = std::fs::read_dir(&dir) else { continue };
            let parent = dir.to_string_lossy().into_owned();
            for entry in listing.flatten() {
                if entries.len() >= 100_000 || started.elapsed().as_secs() >= 3 {
                    truncated = true;
                    break 'walk;
                }
                let Ok(kind) = entry.file_type() else { continue };
                let path = entry.path();
                // Never recurse through symlinks: cycles and paths outside the
                // requested subtree must not expand the search scope.
                if kind.is_dir() {
                    queue.push_back(path.clone());
                }
                let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
                let name = entry.file_name().to_string_lossy().into_owned();
                let name_from = relative.chars().count() - name.chars().count();
                entries.push(Walked { relative, name_from, name, parent: parent.clone(), is_dir: kind.is_dir() });
            }
        }
        Walk { root: root.to_path_buf(), taken: Instant::now(), truncated, entries }
    }
}

#[cfg(target_os = "macos")]
impl Connection {
    fn status(&mut self) -> Option<IndexStatus> {
        Some(IndexStatus { fresh: true, volumes_done: 1, volumes_total: 1, ..Default::default() })
    }
    fn request(&mut self, _request: &Request) -> Result<Response, String> {
        Ok(Response::Status(self.status().unwrap_or_default()))
    }

    fn search(&mut self, query: &Query) -> Result<SearchResults, QueryError> {
        use std::io::{BufRead, BufReader};
        use std::os::unix::ffi::OsStringExt;
        use std::process::{Command as Process, Stdio};
        let start = Instant::now();
        // `kind:pdf`, `date:week` and `tag:Red` narrow it, as the Finder's
        // search tokens do; the rest is the name. Spotlight's query syntax is
        // escaped by the query, independently of process arguments.
        let typed = neutron_core::query::Query::typed(&query.needle);
        let predicate = typed.predicate();
        let mut child = Process::new("/usr/bin/mdfind").args(["-0", &predicate])
            .stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
            .map_err(|e| QueryError::Transport(e.to_string()))?;

        // Read well past what is shown: the total is worth having, and the
        // best matches are rarely the first ones Spotlight happens to return.
        let mut candidates: Vec<PathBuf> = Vec::new();
        let mut total = 0;
        let mut truncated = false;
        let reader = BufReader::new(child.stdout.take().unwrap());
        for path in reader.split(0) {
            let Ok(bytes) = path else { break };
            if bytes.is_empty() { continue }
            total += 1;
            if candidates.len() < SPOTLIGHT_CANDIDATES {
                candidates.push(PathBuf::from(std::ffi::OsString::from_vec(bytes)));
            }
            if total >= SPOTLIGHT_COUNT_LIMIT { truncated = true; break }
        }
        if truncated { let _ = child.kill(); }
        let status = child.wait().map_err(|e| QueryError::Transport(e.to_string()))?;
        if !truncated && !status.success() {
            return Err(QueryError::Rejected("Spotlight search failed".into()));
        }

        let needle = typed.text.to_lowercase();
        let mut ranked: Vec<(u8, usize, PathBuf)> = candidates
            .into_iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_string_lossy().to_lowercase();
                let class = if name == needle { 0 } else if name.starts_with(&needle) { 1 } else { 2 };
                Some((class, name.chars().count(), path))
            })
            .collect();
        ranked.sort_unstable_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then_with(|| a.2.cmp(&b.2)));
        ranked.truncate(RESULT_LIMIT);

        let hits = ranked
            .into_iter()
            .filter_map(|(_, _, path)| {
                Some(SearchHit {
                    name: path.file_name()?.to_string_lossy().into(),
                    parent: path.parent()?.to_string_lossy().into(),
                    is_dir: path.is_dir(),
                    matched: vec![],
                })
            })
            .collect();
        Ok(SearchResults { query: query.clone(), total, hits, truncated: truncated || total > RESULT_LIMIT,
            elapsed_micros: start.elapsed().as_micros() as u64 })
    }

    fn find(&mut self, query: &Query) -> Result<SearchResults, QueryError> {
        let start = Instant::now();
        let root = PathBuf::from(&query.scope);
        if !root.is_dir() { return Err(QueryError::Rejected("the search folder is unavailable".into())); }

        let stale = self.walk.as_ref().is_none_or(|w| w.root != root || w.taken.elapsed() > WALK_TTL);
        if stale {
            self.walk = Some(Walk::take(&root));
        }
        let Some(walk) = self.walk.as_ref() else { unreachable!("just walked") };

        let mut matcher = neutron_fuzzy::FuzzyMatcher::new();
        let mut hits: Vec<(u32, &Walked, Vec<u32>)> = Vec::new();
        let mut total = 0;
        for entry in &walk.entries {
            let Some(found) = matcher.score(&entry.relative, &query.needle) else { continue };
            total += 1;
            // Only the letters inside the name are drawn highlighted; a match
            // in a parent folder's name has nowhere on the row to show.
            let in_name = found
                .positions
                .iter()
                .filter_map(|&p| (p as usize).checked_sub(entry.name_from).map(|p| p as u32))
                .collect();
            hits.push((found.score, entry, in_name));
            if hits.len() > RESULT_LIMIT * 2 {
                hits.sort_unstable_by_key(|hit| std::cmp::Reverse(hit.0));
                hits.truncate(RESULT_LIMIT);
            }
        }
        hits.sort_unstable_by_key(|hit| std::cmp::Reverse(hit.0));
        hits.truncate(RESULT_LIMIT);
        Ok(SearchResults {
            query: query.clone(),
            hits: hits
                .into_iter()
                .map(|(_, entry, matched)| SearchHit {
                    name: entry.name.clone(),
                    parent: entry.parent.clone(),
                    is_dir: entry.is_dir,
                    matched,
                })
                .collect(),
            total,
            truncated: walk.truncated || total > RESULT_LIMIT,
            elapsed_micros: start.elapsed().as_micros() as u64,
        })
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;
    #[test]
    fn folder_search_finds_nested_files_without_following_symlink_cycles() {
        let root = std::env::temp_dir().join(format!("neutron-find-{}", std::process::id()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/needle.txt"), "data").unwrap();
        std::os::unix::fs::symlink(&root, root.join("nested/cycle")).unwrap();
        let result = Connection::default().find(&Query { needle: "needle.txt".into(), scope: root.to_string_lossy().into() });
        let result = match result { Ok(r) => r, Err(_) => panic!("folder search failed") };
        assert_eq!(result.total, 1);
        assert_eq!(result.hits[0].name, "needle.txt");
        assert_eq!(PathBuf::from(&result.hits[0].parent), root.join("nested"));
        assert!(!result.truncated);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_search_highlights_the_letters_matched_in_the_name() {
        let root = std::env::temp_dir().join(format!("neutron-find-hl-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();
        let mut connection = Connection::default();
        let result = connection.find(&Query { needle: "main".into(), scope: root.to_string_lossy().into() });
        let Ok(result) = result else { panic!("folder search failed") };
        assert_eq!(result.hits[0].name, "main.rs");
        assert_eq!(result.hits[0].matched, [0, 1, 2, 3]);

        // A second query in the same folder reuses the walk: a file made now
        // is not seen until it expires, which is the trade being made.
        std::fs::write(root.join("src/maintenance.md"), "").unwrap();
        let again = connection.find(&Query { needle: "maint".into(), scope: root.to_string_lossy().into() });
        let Ok(again) = again else { panic!("folder search failed") };
        assert_eq!(again.total, 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}
