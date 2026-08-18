# Neutron

A faster Windows file explorer. Rust, `egui`/`wgpu`, single binary.

**Status: M6 complete** — tabs and arbitrarily nested split panes over a
virtualized listing, with sortable columns, keyboard navigation, a live sidebar
of drives, known folders, OneDrive and WSL distributions, and a layout that
survives restart. Real shell icons, native right-click menus, Recycle Bin
deletes, and drag-and-drop from other applications. Everything-style search over
3.3M files across every NTFS volume, a scoped fuzzy finder, and a command
palette. This PC, Network, Control Panel, the Recycle Bin and the inside of a
zip all browse like folders. A 100k-entry folder opens in 88ms and scrolls at 0.14ms per frame.

---

## What it is meant to beat Explorer at

| Explorer's problem | Neutron's answer | Milestone |
|---|---|---|
| Search crawls the tree; minutes on a big volume | Read the NTFS change journal directly — index every file on all volumes in seconds | ✅ M4 |
| Large folders stall the UI while icons load | All shell/COM work on dedicated STA threads; the UI thread never blocks | ✅ M3 |
| Tabs, but no split view | Recursive pane tree — arbitrary splits, tabs per pane | ✅ M2 |
| Mouse-centric | Command palette and fzf/telescope overlay, full keyboard nav | ✅ M5 |
| Cloud drives are second-class | OneDrive pinned as a first-class root; Google Drive over the API | ✅ M7 |
| No way to reach a WSL filesystem short of typing a UNC path | Every installed distribution pinned in the sidebar | ✅ M2 |

---

## Building

The source lives on the WSL filesystem; the binary is built by the **Windows**
toolchain. `cargo xtask` bridges the two — it maps the WSL share to a drive
letter and points cargo at it, because much of the MSVC toolchain refuses UNC
working directories outright.

```bash
cargo xtask build              # debug
cargo xtask build -- --release
cargo xtask run
cargo xtask test
cargo xtask where              # show detected paths, drive mapping, toolchain
```

Requires, on the Windows side: `rustup` with `x86_64-pc-windows-msvc`, and
Visual Studio Build Tools with the Windows SDK.

Overrides, if detection guesses wrong:

| Variable | Purpose |
|---|---|
| `NEUTRON_WIN_CARGO` | Path to the Windows `cargo.exe` |
| `NEUTRON_DRIVE` | Drive letter already mapped to the WSL share |
| `NEUTRON_WIN_TARGET_DIR` | Build output directory (must be local NTFS) |

### Fast inner loop

The pure-logic crates build and test natively on Linux, which is much quicker
than a cross-build:

```bash
cargo test -p neutron-core -p neutron-ui -p neutron-fuzzy
```

`neutron-shell` and `neutron-index` are Windows-only and need `cargo xtask test`.

---

## Layout

```
crates/
  neutron-app/      window, event loop, tabs, panes         (bin: neutron)
  neutron-ui/       theme, ambient layer, virtualized list, formatting
  neutron-core/     domain types, sorting, history, traits  ← no Win32, tests on Linux
  neutron-fuzzy/    fzf-style matching with highlight spans ← no Win32, tests on Linux
  neutron-shell/    Win32 + COM: enumeration, icons, context menus
  neutron-index/    NTFS journal indexing, query engine, wire protocol
  neutron-cloud/    OneDrive and Google Drive providers
  neutron-indexer/  elevated helper process                (bin: neutron-indexer)
xtask/              build driver (Linux-side only)
assets/             app icon (SVG source, .ico, raw RGBA)
scripts/            benchmark directory generator
docs/perf.md        measured performance baseline
```

### The one rule

**The UI thread only ever touches plain memory.** Every filesystem call, every
COM call, every network request happens on a worker and returns through a
channel; the paint thread renders whatever snapshot is current and never waits.

Most of what makes Explorer feel slow is a blocking call on the thread that
draws — a stat on a sleeping drive, an icon handler, a cloud placeholder fetch.
Refusing to have any such call is most of the win, and it is a property that has
to be maintained deliberately: one innocent `std::fs::metadata` inside a paint
function reintroduces the stall.

Two structural choices enforce this. `neutron-core` and `neutron-fuzzy` do not
depend on `windows` at all, so they *cannot* accidentally perform a shell call.
Everything that can block lives behind `Namespace` or `CloudProvider`, whose
implementations are documented as worker-thread-only.

---

## Shell integration

Everything in this section runs on a pool of four **STA apartment threads**
(`neutron-shell/src/sta.rs`), never on the thread that paints. Shell extensions
are arbitrary third-party code loaded into the process — a context-menu handler
doing network I/O, an icon handler for a file on a sleeping drive — and the one
rule this project is built on is that none of it can stall a frame.

