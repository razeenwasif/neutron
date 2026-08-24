# Neutron Progress

## UI Redesign: Depth Through Translucency (Aero-Style Glassmorphism)

### Status: Complete & Verified

#### 1. Visual Architecture
- **3-Layer Depth Model**:
  1. **Ground**: Near-black deep violet `#0c0714` / `#05020a` with radial overhead light source.
  2. **Ambient Drifting Colour Fields**: Two large slow-drifting colour fields on sinusoidal ease-in-out loops, each near window-sized so what shows is the gradient where they overlap rather than a recognisable circle:
     - **Field A (41s loop)**: Vibrant purple (`#9333ea`), top-left, drifting `+7vw, +6vh`, scaling `1.0 → 1.10`.
     - **Field B (57s loop)**: Electric indigo / violet (`#6366f1`), bottom-right, drifting `-8vw, -5vh`, scaling `1.08 → 0.98`.
     - Periods share no common factor, so the pair never settles into a visible repeat. Each field is a three-layer gaussian falloff fan — two layers still left a discernible rim at this size.
  3. **Translucent Glass Panels**: Floating glass cards at `rgba(36, 27, 56, 0.44)` in dark mode and `rgba(252, 250, 255, 0.62)` in light, with soft glass shadow, subtle glass stroke, a top-edge highlight that fades down the upper sides, and a darker bottom edge so the panel has thickness.
     - The dark fill is *lighter* than the ground, not darker. At this transparency a near-ground fill composites to the same colour and the panel edge disappears in the unlit corners.

#### 2. Disciplined Purple Palette & WCAG AA Compliance
- Primary purple brand ramp (`#c084fc` / `#a855f7` / `#9333ea`).
- Strictly reserved amber (`#fbbf24`) and rose (`#fb7185`) accents for warning and error states.
- Dark theme set as default (`ThemeMode::Dark`).
- All text surfaces tested and verified to meet WCAG AA contrast (>= 4.5:1 for primary text, >= 3.0:1 for muted text).
- Contrast is measured at **both** extremes of the lit ground — a card over the deepest unlit corner *and* over the brightest colour field at its peak. Checking only the deep ground is the best case for light text on a dark card, and would pass however washed-out the lit areas became.

