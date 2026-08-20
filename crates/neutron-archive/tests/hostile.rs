//! End-to-end proof that a malicious archive cannot write outside the
//! destination.
//!
//! `path::safe_join` is unit-tested against every hostile name I could think
//! of, but a unit test only proves the function is right — not that the
//! extractor calls it, or that nothing else in the path writes a file. These
//! build real archives containing real traversal entries, extract them, and
//! then check the filesystem.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use neutron_archive::{Continue, Format, extract::extract};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("neutron-archive-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes a zip whose entries are exactly the names given, with no sanitising
/// of any kind — which is the point.
fn hostile_zip(at: &Path, names: &[&str]) {
    let file = fs::File::create(at).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);

    for name in names {
        // `start_file` would reject some of these, so the raw entry API is used
        // to write names a real attacker's archiver would produce.
        zip.start_file(*name, options)
            .or_else(|_| zip.start_file_from_path(Path::new(name), options))
            .unwrap();
        zip.write_all(b"payload").unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn a_traversal_entry_writes_nothing_outside_the_destination() {
    let root = scratch("traversal");
    let archive = root.join("evil.zip");
    let destination = root.join("out");

    // A sentinel beside the destination. If the extractor can be made to climb
    // one level, this is what it would overwrite.
    let victim = root.join("victim.txt");
    fs::write(&victim, "original").unwrap();

    hostile_zip(
        &archive,
        &[
            "../victim.txt",
            "..\\victim.txt",
            "../../victim.txt",
            "harmless.txt",
        ],
    );

    let summary = extract(&archive, &destination, Format::Zip, |_| Continue::Yes).unwrap();

    assert_eq!(
        fs::read_to_string(&victim).unwrap(),
        "original",
        "an entry escaped the destination and overwrote a file beside it"
    );
    assert!(destination.join("harmless.txt").is_file(), "the safe entry was dropped");
    assert_eq!(summary.files, 1);
    assert_eq!(summary.refused.len(), 3, "refusals: {:?}", summary.refused);
}

#[test]
fn an_absolute_entry_writes_nothing_outside_the_destination() {
    let root = scratch("absolute");
    let archive = root.join("evil.zip");
    let destination = root.join("out");

    let victim = root.join("absolute-victim.txt");
    fs::write(&victim, "original").unwrap();
    let absolute = victim.to_string_lossy().replace('\\', "/");

    hostile_zip(&archive, &[&absolute, "fine.txt"]);
    let summary = extract(&archive, &destination, Format::Zip, |_| Continue::Yes).unwrap();

    assert_eq!(fs::read_to_string(&victim).unwrap(), "original");
    assert!(destination.join("fine.txt").is_file());
    assert_eq!(summary.refused.len(), 1, "refusals: {:?}", summary.refused);
}

#[test]
fn everything_written_stays_under_the_destination() {
    let root = scratch("contained");
    let archive = root.join("mixed.zip");
    let destination = root.join("out");

    hostile_zip(
        &archive,
        &["a.txt", "deep/b.txt", "../up.txt", "./c.txt", "deep/../d.txt"],
    );
    extract(&archive, &destination, Format::Zip, |_| Continue::Yes).unwrap();

    // Walk what was produced and assert containment directly, rather than
    // trusting the list of names the test asked for.
    let mut queue = vec![destination.clone()];
    let mut seen = 0;
    while let Some(dir) = queue.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            assert!(
                path.starts_with(&destination),
                "{} is outside the destination",
                path.display()
            );
            if path.is_dir() {
                queue.push(path);
            } else {
                seen += 1;
            }
        }
    }
    assert!(seen > 0, "nothing was extracted at all");
    assert!(!root.join("up.txt").exists());
}

#[test]
fn a_cancelled_extraction_stops_and_says_so() {
    let root = scratch("cancel");
    let archive = root.join("many.zip");
    let destination = root.join("out");

    let names: Vec<String> = (0..50).map(|i| format!("file{i}.txt")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    hostile_zip(&archive, &refs);

    let mut seen = 0;
    let summary = extract(&archive, &destination, Format::Zip, |_| {
        seen += 1;
        if seen >= 3 { Continue::Stop } else { Continue::Yes }
    })
    .unwrap();

    assert!(summary.cancelled);
    assert!(summary.files < 50, "it kept going after being told to stop");
}

#[test]
fn a_file_that_is_not_an_archive_is_reported_rather_than_panicking() {
    let root = scratch("garbage");
    let archive = root.join("notreally.zip");
    fs::write(&archive, b"this is not a zip file at all").unwrap();

    let result = extract(&archive, &root.join("out"), Format::Zip, |_| Continue::Yes);
    assert!(result.is_err());
}

#[test]
fn modification_times_survive_a_round_trip() {
    // A file manager that loses dates through a zip is losing information the
    // user sorts by. The first version did: everything came out dated 1980,
    // which is what the format's zero value means.
    use neutron_archive::create;
    use std::time::{Duration, UNIX_EPOCH};

    let root = scratch("times");
    let source = root.join("src");
    fs::create_dir_all(&source).unwrap();
    let file = source.join("dated.txt");
    fs::write(&file, "content").unwrap();

    // 2021-03-04T05:06:07Z, chosen so every field is distinct and none is zero.
    let when = UNIX_EPOCH + Duration::from_secs(1_614_827_167);
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(when)
        .unwrap();

    let archive = root.join("out.zip");
    create::zip(&[file.clone()], &source, &archive, |_| Continue::Yes).unwrap();

    let destination = root.join("back");
    extract(&archive, &destination, Format::Zip, |_| Continue::Yes).unwrap();

    let restored = fs::metadata(destination.join("dated.txt"))
        .unwrap()
        .modified()
        .unwrap();

    // Two seconds of tolerance: a zip stores even seconds only.
    let (a, b) = (
        restored.duration_since(UNIX_EPOCH).unwrap().as_secs(),
        when.duration_since(UNIX_EPOCH).unwrap().as_secs(),
    );
    assert!(
        a.abs_diff(b) <= 2,
        "expected about {b}, got {a} — the timestamp did not survive"
    );
}
