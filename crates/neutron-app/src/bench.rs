//! Headless performance measurement: `neutron --bench <path> [iterations]`.
//!
//! Exists because the targets in `docs/perf.md` are only meaningful if they are
//! re-measured. Timing by eye in the running app conflates enumeration with GPU
//! bring-up and frame pacing; this isolates the two operations that scale with
//! directory size — enumerate and sort — and reports them separately.
//!
//! Runs on the calling thread with no window, so nothing here competes with the
//! renderer for a core.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use neutron_core::{NodeId, SortSpec, sort};

pub fn run(path: &str, iterations: usize) -> ! {
    let id = NodeId::Path(PathBuf::from(path));
    let namespace = neutron_shell::fs::FsNamespace;
    let spec = SortSpec::default();

    println!("neutron --bench {path}  ({iterations} iterations)\n");

    let mut enumerate_times = Vec::with_capacity(iterations);
    let mut sort_times = Vec::with_capacity(iterations);
    let mut entries = 0usize;

    for i in 0..iterations {
        let t0 = Instant::now();
        let mut list = match <neutron_shell::fs::FsNamespace as neutron_core::Namespace>::enumerate(
            &namespace, &id,
        ) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        };
        let enumerated = t0.elapsed();

        let t1 = Instant::now();
        sort::apply(&mut list, spec, false);
        let sorted = t1.elapsed();

        entries = list.len();
        enumerate_times.push(enumerated);
        sort_times.push(sorted);

        // The first pass warms the OS directory cache, so it is reported but
        // excluded from the summary — a cold figure and a warm figure measure
        // different things and averaging them measures neither.
        let tag = if i == 0 { "  (cold)" } else { "" };
        println!(
            "  run {:>2}: enumerate {:>7.2}ms   sort {:>7.2}ms{tag}",
            i + 1,
            ms(enumerated),
            ms(sorted),
        );
    }

    println!("\n  entries: {entries}");

    if iterations > 1 {
        report("enumerate", &enumerate_times[1..], entries);
        report("sort", &sort_times[1..], entries);
    }

    std::process::exit(0);
}

fn report(label: &str, times: &[Duration], entries: usize) {
    if times.is_empty() {
        return;
    }
    let mut sorted: Vec<f64> = times.iter().map(|d| ms(*d)).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
    let median = sorted[sorted.len() / 2];
    let min = sorted[0];
    let max = sorted[sorted.len() - 1];
    let per_entry_us = if entries > 0 {
        mean * 1000.0 / entries as f64
    } else {
        0.0
    };

    println!(
        "  {label:<10} warm: median {median:.2}ms  mean {mean:.2}ms  \
         min {min:.2}ms  max {max:.2}ms  ({per_entry_us:.3}µs/entry)"
    );
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
