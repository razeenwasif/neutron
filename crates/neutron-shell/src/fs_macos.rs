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
    fn dataless_files_are_cloud_only() {
        assert_eq!(sync_state(SF_DATALESS), SyncState::CloudOnly);
        // Alongside other flags — UF_HIDDEN, and the compressed bit
        // (0x20) that APFS sets on many system files.
        assert_eq!(sync_state(SF_DATALESS | UF_HIDDEN | 0x20), SyncState::CloudOnly);
        assert_eq!(sync_state(0), SyncState::None);
        assert_eq!(sync_state(UF_HIDDEN | 0x20), SyncState::None);
    }
}
