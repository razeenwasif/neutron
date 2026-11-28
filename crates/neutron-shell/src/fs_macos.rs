//! Local directory enumeration on macOS, preserving symlink identity.
use neutron_core::{Entry, EntryKind, EntryList, Namespace, NamespaceError, NodeId};
use neutron_core::entry::{attr, SyncState};
use std::os::macos::fs::MetadataExt;
use std::path::Path;

const UF_HIDDEN: u32 = 0x8000;
/// Set on a file whose contents live only in the cloud (an evicted iCloud
/// Drive item). Reading it triggers a download, so it is shown as cloud-only.
const SF_DATALESS: u32 = 0x4000_0000;

pub struct FsNamespace;
impl Namespace for FsNamespace {
    fn handles(&self, id: &NodeId) -> bool { id.is_filesystem() }
    fn enumerate(&self, id: &NodeId) -> Result<EntryList, NamespaceError> {
        let path = id.as_path().ok_or_else(|| NamespaceError::Unsupported(id.to_string()))?;
        match enumerate_bulk(path) {
            Ok(list) => Ok(list),
            Err(e) if matches!(e.raw_os_error(), Some(libc::ENOTSUP | libc::EINVAL)) => {
                enumerate_fallback(path).map_err(Into::into)
            }
            Err(e) => Err(e.into()),
        }
    }
}

const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_CRTIME: u32 = 0x0000_0200;
const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
const ATTR_CMN_FNDRINFO: u32 = 0x0000_4000;
const ATTR_CMN_ACCESSMASK: u32 = 0x0002_0000;
const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
const ATTR_CMN_ADDEDTIME: u32 = 0x1000_0000;
const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

#[repr(C)]
#[derive(Clone, Copy)]
struct AttrSet {
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

unsafe extern "C" {
    fn getattrlistbulk(
        dirfd: libc::c_int,
        alist: *mut libc::attrlist,
        attributeBuffer: *mut libc::c_void,
        bufferSize: libc::size_t,
        options: libc::c_ulong,
    ) -> libc::c_int;
}

struct OwnedFd(libc::c_int);
impl Drop for OwnedFd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe { libc::close(self.0) };
        }
    }
}

#[inline]
fn read_unaligned<T: Copy>(buf: &[u8], off: &mut usize) -> Option<T> {
    let sz = std::mem::size_of::<T>();
    if *off + sz > buf.len() {
        return None;
    }
    let val = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(*off).cast::<T>()) };
    *off += sz;
    Some(val)
}

#[inline]
fn ts_to_millis(sec: i64, nsec: i64) -> i64 {
    sec.saturating_mul(1000).saturating_add(nsec / 1_000_000)
}

