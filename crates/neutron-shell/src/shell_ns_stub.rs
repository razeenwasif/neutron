//! Non-Windows placeholder. See `lib.rs` for why the stubs exist.

use neutron_core::{EntryList, Namespace, NamespaceError, NodeId};

pub struct ShellNamespace;

impl Namespace for ShellNamespace {
    fn handles(&self, id: &NodeId) -> bool {
        matches!(id, NodeId::Shell { .. })
    }

    fn enumerate(&self, id: &NodeId) -> Result<EntryList, NamespaceError> {
        Err(NamespaceError::Unsupported(id.to_string()))
    }
}

pub fn enumerate_parsing_name(parsing: &str) -> Result<EntryList, NamespaceError> {
    Err(NamespaceError::Unsupported(parsing.to_owned()))
}

pub fn display_name_of(_parsing: &str) -> Option<String> {
    None
}

pub fn is_shell_container(_path: &str) -> bool {
    false
}

pub const WELL_KNOWN: &[(&str, &str)] = &[];
