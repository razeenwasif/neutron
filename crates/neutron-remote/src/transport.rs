//! What both protocols look like from the outside.
//!
//! FTP and SFTP have almost nothing in common — one is a 1971 line protocol
//! with a second TCP connection per transfer, the other is a subsystem of SSH —
//! so the shared surface is deliberately small: list a directory, read a file.
//! Everything either protocol does that the other cannot stays inside its own
//! module rather than being smeared into a trait nobody can implement.
//!
//! Every method here blocks. They run on the loader thread or a job worker,
//! never on the UI thread.

use std::io::{Read, Write};

use neutron_core::entry::{Entry, EntryKind, attr};

/// Whether a transfer should keep going. Returned by the progress callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continue {
    Yes,
    Stop,
}

/// One entry in a remote directory.
///
/// Deliberately not [`neutron_core::Entry`]: the conversion needs to happen
/// somewhere, and doing it here keeps the protocol modules free of decisions
/// about how Neutron displays things.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    /// A single name, already checked by [`crate::path::safe_name`].
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Milliseconds since the Unix epoch, matching [`neutron_core::Entry`].
    /// Zero when the server did not say — plenty of FTP listings carry only a
    /// date, and some carry nothing at all.
    pub modified: i64,
}

impl RemoteEntry {
    pub fn to_entry(&self) -> Entry {
        Entry {
            name: self.name.clone(),
            kind: self.kind,
            size: self.size,
            modified: self.modified,
            created: 0,
            attrs: if self.kind == EntryKind::Directory {
                attr::DIRECTORY
            } else {
                0
            },
            // Nothing remote is a cloud placeholder. The whole file is either
            // here or it is not.
            sync: neutron_core::entry::SyncState::Local,
        }
    }
}

/// What went wrong.
///
/// Every variant renders to something a user can act on, which is why the
/// server's own text is carried through rather than replaced with a category.
/// "550 Permission denied" from the server beats "the operation failed".
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("could not reach {address}: {reason}")]
    Connect { address: String, reason: String },

    #[error("{address} rejected the username or password")]
    Auth { address: String },

    /// No password stored, and this server wants one.
    #[error("no password saved for {address} — connect to it again to enter one")]
    NoPassword { address: String },

    /// The site's recorded SSH host key does not match the one presented.
    #[error("{0}")]
    HostKeyChanged(String),

    /// The server refused an operation, in its own words.
    #[error("{path}: {reason}")]
    Server { path: String, reason: String },

    /// A name in a listing was not usable — see [`crate::path`].
    #[error("{0}")]
    Refused(#[from] crate::path::Refusal),

    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// The transfer was stopped by the user.
    #[error("stopped")]
    Cancelled,
}

/// A live connection to one server.
pub trait Transport: Send {
    /// Lists a directory.
    ///
    /// Entries whose names are not usable are skipped rather than failing the
    /// listing, and reported through `refused`. A folder that will not open
    /// because one file in it has a hostile name would be a denial of service
    /// dressed up as strictness.
    fn list(&mut self, path: &str, refused: &mut Vec<String>)
    -> Result<Vec<RemoteEntry>, RemoteError>;

    /// Copies a remote file into `sink`.
    ///
    /// `on_progress` receives the running byte count and may stop the transfer.
    fn download(
        &mut self,
        path: &str,
        sink: &mut dyn Write,
        on_progress: &mut dyn FnMut(u64) -> Continue,
    ) -> Result<u64, RemoteError>;


    // --- writing ------------------------------------------------------------
    //
    // Everything below acts on someone else's machine, where a mistake has no
    // Recycle Bin behind it. Two rules hold across both implementations.
    //
    // **Paths are built, never concatenated by the caller.** Every one of these
    // takes an absolute POSIX path that `crate::path::join` produced from a
    // name already through `safe_name`, for the same reason a listing's names
    // are checked: a name carrying `..` or a separator is how a write lands
    // somewhere nobody asked for.
    //
    // **A directory is removed only when it is empty.** Neither protocol has a
    // recursive delete, so a folder is a walk the caller has to do — and a walk
    // that deletes is exactly where a bug is unrecoverable. Making the caller
    // ask for each step keeps the decision where the confirmation is.

    /// Creates one directory. Its parent must already exist.
    fn create_dir(&mut self, path: &str) -> Result<(), RemoteError>;

    /// Renames or moves an entry.
    ///
    /// Both paths are absolute. Servers differ on whether this can move across
    /// directories and on whether it will overwrite; Neutron only ever renames
    /// within one directory, and checks for a collision first.
    fn rename(&mut self, from: &str, to: &str) -> Result<(), RemoteError>;

    /// Deletes one file. Permanent: there is no server-side bin.
    fn remove_file(&mut self, path: &str) -> Result<(), RemoteError>;

    /// Deletes one *empty* directory.
    ///
    /// A server that refuses because the directory is not empty says so in its
    /// own words, which is the answer the user needs.
    fn remove_dir(&mut self, path: &str) -> Result<(), RemoteError>;

    /// Writes `source` to `path`, replacing whatever is there.
    ///
    /// `on_progress` receives the running byte count and may stop the transfer.
    /// A stopped or failed upload leaves a partial file on the server, which
    /// the caller is expected to remove — this layer cannot know whether the
    /// file it was overwriting mattered.
    fn upload(
        &mut self,
        path: &str,
        source: &mut dyn Read,
        on_progress: &mut dyn FnMut(u64) -> Continue,
    ) -> Result<u64, RemoteError>;

    /// The host key fingerprint this connection accepted, when the protocol has
    /// one and it was not already recorded. The caller saves it against the
    /// site so the next connection can check it.
    fn new_fingerprint(&self) -> Option<&str> {
        None
    }
}

/// How long to wait for a server that is not answering.
///
/// Generous, because these are often on the far side of a domestic uplink, and
/// bounded, because a tab stuck on a dead host with no error is worse than a
/// slow one.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Transfer chunk. Large enough that the syscall overhead disappears, small
/// enough that progress moves visibly inside one big file.
pub const CHUNK: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_entry_carries_the_directory_attribute() {
        // The sort and the icon both key off the attribute rather than the
        // kind, so a directory without it sorts among the files.
        let entry = RemoteEntry {
            name: "sub".into(),
            kind: EntryKind::Directory,
            size: 0,
            modified: 0,
        }
        .to_entry();
        assert_eq!(entry.attrs & attr::DIRECTORY, attr::DIRECTORY);
    }

    #[test]
    fn a_file_entry_keeps_its_size_and_time() {
        let entry = RemoteEntry {
            name: "notes.txt".into(),
            kind: EntryKind::File,
            size: 4096,
            modified: 1_700_000_000_000,
        }
        .to_entry();
        assert_eq!(entry.size, 4096);
        assert_eq!(entry.modified, 1_700_000_000_000);
        assert_eq!(entry.attrs & attr::DIRECTORY, 0);
    }
}