| Feature | How |
|---|---|
| File icons | `SHGetFileInfoW` into a single 1024×1024 texture atlas |
| Opening files | `ShellExecuteExW` with the item's default verb |
| Right-click menu | `IContextMenu` + `TrackPopupMenuEx` on a worker apartment |
| Delete | `IFileOperation` — Recycle Bin, undo record, progress UI |
| Drag and drop in | `IFileOperation` copy/move, with Explorer's same-volume rule |

Two details are worth knowing about:

**Icons never open the file.** Lookups pass `SHGFI_USEFILEATTRIBUTES`, so the
shell answers from the name and attribute bits alone. Without it, listing a
OneDrive folder would download every placeholder in it to draw a 32-pixel
square. The consequence is that an icon is a function of the *extension*, which
is also what makes the cache work — 5,000 files in `System32` resolve to seven
distinct icons. Executables, shortcuts and `.ico` files carry their icon inside
themselves and are the only ones looked up per path.

**The context menu runs on a worker, not the UI thread.** `TrackPopupMenuEx`
runs a modal message loop until the menu closes, and `QueryContextMenu` can
block for seconds inside a third-party handler. Doing it on the UI thread is why
Explorer freezes on right-click. Neutron creates a hidden owner window *on the
apartment thread* and tracks the menu there — which also gives
`IContextMenu2/3::HandleMenuMsg` somewhere to be forwarded from, so owner-drawn
third-party items keep their icons, without subclassing winit's window.

**Not yet:** dragging *out* of Neutron into another application. That needs
`DoDragDrop` with our own `IDataObject`, and `DoDragDrop` is modal on the thread
holding the mouse capture — the UI thread. Doing it properly means an
asynchronous data object rather than blocking a frame, which is its own design
rather than a loose end to tie off here.

---

## Search

`Ctrl+Shift+F` searches every fixed NTFS volume. 3.3M files index in ~2.6s and
answer in microseconds.

**Why it is fast.** `FSCTL_ENUM_USN_DATA` hands back every file reference number
on a volume with its parent and name, in one sequential pass over the MFT — no
directory traversal at all. Paths are never stored: each record keeps its
parent's index, and a path is rebuilt by walking that chain, only for the rows
about to be displayed. Storing materialised paths for 3.3M files would cost
hundreds of megabytes to save a microsecond on the fifty rows anyone can see.

**Three ways in.** `Ctrl+P` fuzzy-matches filenames beneath the current folder,
`Ctrl+Shift+P` fuzzy-matches the command palette, and `Ctrl+Shift+F` matches by
substring across every volume. One overlay serves all three; what differs is the
corpus and the matching, and that is a property of the corpus rather than a
preference. Fuzzy is right when a near-miss is plausibly what you meant; over
3.3M names it costs far more per candidate *and* buries the exact hit under
everything that shares a few letters.

Fuzzy results highlight the characters that matched. Without that a ranked fuzzy
list is unreadable — the third result looks arbitrary until you can see why it
is there.

**Why typing is free.** Extending a query can only shrink its match set, so
`repo` filters the results of `rep` rather than rescanning. The first character
costs a full parallel scan (~6-9ms); every character after it is ~0.003ms. A
query whose results exceed the 4,096-hit cap is deliberately rescanned instead —
narrowing from an incomplete set would hide matching files, which for a search
tool is the worst possible failure, because it looks like the file is not there.

## The shell namespace

Explorer shows one tree; underneath there are two mechanisms. `C:\Users` is a
filesystem directory. **This PC is not** — it is a COM object that answers
`EnumObjects`, with no path to walk, and the same is true of Network, Control
Panel, the Recycle Bin, and the inside of a zip. A file manager that only
understands paths cannot show any of them.

So `NodeId` has a `Shell` variant and the loader dispatches on it: ordinary
directories never touch COM, which is the whole performance argument, and the
shell backend is asked only for the nodes it claims.

**Identity is a parsing name, not a PIDL.** A PIDL is the shell's native handle
and would save a parse per navigation, but it is opaque bytes — a saved session
could not be inspected or migrated, it is not guaranteed stable across reboots
for every extension, and it carries no name, so titling a tab would need a COM
call on the paint thread. A parsing name — `::{20D04FE0-…}` for This PC,
`C:\archive.zip\inner` for a folder in a zip — round-trips through
`SHParseDisplayName`, persists, and travels with its display name.

**Zip files.** Nothing in a `.zip`'s own attributes says it is browsable; only
the shell knows a namespace handler is registered for that extension. So opening
a file asks the shell, on a worker, rather than matching against a list of
extensions that would go stale.

---

## Google Drive

Drive has no local presence unless Drive for Desktop is installed, so Neutron
talks to the API directly. `Connect Google Drive` in the sidebar opens a browser
once; after that the session is restored silently.

**Set up client credentials.** Create an OAuth client of type *Desktop app* in a
Google Cloud project with the Drive API enabled, then supply both values it
gives you:

