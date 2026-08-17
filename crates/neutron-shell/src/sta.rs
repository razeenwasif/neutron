//! A pool of single-threaded-apartment worker threads for shell COM calls.
//!
//! # Why an STA, and why a pool
//!
//! Most shell COM objects — `IShellFolder`, `IContextMenu`, icon handlers,
//! `IFileOperation` — are apartment-threaded. They must be created and used on
//! a thread that called `CoInitializeEx(COINIT_APARTMENTTHREADED)`, and a given
//! object may only be touched from the thread that made it. Calling them from a
//! multithreaded apartment either fails outright or silently marshals through a
//! proxy, which is slower and reintroduces the blocking this design exists to
//! avoid.
//!
//! The UI thread is not a candidate. Every one of these calls can block for
//! seconds: a third-party context-menu handler doing network I/O, an icon
//! handler for a file on a sleeping drive, a cloud placeholder. Neutron's whole
//! performance argument is that the paint thread never waits, so all of it goes
//! here and comes back over a channel.
//!
//! It is a *pool* rather than one thread because the work is independent and
//! latency-bound rather than CPU-bound: resolving thirty icons for a screenful
//! of rows means thirty calls that each spend most of their time waiting, and
//! serialising them behind one thread would make scrolling into a slideshow.
//!
//! # What a job may not do
//!
//! Jobs must not retain COM interface pointers past their own body. A pool
//! thread is not dedicated to any caller, so the next job on that thread — or
//! the same job re-run later — may land on a different apartment. Anything that
//! needs object affinity across calls needs its own dedicated thread, not this.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossbeam_channel::{Receiver, Sender};

/// Marks the calling thread as a single-threaded apartment for its lifetime.
///
/// For threads that are not part of the pool but still need shell COM — the
/// directory loader, which must be able to enumerate a namespace extension on
/// the same thread it enumerates ordinary directories, because routing one
/// listing through two threads would mean two staleness checks and a second
/// channel for the answer to come back on.
pub struct Apartment {
    /// Set only when this guard performed the initialisation, so a nested
    /// guard cannot uninitialise an apartment it did not create.
    owned: bool,
}

