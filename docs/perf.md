# Performance baseline

Measured numbers, not estimates. Every figure came from running the actual
binary on the target machine.

Re-measure after each milestone and update this file. A target that is never
checked is decoration.

**Machine:** Windows 11, WSL2 host, 6 fixed NTFS volumes (A, B, C, F, G, I),
hybrid graphics (integrated + discrete).

---

## ⚠ Measuring from WSL: `WSLENV` is mandatory

Environment variables set in a WSL shell **do not reach a Windows process**
unless they are also named in `WSLENV`:

```bash
NEUTRON_LOG=debug ./neutron.exe              # variable never arrives
WSLENV=NEUTRON_LOG NEUTRON_LOG=debug ./neutron.exe   # correct
```

This is not a footnote — it silently invalidated a whole round of measurement
here. An earlier version of this document reported a backend comparison
(“Vulkan is 35% slower than DX12”) that was **wrong**: `NEUTRON_GPU_BACKEND`
never reached the process, so all three "backends" were the DX12 default and
the table was recording nothing but run-to-run variance. The same problem had
already bitten the build wrapper via `CARGO_TARGET_DIR`, which is why `xtask`
passes `--target-dir` as a flag instead.

**Any measurement configured through an environment variable must set `WSLENV`,
and should be sanity-checked by confirming the setting actually took effect.**

---

## M1 — real browsing (2026-08-14)

Release build, `neutron.exe` ~11 MB.

### Directory scaling — `neutron --bench <path> <iterations>`

Enumerate and sort measured headless, so the numbers are not confounded by GPU
bring-up or frame pacing. First iteration is reported but excluded from the
summary; it warms the OS directory cache, and averaging a cold run with warm
runs measures neither.

| Directory | Entries | Enumerate | Sort | Total | µs/entry |
|---|---:|---:|---:|---:|---:|
| `C:\Windows\System32` | 5,020 | 2.4 ms | 1.0 ms | **3.4 ms** | 0.67 |
| `C:\Windows\WinSxS` | 27,437 | 42.8 ms | 23.3 ms | **66 ms** | 2.45 |
| synthetic bench dir | 100,200 | 53.4 ms | 34.8 ms | **88 ms** | 0.88 |

**100k target: 88 ms against a 200 ms budget — 2.3× under.** ✅

WinSxS costs ~3× more per entry than the synthetic directory despite being
smaller. Its entries are almost all directories with long, near-identical names
(`amd64_microsoft-windows-…`), so the natural-order comparison has to walk deep
into each string before it can decide. Worth remembering as the realistic
worst case: per-entry cost depends on name length and shared prefixes, not just
entry count.

### Frame time — scrolling a 100,200-entry directory

Measured around the app's `ui()` body: the cost of *building* a frame,
excluding GPU submission and vsync wait.

| Metric | Value | Budget |
|---|---:|---:|
| p50 | **0.14 ms** | — |
| p99 | **0.40 ms** | 8 ms ✅ |

Sustained over 300 PageDown presses through the full list. **20× under budget**,
and effectively identical to the same measurement on a small directory — which
is the entire point of virtualizing: `ScrollArea::show_rows` renders roughly 30
rows whether the directory holds 30 entries or 500,000.

> An earlier version of this metric timed the *interval between* frames rather
> than the cost of building one. That conflates idle throttling with slowness,
> and reported ~9 ms where the real figure is 0.14 ms. (At the time the ambient
> layer was animated and capped repaints at ~30fps, which is what the interval
> was really measuring; it is static now.)

### Cold start

| Phase | Time | What it covers |
|---|---:|---|
| `setup` | 0 ms | Logging init, option construction |
| `gpu` | ~720–1570 ms | winit window + wgpu adapter, device, surface |
| `paint` | 2–5 ms | Building and rendering the first frame |

**GPU bring-up is ~99% of cold start.** Our own frame construction is 2–5 ms.

#### Backend comparison (6 runs each, `WSLENV` correctly set)