```bash
export WSLENV=NEUTRON_GOOGLE_CLIENT_ID:NEUTRON_GOOGLE_CLIENT_SECRET
export NEUTRON_GOOGLE_CLIENT_ID=<id>.apps.googleusercontent.com
export NEUTRON_GOOGLE_CLIENT_SECRET=<secret>
```

Both are needed. Google's token endpoint rejects an installed-app exchange
without the secret *even under PKCE* — `invalid_request: client_secret is
missing`, and only after the user has already consented.

Neither is compiled in. Neither is a secret in the cryptographic sense — Google
documents that an installed app's secret "is obviously not treated as a secret",
since it ships in every copy of the binary — but hard-coding them would tie every
build to one project's quota and audit trail.

Without them the sidebar row stays greyed with that instruction; the rest of
Neutron is unaffected.

**Security.** The flow is PKCE with a loopback redirect. A desktop app cannot
keep a secret — anything compiled in is readable by whoever has the binary, which
is exactly why Google's own client secret for installed apps protects nothing —
so PKCE generates a fresh verifier per attempt and an intercepted authorisation
code is useless without it. The redirect binds `127.0.0.1:0` rather than a
custom URI scheme, which any other program on the machine could register and
intercept. The `state` parameter is checked before the code is even read.

The refresh token lives in Windows Credential Manager, not on disk. That is not
a strong boundary — anything running as the user can read it back, which is what
Neutron does — but it is encrypted under the login, scoped to the account, and
markedly better than a config file that ends up in backups. Only
`drive.readonly` is requested: Neutron browses and downloads, and asking for
write access it never uses is a worse consent screen and a larger blast radius.

**Drive is not a filesystem**, and three differences shape the code: objects are
addressed by id rather than path (a file can live in several folders), names are
not unique within a folder, and native Google formats have no bytes to download
without an export format.

> If you install Google Drive for Desktop, Drive becomes a drive letter and a
> shell namespace extension — which M6 already handles, for free. This path
> exists for machines without it.

---

## Privileges

`neutron.exe` runs **unelevated**, deliberately. Indexing needs administrator
rights — a volume handle opened for `GENERIC_READ`, which is what the journal
control codes operate on — but an elevated window cannot accept drag-and-drop
from an unelevated Explorer, because UIPI silently drops the messages. So journal
access lives in the separate `neutron-indexer.exe` helper, launched on demand
with a single UAC prompt and spoken to over a named pipe.

The pipe is a channel from a lower-privilege process to a higher-privilege one,
so it carries an explicit DACL admitting only the user's own SID, and
`FILE_FLAG_FIRST_PIPE_INSTANCE` makes a squatted pipe name a hard failure rather
than something silently shared with whatever claimed it first.

> The obvious DACL — `D:P(A;;GA;;;CO)`, creator/owner — is wrong here in a way
> that looks right. `CO` resolves to the *owner* of the creating token, and an
> elevated process's default owner is Administrators, so the pipe admitted
> administrators only and denied the unelevated UI it exists to serve. The
> token's user SID is unaffected by elevation and is what the DACL names.

The helper is left running when the UI closes, so a later session reconnects
without another prompt. **Stop the search indexer** in the command palette
(`Ctrl+Shift+P`) shuts it down when you want the memory back.

A helper left running from an older build answers the requests it knows and
refuses the rest. That is treated as a refused *request*, not a dead connection —
the modes it does understand keep working, and the finder says why the others
do not.

---

## Keys

| Key | Action |
|---|---|
| `↑` `↓` | Move cursor (hold `Shift` to extend the selection) |
| `PgUp` `PgDn` `Home` `End` | Move by page / to either end |
| `Enter` | Open the focused entry |
| `Backspace`, `Alt+↑` | Go up one level |
| `Alt+←` `Alt+→` | Back / forward |
| *type a name* | Jump to it; repeat a letter to cycle matches |
| `Ctrl+A` | Select all |
| `Ctrl+H` | Show hidden files |
| `Ctrl+F` | Filter the current listing (`Esc` clears it) |
| `Ctrl+Shift+F` | Search every volume (arrows move, `Enter` opens, `Esc` closes) |
| `Ctrl+P` | Fuzzy-find a file in this folder and below |
| `Ctrl+Shift+P` | Command palette |
| *double-click a `.zip`* | Browse inside it, as Explorer does |
| `Delete` | Send the selection to the Recycle Bin |
| `Shift+Delete` | Delete permanently (the shell still confirms) |
| *right-click* | Native shell context menu |
| `F5`, `Ctrl+R` | Refresh |
| `Esc` | Clear selection |
| `Ctrl+D` | Toggle dark/light theme |

