//! Location identity and the enumeration trait every backend implements.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::entry::EntryList;

/// Identifies a browsable location.
///
/// Neutron browses four fundamentally different kinds of place, and conflating
/// them into a string path is how file managers end up unable to represent
/// "This PC" or a Drive folder. Keeping them as distinct variants means the
/// dispatch in `neutron-shell` is a match rather than a guess.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeId {
    /// The dashboard: where a window opens, and the one location that is not a
    /// container of files.
    ///
    /// A variant rather than a flag on a path, because it genuinely is a
    /// different kind of place: no children to enumerate, no parent to go up
    /// to, nothing in it to rename or delete. That is what the other virtual
    /// variants already model, and what every `match` here is already shaped
    /// to handle.
    Home,

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
        /// What to show in the tab and breadcrumb, carried for the same reason
        /// `Shell` carries one: the id is opaque. A Drive folder id says
        /// nothing about which folder it is, and resolving it would mean an
        /// API call on the paint thread. Without this every folder in Drive
        /// was titled "Google Drive", including all of its subfolders.
        name: Arc<str>,
    },

    /// A directory on a saved FTP, FTPS or SFTP server.
    ///
    /// Two parts, because neither alone identifies anything: `/pub` means
    /// nothing without knowing whose `/pub`, and a server is not a location.
    /// The path is POSIX and absolute — see `neutron_remote::path` — which is
    /// why it is a string rather than a `PathBuf`: a `PathBuf` on Windows would
    /// normalise the separators to backslashes and stop being a remote path.
    Remote {
        /// The site's id, from `sites.json`.
        site: Arc<str>,
        /// An absolute POSIX path on that server.
        path: Arc<str>,
    },

    /// A folder inside an archive on disk: the archive, and a `/`-separated
    /// path within it, empty at its top. Read-only — browsed, opened from and
    /// copied out of, never written to.
    Archive {
        archive: PathBuf,
        inner: Arc<str>,
    },

    /// Every file carrying a Finder tag, wherever it is, as Spotlight finds
    /// them. Its rows lead to their own paths, so it has none of its own.
    Tag { name: Arc<str> },

    /// A terminal, by the id of its session, started in `dir`. Not a place
    /// with files: the tab shows the session instead of a listing. Saved as
    /// the folder, and opened again there as a fresh shell.
    Terminal { id: u64, dir: PathBuf },

    /// A file open in Neutron's viewer: a PDF, an image, text or code. Up is
    /// the folder it is in.
    Viewer { path: PathBuf },

    /// The files a search finds, wherever they are: a Smart Folder. `query`
    /// is a [`crate::query::Query`], written down; `name` what it was saved
    /// as, or empty.
    Search { name: Arc<str>, query: Arc<str> },
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

    /// Whether the fast `FindFirstFileExW` enumeration path applies. Shell,
    /// cloud and remote nodes must go through their own backends.
    pub fn is_filesystem(&self) -> bool {
        matches!(self, NodeId::Path(_))
    }

    /// A directory on a saved server.
    pub fn remote(site: impl Into<Arc<str>>, path: impl Into<Arc<str>>) -> Self {
        NodeId::Remote {
            site: site.into(),
            path: path.into(),
        }
    }

    /// The site and path, when this is a remote node.
    pub fn archive(archive: impl Into<PathBuf>, inner: impl Into<Arc<str>>) -> Self {
        NodeId::Archive {
            archive: archive.into(),
            inner: inner.into(),
        }
    }

    pub fn as_archive(&self) -> Option<(&Path, &str)> {
        match self {
            NodeId::Archive { archive, inner } => Some((archive, inner)),
            _ => None,
        }
    }

    pub fn as_remote(&self) -> Option<(&str, &str)> {
        match self {
            NodeId::Remote { site, path } => Some((site, path)),
            _ => None,
        }
    }

    /// A location inside a cloud provider.
    pub fn cloud(
        provider: CloudProviderId,
        id: impl Into<Arc<str>>,
        name: impl Into<Arc<str>>,
    ) -> Self {
        NodeId::Cloud {
            provider,
            id: id.into(),
            name: name.into(),
        }
    }

    /// A shell namespace location.
    pub fn shell(parsing: impl Into<Arc<str>>, display: impl Into<Arc<str>>) -> Self {
        NodeId::Shell {
            parsing: parsing.into(),
            display: display.into(),
        }
    }

    /// A new terminal started in `dir`, with an id no other has had in this
    /// process.
    pub fn terminal(dir: impl Into<PathBuf>) -> NodeId {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NodeId::Terminal { id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed), dir: dir.into() }
    }

    /// Whether this is a terminal rather than a place with files.
    pub fn is_terminal(&self) -> bool {
        matches!(self, NodeId::Terminal { .. })
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
            NodeId::Home => "Dashboard".to_owned(),
            NodeId::Path(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                // A drive root has no file_name; show the root itself.
                .unwrap_or_else(|| p.to_string_lossy().into_owned()),
            NodeId::Shell { display, .. } => display.to_string(),
            NodeId::Cloud { name, .. } => name.to_string(),
            // The last segment, or the site's id at the root — a tab titled
            // "/" says nothing about which server it is on.
            NodeId::Remote { site, path } => match path.rsplit('/').next() {
                Some(name) if !name.is_empty() => name.to_owned(),
                _ => site.to_string(),
            },
            NodeId::Tag { name } => name.to_string(),
            NodeId::Terminal { .. } => "Terminal".to_owned(),
            NodeId::Viewer { path } => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            NodeId::Search { name, .. } if !name.is_empty() => name.to_string(),
            NodeId::Search { query, .. } => crate::query::Query::decode(query).describe(),
            // The folder's own name, or the archive's at its top.
            NodeId::Archive { archive, inner } => match inner.rsplit('/').next() {
                Some(name) if !name.is_empty() => name.to_owned(),
                _ => archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            },
        }
    }

    /// The containing location, or `None` at a root.
    pub fn parent(&self) -> Option<NodeId> {
        match self {
            NodeId::Path(p) => p.parent().map(|p| NodeId::Path(p.to_path_buf())),
            NodeId::Viewer { path } => path.parent().map(|p| NodeId::Path(p.to_path_buf())),
            // A remote path is POSIX and absolute, so its parent is derivable
            // without asking the server. `/` has none, which is what stops the
            // Up button walking off the top of a site.
            NodeId::Remote { site, path } => {
                let trimmed = path.trim_end_matches('/');
                match trimmed.rfind('/') {
                    None => None,
                    // The parent of `/pub` is the root, not the empty string.
                    Some(0) if trimmed.len() > 1 => Some(NodeId::remote(site.clone(), "/")),
                    Some(0) => None,
                    Some(cut) => Some(NodeId::remote(site.clone(), &trimmed[..cut])),
                }
            }
            // Up from an archive's top is the folder the archive sits in, as
            // it is from any other folder.
            NodeId::Archive { archive, inner } => match inner.rsplit_once('/') {
                Some((up, _)) => Some(NodeId::archive(archive.clone(), up)),
                None if inner.is_empty() => archive.parent().map(|p| NodeId::Path(p.to_path_buf())),
                None => Some(NodeId::archive(archive.clone(), "")),
            },
            // A shell parent needs COM to walk, which cannot happen while
            // painting.
            //
            // A cloud parent is not merely unresolvable here — for Drive it is
            // not well defined. An object can sit in several folders at once,
            // so "the" parent of a Drive folder is a question with more than
            // one answer, and picking one would send Up somewhere the user did
            // not come from. Back walks the trail that actually happened.
            _ => None,
        }
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeId::Home => write!(f, "<home>"),
            NodeId::Path(p) => write!(f, "{}", p.display()),
            NodeId::Shell { parsing, .. } => write!(f, "{parsing}"),
            NodeId::Cloud { provider, id, .. } => write!(f, "<{provider:?}:{id}>"),
            NodeId::Remote { site, path } => write!(f, "{site}:{path}"),
            NodeId::Tag { name } => write!(f, "<tag:{name}>"),
            NodeId::Terminal { id, dir } => write!(f, "<terminal {id}:{}>", dir.display()),
            NodeId::Viewer { path } => write!(f, "<viewer:{}>", path.display()),
            NodeId::Search { query, .. } => write!(f, "<search:{}>", crate::query::Query::decode(query).describe()),
            NodeId::Archive { archive, inner } if inner.is_empty() => write!(f, "{}", archive.display()),
            NodeId::Archive { archive, inner } => write!(f, "{}/{inner}", archive.display()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_parent_walks_up_and_stops_at_the_site_root() {
        let deep = NodeId::remote("nas", "/volume1/media/photos");
        let parent = deep.parent().expect("a parent");
        assert_eq!(parent, NodeId::remote("nas", "/volume1/media"));

        let one_deep = NodeId::remote("nas", "/pub");
        assert_eq!(one_deep.parent(), Some(NodeId::remote("nas", "/")));

        // The root of a site has no parent, which is what stops the Up button
        // walking off the top of it.
        assert_eq!(NodeId::remote("nas", "/").parent(), None);
    }

    #[test]
    fn a_remote_parent_keeps_the_site() {
        // Losing the site would send the pane to a path on nothing in
        // particular, which is far worse than refusing to move.
        let parent = NodeId::remote("nas", "/a/b").parent().unwrap();
        assert_eq!(parent.as_remote().map(|(site, _)| site), Some("nas"));
    }

    #[test]
    fn a_remote_tab_is_titled_by_its_folder_and_a_site_root_by_its_site() {
        assert_eq!(NodeId::remote("nas", "/a/photos").display_name(), "photos");
        // A tab titled "/" says nothing about which server it is on.
        assert_eq!(NodeId::remote("nas", "/").display_name(), "nas");
    }

    #[test]
    fn a_remote_node_is_not_a_filesystem_path() {
        // Anything that treated it as one would hand a POSIX string to
        // FindFirstFileExW and enumerate a folder on the local disk.
        let node = NodeId::remote("nas", "/pub");
        assert!(!node.is_filesystem());
        assert_eq!(node.as_path(), None);
    }

    #[test]
    fn remote_nodes_on_different_sites_are_different_locations() {
        assert_ne!(NodeId::remote("a", "/pub"), NodeId::remote("b", "/pub"));
    }

    #[test]
    fn up_inside_an_archive_climbs_to_its_top_and_then_out_of_it() {
        let deep = NodeId::archive("/x/a.zip", "docs/inner");
        assert_eq!(deep.display_name(), "inner");
        let docs = deep.parent().unwrap();
        assert_eq!(docs, NodeId::archive("/x/a.zip", "docs"));
        let top = docs.parent().unwrap();
        assert_eq!(top, NodeId::archive("/x/a.zip", ""));
        assert_eq!(top.display_name(), "a.zip");
        assert_eq!(top.parent(), Some(NodeId::path("/x")));
    }
}