| Backend | Median | Min | Max |
|---|---:|---:|---:|
| **DX12** (selected) | **1040 ms** | 722 ms | 1571 ms |
| GL | 1618 ms | 1007 ms | 1984 ms |
| Vulkan (wgpu default) | 1974 ms | 1013 ms | 2173 ms |

DX12 is the fastest of the three at the median, by a wide margin. Two caveats
worth keeping honest:

* **Run-to-run variance is enormous** — DX12 alone spans 722–1571 ms. Any
  comparison based on one run per backend is meaningless, which is precisely
  how the earlier invalid table got written.
* A second, independent reason to prefer DX12: the Vulkan loader enumerates
  every registered layer at instance creation, and the logs showed it pulling
  OBS capture-hook DLLs into the process before the first frame. That is an
  observed fact rather than an inference, and it holds regardless of timing.

Both backend and power preference stay overridable (`NEUTRON_GPU_BACKEND`,
`NEUTRON_GPU_POWER`) so this table can be regenerated on other hardware.

### Memory — 100k-entry directory open, idle

| Metric | Value |
|---|---:|
| Working set | ~280 MB |
| Private bytes | ~470 MB |
| Threads | 43 |

Dominated by the wgpu/DX12 stack and driver allocations. The 100k-entry
`EntryList` itself is small: one string arena plus eight parallel arrays, on the
order of 6 MB.

### Idle CPU — window open, focused, untouched

| Build | CPU over the sample | Share of one core |
|---|---:|---:|
| Animated background, per-frame title push | 44 ms / 5 s | 8.8% |
| After both fixes | 0 ms / 5 s | 0.0% |
| With the static prismatic ground (2026-08-17) | 16 ms / 6 s | 0.3% |

egui only repaints in response to input or an explicit request, so an idle
window should cost nothing. Two things were quietly preventing that:

1. **An animated background.** Drifting the ground's colour fields required
   `request_repaint_after(33ms)` forever — a decoration holding the event loop
   permanently awake. The orbs are now static, which is why they can exist at
   all.
2. **`ViewportCommand::Title` sent every frame.** The title only changes when
   the focused tab does, but it was pushed unconditionally from `drain_loads`,
   and issuing any viewport command wakes the loop. Now gated on a stored
   `last_title`.

The 0.3% reading is the whole-window redraw the compositor asks for
occasionally, not a spin — the background itself is four triangle fans, ~500
triangles, submitted only when a frame is being drawn anyway.

**Watch for this.** Anything that calls `request_repaint`, `request_repaint_after`
or `send_viewport_cmd` unconditionally in the frame path costs a permanently
awake event loop, and it is invisible in frame-time numbers: p99 frame build was
0.40 ms throughout, because each frame really was fast. There were just tens of
thousands of them that nobody asked for.

---

## Targets vs. reality

| Metric | Target | Actual | Status |
|---|---:|---:|---|
| Enumerate 100k-file dir | <200 ms | ~92 ms | ✅ 2.2× under |
| Frame time p99 scrolling | <8 ms | 0.23 ms | ✅ 35× under |
| Cold start | <100 ms | 374 ms | ❌ **not achievable as architected** (369 ms of it is GPU device creation) |
| Idle memory | <60 MB | 310 MB | ❌ **not achievable as architected** |
| Idle CPU, focused | ~0% | 1.17% | ➖ 10 fps for a drifting ground |
| Idle CPU, unfocused | ~0% | 0.39% | ✅ |
| Binary size | ~15 MB | 11 MB | ✅ |

### The two failures are the same failure

Both come from one decision: a GPU-accelerated renderer. Creating a D3D12
device, allocating a swapchain, and compiling the first pipeline costs the best
part of a second and a few hundred megabytes of driver-side allocation before a
single pixel of *our* UI exists. No optimization of Neutron's code moves either
number — which is exactly what `paint 2ms` and `p99 0.40ms` prove.

