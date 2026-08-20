//! Saving an index to disk, and loading it back without trusting it.
//!
//! # Why this exists
//!
//! Building the index needs a volume handle, which needs administrator rights,
//! which means a UAC prompt. Without a cache that prompt comes back after every
//! reboot, because the helper has to rebuild from the journal before it can
//! answer anything.
//!
//! With one, an *unelevated* helper can load the last index and start answering
//! immediately. Elevation is then only needed to bring it up to date, which is
//! something the user can be asked about at a moment of their choosing rather
//! than the moment they first type in a search box.
//!
//! # Why a hand-rolled format
//!
//! Because the index is already six flat arrays and a string. A serialisation
//! framework would add a dependency and a derive to save writing the twenty
//! lines below, and would not remove the part that actually matters — which is
//! not writing the file but refusing to trust it when it comes back.
//!
//! # Nothing here is trusted
//!
//! The arrays index into each other: `name_start[i]` is an offset into `names`,
//! `parent[i]` is a record number. A truncated or edited file whose offsets
//! point past the end would panic on the first search, or worse. [`load`]
//! therefore validates every cross-reference before handing back an index, and
//! a file that fails any check is discarded rather than repaired — the cost of
//! being wrong is a rebuild, which is what would have happened anyway.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::VolumeId;
use crate::volume::VolumeIndex;

/// Identifies the file and the layout it was written with.
///
/// The version is bumped whenever the field list changes. An old cache is then
/// rejected as unreadable rather than misread — the arrays have no internal
/// markers, so a layout mismatch would otherwise be silent corruption.
const MAGIC: &[u8; 8] = b"NTRNIDX\x02";

/// Where a volume's cache lives.
pub fn path_for(dir: &Path, volume: VolumeId) -> PathBuf {
    dir.join(format!("{}.idx", volume.0))
}

/// Writes `index` to `dir`, creating it if needed.
///
/// Written to a temporary file and renamed, so a crash or a full disk leaves
/// the previous cache intact rather than a half-written one that looks valid
/// until it is read.
pub fn save(dir: &Path, index: &VolumeIndex) -> io::Result<()> {
    fs::create_dir_all(dir)?;

    let final_path = path_for(dir, index.volume());
    let temp_path = final_path.with_extension("idx.tmp");

    {
        let mut out = io::BufWriter::new(fs::File::create(&temp_path)?);
        let parts = index.parts();

        out.write_all(MAGIC)?;
        out.write_all(&[index.volume().0 as u8])?;
        out.write_all(&index.next_usn.to_le_bytes())?;

        write_bytes(&mut out, parts.names.as_bytes())?;
        write_slice_u32(&mut out, parts.name_start)?;
        write_slice_u16(&mut out, parts.name_meta)?;
        write_slice_u64(&mut out, parts.frn)?;
        write_slice_u32(&mut out, parts.parent)?;
        write_slice_u32(&mut out, parts.byte_counts)?;
        out.flush()?;
    }

    fs::rename(&temp_path, &final_path)
}

/// How long ago `dir`'s newest cache file was written.
///
/// From the file's own timestamp rather than a field inside it: the writer
/// would have to be trusted about the time, and the filesystem already knows.
pub fn age(dir: &Path) -> Option<std::time::Duration> {
    let newest = fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "idx"))
        .filter_map(|entry| entry.metadata().ok()?.modified().ok())
        .max()?;
    std::time::SystemTime::now().duration_since(newest).ok()
}

