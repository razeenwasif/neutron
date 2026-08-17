//! Named-pipe server: builds the index, then answers queries.
//!
//! # Security
//!
//! This process is elevated and its client is not, so the pipe is a channel
//! from lower privilege to higher. Two things follow.
//!
//! First, the pipe is created with an explicit DACL granting access to the
//! **current user only**. The default would let any account on the machine
//! connect to an administrator-privileged endpoint, which is a genuine local
//! privilege-escalation surface rather than a theoretical one.
//!
//! Second, [`FILE_FLAG_FIRST_PIPE_INSTANCE`] is set, so creation *fails* if the
//! name is already taken rather than quietly joining an existing pipe. Without
//! it, a process that squatted the name first would receive queries intended
//! for us — and, since it would be answering them, could return whatever paths
//! it liked to a UI that trusts them.
//!
//! # Threading
//!
//! One connection at a time, served synchronously. The client is a single UI
//! process making one query per keystroke; a thread pool would add concurrency
//! that nothing needs and make the searcher's incremental narrowing — which is
//! inherently sequential — useless.

use std::io::{BufRead, BufReader, Write};
use std::time::Instant;

use neutron_index::protocol::{IndexStatus, Request, Response, SearchHit, sanitize_needle};
use neutron_index::{Searcher, VolumeIndex, usn};

use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows::core::PCWSTR;

/// Buffer sizes advertised to the pipe. A large response — four thousand hits
/// with full paths — is a few hundred kilobytes, so this keeps the common case
/// to a single round trip.
const PIPE_BUFFER: u32 = 256 * 1024;

pub fn run(pipe_name: &str) -> anyhow::Result<()> {
    // Indexing happens before the first connection is accepted. The client
    // tolerates the pipe not existing yet and retries; the alternative — accept
    // early and answer "not ready" — is the same wait with more moving parts.
    let indexes = build_indexes();

    let path = format!(r"\\.\pipe\{pipe_name}");
    tracing::info!(pipe = %path, "serving");

    let mut searcher = Searcher::new();
    let status = status_of(&indexes);

    loop {
        let pipe = match Pipe::create(&path) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("could not create the pipe: {e}");
                return Err(e);
            }
        };

        if let Err(e) = pipe.accept() {
            tracing::warn!("connection failed: {e}");
            continue;
        }

        // One client at a time; when it disconnects the pipe is recreated.
        match serve_client(&pipe, &indexes, &mut searcher, &status) {
            Ok(Exit::Shutdown) => {
                tracing::info!("client asked to shut down");
                return Ok(());
            }
            Ok(Exit::Disconnected) => tracing::debug!("client disconnected"),
            Err(e) => tracing::warn!("client error: {e}"),
        }
    }
}

enum Exit {
    Disconnected,
    Shutdown,
}

fn build_indexes() -> Vec<VolumeIndex> {
    use rayon::prelude::*;

    let volumes = usn::indexable_volumes();
    let started = Instant::now();

    // One thread per volume. Volumes are independent, they are usually separate
    // physical devices, and the work is dominated by waiting on each device —
    // so wall time becomes the slowest volume rather than the sum. Measured
    // sequentially at 34.7s across six volumes whose largest took 15.7s.
    let mut indexes: Vec<VolumeIndex> = volumes
        .par_iter()
        .filter_map(|volume| match usn::index_volume(*volume) {
            Ok(index) => Some(index),
            Err(e) => {
                tracing::warn!("{e}");
                None
            }
        })
        .collect();

    // Deterministic order, so a hit's volume index means the same thing across
    // runs and the results of a search do not reshuffle between sessions.
    indexes.sort_by_key(|i| i.volume().0);

    tracing::info!(
        volumes = indexes.len(),
        records = indexes.iter().map(|i| i.len()).sum::<usize>(),
        elapsed_ms = started.elapsed().as_millis(),
        "index ready"
    );
    indexes
}

fn status_of(indexes: &[VolumeIndex]) -> IndexStatus {
    IndexStatus {
        volumes_done: indexes.len(),
        volumes_total: indexes.len(),
        records: indexes.iter().map(|i| i.len()).sum(),
        memory_bytes: indexes.iter().map(|i| i.memory_bytes()).sum(),
        skipped: Vec::new(),
    }
}

