//! What the pane's filter field costs per keystroke.
//!
//! ```text
//! cargo run -p neutron-core --release --example filter_bench
//! ```
//!
//! An example rather than a test because it asserts nothing: a timing that
//! fails on a busy machine teaches nobody anything. It exists so the number can
//! be reproduced rather than remembered.
//!
//! The three needles separate the two costs. `zzz` matches nothing, so it is
//! the filter alone. `component` matches everything, so it is dominated by
//! sorting the hundred thousand survivors — filtering faster does not help
//! there, and that is worth seeing.

use std::time::Instant;

use neutron_core::entry::{Entry, EntryKind, SyncState};
use neutron_core::{EntryList, SortSpec};

const ENTRIES: usize = 100_000;

fn main() {
    let mut list = EntryList::new();
    for i in 0..ENTRIES {
        list.push(&Entry {
            name: format!("Microsoft.Windows.Component.{i}.dll"),
            kind: EntryKind::File,
            size: i as u64,
            modified: 0,
            created: 0,
            attrs: 0,
            sync: SyncState::None,
        });
    }
    println!("{ENTRIES} entries");

    for needle in ["component", "9999", "zzz"] {
        let mut best = f64::MAX;
        for _ in 0..5 {
            let started = Instant::now();
            neutron_core::sort::apply_filtered(&mut list, SortSpec::default(), true, needle);
            best = best.min(started.elapsed().as_secs_f64());
        }
        println!(
            "  filter {needle:>10}: {:>5.2} ms   ({} shown)",
            best * 1e3,
            list.order().len()
        );
    }
}
