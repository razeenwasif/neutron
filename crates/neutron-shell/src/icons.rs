//! Resolving system file icons to RGBA pixels.
//!
//! # Never touch the file
//!
//! The single most important rule here. `SHGetFileInfoW` will happily open a
//! file to ask a per-file icon handler what it looks like, and for a OneDrive
//! placeholder that means **downloading it** — potentially gigabytes, to draw a
//! 32-pixel square. Listing a synced folder would silently pull the whole thing
//! onto disk.
//!
//! So the default path passes `SHGFI_USEFILEATTRIBUTES`, which tells the shell
//! to answer from the name and attribute bits alone and never open anything.
//! The icon is then a function of the *extension*, not the file, which is also
//! what makes caching effective: ten thousand `.txt` files share one lookup.
//!
//! The exceptions are types that carry their own icon in their contents —
//! executables, shortcuts, icon files. Those are looked up per path, and only
//! those pay for disk access. [`IconKey::for_entry`] draws the line.
//!
//! # Threading
//!
//! **STA pool only.** Icon handlers are shell extensions: arbitrary
//! third-party code, loaded into this process, doing whatever it likes
//! including network I/O.

use std::path::{Path, PathBuf};

/// Edge of a resolved icon, in pixels.
///
/// 32 is the shell's "large" size. Rows draw it smaller, but downscaling a
/// 32px icon looks far better than upscaling the 16px one, and on a high-DPI
/// display the 16px version is visibly soft.
pub const ICON_PX: u32 = 32;

/// What an icon is cached against.
///
/// Choosing the coarsest key that is still correct is the whole performance
/// story: a directory of 10,000 photos is one lookup, not 10,000.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IconKey {
    /// Every ordinary file of this (lowercased) extension looks the same.
    Extension(String),
    /// Files with no extension at all.
    Extensionless,
    /// This specific file carries its own icon — an executable, a shortcut.
    Path(PathBuf),
    /// One folder icon for all folders.
    Directory,
}

/// Extensions whose icon lives inside the file rather than in its association.
///
/// `.lnk` is here because a shortcut's icon is the icon of its *target*, which
/// cannot be known from the name. `.url` likewise.
const SELF_ICONED: &[&str] = &[
    "exe", "lnk", "ico", "cur", "ani", "url", "scr", "msc", "cpl",
];

impl IconKey {
    /// The key for one directory entry.
    pub fn for_entry(name: &str, is_dir: bool) -> IconKey {
        if is_dir {
            // Custom per-folder icons (a `desktop.ini` with an `IconResource`)
            // are not honoured. Doing so needs a per-path lookup for every
            // folder, and that lookup reads `desktop.ini` off the disk — the
            // cost falls on every directory listing to serve a rare case.
            return IconKey::Directory;
        }

        match extension(name) {
            None => IconKey::Extensionless,
            Some(ext) if SELF_ICONED.contains(&ext.as_str()) => {
                // Resolved by the caller against the real path.
                IconKey::Path(PathBuf::from(name))
            }
            Some(ext) => IconKey::Extension(ext),
        }
    }

    /// Whether resolving this key reads the file itself.
    pub fn touches_disk(&self) -> bool {
        matches!(self, IconKey::Path(_))
    }

    /// Rebinds a per-path key onto its real location. Entry names are relative
    /// to the directory being listed; the shell needs the full path.
    pub fn rooted_at(self, dir: &Path) -> IconKey {
        match self {
            IconKey::Path(name) => IconKey::Path(dir.join(name)),
            other => other,
        }
    }
}

/// Lowercased extension, or `None`. A leading dot is part of the name
/// (`.gitignore`), not an extension — matching Explorer and `file_list`.
fn extension(name: &str) -> Option<String> {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => Some(name[i + 1..].to_ascii_lowercase()),
        _ => None,
    }
}

