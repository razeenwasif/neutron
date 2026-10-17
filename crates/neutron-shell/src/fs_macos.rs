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
        let mut list = EntryList::new();
        for entry in std::fs::read_dir(path)? {
            let Ok(entry) = entry else { continue };
            let Ok(meta) = entry.path().symlink_metadata() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            let link = meta.file_type().is_symlink();
            let mut directory = meta.is_dir() || (link && entry.path().is_dir());
            // An application, a Pages document, a photo library: a folder on
            // disk, a single item to the user. Listed as a file so activating
            // it opens it with its application, the way Finder does.
            if directory && is_package(&entry.path(), link) { directory = false; }
            let mut attrs = 0;
            if directory { attrs |= attr::DIRECTORY; }
            if link { attrs |= attr::REPARSE_POINT; }
            // Dot-files, and items carrying UF_HIDDEN the way Finder hides ~/Library.
            if name.starts_with('.') || meta.st_flags() & UF_HIDDEN != 0 { attrs |= attr::HIDDEN; }
            if meta.permissions().readonly() { attrs |= attr::READONLY; }
            // Its tag colours, for the dots beside its name: one attribute
            // read, which for an untagged file is a quick "not there".
            attrs |= (crate::tags::colors(&entry.path()) as u32) << attr::TAG_SHIFT;
            let kind = if directory { EntryKind::Directory }
                else if link { EntryKind::Symlink } else { EntryKind::File };
            // The system's name for this kind of file, learned from the first
            // file of each extension met: once per extension, not per file.
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
    fn dataless_files_are_cloud_only() {
        assert_eq!(sync_state(SF_DATALESS), SyncState::CloudOnly);
        // Alongside other flags — UF_HIDDEN, and the compressed bit
        // (0x20) that APFS sets on many system files.
        assert_eq!(sync_state(SF_DATALESS | UF_HIDDEN | 0x20), SyncState::CloudOnly);
        assert_eq!(sync_state(0), SyncState::None);
        assert_eq!(sync_state(UF_HIDDEN | 0x20), SyncState::None);
    }
}
