//! Finder tags: reading them as a folder is listed, setting them, and finding
//! every file that carries one.
//!
//! A file's tags are a binary property list in its
//! `com.apple.metadata:_kMDItemUserTags` attribute: an array of strings, each
//! a tag's name and, after a newline, its colour — `"Red\n6"`, `"Work\n0"`.
//! Reading that attribute is one system call and no Objective-C, which is what
//! lets every row of a listing show its tags. Setting them goes through the
//! system's own API instead, which writes the colours the way the Finder does
//! and keeps Spotlight's index up to date.

use std::path::{Path, PathBuf};

/// A tag on a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    /// 1 to 7, as below; 0 for a tag with no colour.
    pub color: u8,
}

/// The Finder's colour tags, by the number its tags carry. Their names are
/// the English ones the system writes when nothing has been renamed.
pub const COLORS: [(&str, u8); 7] = [
    ("Red", 6),
    ("Orange", 7),
    ("Yellow", 5),
    ("Green", 2),
    ("Blue", 4),
    ("Purple", 3),
    ("Gray", 1),
];

const ATTRIBUTE: &std::ffi::CStr = c"com.apple.metadata:_kMDItemUserTags";

/// The tags on `path`, in the order the file lists them. Empty when it has
/// none, which is nearly every file.
pub fn read(path: &Path) -> Vec<Tag> {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return Vec::new();
    };
    // SAFETY: valid NUL-terminated strings, and a buffer of the stated length
    // or none, which asks for the size.
    let fetch = |buffer: &mut [u8]| unsafe {
        libc::getxattr(
            c_path.as_ptr(),
            ATTRIBUTE.as_ptr(),
            if buffer.is_empty() { std::ptr::null_mut() } else { buffer.as_mut_ptr().cast() },
            buffer.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    // Room for a handful of tags, which is what files have; asked again at
    // the right size for one with more.
    let mut buffer = vec![0u8; 512];
    let mut len = fetch(&mut buffer);
    if len < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ERANGE) {
        let size = fetch(&mut []);
        if size <= 0 {
            return Vec::new();
        }
        buffer = vec![0u8; size as usize];
        len = fetch(&mut buffer);
    }
    if len <= 0 {
        return Vec::new();
    }
    buffer.truncate(len as usize);
    parse(&buffer)
        .into_iter()
        .map(|entry| match entry.rsplit_once('\n') {
            Some((name, color)) => Tag { name: name.to_owned(), color: color.parse().unwrap_or(0) },
            None => Tag { color: color_named(&entry), name: entry },
        })
        .collect()
}

/// The colours of the tags on `path`, as bits: bit `n - 1` for colour `n`.
pub fn colors(path: &Path) -> u8 {
    read(path)
        .iter()
        .filter(|t| (1..=7).contains(&t.color))
        .fold(0, |bits, t| bits | (1 << (t.color - 1)))
}

/// The colour a tag of this name has when the file does not say.
fn color_named(name: &str) -> u8 {
    COLORS.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map_or(0, |(_, c)| *c)
}

/// Gives each of `paths` the tag `name`, or takes it away, leaving their other
/// tags as they are.
pub fn toggle(paths: &[PathBuf], name: &str, on: bool) -> Result<(), String> {
    let mut failed = Vec::new();
    for path in paths {
        let mut names: Vec<String> = read(path).into_iter().map(|t| t.name).collect();
        let had = names.iter().any(|n| n == name);
        if on && !had {
            names.push(name.to_owned());
        } else if !on && had {
            names.retain(|n| n != name);
        } else {
            continue;
        }
        if let Err(why) = set(path, &names) {
            failed.push(format!(
                "{}: {why}",
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ));
        }
    }
    if failed.is_empty() { Ok(()) } else { Err(failed.join("; ")) }
}

/// Sets `path`'s tags to exactly `names`, through the system's API.
fn set(path: &Path, names: &[String]) -> Result<(), String> {
    use objc2_foundation::{NSArray, NSString, NSURL, NSURLTagNamesKey};
    let text = crate::macos::path_str(path)?;
    objc2::rc::autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(text));
        let values: Vec<objc2::rc::Retained<NSString>> = names.iter().map(|n| NSString::from_str(n)).collect();
        let array = NSArray::from_retained_slice(&values);
        // SAFETY: the tag names key takes an array of strings.
        unsafe { url.setResourceValue_forKey_error(Some(&array), NSURLTagNamesKey) }
            .map_err(|e| e.localizedDescription().to_string())
    })
}

