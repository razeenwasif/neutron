//! Walking the NTFS change journal to build a volume index.
//!
//! # Why the journal rather than a directory walk
//!
//! `FSCTL_ENUM_USN_DATA` hands back every file reference number on the volume,
//! with its parent and name, in one sequential pass over the MFT. There is no
//! directory traversal, no per-directory open, and no seeking: it is a bulk
//! read of a table the filesystem already maintains. Millions of files in
//! seconds, against minutes for `FindFirstFileExW` recursion — and it is the
//! entire reason search can be instant.
//!
//! # Privileges
//!
//! Opening `\\.\C:` for reading requires administrator rights. This is not
//! avoidable: the volume handle is what the control code operates on. Callers
//! must expect [`UsnError::AccessDenied`] and have a plan for it — see the
//! `neutron-indexer` helper.
//!
//! # Threading
//!
//! Blocking and long-running. Worker thread only.

use std::path::PathBuf;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_HANDLE_EOF, ERROR_INVALID_FUNCTION,
    ERROR_JOURNAL_NOT_ACTIVE, HANDLE,
};
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL, MFT_ENUM_DATA_V0, USN_JOURNAL_DATA_V0,
};
use windows::core::PCWSTR;

use crate::VolumeId;
use crate::volume::{RawRecord, VolumeIndex};

/// Why indexing a volume failed.
#[derive(Debug, thiserror::Error)]
pub enum UsnError {
    /// The overwhelmingly common case, and the one the whole helper-process
    /// design exists for: reading a volume handle needs administrator rights.
    #[error("administrator rights are required to read {0}:")]
    AccessDenied(char),
    /// FAT32, exFAT, and network shares have no USN journal.
    #[error("{0}: is not an NTFS volume with an active journal")]
    NoJournal(char),
    #[error("{0}: could not be opened")]
    Unavailable(char),
    #[error("{volume}: journal read failed: {source}")]
    Io {
        volume: char,
        #[source]
        source: windows::core::Error,
    },
}

/// Buffer for one `FSCTL_ENUM_USN_DATA` call.
///
/// Large on purpose. Each call is a kernel transition plus an MFT read, and the
/// per-call overhead dominates at small sizes; 1MB brings back thousands of
/// records at a time and cuts a multi-million-file enumeration from minutes of
/// syscall overhead to seconds.
const BUFFER_BYTES: usize = 1024 * 1024;

/// Builds a complete index of one volume.
///
/// `filter` decides which records to keep. Returning false for a record also
/// drops it as a *parent*, so anything beneath it becomes a path root — used to
/// exclude system areas without leaving dangling references.
pub fn index_volume(volume: VolumeId) -> Result<VolumeIndex, UsnError> {
    let handle = open_volume(volume)?;
    let _guard = Handle(handle);

    let journal = query_journal(handle, volume)?;
    let records = enumerate(handle, volume)?;

    tracing::info!(
        volume = %volume.0,
        records = records.len(),
        "indexed volume"
    );

    Ok(VolumeIndex::build(volume, records, journal.NextUsn))
}

/// Opens `\\.\X:` for the control codes below.
fn open_volume(volume: VolumeId) -> Result<HANDLE, UsnError> {
    let path = format!(r"\\.\{}:", volume.0);
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: `wide` is NUL-terminated and outlives the call. Sharing both read
    // and write is required — the volume is in active use by the whole system,
    // and requesting exclusive access would fail every time.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            // GENERIC_READ. Measured: a zero-access handle opens fine but then
            // fails every journal FSCTL with ERROR_INVALID_FUNCTION, which
            // reads as "this volume has no journal" and is entirely
            // misleading — the control codes operate on the volume's data, so
            // the handle has to carry the right to read it.
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    };

    handle.map_err(|e| match e.code().0 as u32 & 0xFFFF {
        c if c == ERROR_ACCESS_DENIED.0 => UsnError::AccessDenied(volume.0),
        _ => {
            tracing::debug!(volume = %volume.0, "could not open volume: {e}");
            UsnError::Unavailable(volume.0)
        }
    })
}

/// Reads the journal header, mainly for the current USN cursor.
fn query_journal(handle: HANDLE, volume: VolumeId) -> Result<USN_JOURNAL_DATA_V0, UsnError> {
    let mut data = USN_JOURNAL_DATA_V0::default();
    let mut returned = 0u32;

    // SAFETY: `data` is a valid writable output buffer of the size given.
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_QUERY_USN_JOURNAL,
            None,
            0,
            Some(&mut data as *mut _ as *mut _),
            std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32,
            Some(&mut returned),
            None,
        )
    };

    ok.map_err(|e| {
        let code = e.code().0 as u32 & 0xFFFF;
        // A volume with no journal is not an error worth surfacing loudly —
        // FAT32 sticks and network mounts simply do not have one.
        tracing::debug!(volume = %volume.0, code, "journal query failed");
        if code == ERROR_JOURNAL_NOT_ACTIVE.0 || code == ERROR_INVALID_FUNCTION.0 {
            UsnError::NoJournal(volume.0)
        } else {
            UsnError::Io {
                volume: volume.0,
                source: e,
            }
        }
    })?;

    Ok(data)
}