The filter narrows the listing already in memory — a case-insensitive substring
over names, applied without touching the disk, so it stays instant while typing
even in a 100k-entry directory. It clears on navigation, because a filter
describing the folder you left would silently empty the one you entered. Fuzzy
matching across the whole index is a different thing and arrives with the finder
overlay at M5.

### Tabs and panes

| Key | Action |
|---|---|
| `Ctrl+T` | New tab in the focused pane, at its current location |
| `Ctrl+W` | Close tab (closes the window when it is the last one) |
| `Ctrl+1`…`9` | Jump to a tab in the focused pane |
| `Ctrl+\` | Split right |
| `Ctrl+Shift+\` | Split down |
| `F6` | Focus the next pane |

Each pane has its own header — back/forward/up, breadcrumb, location title and
filter — because with split panes there is no single current location for a
window-wide toolbar to describe.

Panes nest arbitrarily — split a pane, then split one of its halves. Closing the
last tab in a pane collapses it and its sibling takes the space. Drag a tab onto
another pane's tab strip to move it there. Dividers are draggable, and the whole
arrangement is restored on next launch.

Everything else arrives with its milestone.

## Command line

```
neutron [path]                     open a folder (defaults to your home directory)
neutron --bench <path> [runs]      headless enumerate + sort timing
```

| Variable | Effect |
|---|---|
| `NEUTRON_LOG` | Log filter, e.g. `debug` or `neutron=info` |
| `NEUTRON_GPU_BACKEND` | `dx12` (default), `vulkan`, `gl`, `all` |
| `NEUTRON_GPU_POWER` | `low` (default) or `high` |

> **Running from WSL:** environment variables do not reach a Windows process
> unless they are also listed in `WSLENV` —
> `WSLENV=NEUTRON_LOG NEUTRON_LOG=debug ./neutron.exe`. Forgetting this fails
> silently and produces measurements of the default configuration; see
> [`docs/perf.md`](docs/perf.md).

---

## Design

**Cards on a lit ground.** The sidebar and each pane are rounded cards with a
soft shadow and real margin from the window edge. Between and around them the
**ground** shows through, carrying a pale lavender wash and three static
coloured lights whose overlap gives the window a prismatic cast. Cards are ~88%
opaque, so that cast tints them — and tints them differently in different parts
of the window, which is what stops them reading as flat white cutouts.

Depth comes from elevation, not from ruled borders. Light is the default; dark
is `Ctrl+D`.

**Colour is scarce, except on the ground.** Every surface that carries content
is near-neutral. The accent appears in roughly three places — the selected row,
the focus ring, the active tab's underline — and file icons are grey, not
purple. Saturated colour spread across every row destroys its usefulness as a
signal: if everything is purple, purple stops meaning "this is selected". The
ground is exempt precisely because no text ever lands on it, so colour there
costs nothing in legibility.

**The lights do not move.** An earlier version drifted them on a timer, which
forced a repaint every 33 ms purely for decoration and held the process at ~9%
of a core while nobody was touching it. Static, the app measures 0.3% idle.

**Navigation lives in the pane, not in a toolbar.** Back/forward/up, the
breadcrumb, the location title and the filter field are all inside each pane's
card. With split panes there is no single "current location", so a window-wide
breadcrumb would have to pick one pane and ignore the other — every control it
held was really a control on the focused pane wearing a costume.

All colour lives in `neutron-ui/src/theme.rs`, with tests asserting WCAG contrast
for text, muted text, accents, and selected rows against every surface they can
land on — cards composited over the ground, since a translucent card is partly
made of what it sits on.

> Neutron originally asked Windows for a transparent window and used the DWM
> acrylic backdrop. Measurement showed wgpu cannot supply a surface with a
> transparent `CompositeAlphaMode` on any backend available here, so the ground
> and its lights are painted by Neutron itself and the effect is self-contained.

## Performance

See [`docs/perf.md`](docs/perf.md) for measured numbers.

| Metric | Target | Actual |
|---|---:|---:|
| Enumerate + sort 100k entries | <200 ms | **88 ms** |
| Frame time p99 while scrolling | <8 ms | **0.40 ms** |
| Index 3.3M files, all volumes | <10 s | **2.6 s** (warm cache) |
| Search, incremental keystroke | <1 ms | **0.003 ms** |
| Search, first character | <1 ms | 6–9 ms ❌ |
| Cold start | <100 ms | ~1040 ms ❌ |
| Idle memory | <60 MB | ~280 MB ❌ |

Cold start and idle memory share one cause — GPU device creation, which costs
the best part of a second and a few hundred megabytes before any of our code
runs. The search miss is different: 6–9ms is half a frame, so it is below the
threshold where a keystroke feels delayed, but the target says <1ms and is not
met. Closing it means an n-gram or suffix structure over the name arena, which
is real memory for a latency nobody can perceive. That file records the
measurements, the causes, and the options.