/// Every file Spotlight knows to carry the tag `name`. Blocks; call it from a
/// worker.
pub fn search(name: &str) -> Result<Vec<PathBuf>, String> {
    use std::os::unix::ffi::OsStringExt;
    let escaped: String = name.chars().flat_map(|c| if matches!(c, '\\' | '"') { vec!['\\', c] } else { vec![c] }).collect();
    let output = std::process::Command::new("/usr/bin/mdfind")
        .args(["-0", &format!("kMDItemUserTags == \"{escaped}\"")])
        .output()
        .map_err(|e| format!("Spotlight could not be asked: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(output
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(std::ffi::OsString::from_vec(p.to_vec())))
        .collect())
}

/// The strings of a binary property list whose top object is an array of
/// strings — which is all this attribute ever holds. Anything else, or a
/// damaged list, reads as no tags.
fn parse(data: &[u8]) -> Vec<String> {
    let parsed = || -> Option<Vec<String>> {
        if data.len() < 40 || &data[..8] != b"bplist00" {
            return None;
        }
        let trailer = &data[data.len() - 32..];
        let offset_size = trailer[6] as usize;
        let ref_size = trailer[7] as usize;
        let count = be(&trailer[8..16])? as usize;
        let top = be(&trailer[16..24])? as usize;
        let table = be(&trailer[24..32])? as usize;
        if offset_size == 0 || ref_size == 0 || offset_size > 8 || ref_size > 8 || top >= count {
            return None;
        }
        let offset = |index: usize| -> Option<usize> {
            let at = table.checked_add(index.checked_mul(offset_size)?)?;
            Some(be(data.get(at..at + offset_size)?)? as usize)
        };
        // An object's length: in its marker's low nibble, or after it.
        let length = |at: usize| -> Option<(usize, usize)> {
            let low = (data.get(at)? & 0x0F) as usize;
            if low != 0x0F {
                return Some((low, at + 1));
            }
            let marker = *data.get(at + 1)?;
            if marker & 0xF0 != 0x10 {
                return None;
            }
            let bytes = 1usize << (marker & 0x0F);
            Some((be(data.get(at + 2..at + 2 + bytes)?)? as usize, at + 2 + bytes))
        };
        let string = |index: usize| -> Option<String> {
            let at = offset(index)?;
            let (len, start) = length(at)?;
            match data.get(at)? & 0xF0 {
                0x50 => Some(String::from_utf8_lossy(data.get(start..start.checked_add(len)?)?).into_owned()),
                0x60 => {
                    let raw = data.get(start..start.checked_add(len.checked_mul(2)?)?)?;
                    let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                    Some(String::from_utf16_lossy(&units))
                }
                _ => None,
            }
        };
        let at = offset(top)?;
        if data.get(at)? & 0xF0 != 0xA0 {
            return None;
        }
        let (len, start) = length(at)?;
        (0..len)
            .map(|i| {
                let r = start + i * ref_size;
                string(be(data.get(r..r + ref_size)?)? as usize)
            })
            .collect()
    };
    parsed().unwrap_or_default()
}

fn be(bytes: &[u8]) -> Option<u64> {
    (bytes.len() <= 8).then(|| bytes.iter().fold(0u64, |n, b| (n << 8) | *b as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The attribute exactly as the system wrote it for Red, Work and Blue.
    const WRITTEN: &[u8] = &[
        0x62, 0x70, 0x6C, 0x69, 0x73, 0x74, 0x30, 0x30, 0xA3, 0x01, 0x02, 0x03, 0x55, 0x52, 0x65, 0x64,
        0x0A, 0x36, 0x56, 0x57, 0x6F, 0x72, 0x6B, 0x0A, 0x30, 0x56, 0x42, 0x6C, 0x75, 0x65, 0x0A, 0x34,
        0x08, 0x0C, 0x12, 0x19, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x20,
    ];

    #[test]
    fn the_systems_own_encoding_is_read() {
        assert_eq!(parse(WRITTEN), vec!["Red\n6", "Work\n0", "Blue\n4"]);
    }

    #[test]
    fn anything_else_reads_as_no_tags() {
        assert!(parse(b"bplist00").is_empty());
        assert!(parse(&WRITTEN[..WRITTEN.len() - 3]).is_empty());
        assert!(parse(b"not a property list at all, but long enough to try").is_empty());
    }

    #[test]
    fn tags_are_set_read_back_and_toggled_on_a_real_file() {
        let dir = std::env::temp_dir().join(format!("neutron-tags-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(read(&file).is_empty());

        toggle(std::slice::from_ref(&file), "Red", true).unwrap();
        toggle(std::slice::from_ref(&file), "Work", true).unwrap();
        let tags = read(&file);
        assert_eq!(tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["Red", "Work"]);
        assert_eq!(colors(&file), 1 << (6 - 1), "Red is colour 6");

        toggle(std::slice::from_ref(&file), "Red", false).unwrap();
        assert_eq!(read(&file).iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["Work"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
