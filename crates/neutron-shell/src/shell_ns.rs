//! Enumerating the shell namespace: This PC, Network, Control Panel, zips.
//!
//! # When this runs instead of the fast path
//!
//! Almost never, and that is the design. `FindFirstFileExW` handles every
//! ordinary directory at a fraction of the cost; this exists for the places
//! that are not directories at all. Explorer shows a single tree, but underneath
//! it two completely different mechanisms:
//!
//! * `C:\Users` is a filesystem directory. [`crate::fs`] lists it.
//! * "This PC" is a *namespace extension* — a COM object that answers
//!   `EnumObjects`. There is no path to walk. Nor for Control Panel, Network, or
//!   the inside of a zip, which the shell presents as a folder while the
//!   filesystem sees one opaque file.
//!
//! A file manager that only understands paths simply cannot show those places.
//!
//! # Identity
//!
//! Items are addressed by *parsing name* rather than PIDL — see the note on
//! [`neutron_core::NodeId::Shell`]. This module is where the two meet:
//! `SHParseDisplayName` turns a stored string back into the PIDL the shell
//! wants, and `SHGetNameFromIDList` turns a child PIDL into a string worth
//! storing.
//!
//! # Threading
//!
//! **STA pool only.** `IShellFolder` is apartment-threaded, and a namespace
//! extension can be arbitrary third-party code doing network I/O — enumerating
//! Network in particular waits on browser-service discovery.

use neutron_core::entry::{Entry, EntryKind, SyncState, attr};
use neutron_core::{EntryList, Namespace, NamespaceError, NodeId};

