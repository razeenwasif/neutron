//! Everything-style instant search over the NTFS change journal.
//!
//! # Why the USN journal rather than raw MFT parsing
//!
//! `FSCTL_ENUM_USN_DATA` walks every File Reference Number on a volume and
//! hands back `(FRN, ParentFRN, name, attrs)` without any directory traversal —
//! seconds for millions of files, against minutes for a recursive walk. Parsing
//! raw MFT records directly would be marginally faster still, but requires
//! tracking NTFS on-disk format changes; the journal is a documented, stable
//! interface for the same data.
//!
//! Paths are never stored. Each record keeps only its parent's FRN, so a full
//! path is reconstructed by walking parent links — and only for the handful of
//! results actually on screen. Storing full paths for 5M files would cost
//! hundreds of megabytes to save maybe a microsecond per displayed row.
//!
//! # Privileges
//!
//! Journal reads require administrator rights, so this runs in the separate
//! `neutron-indexer` helper process rather than the UI. Keeping the UI
//! unelevated is a hard requirement: an elevated window cannot accept
//! drag-and-drop from an unelevated Explorer, because UIPI blocks the messages.
//!
//! # Layout
//!
//! * [`volume`] — the in-memory index of one volume, and path reconstruction.
//! * [`query`] — searching it, including the incremental narrowing that makes
//!   typing feel instant.
//! * `usn` — Windows only; walking the change journal to build the above.

/// A file's identity within a volume. NTFS reuses these after deletion, so an
/// FRN is only meaningful alongside the journal sequence number it came from.
pub type Frn = u64;

/// Volume identity — the drive letter, e.g. `C`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VolumeId(pub char);

pub mod protocol;
pub mod query;
pub mod volume;

#[cfg(windows)]
pub mod usn;

pub use query::{Hit, Results, Searcher};
pub use volume::{RawRecord, VolumeIndex};
