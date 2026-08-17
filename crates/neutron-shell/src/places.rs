//! Discovering sidebar destinations: known folders and volumes.
//!
//! Both enumerations touch the disk and must run on a worker thread.
//! `GetVolumeInformationW` in particular blocks for seconds on a removable
//! drive with no media, and `SHGetKnownFolderPath` can trigger a network
//! round-trip for redirected folders.

use std::path::PathBuf;

use neutron_core::namespace::NodeId;
use neutron_core::places::{Capacity, Place, PlaceKind};
use windows::Win32::Foundation::MAX_PATH;
use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
// Not in Storage::FileSystem alongside GetDriveTypeW, despite being its return
// values.
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Diagnostics::Debug::{
    SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE,
};
use windows::Win32::System::WindowsProgramming::{
    DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures,
    FOLDERID_Profile, FOLDERID_SkyDrive, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::{GUID, PCWSTR, PWSTR};

/// The user's profile directory, for use as the initial location.
///
/// Deliberately separate from [`known_folders`]: this is a single shell lookup
/// that never touches removable media or the network, so it is safe to call
/// before the first frame. Everything else in this module is not.
pub fn home() -> Option<PathBuf> {
    known_folder_path(&FOLDERID_Profile)
}

/// The quick-access list, in Explorer's order.
///
/// **Worker thread only** — a redirected known folder can resolve over the
/// network.
pub fn known_folders() -> Vec<Place> {
    [
        (FOLDERID_Profile, "Home"),
        (FOLDERID_Desktop, "Desktop"),
        (FOLDERID_Downloads, "Downloads"),
        (FOLDERID_Documents, "Documents"),
        (FOLDERID_Pictures, "Pictures"),
        (FOLDERID_Music, "Music"),
        (FOLDERID_Videos, "Videos"),
    ]
    .into_iter()
    .filter_map(|(id, name)| {
        known_folder_path(&id).map(|path| Place {
            name: name.to_owned(),
            id: NodeId::Path(path),
            kind: PlaceKind::KnownFolder,
            capacity: None,
        })
    })
    .collect()
}

fn known_folder_path(id: &GUID) -> Option<PathBuf> {
    // SAFETY: `id` is a valid FOLDERID constant; the returned buffer is freed
    // below on every path.
    let raw: PWSTR = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
    if raw.is_null() {
        return None;
    }

    // SAFETY: SHGetKnownFolderPath returns a NUL-terminated string on success.
    let path = unsafe { raw.to_string() }.ok().map(PathBuf::from);

    // The shell allocated this with CoTaskMemAlloc; anything else leaks it.
    // SAFETY: `raw` came from a shell allocation and is not used afterwards.
    unsafe { CoTaskMemFree(Some(raw.0 as *const _)) };

    path.filter(|p| !p.as_os_str().is_empty())
}

/// Cloud-synced folders that exist locally.
///
/// OneDrive is the cheap case: it syncs to a real directory, so it is an
/// ordinary path that the normal filesystem backend enumerates. All that is
/// needed here is finding it. Business and personal accounts get separate
/// folders and both are listed, since a user with two accounts has two roots.
///
/// Google Drive is *not* here — Drive for Desktop is not installed on this
/// machine, so there is no local folder to find. It needs the API client at M7.
pub fn cloud_folders() -> Vec<Place> {
    let mut out = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();

    // The known folder covers the primary account and is the fastest path.
    if let Some(path) = known_folder_path(&FOLDERID_SkyDrive) {
        seen.push(path.clone());
        out.push(Place {
            name: "OneDrive".to_owned(),
            id: NodeId::Path(path),
            kind: PlaceKind::Cloud,
            capacity: None,
        });
    }

    // Additional accounts only appear in the registry. Each `Accounts\Business1`
    // style subkey carries its own `UserFolder`.
    for (label, path) in onedrive_accounts() {
        if seen.contains(&path) {
            continue;
        }
        seen.push(path.clone());
        out.push(Place {
            name: label,
            id: NodeId::Path(path),
            kind: PlaceKind::Cloud,
            capacity: None,
        });
    }

    out
}

/// Reads `HKCU\Software\Microsoft\OneDrive\Accounts\*\UserFolder`.
fn onedrive_accounts() -> Vec<(String, PathBuf)> {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
    };
    use windows::core::w;

    let mut out = Vec::new();
    let mut accounts = HKEY::default();

    // SAFETY: constant key path; the handle is closed on every exit path below.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\OneDrive\Accounts"),
            Some(0),
            KEY_READ,
            &mut accounts,
        )
    };
    if opened.is_err() {
        // OneDrive not configured — normal, not an error.
        return out;
    }

    for index in 0..32u32 {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        // SAFETY: `name`/`len` describe a valid buffer for the duration.
        let ok = unsafe {
            RegEnumKeyExW(
                accounts,
                index,
                Some(windows::core::PWSTR(name.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if ok.is_err() {
            break;
        }

        let subkey = String::from_utf16_lossy(&name[..len as usize]);
        // "Personal" and "Business1" are accounts; other subkeys are settings.
        if !subkey.starts_with("Personal") && !subkey.starts_with("Business") {
            continue;
        }

        if let Some(folder) = read_user_folder(accounts, &subkey) {
            let label = if subkey.starts_with("Personal") {
                "OneDrive".to_owned()
            } else {
                // Business folders are named after the tenant, which is far
                // more useful than "Business1" when a user has several.
                folder
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "OneDrive (Work)".to_owned())
            };
            out.push((label, folder));
        }
    }

    // SAFETY: opened successfully above and not used afterwards.
    let _ = unsafe { RegCloseKey(accounts) };
    out
}

fn read_user_folder(
    accounts: windows::Win32::System::Registry::HKEY,
    subkey: &str,
) -> Option<PathBuf> {
    use windows::core::w;

    let path = PathBuf::from(read_string_value(accounts, subkey, w!("UserFolder"))?);

    // A configured-but-unsynced account can point at a folder that does not
    // exist yet; listing it would only produce an error row.
    path.is_dir().then_some(path)
}

/// Installed WSL distributions, as `\\wsl.localhost\<Distro>` roots.
///
/// Read from the registry rather than by shelling out to `wsl.exe --list`:
/// launching that starts the WSL service and can take seconds on a cold
/// machine, and this runs during sidebar discovery where the whole point is to
/// stay cheap. The registry is authoritative and costs a few microseconds.
///
/// `\\wsl.localhost` is a UNC path served by the P9 redirector, so the ordinary
/// `FindFirstFileExW` backend enumerates it once `\\?\UNC\` prefixing is applied
/// — which [`crate::fs`] already does. No new backend is needed.
///
/// Distributions that are installed but not running are still listed. Opening
/// one starts it on demand, which is the same behaviour as Explorer's Linux
/// node, and the first listing is therefore slower than the rest.
///
/// **Worker thread only** — see the module note.
pub fn wsl_distributions() -> Vec<Place> {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
    };
    use windows::core::w;

    let mut out = Vec::new();
    let mut lxss = HKEY::default();

    // SAFETY: constant key path; the handle is closed on every exit path below.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Lxss"),
            Some(0),
            KEY_READ,
            &mut lxss,
        )
    };
    if opened.is_err() {
        // WSL not installed — normal, not an error.
        return out;
    }

    // Each distribution is a GUID-named subkey carrying a `DistributionName`.
    for index in 0..64u32 {
        let mut name = [0u16; 128];
        let mut len = name.len() as u32;
        // SAFETY: `name`/`len` describe a valid buffer for the duration.
        let ok = unsafe {
            RegEnumKeyExW(
                lxss,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if ok.is_err() {
            break;
        }

        let subkey = String::from_utf16_lossy(&name[..len as usize]);
        // `DefaultDistribution` and friends are values on the parent, but a
        // stray subkey would otherwise be probed pointlessly.
        if !subkey.starts_with('{') {
            continue;
        }

        if let Some(distro) = read_string_value(lxss, &subkey, w!("DistributionName")) {
            if distro.is_empty() {
                continue;
            }
            out.push(Place {
                name: distro.clone(),
                id: NodeId::Path(PathBuf::from(format!(r"\\wsl.localhost\{distro}"))),
                kind: PlaceKind::Wsl,
                capacity: None,
            });
        }
    }

    // SAFETY: opened successfully above and not used afterwards.
    let _ = unsafe { RegCloseKey(lxss) };

    // Registry enumeration order is by subkey GUID, which is arbitrary. Sort so
    // the sidebar does not reshuffle itself between installs.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Reads a `REG_SZ` value from `parent\subkey`.
fn read_string_value(
    parent: windows::Win32::System::Registry::HKEY,
    subkey: &str,
    value: PCWSTR,
) -> Option<String> {
    use windows::Win32::System::Registry::{
        HKEY, KEY_READ, REG_VALUE_TYPE, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };

    let wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let mut key = HKEY::default();
    // SAFETY: `wide` is NUL-terminated; the handle is closed before returning.
    let opened =
        unsafe { RegOpenKeyExW(parent, PCWSTR(wide.as_ptr()), Some(0), KEY_READ, &mut key) };
    if opened.is_err() {
        return None;
    }

    let mut buf = [0u16; 520];
    let mut size = (buf.len() * 2) as u32;
    let mut kind = REG_VALUE_TYPE::default();
    // SAFETY: buffer and size are valid for the duration of the call.
    let read = unsafe {
        RegQueryValueExW(
            key,
            value,
            None,
            Some(&mut kind),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut size),
        )
    };
    // SAFETY: opened successfully and not used afterwards.
    let _ = unsafe { RegCloseKey(key) };

    if read.is_err() {
        return None;
    }

    let chars = (size as usize / 2).min(buf.len());
    let len = buf[..chars].iter().position(|&c| c == 0).unwrap_or(chars);
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// All mounted volumes, with labels and capacity where available.
///
/// **Worker thread only.** Querying a removable drive with no media inserted
/// blocks, and by default Windows shows a modal "There is no disk in the drive"
/// dialog that waits for a human — which hangs the caller *indefinitely*, not
/// just slowly. [`SuppressMediaDialogs`] disables that behaviour for the
/// duration of the scan, but the calls can still take seconds each.
pub fn drives() -> Vec<Place> {
    let mut out = Vec::new();
    drives_streaming(|place| out.push(place));
    out
}

/// Same scan, but hands each volume to `emit` as soon as it resolves.
///
/// Fixed disks are probed first and removable/optical drives last, because a
/// single empty card reader can take many seconds to time out. Streaming in
/// that order means the drives a user actually wants appear immediately instead
/// of the whole sidebar waiting on the slowest device in the machine.
pub fn drives_streaming(mut emit: impl FnMut(Place)) {
    let _quiet = SuppressMediaDialogs::new();

    // SAFETY: no arguments, no failure mode beyond a zero result.
    let mask = unsafe { GetLogicalDrives() };

    let letters: Vec<char> = (0..26u32)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| (b'A' + bit as u8) as char)
        .collect();

    // Two passes so the cheap, always-present devices come first.
    let mut deferred = Vec::new();
    for letter in letters {
        if is_fast_drive(letter) {
            if let Some(place) = drive_place(letter) {
                emit(place);
            }
        } else {
            deferred.push(letter);
        }
    }

    for letter in deferred {
        if let Some(place) = drive_place(letter) {
            emit(place);
        }
    }
}

/// Whether the drive type can be probed without risking a media timeout.
///
/// `GetDriveTypeW` itself is cheap — it reads the mount table and never touches
/// the device — so this classification costs nothing.
fn is_fast_drive(letter: char) -> bool {
    let root = format!("{letter}:\\");
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let t = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
    t == DRIVE_FIXED || t == DRIVE_RAMDISK
}

/// Turns "no disk in drive" and similar hard-error dialogs into plain API
/// failures for the current thread, restoring the previous mode on drop.
///
/// Thread-scoped rather than process-wide (`SetErrorMode`), so it cannot
/// interfere with error handling on other threads.
struct SuppressMediaDialogs(THREAD_ERROR_MODE);

impl SuppressMediaDialogs {
    fn new() -> Self {
        let mut previous = THREAD_ERROR_MODE::default();
        // SAFETY: `previous` is a valid out-param for the duration of the call.
        let _ = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut previous)) };
        Self(previous)
    }
}

