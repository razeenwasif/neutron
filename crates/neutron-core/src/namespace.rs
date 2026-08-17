//! Location identity and the enumeration trait every backend implements.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::entry::EntryList;

/// Identifies a browsable location.
///
/// Neutron browses three fundamentally different kinds of place, and conflating
/// them into a string path is how file managers end up unable to represent
/// "This PC" or a Drive folder. Keeping them as distinct variants means the
/// dispatch in `neutron-shell` is a match rather than a guess.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeId {
    /// A real filesystem location. Always stored `\\?\`-prefixed on Windows so
    /// the 260-char `MAX_PATH` limit never applies.
    Path(PathBuf),

    /// A shell namespace item with no filesystem path — This PC, Network,
    /// Control Panel, the inside of a zip, a third-party namespace extension.
    ///
    /// # Why a parsing name and not a PIDL
    ///
    /// A PIDL is the shell's native handle, and holding one would avoid a parse
    /// per navigation. It is the wrong thing to store here for three reasons:
    /// it is opaque bytes, so a persisted workspace could not be inspected or
    /// migrated; it is not guaranteed stable across reboots for every
    /// extension, so a restored tab could point at nothing; and it carries no
    /// name, so titling a tab would need a COM call on the UI thread — which
    /// the threading contract forbids.
    ///
    /// A parsing name is a string the shell round-trips through
    /// `SHParseDisplayName`: `::{20D04FE0-3AEA-1069-A2D8-08002B30309D}` for This
    /// PC, or `C:\archive.zip\inner` for a folder inside a zip. It persists,
    /// it is greppable in a saved session, and the display name travels beside
    /// it so the UI never has to ask.
    Shell {
        /// Round-trips through `SHParseDisplayName`.
        parsing: Arc<str>,
        /// What to show in the tab and breadcrumb. Resolved once, when the
        /// location is first reached, on a worker.
        display: Arc<str>,
    },

    /// An object inside a cloud provider, addressed by the provider's own id
    /// rather than a path — Drive files have no stable path and can live in
    /// several folders at once.
    Cloud {
        provider: CloudProviderId,
        id: Arc<str>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloudProviderId {
    GoogleDrive,
    OneDrive,
}

impl NodeId {
    pub fn path(p: impl Into<PathBuf>) -> Self {
        NodeId::Path(p.into())
    }

    /// The filesystem path, when there is one. `None` for virtual locations —
    /// callers must not fabricate a path for those.
    pub fn as_path(&self) -> Option<&Path> {
        match self {
            NodeId::Path(p) => Some(p),
            _ => None,
        }
    }

    /// Whether the fast `FindFirstFileExW` enumeration path applies. Shell and
    /// cloud nodes must go through their own backends.
    pub fn is_filesystem(&self) -> bool {
        matches!(self, NodeId::Path(_))
    }

    /// A shell namespace location.
    pub fn shell(parsing: impl Into<Arc<str>>, display: impl Into<Arc<str>>) -> Self {
        NodeId::Shell {
            parsing: parsing.into(),
            display: display.into(),
        }
    }

    /// The shell parsing name, when this is a shell node.
    pub fn parsing_name(&self) -> Option<&str> {
        match self {
            NodeId::Shell { parsing, .. } => Some(parsing),
            _ => None,
        }
    }

    /// Label for the breadcrumb / tab title.
    pub fn display_name(&self) -> String {
        match self {
            NodeId::Path(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                // A drive root has no file_name; show the root itself.
                .unwrap_or_else(|| p.to_string_lossy().into_owned()),
            NodeId::Shell { display, .. } => display.to_string(),
            NodeId::Cloud { provider, .. } => match provider {
                CloudProviderId::GoogleDrive => "Google Drive".to_owned(),
                CloudProviderId::OneDrive => "OneDrive".to_owned(),
            },
        }
    }

    /// The containing location, or `None` at a root.
    pub fn parent(&self) -> Option<NodeId> {
        match self {
            NodeId::Path(p) => p.parent().map(|p| NodeId::Path(p.to_path_buf())),
            // Shell and cloud parents need their backend to resolve, so they
            // are not derivable here.
            _ => None,
        }
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeId::Path(p) => write!(f, "{}", p.display()),
            NodeId::Shell { parsing, .. } => write!(f, "{parsing}"),
            NodeId::Cloud { provider, id } => write!(f, "<{provider:?}:{id}>"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NamespaceError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("access denied: {0}")]
    AccessDenied(String),
    #[error("not a container: {0}")]
    NotAContainer(String),
    #[error("backend cannot handle this location: {0}")]
    Unsupported(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// Enumerates the contents of a location.
///
/// Implementations block, so callers must run them on a worker thread — never
/// on the UI thread. See the module docs in `neutron-app` for the threading
/// contract.
pub trait Namespace: Send + Sync {
    /// Whether this backend can handle `id`. The dispatcher tries backends in
    /// priority order and takes the first that claims it.
    fn handles(&self, id: &NodeId) -> bool;

    /// Lists the contents of `id`.
    ///
    /// A directory the user lacks permission to read partway through must not
    /// fail the whole listing — implementations skip the unreadable entry and
    /// continue, because a half-listed folder is far more useful than an error.
    fn enumerate(&self, id: &NodeId) -> Result<EntryList, NamespaceError>;
}
