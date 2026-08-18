//! How fast a full index scan actually is, and what is limiting it.
//!
//! `#[ignore]`d: this is a measurement, not a test. Nothing here asserts a
//! timing, because a build machine's number is not this machine's number and a
//! test that fails on a busy laptop teaches nobody anything.
//!
//! Run it with:
//!
//! ```text
//! cargo xtask test -- --release -- --ignored --nocapture scan_throughput
//! ```
//!
//! # Why synthetic names
//!
//! Building a real index needs the USN journal, which needs elevation. The
//! question here — is the scan limited by the matcher or by memory bandwidth? —
//! is answered by throughput over a realistic *volume* of realistic-shaped
//! names, and that can be manufactured. Absolute milliseconds will differ from a
//! real volume; bytes per second will not.

use std::time::Instant;

use neutron_index::VolumeId;
use neutron_index::query::Searcher;
use neutron_index::volume::{RawRecord, VolumeIndex};

/// Roughly the record count of six real fixed volumes on the development
/// machine, so the numbers are comparable with `docs/perf.md`.
const RECORDS: usize = 3_300_000;

/// Names shaped like the ones actually on a disk: mostly short, a long tail of
/// generated and hashed names, a realistic scattering of extensions.
fn synthetic_names(count: usize) -> Vec<RawRecord> {
    const STEMS: &[&str] = &[
        "index", "main", "config", "README", "setup", "Microsoft.Windows.Common",
        "libcrypto-3-x64", "en-US", "package-lock", "chunk-vendors", "a7f3c91e2b",
        "IMG_20240817", "notes", "Program", "System.Private.CoreLib", "de",
        "amd64_microsoft-windows-servicingstack", "test_utils", "node_modules",
    ];
    const EXTS: &[&str] = &[
        "", ".dll", ".exe", ".txt", ".json", ".js", ".rs", ".png", ".xml", ".mui", ".pdb",
    ];

    (0..count)
        .map(|i| {
            let stem = STEMS[i % STEMS.len()];
            let ext = EXTS[(i / STEMS.len()) % EXTS.len()];
            RawRecord {
                frn: i as u64 + 1,
                parent: (i as u64 / 40) + 1,
                // The counter keeps names distinct without making them all the
                // same length, which is what a real volume looks like.
                name: format!("{stem}{}{ext}", i % 977),
                is_dir: i % 40 == 0,
            }
        })
        .collect()
}

#[test]
#[ignore = "measurement, not an assertion"]
fn scan_throughput() {
    let built = Instant::now();
    let index = VolumeIndex::build(VolumeId('C'), synthetic_names(RECORDS), 0);
    println!(
        "built {} records in {:.2}s",
        index.len(),
        built.elapsed().as_secs_f64()
    );

    let bytes: usize = (0..index.len()).map(|i| index.name(i).len()).sum();
    println!(
        "name arena: {:.1} MB across {} records ({:.1} bytes each)",
        bytes as f64 / 1e6,
        index.len(),
        bytes as f64 / index.len() as f64
    );

    let volumes = [index];

    // Each needle gets a fresh `Searcher`, so every one of these is a full
    // scan — the path the target is missed on. Narrowing is measured
    // separately below.
    for needle in ["e", "config", "neutron", "setup.exe", "zzqx"] {
        let mut times = Vec::new();
        let mut total = 0;
        for _ in 0..5 {
            let mut searcher = Searcher::new();
            let started = Instant::now();
            let results = searcher.search(&volumes, needle);
            times.push(started.elapsed().as_secs_f64());
            total = results.total;
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let best = times[0];
        println!(
            "{:>10}  {:>9} matches   {:>6.2} ms   {:>5.1} GB/s effective",
            needle,
            total,
            best * 1e3,
            bytes as f64 / best / 1e9
        );
    }

    // How much of the remaining time is per-name call overhead rather than
    // memory bandwidth: the same bytes, swept in one call instead of 3.3M.
    {
        let index = &volumes[0];
        let arena: String = (0..index.len()).map(|i| index.name(i)).collect();
        let mut best = f64::MAX;
        for _ in 0..5 {
            let started = Instant::now();
            let found = memchr::memchr2(b'z', b'Z', arena.as_bytes());
            std::hint::black_box(found);
            best = best.min(started.elapsed().as_secs_f64());
        }
        println!(
            "  one sweep of the whole arena, single-threaded: {:.2} ms  ({:.1} GB/s)",
            best * 1e3,
            bytes as f64 / best / 1e9
        );
    }

    println!("  cores available: {}", std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0));

    // The other half of the design: a query that extends the last one filters
    // the previous result set instead of rescanning.
    let mut searcher = Searcher::new();
    searcher.search(&volumes, "neutro");
    let started = Instant::now();
    searcher.search(&volumes, "neutron");
    println!("  narrowing  {:>28.3} ms", started.elapsed().as_secs_f64() * 1e3);
}