/// Walks every record on the volume.
fn enumerate(handle: HANDLE, volume: VolumeId) -> Result<Vec<RawRecord>, UsnError> {
    let mut input = MFT_ENUM_DATA_V0 {
        StartFileReferenceNumber: 0,
        LowUsn: 0,
        // Everything up to the present. A narrower range would miss files that
        // have not been modified since the journal was created.
        HighUsn: i64::MAX,
    };

    let mut buffer = vec![0u8; BUFFER_BYTES];
    // A rough guess at the record count, to avoid a dozen reallocations of a
    // multi-million-element vector on the way up.
    let mut records = Vec::with_capacity(1 << 16);

    loop {
        let mut returned = 0u32;

        // SAFETY: `input` and `buffer` are valid for the duration, and the
        // sizes passed describe them exactly.
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_ENUM_USN_DATA,
                Some(&input as *const _ as *const _),
                std::mem::size_of::<MFT_ENUM_DATA_V0>() as u32,
                Some(buffer.as_mut_ptr() as *mut _),
                buffer.len() as u32,
                Some(&mut returned),
                None,
            )
        };

        if let Err(e) = ok {
            // EOF is how the enumeration ends, not a failure.
            if e.code().0 as u32 & 0xFFFF == ERROR_HANDLE_EOF.0 {
                break;
            }
            return Err(UsnError::Io {
                volume: volume.0,
                source: e,
            });
        }

        // The first 8 bytes are the FRN to resume from; records follow.
        if (returned as usize) < 8 {
            break;
        }

        input.StartFileReferenceNumber =
            u64::from_ne_bytes(buffer[..8].try_into().expect("8 bytes"));

        parse_records(&buffer[8..returned as usize], &mut records);
    }

    Ok(records)
}

/// Decodes a run of `USN_RECORD_V2` structures.
///
/// Parsed by offset rather than by casting to the `windows` struct: the record
/// is variable-length — the filename lives past the end of the fixed part — so
/// a typed read would need an unsized type anyway, and the field offsets are
/// stable and documented.
fn parse_records(mut buffer: &[u8], out: &mut Vec<RawRecord>) {
    // Field offsets within USN_RECORD_V2.
    const RECORD_LENGTH: usize = 0;
    const FILE_REFERENCE: usize = 8;
    const PARENT_REFERENCE: usize = 16;
    const FILE_ATTRIBUTES: usize = 52;
    const FILE_NAME_LENGTH: usize = 56;
    const FILE_NAME_OFFSET: usize = 58;
    const HEADER_BYTES: usize = 60;

    while buffer.len() >= HEADER_BYTES {
        let length = read_u32(buffer, RECORD_LENGTH) as usize;
        // A zero or oversized length would loop forever or read out of bounds.
        // The kernel does not produce either, but this is parsing bytes from a
        // device and the cost of being sure is one comparison.
        if length < HEADER_BYTES || length > buffer.len() {
            break;
        }

        let record = &buffer[..length];
        let name_len = read_u16(record, FILE_NAME_LENGTH) as usize;
        let name_off = read_u16(record, FILE_NAME_OFFSET) as usize;

        if name_off + name_len <= length {
            let raw = &record[name_off..name_off + name_len];
            // UTF-16, and the length is in bytes rather than code units.
            let utf16: Vec<u16> = raw
                .chunks_exact(2)
                .map(|c| u16::from_ne_bytes([c[0], c[1]]))
                .collect();

            let attributes = read_u32(record, FILE_ATTRIBUTES);
            out.push(RawRecord {
                frn: read_u64(record, FILE_REFERENCE),
                parent: read_u64(record, PARENT_REFERENCE),
                // Lossy: a filename that is not valid UTF-16 is still worth
                // indexing under a mangled name, since it is still findable by
                // the parts that did decode.
                name: String::from_utf16_lossy(&utf16),
                is_dir: attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
            });
        }

        buffer = &buffer[length..];
    }
}

fn read_u16(b: &[u8], at: usize) -> u16 {
    u16::from_ne_bytes(b[at..at + 2].try_into().expect("2 bytes"))
}

fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn read_u64(b: &[u8], at: usize) -> u64 {
    u64::from_ne_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// Closes a volume handle on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: opened by CreateFileW above and not used afterwards.
        unsafe { let _ = CloseHandle(self.0); };
    }
}

/// The fixed NTFS volumes worth indexing.
///
/// Removable and network volumes are excluded: they come and go, a network
/// share has no journal to read, and indexing a USB stick that is unplugged a
/// minute later spends the whole budget for nothing.
pub fn indexable_volumes() -> Vec<VolumeId> {
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;

    // SAFETY: no arguments, no failure mode beyond a zero result.
    let mask = unsafe { GetLogicalDrives() };

    (0..26u32)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| (b'A' + bit as u8) as char)
        .filter(|letter| {
            let root = format!("{letter}:\\");
            let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: `wide` is NUL-terminated and outlives the call.
            unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) == DRIVE_FIXED }
        })
        .map(VolumeId)
        .collect()
}