/// A resolved icon: `ICON_PX` square, straight (non-premultiplied) RGBA.
pub struct IconImage {
    pub rgba: Vec<u8>,
}

/// In-place BGRA-premultiplied to RGBA-straight.
///
/// Portable rather than gated with the rest of the Win32 code so the pixel
/// maths — the part with the off-by-one risk — is covered by the Linux test
/// runs, which are the fast loop.
#[cfg_attr(not(windows), allow(dead_code))]
fn bgra_to_rgba(pixels: &mut [u8]) {
    for px in pixels.chunks_exact_mut(4) {
        px.swap(0, 2);

        let a = px[3];
        if a == 0 {
            // Fully transparent pixels carry arbitrary colour under
            // premultiplication. Zeroing them keeps bilinear filtering from
            // bleeding that colour into neighbouring texels — the usual cause
            // of a dark fringe around a scaled icon.
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        } else if a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255) / a as u32).min(255) as u8;
            }
        }
    }
}

/// Nearest-neighbour resize to a square.
///
/// Only ever a no-op or a small integral-ish reduction in practice — the shell
/// returns 32px for a 32px request. It exists so a handler that ignores the
/// requested size cannot put a differently-sized image into the atlas, which
/// would corrupt every cell after it.
#[cfg_attr(not(windows), allow(dead_code))]
fn rescale(src: &[u8], w: u32, h: u32, size: u32) -> Vec<u8> {
    if w == size && h == size {
        return src.to_vec();
    }

    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        let sy = y * h / size;
        for x in 0..size {
            let sx = x * w / size;
            let s = ((sy * w + sx) * 4) as usize;
            let d = ((y * size + x) * 4) as usize;
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    out
}

// --- Win32 resolution ------------------------------------------------------

#[cfg(windows)]
mod win32 {
    use super::*;

    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC,
        GetDIBits, GetObjectW, ReleaseDC,
    };
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES,
    };
    use windows::Win32::UI::Shell::{
        SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
    use windows::core::PCWSTR;

    /// Resolves `key` to pixels.
    ///
    /// **STA pool only.** Returns `None` when the shell has no icon, which is
    /// normal rather than exceptional — callers fall back to a drawn glyph.
    pub fn resolve(key: &IconKey) -> Option<IconImage> {
        let (path, attrs, use_attributes) = match key {
            // A synthetic name: with SHGFI_USEFILEATTRIBUTES the shell parses the
            // string for its extension and never looks for the file, so this need
            // not — and must not — exist.
            IconKey::Extension(ext) => (format!("_.{ext}"), FILE_ATTRIBUTE_NORMAL, true),
            IconKey::Extensionless => ("_".to_owned(), FILE_ATTRIBUTE_NORMAL, true),
            IconKey::Directory => ("_".to_owned(), FILE_ATTRIBUTE_DIRECTORY, true),
            // The only case that reads the disk.
            IconKey::Path(p) => (
                p.to_string_lossy().into_owned(),
                FILE_ATTRIBUTE_NORMAL,
                false,
            ),
        };

        let icon = system_icon(&path, attrs, use_attributes)?;
        let image = icon_to_rgba(icon);

        // SAFETY: `icon` came from SHGetFileInfoW with SHGFI_ICON, which documents
        // the caller as owning it, and it is not used after this point.
        unsafe {
            let _ = DestroyIcon(icon);
        };

        image
    }

    /// Asks the shell for an `HICON`. The caller owns it and must `DestroyIcon`.
    fn system_icon(
        path: &str,
        attrs: FILE_FLAGS_AND_ATTRIBUTES,
        use_attributes: bool,
    ) -> Option<HICON> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        let mut flags = SHGFI_ICON | SHGFI_LARGEICON;
        if use_attributes {
            flags |= SHGFI_USEFILEATTRIBUTES;
        }

        let mut info = SHFILEINFOW::default();
        // SAFETY: `wide` is NUL-terminated and outlives the call; `info` is a valid
        // writable SHFILEINFOW and its size is passed correctly.
        let ok = unsafe {
            SHGetFileInfoW(
                PCWSTR(wide.as_ptr()),
                attrs,
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                flags,
            )
        };

        // Returns zero on failure. A null icon with a non-zero return is also
        // possible for types the shell knows but has no image for.
        if ok == 0 || info.hIcon.is_invalid() {
            return None;
        }
        Some(info.hIcon)
    }

    /// Converts an `HICON` to straight RGBA.
    ///
    /// The colour bitmap inside an icon is bottom-up BGRA with *premultiplied*
    /// alpha. Three conversions are therefore needed and all three are easy to get
    /// wrong: request a top-down DIB by giving the height as negative, swap B and
    /// R, and un-premultiply — without which every semi-transparent edge pixel is
    /// too dark, which reads as a dirty halo around every icon.
    fn icon_to_rgba(icon: HICON) -> Option<IconImage> {
        let mut info = ICONINFO::default();
        // SAFETY: `info` is a valid writable ICONINFO. On success it hands back two
        // bitmap handles that this function owns and deletes below.
        unsafe { GetIconInfo(icon, &mut info) }.ok()?;

        let colour = info.hbmColor;
        let mask = info.hbmMask;

        let result = read_bitmap(colour);

        // SAFETY: both handles came from GetIconInfo, which documents the caller as
        // owning them; leaking them is a GDI handle leak per icon resolved.
        unsafe {
            if !colour.is_invalid() {
                let _ = DeleteObject(colour.into());
            }
            if !mask.is_invalid() {
                let _ = DeleteObject(mask.into());
            }
        }

        result
    }

    fn read_bitmap(bitmap: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<IconImage> {
        if bitmap.is_invalid() {
            // A monochrome icon has no colour bitmap, only a mask. Rare enough that
            // falling back to the drawn glyph is better than implementing the
            // 1-bit path.
            return None;
        }

        let mut bm = BITMAP::default();
        // SAFETY: `bm` is a valid writable BITMAP of the size given.
        let read = unsafe {
            GetObjectW(
                bitmap.into(),
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bm as *mut _ as *mut _),
            )
        };
        if read == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
            return None;
        }

        let (w, h) = (bm.bmWidth as u32, bm.bmHeight as u32);

        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: bm.bmWidth,
                // Negative height requests a top-down DIB. Left positive, the rows
                // arrive bottom-up and every icon renders upside down.
                biHeight: -bm.bmHeight,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut pixels = vec![0u8; (w * h * 4) as usize];

        // SAFETY: a screen DC is valid for a compatible-bitmap read and is released
        // below. `pixels` is exactly the size the header describes.
        let dc = unsafe { GetDC(None) };
        let copied = unsafe {
            GetDIBits(
                dc,
                bitmap,
                0,
                h,
                Some(pixels.as_mut_ptr() as *mut _),
                &mut header,
                DIB_RGB_COLORS,
            )
        };
        // SAFETY: `dc` came from GetDC(None) and is not used afterwards.
        unsafe { ReleaseDC(None, dc) };

        if copied == 0 {
            return None;
        }

        bgra_to_rgba(&mut pixels);

        Some(IconImage {
            rgba: rescale(&pixels, w, h, ICON_PX),
        })
    }
}