/// Fast macOS directory enumeration via `getattrlistbulk(2)`: fetches name,
/// vnode type, timestamps, Finder info, permissions, BSD flags, added time,
/// and file length in bulk batches without per-entry `lstat`/`getattrlist`/`getxattr`.
fn enumerate_bulk(path: &Path) -> Result<EntryList, std::io::Error> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let raw_fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if raw_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = OwnedFd(raw_fd);

    let mut req = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_NAME
            | ATTR_CMN_OBJTYPE
            | ATTR_CMN_CRTIME
            | ATTR_CMN_MODTIME
            | ATTR_CMN_FNDRINFO
            | ATTR_CMN_ACCESSMASK
            | ATTR_CMN_FLAGS
            | ATTR_CMN_ADDEDTIME,
        volattr: 0,
        dirattr: 0,
        fileattr: ATTR_FILE_DATALENGTH,
        forkattr: 0,
    };

    let mut list = EntryList::new();
    let mut buf = vec![0u8; 256 * 1024];

    loop {
        let count = unsafe {
            getattrlistbulk(
                fd.0,
                &mut req,
                buf.as_mut_ptr().cast(),
                buf.len(),
                libc::FSOPT_NOFOLLOW as u64,
            )
        };
        if count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if count == 0 {
            break;
        }

        let mut rec_off = 0usize;
        for _ in 0..count {
            if rec_off + 4 > buf.len() {
                break;
            }
            let rec_len = unsafe {
                std::ptr::read_unaligned(buf.as_ptr().add(rec_off).cast::<u32>())
            } as usize;
            if rec_len < 24 || rec_off + rec_len > buf.len() {
                break;
            }
            let rec = &buf[rec_off..rec_off + rec_len];
            rec_off += rec_len;

            let mut off = 4usize;
            let Some(ret) = read_unaligned::<AttrSet>(rec, &mut off) else { continue };
            let cmn = ret.commonattr;

            let mut name_owned: Option<String> = None;
            if cmn & ATTR_CMN_NAME != 0 {
                let name_field_off = off;
                let Some(data_offset) = read_unaligned::<i32>(rec, &mut off) else { continue };
                let Some(data_len) = read_unaligned::<u32>(rec, &mut off) else { continue };
                let str_start = (name_field_off as isize + data_offset as isize) as usize;
                let str_end = str_start.saturating_add(data_len as usize).min(rec.len());
                if str_start <= str_end {
                    let raw = &rec[str_start..str_end];
                    let trimmed = match raw.iter().position(|&b| b == 0) {
                        Some(z) => &raw[..z],
                        None => raw,
                    };
                    name_owned = Some(String::from_utf8_lossy(trimmed).into_owned());
                }
            }
            let Some(name) = name_owned else { continue };
            if name.is_empty() || name == "." || name == ".." {
                continue;
            }

            let mut obj_type = VREG;
            if cmn & ATTR_CMN_OBJTYPE != 0 {
                if let Some(t) = read_unaligned::<u32>(rec, &mut off) {
                    obj_type = t;
                }
            }

            let mut created = 0i64;
            if cmn & ATTR_CMN_CRTIME != 0 {
                if let (Some(sec), Some(nsec)) = (
                    read_unaligned::<i64>(rec, &mut off),
                    read_unaligned::<i64>(rec, &mut off),
                ) {
                    created = ts_to_millis(sec, nsec);
                }
            }

            let mut modified = 0i64;
            if cmn & ATTR_CMN_MODTIME != 0 {
                if let (Some(sec), Some(nsec)) = (
                    read_unaligned::<i64>(rec, &mut off),
                    read_unaligned::<i64>(rec, &mut off),
                ) {
                    modified = ts_to_millis(sec, nsec);
                }
            }

            let mut alias = false;
            let mut has_label_color = false;
            if cmn & ATTR_CMN_FNDRINFO != 0 {
                if let Some(fndr) = read_unaligned::<[u8; 32]>(rec, &mut off) {
                    let fndr_flags = u16::from_be_bytes([fndr[8], fndr[9]]);
                    alias = (fndr_flags & 0x8000) != 0;
                    has_label_color = (fndr_flags & 0x000E) != 0;
                }
            }

            let mut readonly = false;
            if cmn & ATTR_CMN_ACCESSMASK != 0 {
                if let Some(mode) = read_unaligned::<u32>(rec, &mut off) {
                    readonly = (mode & 0o222) == 0;
                }
            }

            let mut st_flags = 0u32;
            if cmn & ATTR_CMN_FLAGS != 0 {
                if let Some(flags) = read_unaligned::<u32>(rec, &mut off) {
                    st_flags = flags;
                }
            }

            let mut added = 0i64;
            if cmn & ATTR_CMN_ADDEDTIME != 0 {
                if let (Some(sec), Some(nsec)) = (
                    read_unaligned::<i64>(rec, &mut off),
                    read_unaligned::<i64>(rec, &mut off),
                ) {
                    added = ts_to_millis(sec, nsec);
                }
            }

            let mut size = 0u64;
            if ret.fileattr & ATTR_FILE_DATALENGTH != 0 {
                if let Some(len) = read_unaligned::<i64>(rec, &mut off) {
                    size = len.max(0) as u64;
                }
            }

            let link = obj_type == VLNK;
            let is_dir_raw = obj_type == VDIR;
            let mut directory = is_dir_raw;

            // Only materialize a Child PathBuf when an entry is a symlink, a
            // directory with an extension (possible .app/.rtfd package), a tagged
            // item, or a file with a never-before-seen extension.
            let mut child_buf: Option<std::path::PathBuf> = None;

            if link {
                let cp = child_buf.get_or_insert_with(|| path.join(&name)).as_path();
                directory = cp.is_dir();
                if directory && is_package(cp, true) {
                    directory = false;
                }
            } else if directory && name.contains('.') {
                let cp = child_buf.get_or_insert_with(|| path.join(&name)).as_path();
                if is_package(cp, false) {
                    directory = false;
                }
            }

            let mut attrs = 0u32;
            if directory { attrs |= attr::DIRECTORY; }
            if link { attrs |= attr::REPARSE_POINT; }
            if name.starts_with('.') || (st_flags & UF_HIDDEN) != 0 { attrs |= attr::HIDDEN; }
            if readonly { attrs |= attr::READONLY; }
            if has_label_color {
                let cp = child_buf.get_or_insert_with(|| path.join(&name)).as_path();
                attrs |= (crate::tags::colors(cp) as u32) << attr::TAG_SHIFT;
            }

            let kind = if directory {
                EntryKind::Directory
            } else if link {
                EntryKind::Symlink
            } else {
                EntryKind::File
            };
            if kind == EntryKind::File
                && let Some((_, ext)) = name.rsplit_once('.')
                && !neutron_core::kinds::known(ext)
            {
                let cp = child_buf.get_or_insert_with(|| path.join(&name)).as_path();
                learn_kind(cp);
            }

            list.push(&Entry {
                name,
                kind,
                size,
                attrs,
                modified,
                created,
                sync: sync_state(st_flags),
            });
            let idx = list.len() - 1;
            if alias || link {
                list.mark(idx, attr::ALIAS);
            }
            list.set_added(idx, added);
        }
    }

    Ok(list)
}