The `<100 ms` figure was projected from egui's reputation for fast frames. That
reputation is about *steady-state* frame time, where the measured 0.14 ms
genuinely is excellent; it was wrong to extend it to process startup.

### Options, if these targets matter

1. **Revise the targets.** ~1 s is ordinary for a GPU desktop app. Explorer only
   feels instant because `explorer.exe` is already running as the shell.
2. **Warm-start helper.** Keep a preloaded process resident so perceived launch
   is instant — the same advantage Explorer gets for free, and the only approach
   that reaches "instant" while keeping the GPU renderer.
3. **Change renderer.** A CPU-rasterized or Direct2D backend would start far
   faster and use far less memory, at the cost of the icon-atlas and
   smooth-scrolling work that motivated wgpu.

Steady-state performance — the thing the project is actually about, and where
Explorer really does fall over — is unaffected by any of this.

---

## Pending metrics

| Metric | Target | Milestone |
|---|---:|---|
| Full index, all 6 volumes | <10 s | M4 — **met**, 2.98 s warm |
| Search query latency | <1 ms | M4 — **missed**, 1.6–4.7 ms |

## M3 — shell integration (2026-08-17)

| Metric | Target | Actual |
|---|---|---|
| Icon resolution off the UI thread | no frame impact | ✅ — resolved on the STA pool, drawn from an atlas; rows never wait |
| Distinct icons for `C:\Windows\System32` (5,010 entries) | — | **7** |
| Idle CPU with a context menu open | — | 0.0 ms / 4 s |

The icon count is the whole design in one number: keyed by extension rather than
by file, 5,010 entries need seven lookups. Executables are the exception and are
keyed per path, which is why the count rises as `.exe` files scroll into view.

Not measured, and worth doing before M4: enumerate time for a directory of many
*distinct* extensions, where the per-frame submission cap (48 keys) rather than
the cache is the limit.

## M4 — index and search (2026-08-17)

Release build, run elevated. Six fixed volumes; B: and G: have no USN journal
and are skipped.

### Indexing

| Volume | Records | Cold run | Warm run | µs/record (warm) |
|---|---:|---:|---:|---:|
| A: | 178,050 | 1,410 ms | 458 ms | 2.58 |
| C: | 1,395,666 | 8,464 ms | 2,623 ms | 1.88 |
| F: | 1,657,296 | 15,729 ms | 2,614 ms | 1.58 |
| I: | 56,387 | 9,071 ms | 219 ms | 3.89 |
| **total** | **3,287,399** | **34.68 s** | **2.62 s** | — |

**Target <10s: met on the warm run at 2.62s, missed cold at 34.7s.**

Three changes separate the runs:

* **Volumes index in parallel.** They are independent and usually separate
  physical devices, so wall time becomes the slowest volume rather than the sum.
* **The sort is skipped.** `FSCTL_ENUM_USN_DATA` already returns ascending FRN
  order, so `build` verifies rather than sorts — `sort_unstable_by_key` over
  three million `RawRecord`s moves a 48-byte struct carrying an owned `String`
  on every swap.
* **The FRN map uses a multiply, not SipHash.** File reference numbers are dense
  and filesystem-assigned, not attacker-chosen, so a cryptographic mixer is work
  done for nothing three million times.

> **The two runs are not a clean comparison.** The second ran against a warm MFT
> cache. `I:` is the tell: 160.88 → 3.89 µs/record is 41×, which no amount of
> skipped sorting explains for 56k records — that volume's cold cost was device
> I/O. Some part of C:'s and F:'s improvement is warmth too, and separating them
> needs a reboot between runs. **Treat 2.62s as the warm figure and expect the
> first index after a boot to be several times that.**

### Query latency

Against all 3.29M records.

| Query | Matches | Full scan | Next keystroke | Which path |
|---|---:|---:|---:|---|
| `e` | 2,280,312 | 5.78 ms | 10.54 ms | rescan (capped) |
| `config` | 16,356 | 9.39 ms | 9.04 ms | rescan (capped) |
| `neutron` | 639 | 8.69 ms | 0.025 ms | narrow |
| `setup.exe` | 109 | 7.45 ms | 0.003 ms | narrow |

