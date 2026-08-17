//! Fast filesystem enumeration via `FindFirstFileExW`.
//!
//! This is the hot path — the overwhelming majority of locations a user browses
//! are plain directories, and they must not pay COM cost. Three choices matter
//! for throughput:
//!
//! * `FindExInfoBasic` skips the 8.3 short-name lookup, which the filesystem
//!   would otherwise compute per entry and which nothing in Neutron reads.
//! * `FIND_FIRST_EX_LARGE_FETCH` batches directory data from the kernel instead
//!   of round-tripping per entry.
//! * `\\?\` prefixing disables Win32 path parsing, which both lifts the
//!   260-character `MAX_PATH` limit and skips per-call normalization.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use neutron_core::entry::{Entry, EntryKind, SyncState, attr};
use neutron_core::{EntryList, Namespace, NamespaceError, NodeId};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, ERROR_PATH_NOT_FOUND, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic,
    FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, WIN32_FIND_DATAW,
};
use windows::Win32::System::SystemServices::{IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK};
use windows::core::PCWSTR;

/// Difference between the Windows epoch (1601-01-01) and the Unix epoch, in
/// seconds.
const EPOCH_DELTA_SECS: i64 = 11_644_473_600;

pub struct FsNamespace;

impl Namespace for FsNamespace {
    fn handles(&self, id: &NodeId) -> bool {
        id.is_filesystem()
    }

    fn enumerate(&self, id: &NodeId) -> Result<EntryList, NamespaceError> {
        let path = id
            .as_path()
            .ok_or_else(|| NamespaceError::Unsupported(id.to_string()))?;
        enumerate_dir(path)
    }
}

/// Lists `dir`, skipping `.` and `..`.
pub fn enumerate_dir(dir: &Path) -> Result<EntryList, NamespaceError> {
    let pattern = search_pattern(dir);

    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `pattern` is a NUL-terminated UTF-16 buffer that outlives the
    // call, and `data` is a valid writable WIN32_FIND_DATAW.
    let handle = unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            &mut data as *mut _ as *mut core::ffi::c_void,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };

    let handle: HANDLE = match handle {
        Ok(h) => h,
        Err(e) => {
            let code = e.code().0 as u32 & 0xFFFF;
            let what = dir.display().to_string();
            return Err(match code {
                c if c == ERROR_FILE_NOT_FOUND.0 || c == ERROR_PATH_NOT_FOUND.0 => {
                    NamespaceError::NotFound(what)
                }
                c if c == ERROR_ACCESS_DENIED.0 => NamespaceError::AccessDenied(what),
                // An empty directory still yields `.`/`..`, so NO_MORE_FILES
                // here means the directory exists but matched nothing.
                c if c == ERROR_NO_MORE_FILES.0 => return Ok(EntryList::new()),
                _ => NamespaceError::Other(format!("{what}: {e}")),
            });
        }
    };

    // Guard so every early return below still closes the find handle.
    let _guard = FindHandle(handle);

    // Most directories are small; large ones grow the arena a few times, which
    // is cheaper than a syscall to count entries up front.
    let mut list = EntryList::with_capacity(64);
    loop {
        if let Some(entry) = convert(&data) {
            list.push(&entry);
        }

        // SAFETY: `handle` is live until `_guard` drops.
        if unsafe { FindNextFileW(handle, &mut data) }.is_err() {
            // Any error other than NO_MORE_FILES still ends the walk, but the
            // entries gathered so far are returned rather than discarded — a
            // partial listing beats an error page.
            break;
        }
    }

    list.reset_order();
    Ok(list)
}

/// RAII wrapper so `FindClose` runs on every exit path.
struct FindHandle(HANDLE);

impl Drop for FindHandle {
    fn drop(&mut self) {
        // SAFETY: constructed only from a handle FindFirstFileExW returned Ok.
        let _ = unsafe { FindClose(self.0) };
    }
}

/// Builds the `\\?\<dir>\*` search pattern as a NUL-terminated UTF-16 buffer.
fn search_pattern(dir: &Path) -> Vec<u16> {
    let mut s: Vec<u16> = Vec::with_capacity(280);

    // Only prefix paths that are already absolute and not already prefixed.
    // `\\?\` disables all path parsing, so a relative path would break.
    let text = dir.as_os_str();
    let needs_prefix = {
        let bytes: Vec<u16> = text.encode_wide().take(4).collect();
        let is_prefixed = bytes.starts_with(&['\\' as u16, '\\' as u16, '?' as u16, '\\' as u16]);
        !is_prefixed && dir.is_absolute()
    };

    if needs_prefix {
        // UNC paths need `\\?\UNC\server\share`, not `\\?\\\server\share`.
        let wide: Vec<u16> = text.encode_wide().collect();
        if wide.starts_with(&['\\' as u16, '\\' as u16]) {
            s.extend("\\\\?\\UNC\\".encode_utf16());
            s.extend(&wide[2..]);
        } else {
            s.extend("\\\\?\\".encode_utf16());
            s.extend(&wide);
        }
    } else {
        s.extend(text.encode_wide());
    }

    if s.last() != Some(&('\\' as u16)) {
        s.push('\\' as u16);
    }
    s.push('*' as u16);
    s.push(0);
    s
}