/// Reads a volume's cache, or `None` if there is not a usable one.
///
/// Every failure — missing, truncated, wrong version, internally inconsistent —
/// is the same answer, because the caller does the same thing about all of
/// them: build the index from the journal instead.
pub fn load(dir: &Path, volume: VolumeId) -> Option<VolumeIndex> {
    let bytes = fs::read(path_for(dir, volume)).ok()?;
    let mut at = 0usize;

    if take(&bytes, &mut at, MAGIC.len())? != MAGIC {
        tracing::debug!(?volume, "index cache has a different format; ignoring it");
        return None;
    }

    let letter = take(&bytes, &mut at, 1)?[0];
    if letter as char != volume.0 {
        tracing::warn!(?volume, "index cache is for a different volume; ignoring it");
        return None;
    }

    let next_usn = i64::from_le_bytes(take(&bytes, &mut at, 8)?.try_into().ok()?);

    let names = String::from_utf8(read_bytes(&bytes, &mut at)?.to_vec()).ok()?;
    let name_start = read_u32s(&bytes, &mut at)?;
    let name_meta = read_u16s(&bytes, &mut at)?;
    let frn = read_u64s(&bytes, &mut at)?;
    let parent = read_u32s(&bytes, &mut at)?;
    let byte_counts = read_u32s(&bytes, &mut at)?;

    if at != bytes.len() {
        tracing::warn!(?volume, "index cache has trailing bytes; ignoring it");
        return None;
    }

    VolumeIndex::from_parts(volume, names, name_start, name_meta, frn, parent, byte_counts, next_usn)
}

// --- framing ---------------------------------------------------------------
//
// Every array is written as a little-endian length followed by its elements.
// Little-endian throughout and no alignment assumptions, so a cache is
// portable between builds even though in practice it never leaves the machine
// that wrote it.

fn write_bytes(out: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    out.write_all(&(bytes.len() as u64).to_le_bytes())?;
    out.write_all(bytes)
}

fn write_slice_u16(out: &mut impl Write, values: &[u16]) -> io::Result<()> {
    out.write_all(&(values.len() as u64).to_le_bytes())?;
    for v in values {
        out.write_all(&v.to_le_bytes())?;
    }
    Ok(())
}

fn write_slice_u32(out: &mut impl Write, values: &[u32]) -> io::Result<()> {
    out.write_all(&(values.len() as u64).to_le_bytes())?;
    for v in values {
        out.write_all(&v.to_le_bytes())?;
    }
    Ok(())
}

fn write_slice_u64(out: &mut impl Write, values: &[u64]) -> io::Result<()> {
    out.write_all(&(values.len() as u64).to_le_bytes())?;
    for v in values {
        out.write_all(&v.to_le_bytes())?;
    }
    Ok(())
}

/// `count` bytes from `at`, advancing it. `None` if the file is shorter than
/// it claims — the check every read below depends on.
fn take<'a>(bytes: &'a [u8], at: &mut usize, count: usize) -> Option<&'a [u8]> {
    let end = at.checked_add(count)?;
    let slice = bytes.get(*at..end)?;
    *at = end;
    Some(slice)
}

fn read_len(bytes: &[u8], at: &mut usize) -> Option<usize> {
    let raw = u64::from_le_bytes(take(bytes, at, 8)?.try_into().ok()?);
    // Rejected rather than truncated on a 32-bit target, and it also stops a
    // corrupt length from being used to compute a huge allocation below.
    usize::try_from(raw).ok()
}

fn read_bytes<'a>(bytes: &'a [u8], at: &mut usize) -> Option<&'a [u8]> {
    let len = read_len(bytes, at)?;
    take(bytes, at, len)
}

fn read_u16s(bytes: &[u8], at: &mut usize) -> Option<Vec<u16>> {
    let len = read_len(bytes, at)?;
    let raw = take(bytes, at, len.checked_mul(2)?)?;
    Some(
        raw.chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect(),
    )
}