**Target <1ms: met for the incremental path, missed for a full scan at 6–9ms.**

The narrowing works exactly as designed — once a result set is complete, further
typing is effectively free. The gap is the first character of any query, and any
query whose result set stays above the 4,096 hit cap, since a capped set cannot
be narrowed without hiding matches.

8ms is half a frame, so this is below the threshold where a keystroke feels
delayed — but the target says <1ms and it is not met, which is worth stating
plainly rather than redefining. Closing it would mean an n-gram or suffix
structure over the name arena, which is a real memory cost for a latency nobody
can perceive; not obviously worth it.

### Memory

202 MB resident for 3.29M records, in the helper process rather than the UI.
Roughly: names ~100MB, FRNs 26MB, parents 13MB, offsets 13MB.

## M5 — finder and command palette (2026-08-17)

| Path | Corpus | Measured |
|---|---|---:|
| `Ctrl+P` fuzzy, scoped to one folder | 3.29M records scanned, scoped | **14.3 ms** |
| `Ctrl+Shift+P` command palette | 21 commands, in-process | instant |
| `Ctrl+Shift+F` substring, global | 3.29M records | 6–9 ms |

The scoped fuzzy search is three stages, each feeding the next a smaller set:

1. A subsequence test over every name — no allocation, rejects almost
   everything, and cannot reject anything the scorer could have matched.
2. Path reconstruction and a scope prefix test, only for survivors.
3. Real fuzzy scoring, only for what is left.

Order matters: reconstructing a path costs a parent-chain walk, so doing it
before the subsequence gate would pay that walk 3.3 million times per keystroke.

14.3 ms is slower than the global substring search despite touching fewer
candidates, which is the expected shape — fuzzy scoring is far more expensive
per candidate, and the parent walk for survivors is not free. It is still under
a frame.

> **The one-off indexing cost is variable.** This session's restart took 10.5s
> against 2.6s earlier, on the same machine and binary. Cache state is the
> difference, as documented above — worth remembering before reading any single
> index timing as the number.

## M6 — shell namespace (2026-08-18)

| Location | Items | Enumerate |
|---|---:|---:|
| This PC | 8 | 79 ms |
| Recycle Bin | 9 | — |
| Inside a zip | 2 | — |

79ms for eight items is three orders of magnitude worse per entry than the
filesystem path, and that is expected rather than a regression: each item costs
a `GetAttributesOf`, a `GetDisplayNameOf`, an `ILCombine` and an
`SHGetNameFromIDList`, all COM, and the drive entries additionally spin up
volume queries. It is why the dispatch exists — ordinary directories must never
take this path.

Neither size nor timestamps are read for shell items. `IShellFolder2::GetDetailsEx`
would supply them at a COM call per column per row; these places are browsed,
not audited, and a blank column beats a listing that takes a second.

## Perf pass (2026-08-19)

All figures below are **release** builds on the development machine (16 cores),
re-measured together so they are comparable with each other. The earlier numbers
in this file were a mix of debug and release, and some were taken with a sampling
script that read a stale `TotalProcessorTime` before its first interval — the
idle-CPU figures above are inflated by roughly the process's startup burst.
Treat this section as the current state.

### Idle CPU — the ground was repainting far faster than it moves

Sampled over 12 s after a 5 s settle, window open on a 149-entry folder.

| | before | after |
|---|---:|---:|
| Focused | 4.69% of one core | **1.17%** |
| Unfocused | 5.99% | **0.39%** |
| Working set | 305 MB | 310 MB |

The repaint interval was a flat 30 fps, chosen as "a sensible frame rate" rather
than from anything about the animation. The two colour fields loop over 41 s and
57 s; at their fastest that is about 8 px/s across a 1500 px window, so 10 fps
moves a soft gradient by under a pixel per frame. Twenty of those thirty frames
a second were producing no visible change at all.