#[cfg(windows)]
pub use win32::resolve;

/// Non-Windows placeholder, so the portable key logic above can be built and
/// tested on Linux. See `fs_stub` for why these exist.
#[cfg(not(windows))]
pub fn resolve(_key: &IconKey) -> Option<IconImage> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_files_are_keyed_by_extension() {
        // The whole cache depends on this: a folder of 10,000 photos must cost
        // one lookup, not 10,000.
        assert_eq!(
            IconKey::for_entry("holiday.JPG", false),
            IconKey::Extension("jpg".into())
        );
        assert_eq!(
            IconKey::for_entry("a.txt", false),
            IconKey::for_entry("b.txt", false)
        );
    }

    #[test]
    fn self_iconed_types_are_keyed_by_path() {
        // An executable's icon is inside the executable, and a shortcut's is
        // its target's — neither can be predicted from the extension.
        assert_eq!(
            IconKey::for_entry("game.exe", false),
            IconKey::Path(PathBuf::from("game.exe"))
        );
        assert!(IconKey::for_entry("thing.lnk", false).touches_disk());
        assert!(!IconKey::for_entry("thing.txt", false).touches_disk());
    }

    #[test]
    fn folders_share_one_icon() {
        assert_eq!(IconKey::for_entry("src", true), IconKey::Directory);
        // Even when the name looks like a file.
        assert_eq!(IconKey::for_entry("archive.zip", true), IconKey::Directory);
        assert!(!IconKey::Directory.touches_disk());
    }

    #[test]
    fn dotfiles_have_no_extension() {
        assert_eq!(
            IconKey::for_entry(".gitignore", false),
            IconKey::Extensionless
        );
        assert_eq!(
            IconKey::for_entry("Makefile", false),
            IconKey::Extensionless
        );
        assert_eq!(
            IconKey::for_entry("trailing.", false),
            IconKey::Extensionless
        );
    }

    #[test]
    fn per_path_keys_are_rooted_but_shared_keys_are_not() {
        let dir = Path::new(r"C:\games");
        // Built with `join` rather than written out, so the assertion holds on
        // the Linux test runs too — `Path::join` uses the host separator, and
        // hard-coding a backslash only passes on Windows.
        assert_eq!(
            IconKey::for_entry("game.exe", false).rooted_at(dir),
            IconKey::Path(dir.join("game.exe"))
        );
        // Rooting a shared key would defeat the sharing.
        assert_eq!(
            IconKey::Extension("txt".into()).rooted_at(dir),
            IconKey::Extension("txt".into())
        );
    }

    #[test]
    fn un_premultiplying_restores_the_source_colour() {
        // A half-transparent pure white pixel is stored as BGRA (128,128,128,128).
        let mut px = vec![128u8, 128, 128, 128];
        bgra_to_rgba(&mut px);
        assert_eq!(px[3], 128, "alpha must be untouched");
        for c in &px[..3] {
            assert!(*c >= 254, "expected white, got {c} — icons will look dirty");
        }
    }

    #[test]
    fn channels_are_swapped() {
        // Pure blue in BGRA is (255, 0, 0, 255) and must become (0, 0, 255, 255).
        let mut px = vec![255u8, 0, 0, 255];
        bgra_to_rgba(&mut px);
        assert_eq!(px, vec![0, 0, 255, 255]);
    }

    #[test]
    fn transparent_pixels_are_cleared_rather_than_amplified() {
        // Dividing by a zero alpha is the crash; leaving the colour is the
        // dark fringe. Neither is acceptable.
        let mut px = vec![200u8, 100, 50, 0];
        bgra_to_rgba(&mut px);
        assert_eq!(px, vec![0, 0, 0, 0]);
    }

    #[test]
    fn rescaling_a_matching_image_is_a_copy() {
        let src = vec![7u8; (ICON_PX * ICON_PX * 4) as usize];
        assert_eq!(rescale(&src, ICON_PX, ICON_PX, ICON_PX), src);
    }

    #[test]
    fn rescaling_always_produces_exactly_one_cell() {
        // An atlas cell is a fixed size. A handler returning 48px where 32 was
        // asked for must not be able to overrun the next cell.
        for (w, h) in [(16u32, 16u32), (48, 48), (24, 32), (256, 256)] {
            let src = vec![0u8; (w * h * 4) as usize];
            let out = rescale(&src, w, h, ICON_PX);
            assert_eq!(out.len(), (ICON_PX * ICON_PX * 4) as usize, "{w}x{h}");
        }
    }
}