fn enumerate_fallback(path: &Path) -> Result<EntryList, std::io::Error> {
    let mut list = EntryList::new();
    for entry in std::fs::read_dir(path)? {
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.path().symlink_metadata() else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let link = meta.file_type().is_symlink();
        let mut directory = meta.is_dir() || (link && entry.path().is_dir());
        if directory && is_package(&entry.path(), link) { directory = false; }
        let mut attrs = 0;
        if directory { attrs |= attr::DIRECTORY; }
        if link { attrs |= attr::REPARSE_POINT; }
        if name.starts_with('.') || meta.st_flags() & UF_HIDDEN != 0 { attrs |= attr::HIDDEN; }
        if meta.permissions().readonly() { attrs |= attr::READONLY; }
        attrs |= (crate::tags::colors(&entry.path()) as u32) << attr::TAG_SHIFT;
        let kind = if directory { EntryKind::Directory }
            else if link { EntryKind::Symlink } else { EntryKind::File };
        if kind == EntryKind::File {
            learn_kind(&entry.path());
        }
        list.push(&Entry { name, kind, size: meta.len(), attrs,
            modified: millis(meta.modified()), created: millis(meta.created()),
            sync: sync_state(meta.st_flags()) });
        let (added, alias) = facts(&entry.path());
        if alias {
            let i = list.len() - 1;
            list.mark(i, attr::ALIAS);
        }
        list.set_added(list.len() - 1, added);
    }
    Ok(list)
}

/// The sync state implied by a file's BSD flags.
fn sync_state(flags: u32) -> SyncState {
    if flags & SF_DATALESS != 0 { SyncState::CloudOnly } else { SyncState::None }
}

/// Whether the directory at `path` is a package, by LaunchServices' account —
/// which covers registered package types (`.app`, `.rtfd`, `.pages`,
/// `.photoslibrary`, and whatever installed applications declare) as well as
/// folders carrying the bundle bit.
///
/// Only directories with an extension are asked. Every package type is
/// declared by extension, and the lookup costs a LaunchServices round trip;
/// asking for every folder would make a `node_modules` listing pay it
/// thousands of times to find none. A bundle-bit folder with no extension is
/// rare enough to go on browsing as a folder.
fn is_package(path: &Path, link: bool) -> bool {
    use objc2_foundation::{NSNumber, NSString, NSURL, NSURLIsPackageKey};
    // A link's own resource values describe the link, so ask about the target.
    let target = if link { match path.canonicalize() { Ok(t) => t, Err(_) => return false } }
        else { path.to_path_buf() };
    if target.extension().is_none() { return false; }
    let Some(target) = target.to_str() else { return false };
    objc2::rc::autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(target), true);
        let mut value = None;
        // SAFETY: `NSURLIsPackageKey` yields an `NSNumber`, checked by the
        // downcast below rather than assumed.
        let read = unsafe { url.getResourceValue_forKey_error(&mut value, NSURLIsPackageKey) };
        read.is_ok() && value.and_then(|v| v.downcast::<NSNumber>().ok()).is_some_and(|n| n.boolValue())
    })
}

/// Learns what the system calls files like `path`, if its extension is new.
fn learn_kind(path: &Path) {
    use objc2_foundation::{NSString, NSURL, NSURLLocalizedTypeDescriptionKey};
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else { return };
    if neutron_core::kinds::known(ext) {
        return;
    }
    let Some(text) = path.to_str() else { return };
    let name = objc2::rc::autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(text));
        let mut value = None;
        // SAFETY: the localized type description is an `NSString`, checked
        // by the downcast rather than assumed.
        let read = unsafe { url.getResourceValue_forKey_error(&mut value, NSURLLocalizedTypeDescriptionKey) };
        read.ok()
            .and(value)
            .and_then(|v| v.downcast::<NSString>().ok())
            .map(|s| s.to_string())
    });
    // Learned even when the system has nothing better than a generic name,
    // so the same extension is not asked about again.
    neutron_core::kinds::learn(ext, name.unwrap_or_else(|| format!("{} file", ext.to_uppercase())));
}