Now 10 fps focused, 2 fps unfocused, 0.5 fps minimised. This governs only the
*idle* rate — input, an arriving icon and a finished background job each request
their own repaint, so scrolling and hovering are unaffected.

### Search — 3× faster, and now bandwidth-bound rather than overhead-bound

Measured with `crates/neutron-index/tests/scan_throughput.rs`, which builds
3.3M synthetic records (58.2 MB of names, 17.6 bytes each) and times full scans.
It is `#[ignore]`d and asserts nothing: a timing that fails on a busy machine
teaches nobody anything. Synthetic because a real index needs the USN journal
and therefore elevation, and the question — what is the limit? — is answered by
throughput over a realistic *volume* of realistic-shaped names.

| Query | Matches | Before | After |
|---|---:|---:|---:|
| `e` | 2,352,634 | 4.53 ms | **2.79 ms** |
| `config` | 173,685 | 5.08 ms | **1.38 ms** |
| `neutron` | 0 | 4.02 ms | **1.43 ms** |
| `setup.exe` | 0 | 3.79 ms | **1.48 ms** |
| `zzqx` | 0 | 5.37 ms | **1.49 ms** |
| next keystroke (narrowing) | — | 0.001 ms | 0.001 ms |

Two changes, and the first was the one that mattered:

**Scan the arena, not the names.** The names are already one contiguous buffer
and the old scan threw that away, calling a substring search 3.3M times on
18-byte slices. The measurement that settled it: sweeping the *whole* 58 MB
arena with a single vectorised call takes **0.74 ms on one core**, while the
same bytes searched name by name took **3.4 ms across sixteen**. Nearly all the
cost was per-call setup. The scan now sweeps the byte range a chunk of records
occupies and walks a cursor along the record boundaries beside it.

The names are packed with no separator, so a candidate can straddle two records
— `…report` followed by `card…` contains "portca". The boundary test that
rejects those is not optional, and has its own test.

**Hunt the rarest byte of the needle, not the first.** A sweep is only as cheap
as its candidate rate: searching `setup.exe` by its `s` stops on every `s` on
the disk, and each stop costs a comparison and a cursor advance. Its `x` occurs
a hundredth as often and rejects just as conclusively. Worth 3.5 ms → 1.5 ms on
its own.

**Where the remaining time goes.** Effective throughput rose from ~13 GB/s to
~40 GB/s, which is DRAM bandwidth on this machine. The scan is no longer
limited by the matcher; it is limited by having to read every name.

**Target <1ms: still missed, and now for a reason that can be stated.** On this
58 MB corpus a full scan is ~1.4 ms; real filenames average nearer 30 bytes, so
a real 3.3M-record index is ~100 MB and should land around 2.5 ms. Getting under
a millisecond means touching fewer bytes, not scanning faster — a per-record
signature (say a 64-bit mask of which character pairs a name contains) would cut
the bytes read by roughly four, at a cost of ~26 MB on top of the index's 202 MB.
That is a real trade for a latency that is already a fraction of a frame and sits
behind a 30 ms debounce. Not taken; recorded so the option is a decision rather
than an oversight.

> These are synthetic figures. The real index was measured separately and is
> reported below.

### Sorting and filtering — an allocation inside two inner loops

`C:\Windows\WinSxS`, 27,436 entries, release, `--bench` 6 iterations:

| | before | after |
|---|---:|---:|
| Enumerate (warm median) | 26.6 ms | 24.7 ms |
| **Sort (warm median)** | **15.8 ms** | **3.8 ms** |

Per entry that is 0.589 µs → 0.169 µs, so a column-header click on a
100k-entry folder goes from roughly 59 ms to 17 ms.

The cause was `natural_cmp` copying each run of digits into a fresh `String`
to compare it. That put a heap allocation in the sort's innermost loop, and
`WinSxS` — where every name is a version number and a hash — hit it on nearly
every comparison. It now compares the digit runs as slices of the original
names, and walks bytes rather than `chars`, decoding UTF-8 only when a
non-ASCII byte actually turns up.