use windows::Win32::UI::Shell::Common::{ITEMIDLIST, STRRET};
use windows::Win32::UI::Shell::{
    ILCombine, IEnumIDList, IShellFolder, SHCONTF_FOLDERS, SHCONTF_NONFOLDERS, SHGDN_INFOLDER,
    SHGDN_NORMAL, SHGDNF, SHGetDesktopFolder, SHGetNameFromIDList, SHParseDisplayName,
    SIGDN_DESKTOPABSOLUTEPARSING,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::PCWSTR;

/// Attributes asked of each child in one batch.
///
/// `GetAttributesOf` takes a set of flags and answers only those, so asking for
/// everything at once costs one call per item instead of four.
mod sfgao {
    /// Has children — the thing that decides whether a row is navigable.
    pub const FOLDER: u32 = 0x2000_0000;
    /// Backed by a real file or directory, so it has a path.
    pub const FILESYSTEM: u32 = 0x4000_0000;
    /// The item is a *file* — a stream of bytes.
    ///
    /// This, combined with FOLDER, is what identifies an archive: a zip is both
    /// a file and something you can walk into. A drive or a directory is FOLDER
    /// without STREAM.
    ///
    /// `SFGAO_STORAGEANCESTOR` looks like the right flag and is not — it means
    /// "contains storage items", which every drive and directory also does. Used
    /// as the archive test it routed every drive down the shell path, so opening
    /// `C:` from This PC landed on a shell node instead of a filesystem one.
    pub const STREAM: u32 = 0x0040_0000;
    pub const HIDDEN: u32 = 0x0008_0000;
    pub const LINK: u32 = 0x0001_0000;
}

pub struct ShellNamespace;

impl Namespace for ShellNamespace {
    fn handles(&self, id: &NodeId) -> bool {
        matches!(id, NodeId::Shell { .. })
    }

    fn enumerate(&self, id: &NodeId) -> Result<EntryList, NamespaceError> {
        let parsing = id
            .parsing_name()
            .ok_or_else(|| NamespaceError::Unsupported(id.to_string()))?;
        enumerate_parsing_name(parsing)
    }
}

/// Lists the shell item named by `parsing`.
///
/// **STA pool only.**
pub fn enumerate_parsing_name(parsing: &str) -> Result<EntryList, NamespaceError> {
    let pidl = Pidl::parse(parsing)?;

    // SAFETY: standard shell initialisation; the desktop folder is a process
    // singleton and is not freed by us.
    let desktop: IShellFolder =
        unsafe { SHGetDesktopFolder() }.map_err(|e| NamespaceError::Other(e.message()))?;

    // The desktop is its own root, so binding to it would fail; everything else
    // binds through it.
    let folder: IShellFolder = if pidl.is_desktop() {
        desktop.clone()
    } else {
        // SAFETY: `pidl` is a live absolute PIDL owned by this scope.
        unsafe { desktop.BindToObject(pidl.0, None) }.map_err(|e| {
            let what = parsing.to_owned();
            match e.code().0 as u32 & 0xFFFF {
                5 => NamespaceError::AccessDenied(what),
                2 | 3 => NamespaceError::NotFound(what),
                _ => NamespaceError::Other(format!("{what}: {}", e.message())),
            }
        })?
    };

    // Folders *and* non-folders: a zip's contents include files, and Explorer
    // shows both. These are `_SHCONTF` newtypes with no BitOr, so the flags are
    // combined as the plain bits the API actually takes.
    let flags = (SHCONTF_FOLDERS.0 | SHCONTF_NONFOLDERS.0) as u32;
    let mut enumerator: Option<IEnumIDList> = None;
    // SAFETY: `folder` is live; `enumerator` is a valid out-param.
    unsafe {
        folder.EnumObjects(
            windows::Win32::Foundation::HWND(std::ptr::null_mut()),
            flags,
            &mut enumerator,
        )
    }
    .ok()
    .map_err(|e| NamespaceError::Other(e.message()))?;

    // An extension may report success and hand back nothing, which is an empty
    // folder rather than a failure.
    let Some(enumerator) = enumerator else {
        return Ok(EntryList::new());
    };

    let mut list = EntryList::with_capacity(64);
    let mut fetched = [std::ptr::null_mut::<ITEMIDLIST>(); 1];

    loop {
        let mut count = 0u32;
        // SAFETY: `fetched` is a valid one-element output buffer; each PIDL it
        // receives is owned by us and freed below.
        let more = unsafe { enumerator.Next(&mut fetched, Some(&mut count)) };
        if more.is_err() || count == 0 {
            break;
        }

        let child = Pidl(fetched[0]);
        if let Some((entry, target, is_path)) = describe(&folder, &child, &pidl, parsing) {
            list.push(&entry);
            list.push_target(&target, is_path);
        }
        // `child` frees itself here.
    }

    list.reset_order();
    Ok(list)
}

/// Turns one child PIDL into an entry plus where it leads.
fn describe(
    folder: &IShellFolder,
    child: &Pidl,
    parent: &Pidl,
    parent_parsing: &str,
) -> Option<(Entry, String, bool)> {
    let name = display_name(folder, child)?;

    let mut flags =
        sfgao::FOLDER | sfgao::FILESYSTEM | sfgao::STREAM | sfgao::HIDDEN | sfgao::LINK;
    // SAFETY: one live child PIDL; `flags` is in/out as the API documents.
    let queried = unsafe { folder.GetAttributesOf(&[child.0 as *const _], &mut flags) };
    if queried.is_err() {
        flags = 0;
    }

    let is_folder = flags & sfgao::FOLDER != 0;
    let is_filesystem = flags & sfgao::FILESYSTEM != 0;
    // A zip is FOLDER *and* STREAM: a real file the shell walks into. It must
    // be navigated through the shell rather than by path, or the listing would
    // try to enumerate a file as a directory and fail. A drive or directory is
    // FOLDER without STREAM and takes the fast path.
    let is_archive = is_folder && flags & sfgao::STREAM != 0;

    // The absolute parsing name is what identifies this child later.
    //
    // `IEnumIDList` yields *relative* PIDLs — one level, meaningful only against
    // their parent — so this has to combine them before asking the shell to name
    // it. Asking with the relative PIDL simply fails, and the earlier fallback
    // then produced `::{20D04FE0-…}\Local Disk (C:)`: a string that looks like a
    // path, parses like a path, and names nothing.
    let absolute = parent.combined_with(child);
    let parsing = absolute.as_ref().and_then(absolute_parsing_name);

    let (target, target_is_path) = match parsing {
        // Route by what the item *is*, not by what its string looks like.
        Some(parsing) if is_filesystem && !is_archive => (parsing, true),
        Some(parsing) => (parsing, false),
        // No absolute name available. Concatenation is how the shell itself
        // parses a path into a namespace extension, so it is a reasonable guess
        // — but only ever as a *shell* target. Handing a fabricated string to
        // the filesystem backend is how the wrong-path bug happened.
        None => (
            format!("{}\\{}", parent_parsing.trim_end_matches('\\'), name),
            false,
        ),
    };

    let kind = if is_folder {
        EntryKind::Directory
    } else if flags & sfgao::LINK != 0 {
        EntryKind::Symlink
    } else if is_filesystem || flags & sfgao::STREAM != 0 {
        // STREAM as well as FILESYSTEM: a file *inside a zip* is a stream with
        // no filesystem path of its own. Typed as Virtual it rendered as
        // "System" with a folder icon, because `EntryKind::Virtual` counts as a
        // container — so every file in an archive looked like a folder.
        EntryKind::File
    } else {
        // Control Panel applets and similar: real items with no file behind
        // them and nothing to enumerate.
        EntryKind::Virtual
    };

    let mut attrs = 0u32;
    if is_folder {
        attrs |= attr::DIRECTORY;
    }
    if flags & sfgao::HIDDEN != 0 {
        attrs |= attr::HIDDEN;
    }

    Some((
        Entry {
            name,
            kind,
            // The shell does not hand back size or timestamps from
            // `GetAttributesOf`, and asking per item through `IShellFolder2`
            // would cost a call per column per row. These places are browsed,
            // not audited; a blank column beats a slow listing.
            size: 0,
            modified: 0,
            created: 0,
            attrs,
            sync: SyncState::None,
        },
        target,
        target_is_path,
    ))
}

/// The child's name as shown inside its parent.
fn display_name(folder: &IShellFolder, child: &Pidl) -> Option<String> {
    let mut ret = STRRET::default();
    // SAFETY: live folder and child; `ret` is a valid out-param.
    let flags = SHGDNF(SHGDN_INFOLDER.0 | SHGDN_NORMAL.0);
    unsafe { folder.GetDisplayNameOf(child.0, flags, &mut ret) }.ok()?;

    // STRRET is a union whose discriminant says how to read it, and the helper
    // handles all three forms — including the offset-into-the-PIDL case, which a
    // naive read of the pointer member would get wrong.
    let mut wide = windows::core::PWSTR::null();
    // SAFETY: `ret` was filled by the call above for this child, and `wide` is
    // a valid out-param that receives a shell allocation.
    unsafe { windows::Win32::UI::Shell::StrRetToStrW(&mut ret, Some(child.0), &mut wide) }.ok()?;
    if wide.is_null() {
        return None;
    }

    // SAFETY: NUL-terminated on success; freed immediately after copying.
    let name = unsafe { wide.to_string() }.ok();
    // SAFETY: allocated by the shell with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(Some(wide.0 as *const _)) };
    name
}

/// The child's absolute parsing name, which is what identifies it later.
fn absolute_parsing_name(child: &Pidl) -> Option<String> {
    // SAFETY: live PIDL; the returned string is freed below.
    let raw = unsafe { SHGetNameFromIDList(child.0, SIGDN_DESKTOPABSOLUTEPARSING) }.ok()?;
    if raw.is_null() {
        return None;
    }
    // SAFETY: NUL-terminated on success.
    let name = unsafe { raw.to_string() }.ok();
    // SAFETY: allocated by the shell.
    unsafe { CoTaskMemFree(Some(raw.0 as *const _)) };
    name.filter(|s| !s.is_empty())
}

/// Resolves a parsing name to a display name, for titling a tab.
///
/// **STA pool only.**
pub fn display_name_of(parsing: &str) -> Option<String> {
    let pidl = Pidl::parse(parsing).ok()?;
    // SAFETY: live PIDL; the returned string is freed below.
    let raw = unsafe { SHGetNameFromIDList(pidl.0, windows::Win32::UI::Shell::SIGDN_NORMALDISPLAY) }
        .ok()?;
    if raw.is_null() {
        return None;
    }
    // SAFETY: NUL-terminated on success.
    let name = unsafe { raw.to_string() }.ok();
    // SAFETY: allocated by the shell.
    unsafe { CoTaskMemFree(Some(raw.0 as *const _)) };
    name.filter(|s| !s.is_empty())
}

/// Whether the shell treats `path` as a folder it can enumerate.
///
/// True for directories, and — the reason this exists — for archives. Opening a
/// `.zip` should walk into it the way Explorer does, and nothing in the file's
/// own attributes says so; only the shell knows a namespace handler is
/// registered for that extension.
///
/// **STA pool only.**
pub fn is_shell_container(path: &str) -> bool {
    let Ok(pidl) = Pidl::parse(path) else {
        return false;
    };
    let Ok(desktop) = (unsafe { SHGetDesktopFolder() }) else {
        return false;
    };

    let _ = desktop;

    let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: live absolute PIDL; the child pointer borrows from it and must
    // not be freed separately.
    let Ok(parent) =
        (unsafe { windows::Win32::UI::Shell::SHBindToParent::<IShellFolder>(pidl.0, Some(&mut child)) })
    else {
        return false;
    };
    let mut flags = sfgao::FOLDER | sfgao::STREAM;
    // SAFETY: live parent folder and a child PIDL borrowed from `pidl`.
    if unsafe { parent.GetAttributesOf(&[child as *const _], &mut flags) }.is_err() {
        return false;
    }
    flags & sfgao::FOLDER != 0
}

/// An absolute PIDL from the shell allocator, freed on drop.
struct Pidl(*mut ITEMIDLIST);

impl Pidl {
    fn parse(parsing: &str) -> Result<Self, NamespaceError> {
        let wide: Vec<u16> = parsing.encode_utf16().chain(std::iter::once(0)).collect();
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();

        // SAFETY: `wide` is NUL-terminated and outlives the call; on success the
        // out-param owns a shell allocation freed by this type's Drop.
        unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None) }
            .map_err(|e| match e.code().0 as u32 & 0xFFFF {
                2 | 3 => NamespaceError::NotFound(parsing.to_owned()),
                5 => NamespaceError::AccessDenied(parsing.to_owned()),
                _ => NamespaceError::Other(format!("{parsing}: {}", e.message())),
            })?;

        if pidl.is_null() {
            return Err(NamespaceError::NotFound(parsing.to_owned()));
        }
        Ok(Pidl(pidl))
    }

    /// Joins a relative child PIDL onto this absolute one.
    fn combined_with(&self, child: &Pidl) -> Option<Pidl> {
        // SAFETY: both PIDLs are live; the result is a fresh shell allocation
        // this type owns.
        let joined = unsafe { ILCombine(Some(self.0), Some(child.0)) };
        (!joined.is_null()).then_some(Pidl(joined))
    }

    /// The desktop's PIDL is empty — a zero-length item list.
    fn is_desktop(&self) -> bool {
        // SAFETY: a valid PIDL always addresses at least the terminating
        // two-byte zero, which is what an empty list consists of.
        unsafe { (*self.0).mkid.cb == 0 }
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by the shell.
            unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
        }
    }
}

