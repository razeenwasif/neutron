//! Domain types for Neutron. Deliberately free of Win32 so it builds and tests
//! on Linux, which keeps the inner development loop fast.

pub mod entry;
pub mod history;
pub mod layout;
pub mod namespace;
pub mod places;
pub mod selection;
pub mod sort;

pub use entry::{Entry, EntryKind, EntryList};
pub use history::History;
pub use layout::{Axis, GroupId, Layout};
pub use namespace::{Namespace, NamespaceError, NodeId};
pub use selection::{SelectMode, Selection};
pub use sort::{SortColumn, SortOrder, SortSpec};

/// Re-exported so callers do not need a separate `use` for the common pairing
/// of an entry list and the filter+sort that populates its display order.
pub use sort::apply as sort_and_filter;