The filter field had the same shape of bug: `name.to_lowercase().contains(…)`
allocates a lowercased copy of every name in the directory, on every keystroke.
Measured with `cargo run -p neutron-core --release --example filter_bench`, over
100,000 entries:

| Needle | Matches | before | after |
|---|---:|---:|---:|
| `zzz` | 0 | 2.92 ms | **0.63 ms** |
| `9999` | 19 | 3.04 ms | **0.88 ms** |
| `component` | 100,000 | 6.45 ms | 6.26 ms |

The last row is the useful one to keep in view: when everything matches, the
cost is sorting the survivors, and a faster filter does not touch it.

Both callers now share one non-allocating matcher in `neutron_core::text`.

> Writing that benchmark found a real bug on the way: `EntryList` derived
> `Default`, which left `name_offsets` empty. That vector holds one *more*
> element than there are names — `[i]..[i + 1]` bounds name `i` — so the first
> `name()` after the first `push` on a defaulted list read past the end.
> Nothing in the application used `EntryList::default()`, so it had never
> fired. `Default` is now written out, and there is a test for it.

### Frame time, re-measured

Scrolling `C:\Windows\WinSxS` (27,436 entries) with 400 wheel notches, release:

```
frame time p50 0.11 ms   p99 0.23 ms
```

Against an 8 ms budget, so 35× under. This was the metric flagged as "reasoned
about but not measured" after the UI rebuild; the reasoning held. Virtualized
rows and a fixed-cost background mean frame time does not depend on directory
size.

### The real index, measured (elevated)

`neutron-indexer.exe --bench` from an administrator prompt, release, run twice.

**Indexing: 3,312,564 records across four volumes in 2.98 s, 203 MB resident.**

| Volume | Records | Time | Memory | Per record |
|---|---:|---:|---:|---:|
| A: | 178,050 | 495 ms | 7.1 MB | 2.78 µs |
| C: | 1,420,824 | 2,981 ms | 89.4 MB | 2.10 µs |
| F: | 1,657,296 | 2,866 ms | 104.3 MB | 1.73 µs |
| I: | 56,394 | 226 ms | 2.3 MB | 4.01 µs |

B: and G: are skipped — not NTFS with an active journal. Volumes are indexed in
parallel, so the total is the slowest volume rather than the sum.

**Target <10 s: met, at 2.98 s.** The immediately preceding run of the same
binary took 11.35 s, and an earlier one 34.7 s. The difference is the Windows
file cache, not the code — the journal has to come off the disk the first time
after a boot. Every figure here is a warm one; treat the first index after a
restart as several times longer and do not read a regression into it.

**Query latency**, against all 3.31 M records:

| Query | Matches | M4 baseline | After the arena scan | After the measured pivot |
|---|---:|---:|---:|---:|
| `e` | 2,296,497 | 5.78 ms | 4.54 ms | 4.73 ms |
| `config` | 16,393 | 9.39 ms | 5.75 ms | **3.54 ms** |
| `neutron` | 1,550 | 8.69 ms | 4.15 ms | **3.25 ms** |
| `setup.exe` | 110 | 7.45 ms | 1.86 ms | **1.61 ms** |
| `e` + one char | — | 10.54 ms | 2.60 ms | 4.08 ms |
| `config` + one char | — | 9.04 ms | 1.96 ms | **2.17 ms** |
| `neutron` + one char | — | 0.025 ms | 0.056 ms | 0.064 ms |

The middle column is why the pivot is now measured rather than guessed. A fixed
table of English letter frequencies sent `config` hunting its `f`, which on a
disk of `.dll`s and `Microsoft.*` is common — and made it the *slowest* of the
four. Counting the bytes actually present, one kilobyte per volume built during
the pass that was happening anyway, made it the second fastest.

`e` is unchanged and cannot improve: a one-character needle offers no choice of
pivot, and it matches two million files.

