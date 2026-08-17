//! Elevated helper that owns NTFS journal access.
//!
//! Split into its own process for one reason: reading the change journal needs
//! administrator rights, but the UI must *not* have them. An elevated window
//! cannot accept drag-and-drop from an unelevated Explorer — UIPI silently
//! drops the messages — and drag-and-drop is a core feature.
//!
//! So `neutron.exe` runs `asInvoker` and launches this once via
//! `ShellExecuteW(.., "runas", ..)`, producing a single UAC prompt. The two
//! communicate over a named pipe.
//!
//! ```text
//! neutron-indexer --serve <pipe>   index every fixed volume, then answer queries
//! neutron-indexer --bench          index and report timings, then exit
//! ```

use std::time::Instant;

use neutron_index::VolumeIndex;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("NEUTRON_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--bench") {
        return bench();
    }

    match args.iter().position(|a| a == "--serve") {
        Some(pos) => {
            let pipe = args
                .get(pos + 1)
                .cloned()
                .unwrap_or_else(|| neutron_index::protocol::DEFAULT_PIPE.to_owned());
            serve(&pipe)
        }
        None => {
            eprintln!("usage: neutron-indexer --serve <pipe> | --bench");
            std::process::exit(2);
        }
    }
}

/// Indexes every fixed volume and reports what it cost.
///
/// Exists to make the <10s target checkable without the pipe, the UI, or a UAC
/// prompt in the middle of a measurement.
#[cfg(windows)]
fn bench() -> anyhow::Result<()> {
    use neutron_index::usn;

    let volumes = usn::indexable_volumes();
    println!(
        "volumes: {}",
        volumes
            .iter()
            .map(|v| format!("{}:", v.0))
            .collect::<Vec<_>>()
            .join(" ")
    );

    if !usn::can_read_volumes() {
        println!("\nNOT ELEVATED — volume handles cannot be opened.");
        println!("Re-run this from an administrator prompt.");
        return Ok(());
    }

    use rayon::prelude::*;

    let overall = Instant::now();

    // Indexed in parallel, as the server does, so the number reported is the
    // one users actually wait for.
    /// One volume's outcome: the index and how long it took, or why it was
    /// skipped.
    type Indexed = Result<(VolumeIndex, f64), String>;

    let mut results: Vec<(char, Indexed)> = volumes
        .par_iter()
        .map(|volume| {
            let started = Instant::now();
            let outcome = usn::index_volume(*volume)
                .map(|index| (index, started.elapsed().as_secs_f64() * 1000.0))
                .map_err(|e| e.to_string());
            (volume.0, outcome)
        })
        .collect();
    results.sort_by_key(|(letter, _)| *letter);

    let mut indexes = Vec::new();
    let mut total_records = 0usize;
    let mut total_bytes = 0usize;

    for (letter, outcome) in results {
        match outcome {
            Ok((index, ms)) => {
                println!(
                    "  {}:  {:>9} records  {:>7.0}ms  {:>6.1} MB  ({:.2} µs/record)",
                    letter,
                    index.len(),
                    ms,
                    index.memory_bytes() as f64 / (1024.0 * 1024.0),
                    ms * 1000.0 / index.len().max(1) as f64,
                );
                total_records += index.len();
                total_bytes += index.memory_bytes();
                indexes.push(index);
            }
            Err(e) => println!("  {letter}:  skipped — {e}"),
        }
    }

    println!(
        "\ntotal: {total_records} records in {:.2}s, {:.0} MB resident",
        overall.elapsed().as_secs_f64(),
        total_bytes as f64 / (1024.0 * 1024.0),
    );

    query_bench(&indexes);
    Ok(())
}

#[cfg(not(windows))]
fn bench() -> anyhow::Result<()> {
    anyhow::bail!("the indexer is Windows-only")
}

/// Times a few representative queries against the freshly built index.
fn query_bench(indexes: &[VolumeIndex]) {
    if indexes.is_empty() {
        return;
    }
    println!("\nqueries:");

    // Each pair is a fresh searcher, so the first figure is always a full scan.
    // The second extends the query by a character — what every keystroke after
    // the first does — and only takes the narrowing path when the first result
    // set was complete. A capped result set is deliberately rescanned, so the
    // column says which happened rather than assuming.
    for needle in ["e", "config", "neutron", "setup.exe"] {
        let mut searcher = neutron_index::Searcher::new();
        let started = Instant::now();
        let results = searcher.search(indexes, needle);
        let cold = started.elapsed();

        let extended = format!("{needle}x");
        let started = Instant::now();
        searcher.search(indexes, &extended);
        let second = started.elapsed();

        println!(
            "  {:<12} {:>9} hits{}  scan {:>7.2}ms   +1 char {:>8.3}ms  ({})",
            format!("\"{needle}\""),
            results.total,
            if results.truncated { "*" } else { " " },
            cold.as_secs_f64() * 1000.0,
            second.as_secs_f64() * 1000.0,
            if results.truncated { "rescan" } else { "narrow" },
        );
    }
    println!("  * hit list capped at {} of the total", neutron_index::query::MAX_HITS);
}

#[cfg(windows)]
fn serve(pipe: &str) -> anyhow::Result<()> {
    server::run(pipe)
}

#[cfg(not(windows))]
fn serve(_pipe: &str) -> anyhow::Result<()> {
    anyhow::bail!("the indexer is Windows-only")
}

#[cfg(windows)]
mod server;
