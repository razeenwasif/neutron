//! Cloud storage providers pinned in the sidebar.
//!
//! The two supported providers need very different amounts of work, because
//! they integrate with Windows very differently:
//!
//! * **OneDrive** already syncs to a local folder, so it is an ordinary
//!   directory that the normal filesystem backend enumerates. The only extra
//!   work is reading placeholder state to badge cloud-only files — and doing so
//!   from file *attributes*, never by opening the file, since opening a
//!   placeholder triggers a download.
//!
//! * **Google Drive** has no local presence on this machine (Drive for Desktop
//!   is not installed), so it needs a real Drive API v3 client with OAuth. It is
//!   staged last for that reason.
//!
//! Filled in at M2 (OneDrive) and M7 (Google Drive).

pub mod credentials;
pub mod drive;
pub mod flow;
pub mod google;
pub mod oauth;

use neutron_core::entry::SyncState;

/// A location within a provider. Drive addresses objects by opaque id rather
/// than path — a Drive file can live in several folders at once and has no
/// single canonical path — so this is not a `PathBuf`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudPath(pub String);

pub trait CloudProvider: Send + Sync {
    fn name(&self) -> &str;
    fn sync_state(&self, path: &CloudPath) -> SyncState;
}