impl Drop for SuppressMediaDialogs {
    fn drop(&mut self) {
        // SAFETY: restoring a mode this thread previously reported.
        let _ = unsafe { SetThreadErrorMode(self.0, None) };
    }
}

fn drive_place(letter: char) -> Option<Place> {
    let root = format!("{letter}:\\");
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };

    // Skip drive letters that exist in the bitmask but have nothing behind
    // them, which is what an empty card reader looks like.
    //
    // These are `const u32`, not an enum, so they must be compared rather than
    // matched — a `match` arm would bind a fresh variable that matches
    // everything, silently labelling every volume "Local Disk".
    let type_name = if drive_type == DRIVE_FIXED {
        "Local Disk"
    } else if drive_type == DRIVE_REMOVABLE {
        "Removable Disk"
    } else if drive_type == DRIVE_REMOTE {
        "Network Drive"
    } else if drive_type == DRIVE_CDROM {
        "CD Drive"
    } else if drive_type == DRIVE_RAMDISK {
        "RAM Disk"
    } else {
        return None;
    };

    let label = volume_label(&wide).unwrap_or_default();
    let name = if label.is_empty() {
        format!("{type_name} ({letter}:)")
    } else {
        format!("{label} ({letter}:)")
    };

    Some(Place {
        name,
        id: NodeId::Path(PathBuf::from(&root)),
        kind: PlaceKind::Drive,
        capacity: capacity(&wide),
    })
}

fn volume_label(root_wide: &[u16]) -> Option<String> {
    let mut buf = [0u16; MAX_PATH as usize + 1];

    // SAFETY: `buf` is sized per the API contract; all other outputs are None.
    let ok = unsafe {
        GetVolumeInformationW(
            PCWSTR(root_wide.as_ptr()),
            Some(&mut buf),
            None,
            None,
            None,
            None,
        )
    };
    if ok.is_err() {
        // Normal for an empty removable drive — not worth logging.
        return None;
    }

    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn capacity(root_wide: &[u16]) -> Option<Capacity> {
    let mut free_to_caller = 0u64;
    let mut total = 0u64;

    // SAFETY: both out-params are valid for the duration of the call.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(root_wide.as_ptr()),
            Some(&mut free_to_caller),
            Some(&mut total),
            None,
        )
    };
    if ok.is_err() || total == 0 {
        return None;
    }

    // "Free to caller" rather than raw free space, so the figure respects any
    // per-user quota — which is what the user can actually write.
    Some(Capacity {
        total,
        free: free_to_caller,
    })
}