/// Converts one find record, returning `None` for the `.` and `..` pseudo-entries.
fn convert(data: &WIN32_FIND_DATAW) -> Option<Entry> {
    let name = wide_to_string(&data.cFileName);
    if name == "." || name == ".." || name.is_empty() {
        return None;
    }

    let attrs = data.dwFileAttributes;
    let is_dir = attrs & attr::DIRECTORY != 0;

    let kind = if attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        // dwReserved0 carries the reparse tag when the reparse-point bit is
        // set. Distinguishing these matters: a junction can point at an
        // ancestor, and a recursive walk that treats it as a plain directory
        // will loop forever.
        match data.dwReserved0 {
            IO_REPARSE_TAG_SYMLINK => EntryKind::Symlink,
            IO_REPARSE_TAG_MOUNT_POINT => EntryKind::Junction,
            _ if is_dir => EntryKind::Directory,
            _ => EntryKind::File,
        }
    } else if is_dir {
        EntryKind::Directory
    } else {
        EntryKind::File
    };

    // Cloud placeholders must be detected from attributes alone. Calling
    // anything that opens the file would trigger a multi-megabyte download just
    // to list a folder.
    //
    // Three attributes mean the same thing here: OneDrive marks dehydrated
    // files with the recall bits, while older providers and archived files use
    // OFFLINE. All of them mean "the bytes are not on this disk".
    const NOT_LOCAL: u32 = attr::RECALL_ON_OPEN | attr::RECALL_ON_DATA_ACCESS | attr::OFFLINE;
    let sync = if attrs & NOT_LOCAL != 0 {
        SyncState::CloudOnly
    } else {
        SyncState::None
    };

    Some(Entry {
        name,
        kind,
        size: if is_dir {
            0
        } else {
            ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64
        },
        modified: filetime_to_unix_millis(
            data.ftLastWriteTime.dwHighDateTime,
            data.ftLastWriteTime.dwLowDateTime,
        ),
        created: filetime_to_unix_millis(
            data.ftCreationTime.dwHighDateTime,
            data.ftCreationTime.dwLowDateTime,
        ),
        attrs,
        sync,
    })
}

/// FILETIME (100ns ticks since 1601-01-01 UTC) to Unix milliseconds.
fn filetime_to_unix_millis(high: u32, low: u32) -> i64 {
    let ticks = ((high as u64) << 32 | low as u64) as i64;
    if ticks == 0 {
        return 0;
    }
    ticks / 10_000 - EPOCH_DELTA_SECS * 1_000
}

/// Decodes a fixed-size UTF-16 field, stopping at the first NUL.
///
/// Lossy on purpose: NTFS permits unpaired surrogates in filenames, and a file
/// that cannot be named is worse than one named with a replacement character.
fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_converts_to_the_unix_epoch() {
        // 1970-01-01T00:00:00Z in FILETIME ticks.
        let ticks: u64 = 11_644_473_600 * 10_000_000;
        assert_eq!(
            filetime_to_unix_millis((ticks >> 32) as u32, ticks as u32),
            0
        );
    }

    #[test]
    fn zero_filetime_stays_zero() {
        // Some filesystems leave creation time unset; it must not become a
        // large negative 1601 date.
        assert_eq!(filetime_to_unix_millis(0, 0), 0);
    }

    #[test]
    fn wide_decoding_stops_at_nul() {
        let mut buf = [0u16; 260];
        for (i, c) in "hi.txt".encode_utf16().enumerate() {
            buf[i] = c;
        }
        assert_eq!(wide_to_string(&buf), "hi.txt");
    }

    #[test]
    fn search_pattern_prefixes_and_appends_wildcard() {
        let p = search_pattern(Path::new(r"C:\Windows"));
        let s = String::from_utf16_lossy(&p[..p.len() - 1]);
        assert_eq!(s, r"\\?\C:\Windows\*");
    }

    #[test]
    fn search_pattern_handles_a_drive_root_without_doubling_the_slash() {
        let p = search_pattern(Path::new(r"C:\"));
        let s = String::from_utf16_lossy(&p[..p.len() - 1]);
        assert_eq!(s, r"\\?\C:\*");
    }

    #[test]
    fn unc_paths_use_the_unc_prefix_form() {
        let p = search_pattern(Path::new(r"\\server\share"));
        let s = String::from_utf16_lossy(&p[..p.len() - 1]);
        assert_eq!(s, r"\\?\UNC\server\share\*");
    }

    #[test]
    fn already_prefixed_paths_are_not_double_prefixed() {
        let p = search_pattern(Path::new(r"\\?\C:\Windows"));
        let s = String::from_utf16_lossy(&p[..p.len() - 1]);
        assert_eq!(s, r"\\?\C:\Windows\*");
    }
}