**Target <1 ms: still missed, at 1.6–4.7 ms.** Down from 6–9 ms, and the reason
is now understood rather than assumed: at ~120 MB of names the scan is limited
by reading them, not by the matching. See the synthetic measurements above for
what closing the last of it would cost.

## The UAC prompt (2026-08-20)

Not a speed problem, but the last item on the performance list: search cost a
UAC prompt after every reboot, because the helper had to read the volumes before
it could answer anything, and reading a volume needs administrator rights.

**Now it costs one prompt, once.** The elevated helper writes its index to
`%LOCALAPPDATA%\Neutron` when it finishes building. Every later session starts
an *unelevated* helper, which loads that cache and serves it.

| | Time | Prompt |
|---|---:|---|
| Build from the journals (elevated) | 2.98 s | yes |
| Load from cache (unelevated) | **67 ms** | **no** |

67 ms for 3,313,625 records across four volumes. The cache is 164 MB on disk —
5.4 MB for A:, 66 MB for C:, 90 MB for F:, 2.3 MB for I: — which is the name
arena and the four index arrays written out as-is.

### The index is stale, and says so

A cached index is missing everything created since it was written. A search tool
that quietly omits results is worse than one that admits it might, so the finder
says which it is and how old it is — "index is from earlier today", "6 days old"
— next to the result count, and names the command that fixes it. "Rebuild the
search index" is the one action that spends a prompt, and it is spent on
something the user asked for.

### Why not a scheduled task

The usual way to remove the prompt entirely is to register a scheduled task at
highest privileges once, and start it unelevated thereafter. It was rejected,
and the reason is worth writing down so it is not "fixed" later.

A task that runs a binary elevated, startable by any unelevated process, is
exactly as trustworthy as the binary's location. Neutron currently runs from
`%LOCALAPPDATA%`-adjacent paths that the user — and therefore anything running
as the user — can write to. Registering such a task would convert "an attacker
who can write to your profile" into "an attacker with administrator rights on
your machine", which is a privilege boundary this application has no business
dissolving to save a click.

If Neutron ever installs to a location only administrators can write to, the
scheduled task becomes reasonable and worth revisiting. Until then the cache
gets the same result for the common case without the hole.

### Two things this turned up

The helper is elevated, and an unelevated process cannot `taskkill` it. Stopping
it has to go through its own protocol, which the pipe's DACL already permits.
That works — but "Rebuild the search index" stops the running helper and starts
an elevated one, and the old process does not release the pipe name the instant
it agrees to exit. The replacement was arriving during that window and dying on
`All pipe instances are busy`. It now retries for fifteen seconds, because the
only honest error message would have been "try again in a moment".

And the cache is read from a directory the user can write to, so
`VolumeIndex::from_parts` treats every file as hostile: the arrays index into
each other, and an offset pointing past the end would panic on the first search.
Lengths, character boundaries, parent references and both sort orders are all
re-established on load, and anything that fails is discarded rather than
repaired. There are tests for truncation at seven different lengths, a wrong
magic, a wrong volume, trailing bytes and a length field of `u64::MAX`.

---

## Reproducing

```bash
cargo xtask build -- --release

# Directory scaling (headless).
cd /mnt/c && ./Users/Razeen/.neutron-target/release/neutron.exe \
    --bench 'C:\Windows\WinSxS' 6

# Frame time while scrolling — note WSLENV.
WSLENV=NEUTRON_LOG NEUTRON_LOG=debug ./neutron.exe 'C:\some\big\folder'
```

The 100k-entry directory is synthetic; recreate it with
[`scripts/make-bench-dir.ps1`](../scripts/make-bench-dir.ps1). It is not kept on
disk between runs.

> Killing the WSL-side wrapper (Ctrl-C, `timeout`) does **not** kill the Windows
> process. Stale instances hold a lock on `neutron.exe` and make the next build
> fail with "Access is denied". Clear them with
> `taskkill.exe /IM neutron.exe /F`.