/// When `path` was put in the folder it is in, in milliseconds — 0 when the
/// volume does not keep it — and whether it is a Finder alias. One
/// `getattrlist`, as cheap as the stat before it.
fn facts(path: &Path) -> (i64, bool) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return (0, false) };
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS | libc::ATTR_CMN_FNDRINFO | libc::ATTR_CMN_ADDEDTIME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    // A length, which attributes came back, then each that did in the order
    // of their bits: the Finder's thirty-two bytes, then the time.
    let mut buffer = [0u8; 96];
    // SAFETY: a valid path, request and buffer of the stated size.
    let rc = unsafe {
        libc::getattrlist(
            c_path.as_ptr(),
            (&mut request as *mut libc::attrlist).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            libc::FSOPT_NOFOLLOW,
        )
    };
    if rc != 0 {
        return (0, false);
    }
    let returned = u32::from_ne_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]);
    let mut at = 24;
    let mut alias = false;
    if returned & libc::ATTR_CMN_FNDRINFO != 0 {
        // The Finder's flags, big-endian, eight bytes in; `kIsAlias` is the
        // top bit.
        alias = u16::from_be_bytes([buffer[at + 8], buffer[at + 9]]) & 0x8000 != 0;
        at += 32;
    }
    if returned & libc::ATTR_CMN_ADDEDTIME == 0 {
        return (0, alias);
    }
    let seconds = i64::from_ne_bytes(buffer[at..at + 8].try_into().unwrap_or([0; 8]));
    let nanos = i64::from_ne_bytes(buffer[at + 8..at + 16].try_into().unwrap_or([0; 8]));
    (seconds.saturating_mul(1000) + nanos / 1_000_000, alias)
}

/// When `path` was put in its folder, in milliseconds; 0 when not known.
pub fn added(path: &Path) -> i64 {
    facts(path).0
}

/// What an alias or a symbolic link stands for. Blocks on the disk.
pub fn original(path: &Path) -> Result<std::path::PathBuf, String> {
    if let Ok(target) = std::fs::read_link(path) {
        let target = if target.is_relative() { path.parent().unwrap_or(Path::new("/")).join(target) } else { target };
        return if target.symlink_metadata().is_ok() {
            Ok(target)
        } else {
            Err(format!("{} is not there any more", target.display()))
        };
    }
    use objc2_foundation::{NSString, NSURL, NSURLBookmarkResolutionOptions};
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let resolved = NSURL::URLByResolvingAliasFileAtURL_options_error(&url, NSURLBookmarkResolutionOptions::WithoutUI)
        .map_err(|e| e.localizedDescription().to_string())?;
    let target = resolved.path().map(|p| std::path::PathBuf::from(p.to_string())).ok_or("the original has no path")?;
    if target == path {
        return Err("this is not an alias".into());
    }
    Ok(target)
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn MDItemCreate(allocator: *const std::ffi::c_void, path: *const objc2::runtime::AnyObject) -> *mut std::ffi::c_void;
    fn MDItemCopyAttribute(item: *mut std::ffi::c_void, name: *const objc2::runtime::AnyObject) -> *mut objc2::runtime::AnyObject;
    fn CFRelease(object: *const std::ffi::c_void);
}

/// When `path` was last opened, as Spotlight recorded it, in milliseconds; 0
/// when it never was or Spotlight does not know the file. A Spotlight lookup
/// per file, so it is asked only when a listing is sorted or grouped by it.
pub fn last_opened(path: &Path) -> i64 {
    use objc2_foundation::{NSDate, NSString};
    let Some(text) = path.to_str() else { return 0 };
    objc2::rc::autoreleasepool(|_| {
        let path = NSString::from_str(text);
        let key = NSString::from_str("kMDItemLastUsedDate");
        // SAFETY: NSString is toll-free bridged to CFString. The item and the
        // copied attribute are owned here (the Create and Copy rules) and
        // released below or by `Retained`.
        unsafe {
            let item = MDItemCreate(std::ptr::null(), (&*path as *const NSString).cast());
            if item.is_null() {
                return 0;
            }
            let value = MDItemCopyAttribute(item, (&*key as *const NSString).cast());
            CFRelease(item);
            let Some(value) = objc2::rc::Retained::from_raw(value) else { return 0 };
            value
                .downcast::<NSDate>()
                .map_or(0, |date| (date.timeIntervalSince1970() * 1000.0) as i64)
        }
    })
}