fn serve_client(
    pipe: &Pipe,
    indexes: &[VolumeIndex],
    searcher: &mut Searcher,
    status: &IndexStatus,
) -> anyhow::Result<Exit> {
    let mut reader = BufReader::new(PipeStream(pipe.0));
    let mut line = String::new();

    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(Exit::Disconnected);
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Request>(line) {
            Ok(Request::Shutdown) => return Ok(Exit::Shutdown),
            Ok(Request::Status) => Response::Status(status.clone()),
            Ok(Request::Search { needle, limit }) => search(indexes, searcher, &needle, limit),
            Ok(Request::Find {
                needle,
                scope,
                limit,
            }) => find(indexes, &needle, &scope, limit),
            // A malformed request is answered rather than fatal: the client is
            // a separate process that may be a different version.
            Err(e) => Response::Error(format!("malformed request: {e}")),
        };

        let mut encoded = serde_json::to_string(&response)?;
        encoded.push('\n');
        let mut stream = PipeStream(pipe.0);
        stream.write_all(encoded.as_bytes())?;
        stream.flush()?;
    }
}

fn search(
    indexes: &[VolumeIndex],
    searcher: &mut Searcher,
    needle: &str,
    limit: usize,
) -> Response {
    let needle = sanitize_needle(needle);
    let started = Instant::now();
    let results = searcher.search(indexes, &needle);

    // Paths are resolved here rather than client-side because the parent chains
    // live here — and only for the rows actually being sent, which is what
    // makes reconstruction affordable.
    let hits = results
        .hits
        .iter()
        .take(limit.min(results.hits.len()))
        .map(|h| {
            let index = &indexes[h.volume as usize];
            let record = h.record as usize;
            SearchHit {
                name: index.name(record).to_owned(),
                parent: index.parent_path(record),
                is_dir: index.is_dir(record),
                // Substring matches are contiguous, so there is nothing a
                // highlight would tell the reader that the query does not.
                matched: Vec::new(),
            }
        })
        .collect();

    Response::Results {
        hits,
        total: results.total,
        truncated: results.truncated,
        elapsed_micros: started.elapsed().as_micros() as u64,
    }
}

/// Fuzzy search beneath one directory, for the finder overlay.
fn find(indexes: &[VolumeIndex], needle: &str, scope: &str, limit: usize) -> Response {
    let needle = sanitize_needle(needle);
    let started = Instant::now();

    // Unlike the substring search this keeps no state between calls: the
    // candidate set is already narrow, and a scope change invalidates any
    // previous result anyway.
    let found = neutron_index::query::fuzzy_in_scope(indexes, &needle, scope, limit.min(2048));

    let hits = found
        .iter()
        .map(|f| {
            let index = &indexes[f.hit.volume as usize];
            let record = f.hit.record as usize;
            SearchHit {
                name: index.name(record).to_owned(),
                parent: index.parent_path(record),
                is_dir: index.is_dir(record),
                matched: f.positions.clone(),
            }
        })
        .collect::<Vec<_>>();

    Response::Results {
        total: hits.len(),
        truncated: hits.len() >= limit,
        hits,
        elapsed_micros: started.elapsed().as_micros() as u64,
    }
}

// --- pipe plumbing ---------------------------------------------------------

/// A named-pipe server endpoint, closed on drop.
struct Pipe(HANDLE);

