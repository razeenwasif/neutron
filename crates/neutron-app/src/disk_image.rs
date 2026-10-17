//! Disk images: opening a `.dmg` mounts it and goes to the disk it holds,
//! rather than handing it to the system to mount and open in the Finder.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Whether `path` is a disk image Neutron mounts itself.
pub fn is_disk_image(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("dmg"))
}

/// Mounts the image at `path` and returns where its volume is. Blocks while
/// `hdiutil` works, which for a large image is seconds: call it from a worker.
pub fn mount(path: &Path) -> Result<PathBuf, String> {
    let out = Command::new("/usr/bin/hdiutil")
        .args(["attach", "-plist", "-noautoopen"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("hdiutil could not be started: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    mount_point(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| "the image holds no volume that can be mounted".into())
}

/// The first `mount-point` in `hdiutil attach -plist`'s answer.
fn mount_point(plist: &str) -> Option<PathBuf> {
    let at = plist.find("<key>mount-point</key>")?;
    let rest = &plist[at..];
    let open = rest.find("<string>")? + "<string>".len();
    let close = rest[open..].find("</string>")?;
    Some(PathBuf::from(&rest[open..open + close]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mount_point_is_read_from_hdiutils_answer() {
        let plist = "<dict><key>system-entities</key><array><dict><key>content-hint</key>\
                     <string>GUID_partition_scheme</string></dict><dict><key>mount-point</key>\
                     <string>/Volumes/My Disk</string></dict></array></dict>";
        assert_eq!(mount_point(plist), Some(PathBuf::from("/Volumes/My Disk")));
        assert_eq!(mount_point("<dict/>"), None);
        assert!(is_disk_image(Path::new("/a/Setup.DMG")) && !is_disk_image(Path::new("a.zip")));
    }

    /// Makes, mounts and ejects a small image.
    #[test]
    fn a_disk_image_is_mounted_and_its_volume_found() {
        let dir = std::env::temp_dir().join(format!("neutron-dmg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/inside.txt"), "x").unwrap();
        let image = dir.join("test.dmg");
        let made = Command::new("/usr/bin/hdiutil")
            .args(["create", "-quiet", "-fs", "HFS+", "-volname", "NeutronTest", "-srcfolder"])
            .arg(dir.join("src"))
            .arg(&image)
            .status()
            .unwrap();
        assert!(made.success());
        let volume = mount(&image).unwrap();
        assert!(volume.join("inside.txt").exists(), "{}", volume.display());
        let _ = Command::new("/usr/bin/hdiutil").args(["detach", "-quiet"]).arg(&volume).status();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
