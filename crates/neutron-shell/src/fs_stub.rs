//! Non-Windows placeholder so the workspace still builds and unit-tests on
//! Linux. Neutron only ships for Windows; this exists purely to keep the
//! development inner loop off the slow cross-build.

use neutron_core::{EntryList, Namespace, NamespaceError, NodeId};

pub struct FsNamespace;

impl Namespace for FsNamespace {
    fn handles(&self, id: &NodeId) -> bool {
        id.is_filesystem()
    }

    fn enumerate(&self, id: &NodeId) -> Result<EntryList, NamespaceError> {
        Err(NamespaceError::Unsupported(format!(
            "filesystem enumeration is Windows-only (requested {id})"
        )))
    }
}

/// Spotlight's last-opened date is a Mac thing; elsewhere it is not known.
pub fn last_opened(_path: &std::path::Path) -> i64 {
    0
}