#### 3. Component Refinements
- **Sidebar**:
  - Wordmark brand icon with purple gradient glow and glass highlight.
  - Active navigation item with glowing vertical accent indicator on the left rim (matching Aero's `.nav-item.active::before`).
  - Segmented storage capacity bar in violet-to-purple ramp.
- **Panes & Tab Strip**:
  - Translucent glass card background refracts moving orbs underneath.
  - Inset top-edge glass refraction highlight.
  - Active tab indicator underline in purple accent.
  - Focus is carried by the active tab's accent underline. The pane itself keeps one hairline in both states: an accent ring plus a glow around the largest element on screen read as a selection box, and framed the whole window when only one pane was open.
- **Header & Filter Pill**:
  - Filter search pill with purple focus ring and glowing search glyph when active.
  - Breadcrumbs with responsive ellipsis truncation.
- **Finder & Command Palette Overlay**:
  - Elevated floating card with glass highlight and deep shadow.
  - Purple mode chip and fuzzy match character highlighting.
- **File List**:
  - Selection row pill with purple highlight and border.
  - Focus cursor indicator.

#### 4. Grid View
- `Ctrl+Shift+L`, a header toggle, and a command palette entry switch between list and grid.
- Tiles are virtualized a row at a time, so a 500k-entry folder costs the same per frame as a 50-entry one.
- Arrow keys move by a whole row in the grid; the layout reports its column count back to the tab, because the pane is drawn from a clone of the view state and the count would otherwise be discarded every frame.

#### 5. Selection & Context Menu
- **Rubber-band select**: press and drag anywhere in the list or grid to sweep a
  selection; Ctrl adds to what is already selected. The band is tracked in
  *content* coordinates so it stays glued to the rows while the list
  auto-scrolls under it. Its origin lives in egui memory rather than in
  `FileListState`, because the pane is drawn from a clone of that state and
  anything written during paint is discarded when the frame ends.
- **Themed context menu**: the shell still builds the menu — every installed
  extension (WinRAR, 7-Zip, Git, PowerRename, IntelliJ) is present — but Neutron
  reads the `HMENU` into plain data and draws the rows itself, so the menu is
  the same glass as the rest of the window instead of a system-grey rectangle.
  Submenus populate lazily through a forwarded `WM_INITMENUPOPUP`, the default
  verb is accent-tinted, and keyboard navigation works.
  - Not carried over: per-item icons supplied as `HBITMAP`. A fully owner-drawn
    item with no menu string falls back to its verb.

#### 6. Everyday File Operations
- **Clipboard**: `Ctrl+C` / `Ctrl+X` / `Ctrl+V` through the real Windows
  clipboard, written as raw `CF_HDROP` plus `Preferred DropEffect` — so it
  interoperates with Explorer both ways and survives Neutron exiting. Cut files
  are drawn faded, matched per visible row so a pending cut costs nothing on a
  large listing.
  - `Ctrl+V` arrives through a window subclass. egui-winit turns the chord into
    `Event::Paste(text)` and drops it when the clipboard holds no text — which
    is exactly what a copied *file* looks like.
- **Rename**: `F2` opens a box over the name column with the stem selected.
  Invalid names show a red border and Enter leaves the box open.
- **New folder**: `Ctrl+Shift+N`, created and immediately renameable.
- **Background menu**: right-clicking empty space opens the folder's own shell
  menu (New, Open in Terminal, Properties) via `CreateViewObject`. Paste is
  absent — Explorer's copy comes from its own view object, not the folder.
- **Drag out**: pressing an already-selected row and moving hands the selection
  to the system as an OLE drag, using `SHCreateDataObject` so every format a
  target might ask for is answered. Runs on the STA pool with the input queues
  joined, so the window keeps painting for the whole drag.

#### 7. Preview Pane
- `Alt+P` (or the command palette) opens a right-hand pane showing what the
  cursor row actually contains: images, text, and a card of facts for anything
  else. Its visibility is remembered across sessions.
- Reading and decoding happen on a worker; the pane only ever receives finished
  bytes. Requests are debounced at 120ms and carry a generation, so holding Down
  through a folder of photographs reads almost nothing and never shows a late
  answer for a file the cursor has left.
- Images are downscaled to 1024px on the worker, so a 4000×3000 photograph
  never becomes a 48MB texture. The pane reports the file's real dimensions.
- Text is capped at 256KB, monospaced and unwrapped — a config file is read for
  its structure, and soft-wrapping destroys exactly that.
- Not done: `IPreviewHandler`, which is how Explorer shows PDFs and Office
  documents. Those render into an `HWND` the host supplies, which would mean
  parenting a child window over the wgpu surface and keeping it aligned through
  every scroll and split.

#### 8. Live Listings
- Every folder a tab is showing is watched with `ReadDirectoryChangesW`, so a
  download finishing or a build writing its output appears without an F5. This
  was planned at M1 and only landed now.
- The watcher reports *that* the folder changed and nothing else. Applying
  individual events would mean reproducing the sort position, the filter, the
  hidden-file rule and the rename pairing for each one, and being wrong leaves a
  listing that disagrees with the disk invisibly. Re-reading is 25ms for 27,000
  entries and is right by construction.
- The first change is reported at once, then the watcher waits 250ms before
  looking again. Measured: creating 500 files in 0.76s cost a handful of
  re-reads and held CPU at 1.7% against a 1.2% idle baseline.
- Refreshing is now distinct from navigating. A refresh keeps the filter and
  re-selects by *name*, because re-reading rebuilds every index into the entry
  list. Files that were deleted stay gone, which is what should happen to the
  one just removed.

#### 9. Archives
- **Extract** zip, tar and tar.gz; **compress** to zip. Both run on a worker
  with progress and a Stop in the status bar — extracting is not modal, and
  there is no reason browsing should stop while it runs.
- `7z` and `rar` are deliberately absent. Both need a real implementation or a
  bundled binary, and the installed tools already appear in the context menu,
  which beats a second-rate decoder.
- **Entry names are treated as hostile.** An entry called `..\..\Windows\
  System32\drivers\etc\hosts` is a valid zip entry, and an extractor that
  joins names onto a destination writes exactly there. Names are taken apart and
  rebuilt from components known to be ordinary; anything else is refused and
  reported. Tested end to end with real traversal, absolute, UNC and
  drive-relative archives.
- Modification times survive the round trip in both directions, which needed a
  small UTC calendar conversion — a zip stores MS-DOS date fields, and the
  format's zero value is 1980.
- Cancelling keeps what was already written. Deleting a half-extracted folder is
  a destructive act nobody asked for.

#### 10. Plugins
- A plugin is a folder under `%APPDATA%\Neutron\plugins` holding a
  `plugin.json` and a program. Its commands appear in the command palette and,
  if the manifest asks, in the right-click menu. Choosing one runs the program
  with the folder and selection as one line of JSON on standard input, and acts
  on the lines it prints back — `status`, `refresh`, `navigate`, `open`,
  `error`. Authoring guide in `docs/plugins.md`; a working example in
  `examples/plugins/hashes`.
- **Separate processes, not a dynamic library.** Release builds are
  `panic = "abort"`, so an in-process plugin that panics kills the file manager;
  a child process that dies is an exit status. It also cannot hang the window —
  the host waits on a channel with a deadline, and there is a Stop. And since
  Rust has no stable ABI, the alternative would have been a C boundary that
  restricts plugins to whoever is willing to link one; newline-delimited JSON
  means a plugin can be a shell script.
- **Not a sandbox, and the docs say so.** A plugin runs with the user's rights.
  What the design *does* guarantee: listing a plugin never runs it (discovery
  reads manifests and stops); nothing appears in the palette that is not written
  in a file the user can read; and a plugin's output is treated as untrusted —
  flattened to one line, stripped of control and bidi characters, length-capped,
  and paths checked before anything is done with them.
- Three threads per invocation, one per pipe. Each can block: `stdin` if the
  plugin never reads a request larger than the pipe buffer, `stdout` while it
  thinks, and `stderr` — undrained — blocks *the plugin*, which looks from the
  host side exactly like a hang.
- Every rejected plugin folder keeps its reason and shows it. A plugin system
  that silently skips what it cannot parse is unusable to write for: the symptom
  is an entry that never appears, with nothing anywhere to explain it.
- Menu ids for plugin rows start at `0x1000_0000`, clear of the shell's
  `1..=0x7FFF`, checked at compile time. A chosen plugin id is never forwarded
  to `IContextMenu::InvokeCommand`, which would otherwise invoke an unrelated
  verb.
- Fallout worth noting: the selection is a `HashSet`, so `selection_paths` was
  returning files in a different order every run. It now sorts into the order
  the rows appear in, which also fixes the order files go into a zip and onto
  the clipboard.

#### 11. Saved Servers — FTP, FTPS and SFTP
- A server saved in `%APPDATA%\Neutron\sites.json` appears in the sidebar under
  **Servers**, opens like a folder, and its files sort, filter and preview the
  way local ones do. Added through **Connect to a server** in the palette, since
  the password has to be typed somewhere and it does not belong in a file.
  Guide in `docs/servers.md`.
- **Both protocol clients are pure Rust** — `suppaftp` and `russh`. The
  alternative for SSH is libssh2 through a C build, and this project
  cross-compiles from WSL to the MSVC toolchain, where every C dependency is a
  new way for the build to break. `russh` is async, so `sftp.rs` owns a
  current-thread Tokio runtime and blocks on it; the runtime never leaves that
  struct and no other crate learns about async.
- **MLSD before LIST.** `LIST` has no specification — what comes back is
  whatever the server thought a human would like to read, and a Unix listing
  infers a file's *year* from today's date. `MLSD` (RFC 3659) returns typed
  facts. `FEAT` is asked once per connection and `LIST` parsing is the fallback.
- **Binary mode, set before anything transfers.** FTP's ASCII mode rewrites line
  endings inside the transfer, so a zip arrives corrupt and the transfer reports
  success. There is a test asserting `TYPE I` precedes the first `RETR`.
- **SSH host keys are checked**, trust-on-first-use and then enforced. A changed
  key is a hard refusal showing both fingerprints, in OpenSSH's own
  `SHA256:` format so `ssh-keyscan | ssh-keygen -lf -` is directly comparable.
  Plenty of libraries' example code answers `check_server_key` with `Ok(true)`;
  that is the bug, written down.
- **A listed name is hostile input.** A server can answer with an entry called
  `../../../../Windows/System32/drivers/etc/hosts`, and a client that joins it
  onto a download folder writes exactly there — Zip Slip with a different
  transport. Any listed name with a separator, `..`, a colon, a control
  character or a Windows device name is refused; the folder still opens.
- Passwords live in Credential Manager under `Neutron/Site/<id>`, never in
  `sites.json`. A site with no saved password is not dialled with an empty one —
  some servers count that as a failed attempt.
- Connections are made once per site and reused, with exactly one reconnect on
  failure: servers close idle connections, and retrying forever against one that
  is rate-limiting logins is how an account gets locked. Downloads open their own
  connection, because FTP cannot interleave and a listing would otherwise wait
  behind a transfer.
- Opening a remote file fetches it to `%TEMP%\Neutron\<site>\` and opens the
  copy. The status bar says **opened a local copy**, because editing it does not
  write back.
- **Nothing writes to a server yet.** Rename, delete, paste, new folder and drop
  all refuse with a sentence rather than doing nothing, which is what they did
  before the guard — a silent no-op reads as broken key handling.

#### 12. Everyday Trust
Four things stood between Neutron and being the file manager you actually reach
for. None of them are features; they are what makes a tool one you stop checking
up on.

- **Failures now say so.** Eleven file operations — rename, new folder, paste,
  delete, drop, drag, clipboard, open, context menu — ended in a
  `tracing::warn!` and *nothing on screen*. Renaming to a name that already
  existed looked exactly like a file manager that had ignored the keystroke.
  There is now one status line (`notice.rs`) that everything reports through.
- **An error stays; a receipt clears itself.** Information — "3 files hashed" —
  clears after six seconds. An error stays until it is replaced or clicked away,
  because an error nobody saw is an error that, to the user, did not happen. The
  log still gets everything, in full; the status bar has one line.
- **One surface, not three.** Archives, plugins and servers had each grown a
  private note field, and all three drew into the same corner with no answer for
  which won. The newest thing that happened is what a person wants to see.
- **Preferences survive a restart.** Theme, hidden files, view mode and sort
  were reset on every launch — the session remembered which folders were open
  and nothing about how you wanted to look at them. Now saved with the session
  and applied to every restored tab; a new tab inherits them from the tab it was
  opened beside, so `Ctrl+T` in a grid no longer opens a list.
- **Show hidden files became application-wide.** It was per tab, which with no
  per-tab indicator reads as tabs disagreeing about what is in a folder.
- **`Ctrl+\`` opens a terminal here, `Ctrl+N` opens a second window.** Both were
  reachable — the terminal through the shell menu, a second window by launching
  the exe again — and neither was reachable *quickly*, which for something done
  thirty times a day is the same as not having it.

#### 13. Known Cost
- **The binary grew 15.5 MiB → 19.8 MiB for the servers.** Measured, not
  estimated: release builds of the commit before and the commit after, same
  toolchain and same `lto = "fat"` profile. `russh` + `rustls` + `tokio` +
  `suppaftp` are that 4.3 MiB. For contrast, archives and plugins together cost
  0.22 MiB — plugins added no new dependency at all, being a process and a pipe.
  Worth stating plainly against a project whose pitch includes a single ~15 MB
  binary. Nothing is loaded until a server is opened, so start-up and idle
  memory are unaffected.
- The drifting fields require a continuous repaint. Measured idle CPU: **17.7%** of one core at 60fps, **8.4%** at 30fps (current setting), against **0.3%** for a static ground. The loops are 41s and 57s long, so the lower rate is visually identical.

#### 14. Test Suite
- Pure-logic unit tests (`neutron-core`, `neutron-ui`, `neutron-fuzzy`,
  `neutron-index`, `neutron-archive`, `neutron-plugin`) passing on Linux.
- `neutron-plugin` is tested against **real child processes** — plugins that
  hang, crash, print nonsense, never read their input, print forever, or emit
  one endless line. Those are the cases a mock cannot check, because the failure
  being guarded against is the pipe and the process.
- `neutron-remote` is tested against **real servers over real sockets**, both
  written in the test files: an FTP server that can be told to lie, and an SSH
  server built from `russh`'s own server side. The second exists for one test in
  particular — a server presenting a *different host key* than the one recorded
  is the case host key checking exists for, and no real server can be asked to
  do that on demand.
- Full suite on the Windows target: 592 tests, clippy clean. The FTP and SFTP
  suites run there too — the fake servers are ordinary std sockets.