/// The shell locations worth pinning, as `(parsing name, display name)`.
///
/// CLSIDs rather than localised names: `::{20D04FE0-…}` is "This PC" in every
/// language and every Windows version, while the display string is neither.
pub const WELL_KNOWN: &[(&str, &str)] = &[
    ("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", "This PC"),
    ("::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}", "Network"),
    ("::{26EE0668-A00A-44D7-9371-BEB064C98683}", "Control Panel"),
    ("::{645FF040-5081-101B-9F08-00AA002F954E}", "Recycle Bin"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_roots_use_clsids_not_names() {
        // A localised display name is not an identity: "This PC" is "Dieser PC"
        // on a German install and was "Computer" before Windows 8. The CLSID is
        // the same everywhere.
        for (parsing, display) in WELL_KNOWN {
            assert!(
                parsing.starts_with("::{"),
                "{display} is addressed by name rather than CLSID"
            );
            assert!(!display.is_empty());
        }
    }

    #[test]
    fn well_known_roots_are_distinct() {
        for (i, (a, _)) in WELL_KNOWN.iter().enumerate() {
            for (b, _) in &WELL_KNOWN[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn the_namespace_claims_only_shell_nodes() {
        // Claiming a path would route ordinary directories through COM and
        // throw away the entire fast-path performance argument.
        let ns = ShellNamespace;
        assert!(ns.handles(&NodeId::shell("::{X}", "X")));
        assert!(!ns.handles(&NodeId::Path(std::path::PathBuf::from(r"C:\"))));
    }
}