impl Apartment {
    /// Joins or creates this thread's apartment.
    pub fn enter() -> Self {
        #[cfg(windows)]
        {
            use windows::Win32::System::Com::{
                COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
            };
            // SAFETY: called before any COM use on this thread, and balanced in
            // Drop when it was this call that initialised.
            let hr =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
            // S_FALSE means the thread was already in a compatible apartment,
            // which is success — but *not* ours to tear down.
            Self {
                owned: hr.is_ok() && hr.0 == 0,
            }
        }
        #[cfg(not(windows))]
        Self { owned: false }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.owned {
            // SAFETY: balances the CoInitializeEx above.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

/// A unit of shell work.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// How many apartments to run. Shell work is latency-bound, so this is not
/// derived from the core count: four is enough to keep a screenful of icons
/// flowing while a couple of them are stuck on a slow handler, and more would
/// just multiply the number of threads a hung handler can occupy.
const THREADS: usize = 4;

/// A handle to the apartment pool. Cloning is cheap and shares the queue.
#[derive(Clone)]
pub struct StaPool {
    jobs: Sender<Job>,
    /// Jobs submitted but not yet finished. Lets callers avoid piling work onto
    /// an already-saturated pool.
    pending: Arc<AtomicUsize>,
}

impl StaPool {
    /// Starts the pool. Threads live until the last handle is dropped.
    pub fn spawn() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Job>();
        let pending = Arc::new(AtomicUsize::new(0));

        for i in 0..THREADS {
            let rx = rx.clone();
            let pending = Arc::clone(&pending);
            std::thread::Builder::new()
                .name(format!("neutron-sta-{i}"))
                .spawn(move || apartment(rx, pending))
                .expect("failed to spawn STA thread");
        }

        Self { jobs: tx, pending }
    }

    /// Queues `job`. Returns false if the pool is gone, which only happens
    /// during shutdown.
    pub fn submit(&self, job: impl FnOnce() + Send + 'static) -> bool {
        self.pending.fetch_add(1, Ordering::Relaxed);
        if self.jobs.send(Box::new(job)).is_err() {
            self.pending.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// Jobs queued or running. Callers use this to throttle: submitting a
    /// thousand icon lookups for a directory the user is scrolling past wastes
    /// the pool on results nobody will see.
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }
}

#[cfg(windows)]
fn apartment(jobs: Receiver<Job>, pending: Arc<AtomicUsize>) {
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };

    // SAFETY: called once per thread before any COM use, and paired with
    // CoUninitialize on the way out.
    //
    // DISABLE_OLE1DDE opts out of the OLE1 DDE compatibility layer, which
    // nothing since the 1990s needs and which costs a hidden window and a
    // message pump per apartment.
    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    if init.is_err() {
        tracing::error!(?init, "CoInitializeEx failed; this apartment is unusable");
        return;
    }

    run_jobs(jobs, pending);

    // SAFETY: balances the CoInitializeEx above; no COM object outlives a job.
    unsafe { CoUninitialize() };
}

#[cfg(not(windows))]
fn apartment(jobs: Receiver<Job>, pending: Arc<AtomicUsize>) {
    run_jobs(jobs, pending);
}

fn run_jobs(jobs: Receiver<Job>, pending: Arc<AtomicUsize>) {
    while let Ok(job) = jobs.recv() {
        // A panicking job would otherwise unwind out of the loop and take this
        // apartment with it — permanently, and silently. Losing one of four
        // threads is survivable; losing the accounting is not, because a
        // `pending` count that never comes back down makes the throttle in
        // `IconService` treat the pool as saturated forever and no icon is ever
        // requested again.
        //
        // This is a debug-build safety net only: the release profile sets
        // `panic = "abort"`, so there is nothing to catch there. It is also not
        // protection against third-party shell handlers, which fail with SEH
        // exceptions rather than Rust panics. What it catches is our own bugs
        // in icon conversion, on the thread furthest from the developer's eye.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
        pending.fetch_sub(1, Ordering::Relaxed);

        if outcome.is_err() {
            tracing::error!("a shell job panicked; the apartment continues");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn jobs_run_and_report_back() {
        let pool = StaPool::spawn();
        let (tx, rx) = mpsc::channel();

        for i in 0..16 {
            let tx = tx.clone();
            assert!(pool.submit(move || {
                let _ = tx.send(i);
            }));
        }
        drop(tx);

        let mut seen: Vec<i32> = rx.iter().collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..16).collect::<Vec<_>>());
    }

    #[test]
    fn the_pending_count_returns_to_zero() {
        // Throttling depends on this being accurate. A count that drifts upward
        // would eventually make the pool look permanently busy and stop every
        // icon from ever being requested.
        let pool = StaPool::spawn();
        let (tx, rx) = mpsc::channel();
        for _ in 0..32 {
            let tx = tx.clone();
            pool.submit(move || {
                let _ = tx.send(());
            });
        }
        drop(tx);
        assert_eq!(rx.iter().count(), 32);

        // The decrement happens just after the job body, so allow the last few
        // threads a moment to get there.
        for _ in 0..100 {
            if pool.pending() == 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("pending stuck at {}", pool.pending());
    }

    /// Only meaningful where panics unwind. The release profile aborts, and
    /// the point of the `catch_unwind` under test is the accounting, not
    /// crash-proofing — see the comment in `run_jobs`.
    #[test]
    #[cfg_attr(panic = "abort", ignore = "release profile aborts on panic")]
    fn a_panicking_job_does_not_take_the_apartment_with_it() {
        let pool = StaPool::spawn();

        for _ in 0..THREADS * 2 {
            pool.submit(|| panic!("a shell handler misbehaved"));
        }

        let (tx, rx) = mpsc::channel();
        for _ in 0..THREADS {
            let tx = tx.clone();
            pool.submit(move || {
                let _ = tx.send(());
            });
        }
        drop(tx);

        assert_eq!(
            rx.iter().count(),
            THREADS,
            "the pool stopped accepting work after a job panicked"
        );
    }
}