impl Pipe {
    fn create(path: &str) -> anyhow::Result<Self> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        let mut descriptor = security::current_user_only()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.as_mut_ptr(),
            bInheritHandle: false.into(),
        };

        // SAFETY: `wide` is NUL-terminated, and `attributes` and the descriptor
        // it points at both outlive the call.
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                // FIRST_PIPE_INSTANCE makes a squatted name a hard failure
                // rather than something we silently share. See the module note.
                PIPE_ACCESS_DUPLEX | windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_FIRST_PIPE_INSTANCE.0),
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                Some(&attributes),
            )
        };

        // Returns INVALID_HANDLE_VALUE rather than a Result. The common cause
        // is FIRST_PIPE_INSTANCE refusing a name something else already holds,
        // which is exactly the case that must not be ignored.
        if handle.is_invalid() {
            return Err(anyhow::anyhow!(
                "could not create {path}: {}",
                std::io::Error::last_os_error()
            ));
        }

        Ok(Pipe(handle))
    }

    /// Blocks until a client connects.
    fn accept(&self) -> anyhow::Result<()> {
        // SAFETY: `self.0` is a live server-side pipe handle.
        let result = unsafe { ConnectNamedPipe(self.0, None) };

        match result {
            Ok(()) => Ok(()),
            // A client that connected between CreateNamedPipeW and this call
            // is already connected, which is success reported as an error.
            Err(e) if e.code().0 as u32 & 0xFFFF == ERROR_PIPE_CONNECTED.0 => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: created above and not used afterwards.
        unsafe {
            let _ = DisconnectNamedPipe(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

/// `Read`/`Write` over a pipe handle, so the standard buffered line machinery
/// can be used instead of hand-rolling framing.
struct PipeStream(HANDLE);

impl std::io::Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut read = 0u32;
        // SAFETY: `buf` is a valid writable slice of the length passed.
        unsafe { ReadFile(self.0, Some(buf), Some(&mut read), None) }
            .map_err(std::io::Error::other)?;
        Ok(read as usize)
    }
}

impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut written = 0u32;
        // SAFETY: `buf` is a valid readable slice of the length passed.
        unsafe { WriteFile(self.0, Some(buf), Some(&mut written), None) }
            .map_err(std::io::Error::other)?;
        Ok(written as usize)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

mod security {
    //! Building a DACL that admits only the user running this process.

    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::PSECURITY_DESCRIPTOR;
    use windows::core::PCWSTR;

    /// Owns a security descriptor allocated by the conversion API.
    pub struct Descriptor(PSECURITY_DESCRIPTOR);

    impl Descriptor {
        pub fn as_mut_ptr(&mut self) -> *mut core::ffi::c_void {
            self.0.0
        }
    }

    impl Drop for Descriptor {
        fn drop(&mut self) {
            if !self.0.0.is_null() {
                // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW,
                // which documents LocalFree as the matching release.
                unsafe {
                    let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(self.0.0)));
                }
            }
        }
    }

    /// A DACL granting full access to the user running this process, and to
    /// nobody else.
    ///
    /// SDDL, read left to right: deny inheritance (`P`), then one allow ace
    /// giving generic all (`GA`) to this token's user SID.
    ///
    /// Notably *not* granting `WD` (everyone) or `BU` (built-in users), which
    /// is close to what the default descriptor allows. This endpoint is
    /// elevated; letting an arbitrary local account talk to it is a real
    /// escalation path.
    ///
    /// # Why not `CO`
    ///
    /// The obvious spelling is `D:P(A;;GA;;;CO)` — creator/owner. It is wrong
    /// here, and wrong in a way that looks right: `CO` is replaced at creation
    /// with the *owner* of the creating token, and an elevated process's
    /// default owner is the **Administrators** group, not the user. The pipe
    /// then admits administrators only, and the unelevated UI — the one process
    /// that must be able to connect — is denied by the rule written to admit
    /// it. Measured: `Access to the path is denied` on every connect.
    ///
    /// The token's *user* SID is unaffected by elevation, so it is the right
    /// thing to name.
    pub fn current_user_only() -> anyhow::Result<Descriptor> {
        let sddl = format!("D:P(A;;GA;;;{})", current_user_sid()?);
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();

        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `wide` is NUL-terminated and outlives the call; the out-param
        // allocation is owned by `Descriptor` from here on.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }?;

        Ok(Descriptor(descriptor))
    }

    /// This process token's user SID, in string form.
    fn current_user_sid() -> anyhow::Result<String> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
        use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        let mut token = windows::Win32::Foundation::HANDLE::default();
        // SAFETY: querying our own process token; the handle is closed below.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }?;

        // Sized generously: a TOKEN_USER is a fixed header plus a variable
        // SID, and the largest possible SID is well under this.
        let mut buffer = vec![0u8; 256];
        let mut needed = 0u32;
        // SAFETY: `buffer` is valid for the length passed, and `needed` is a
        // valid out-param.
        let queried = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buffer.as_mut_ptr() as *mut _),
                buffer.len() as u32,
                &mut needed,
            )
        };
        // SAFETY: opened above and not used afterwards.
        unsafe { let _ = CloseHandle(token); };
        queried?;

        // SAFETY: on success the buffer holds a TOKEN_USER whose `Sid` points
        // into that same buffer, which is still alive.
        let sid = unsafe { (*(buffer.as_ptr() as *const TOKEN_USER)).User.Sid };

        let mut raw = windows::core::PWSTR::null();
        // SAFETY: `sid` is valid; the returned string is freed below.
        unsafe { ConvertSidToStringSidW(sid, &mut raw) }?;

        // SAFETY: NUL-terminated on success.
        let text = unsafe { raw.to_string() }?;
        // SAFETY: allocated by ConvertSidToStringSidW, which documents
        // LocalFree as the matching release.
        unsafe {
            let _ = windows::Win32::Foundation::LocalFree(Some(
                windows::Win32::Foundation::HLOCAL(raw.0 as *mut _),
            ));
        }

        Ok(text)
    }
}