/// Whether the current process can read volume handles.
///
/// Checked by trying, rather than by inspecting the token: the answer that
/// matters is whether the open succeeds, and elevation is not the only thing
/// that can affect it.
pub fn can_read_volumes() -> bool {
    indexable_volumes()
        .first()
        .is_some_and(|v| match open_volume(*v) {
            Ok(h) => {
                let _guard = Handle(h);
                true
            }
            Err(_) => false,
        })
}

/// Where the on-disk index cache lives.
pub fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|base| PathBuf::from(base).join("Neutron"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic `USN_RECORD_V2` so the parser can be exercised
    /// without a volume handle — which needs administrator rights and a real
    /// NTFS disk, neither available in a unit test.
    fn record(frn: u64, parent: u64, name: &str, is_dir: bool) -> Vec<u8> {
        let utf16: Vec<u16> = name.encode_utf16().collect();
        let name_bytes = utf16.len() * 2;
        let length = 60 + name_bytes;

        let mut b = vec![0u8; length];
        b[0..4].copy_from_slice(&(length as u32).to_ne_bytes());
        b[4..6].copy_from_slice(&2u16.to_ne_bytes()); // major version
        b[8..16].copy_from_slice(&frn.to_ne_bytes());
        b[16..24].copy_from_slice(&parent.to_ne_bytes());
        b[52..56].copy_from_slice(
            &(if is_dir { FILE_ATTRIBUTE_DIRECTORY.0 } else { 0 }).to_ne_bytes(),
        );
        b[56..58].copy_from_slice(&(name_bytes as u16).to_ne_bytes());
        b[58..60].copy_from_slice(&60u16.to_ne_bytes());
        for (i, unit) in utf16.iter().enumerate() {
            b[60 + i * 2..62 + i * 2].copy_from_slice(&unit.to_ne_bytes());
        }
        b
    }

    #[test]
    fn a_single_record_round_trips() {
        let mut out = Vec::new();
        parse_records(&record(42, 5, "notes.txt", false), &mut out);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].frn, 42);
        assert_eq!(out[0].parent, 5);
        assert_eq!(out[0].name, "notes.txt");
        assert!(!out[0].is_dir);
    }

    #[test]
    fn consecutive_records_are_walked_by_their_length_field() {
        // The records are variable-length and packed back to back; advancing by
        // anything other than the length field desynchronises the whole buffer
        // and produces garbage from the second record on.
        let mut buffer = record(1, 5, "a.txt", false);
        buffer.extend(record(2, 5, "much-longer-name.txt", false));
        buffer.extend(record(3, 5, "Windows", true));

        let mut out = Vec::new();
        parse_records(&buffer, &mut out);

        assert_eq!(out.len(), 3);
        assert_eq!(out[1].name, "much-longer-name.txt");
        assert_eq!(out[2].name, "Windows");
        assert!(out[2].is_dir);
    }

    #[test]
    fn non_ascii_names_decode() {
        let mut out = Vec::new();
        parse_records(&record(1, 5, "日本語 🦀.txt", false), &mut out);
        assert_eq!(out[0].name, "日本語 🦀.txt");
    }

    #[test]
    fn a_truncated_buffer_stops_cleanly() {
        // The last record in a returned buffer can be cut short. Reading past
        // it would panic on a slice, which in the indexer is a lost volume.
        let full = record(1, 5, "abcdefgh.txt", false);
        for cut in 0..full.len() {
            let mut out = Vec::new();
            parse_records(&full[..cut], &mut out);
            // Either it parsed nothing or it parsed the whole record; never a
            // partial one, and never a panic.
            assert!(out.len() <= 1);
        }
    }

    #[test]
    fn a_zero_length_record_does_not_loop_forever() {
        // A malformed length of zero would advance the cursor by nothing. This
        // is bytes from a device driver, so the guard is not theoretical.
        let mut buffer = record(1, 5, "a.txt", false);
        buffer[0..4].copy_from_slice(&0u32.to_ne_bytes());

        let mut out = Vec::new();
        parse_records(&buffer, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn a_length_past_the_buffer_end_is_rejected() {
        let mut buffer = record(1, 5, "a.txt", false);
        buffer[0..4].copy_from_slice(&9999u32.to_ne_bytes());

        let mut out = Vec::new();
        parse_records(&buffer, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn a_name_reaching_past_the_record_is_skipped() {
        // Offset and length are read from the device; a bad pair must not slice
        // out of bounds.
        let mut buffer = record(1, 5, "a.txt", false);
        buffer[56..58].copy_from_slice(&9999u16.to_ne_bytes());

        let mut out = Vec::new();
        parse_records(&buffer, &mut out);
        assert!(out.is_empty());
    }
}