fn read_u32s(bytes: &[u8], at: &mut usize) -> Option<Vec<u32>> {
    let len = read_len(bytes, at)?;
    let raw = take(bytes, at, len.checked_mul(4)?)?;
    Some(
        raw.chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

fn read_u64s(bytes: &[u8], at: &mut usize) -> Option<Vec<u64>> {
    let len = read_len(bytes, at)?;
    let raw = take(bytes, at, len.checked_mul(8)?)?;
    Some(
        raw.chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volume::RawRecord;

    fn sample() -> VolumeIndex {
        let records = ["alpha.txt", "beta", "gamma.dll"]
            .iter()
            .enumerate()
            .map(|(i, name)| RawRecord {
                frn: i as u64 + 1,
                parent: 1,
                name: (*name).to_owned(),
                is_dir: i == 1,
            })
            .collect();
        VolumeIndex::build(VolumeId('C'), records, 4242)
    }

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neutron-cache-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn an_index_survives_a_round_trip() {
        let dir = temp();
        let original = sample();
        save(&dir, &original).unwrap();

        let loaded = load(&dir, VolumeId('C')).expect("a cache just written must load");
        assert_eq!(loaded.len(), original.len());
        assert_eq!(loaded.next_usn, 4242);
        for i in 0..original.len() {
            assert_eq!(loaded.name(i), original.name(i));
            assert_eq!(loaded.is_dir(i), original.is_dir(i));
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_loaded_index_still_searches() {
        // The byte histogram is part of the file; without it every query would
        // pick record zero's pivot and quietly get slower.
        let dir = temp();
        save(&dir, &sample()).unwrap();
        let loaded = load(&dir, VolumeId('C')).unwrap();

        let mut hits = Vec::new();
        loaded.scan(0..loaded.len(), "gamma", |r| hits.push(loaded.name(r).to_owned()));
        assert_eq!(hits, ["gamma.dll"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_cache_is_not_an_error() {
        assert!(load(&temp(), VolumeId('C')).is_none());
    }

    #[test]
    fn a_truncated_cache_is_refused() {
        // The commonest real corruption: the machine lost power mid-write, or
        // a disk filled up. Every prefix of a valid file must be rejected.
        let dir = temp();
        save(&dir, &sample()).unwrap();
        let full = fs::read(path_for(&dir, VolumeId('C'))).unwrap();

        for cut in [0, 1, 8, 12, 20, full.len() / 2, full.len() - 1] {
            fs::write(path_for(&dir, VolumeId('C')), &full[..cut]).unwrap();
            assert!(
                load(&dir, VolumeId('C')).is_none(),
                "a file cut to {cut} bytes was accepted"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cache_with_a_different_magic_is_refused() {
        let dir = temp();
        save(&dir, &sample()).unwrap();
        let mut bytes = fs::read(path_for(&dir, VolumeId('C'))).unwrap();
        bytes[7] = b'\x01';
        fs::write(path_for(&dir, VolumeId('C')), &bytes).unwrap();
        assert!(load(&dir, VolumeId('C')).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cache_for_another_volume_is_refused() {
        // Otherwise a renamed file would attribute one disk's paths to another.
        let dir = temp();
        save(&dir, &sample()).unwrap();
        fs::rename(path_for(&dir, VolumeId('C')), path_for(&dir, VolumeId('D'))).unwrap();
        assert!(load(&dir, VolumeId('D')).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn trailing_bytes_are_refused() {
        // A file that parses but has more after it is not the file we wrote,
        // and guessing which part to believe is worse than rebuilding.
        let dir = temp();
        save(&dir, &sample()).unwrap();
        let mut bytes = fs::read(path_for(&dir, VolumeId('C'))).unwrap();
        bytes.push(0);
        fs::write(path_for(&dir, VolumeId('C')), &bytes).unwrap();
        assert!(load(&dir, VolumeId('C')).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_absurd_length_does_not_allocate() {
        // A corrupt length field is the classic way to turn a parser into an
        // out-of-memory abort. It must fail on the bounds check, not on the
        // allocator.
        let dir = temp();
        save(&dir, &sample()).unwrap();
        let mut bytes = fs::read(path_for(&dir, VolumeId('C'))).unwrap();
        let at = MAGIC.len() + 1 + 8;
        bytes[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(path_for(&dir, VolumeId('C')), &bytes).unwrap();
        assert!(load(&dir, VolumeId('C')).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