fn millis(time: std::io::Result<std::time::SystemTime>) -> i64 {
    time.ok().map(|t| match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_millis().min(i64::MAX as u128) as i64),
    }).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_file_has_its_date_added() {
        let dir = std::env::temp_dir().join(format!("neutron-added-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let now = millis(Ok(std::time::SystemTime::now()));
        let (added, alias) = facts(&dir.join("a.txt"));
        assert!((now - added).abs() < 60_000, "added {added}, now {now}");
        assert!(!alias);
        assert_eq!(facts(&dir.join("missing")), (0, false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_alias_and_a_link_are_known_and_lead_to_their_originals() {
        use objc2_foundation::{NSString, NSURL, NSURLBookmarkCreationOptions, NSURLBookmarkFileCreationOptions};
        let dir = std::env::temp_dir().join(format!("neutron-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("deep")).unwrap();
        let real = dir.join("deep/real.txt").canonicalize().unwrap_or_else(|_| {
            std::fs::write(dir.join("deep/real.txt"), "x").unwrap();
            dir.join("deep/real.txt").canonicalize().unwrap()
        });
        let url = |p: &Path| NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy()));
        let bookmark = url(&real)
            .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
                NSURLBookmarkCreationOptions::SuitableForBookmarkFile,
                None,
                None,
            )
            .unwrap();
        NSURL::writeBookmarkData_toURL_options_error(&bookmark, &url(&dir.join("alias")), 0 as NSURLBookmarkFileCreationOptions).unwrap();
        std::os::unix::fs::symlink("deep/real.txt", dir.join("link")).unwrap();

        assert!(facts(&dir.join("alias")).1, "the Finder's flag says alias");
        assert_eq!(original(&dir.join("alias")).unwrap(), real);
        assert_eq!(original(&dir.join("link")).unwrap().canonicalize().unwrap(), real);
        assert!(original(&real).is_err(), "a plain file is nobody's alias");
        let list = FsNamespace.enumerate(&NodeId::path(&dir)).unwrap();
        let at = |n: &str| (0..list.len()).find(|&i| list.name(i) == n).unwrap();
        assert!(list.is_alias(at("alias")) && list.is_alias(at("link")) && !list.is_alias(at("deep")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Needs Spotlight and something it has seen opened lately.
    #[test]
    #[ignore]
    fn spotlight_says_when_a_used_file_was_last_opened() {
        let out = std::process::Command::new("/usr/bin/mdfind")
            .arg("kMDItemLastUsedDate > $time.today(-30)")
            .output()
            .unwrap();
        let first = String::from_utf8_lossy(&out.stdout).lines().next().map(str::to_owned).expect("something used lately");
        let opened = last_opened(Path::new(&first));
        assert!(opened > 1_600_000_000_000, "{first}: {opened}");
        assert_eq!(last_opened(Path::new("/definitely/not/here")), 0);
    }

    #[test]
    fn bulk_enumerate_captures_metadata_aliases_hidden_and_tags() {
        let dir = std::env::temp_dir().join(format!("neutron-bulk-enum-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let file = dir.join("hello.txt");
        std::fs::write(&file, "123456789").unwrap();
        std::fs::write(dir.join(".secret"), "h").unwrap();
        std::os::unix::fs::symlink("hello.txt", dir.join("sym")).unwrap();
        crate::tags::toggle_color(std::slice::from_ref(&file), 6, true).unwrap();

        let list = FsNamespace.enumerate(&NodeId::path(&dir)).unwrap();
        let find = |n: &str| (0..list.len()).find(|&i| list.name(i) == n).unwrap();

        let hi = find("hello.txt");
        assert_eq!(list.size(hi), 9);
        assert!(list.added(hi) > 0);
        assert_eq!(list.tag_colors(hi), 1 << (6 - 1));

        let si = find("sub");
        assert_eq!(list.kind(si), EntryKind::Directory);

        let di = find(".secret");
        assert!(list.attrs(di) & attr::HIDDEN != 0);

        let li = find("sym");
        assert_eq!(list.kind(li), EntryKind::Symlink);
        assert!(list.is_alias(li));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dataless_files_are_cloud_only() {
        assert_eq!(sync_state(SF_DATALESS), SyncState::CloudOnly);
        // Alongside other flags — UF_HIDDEN, and the compressed bit
        // (0x20) that APFS sets on many system files.
        assert_eq!(sync_state(SF_DATALESS | UF_HIDDEN | 0x20), SyncState::CloudOnly);
        assert_eq!(sync_state(0), SyncState::None);
        assert_eq!(sync_state(UF_HIDDEN | 0x20), SyncState::None);
    }
}
