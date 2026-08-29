# Neutron Progress

## UI: Light Surfaces On A Still Ground

> **Superseded, 2026-08-29.** Sections 1–3 used to describe an Aero-style
> glassmorphism pass: a near-black violet ground, two colour fields drifting on
> 41- and 57-second loops, and translucent panels refracting them. None of that
> is in the program any more — see section 20 for what replaced it and why. The
> headings below are the current design.

### Status: Complete & Verified

#### 1. Visual Architecture
- **Two layers, and nothing moves**:
  1. **Ground**: near-white `#eef0f4` / `#e6e9ef` with a wide overhead wash, plus
     two *static* colour washes placed far off-centre so neither reads as a
     circle. Held under alpha 40 by a test: a wash heavy enough to notice is
     heavy enough to tint a card sitting on it.
  2. **Opaque cards**: the sidebar, the action rail and each pane are solid
     white with a hairline border, a 14px radius and a shadow at alpha 18.
- The cards being opaque is what makes the hairline enough. It is also why every
  contrast figure below is a single number rather than a range — a card is the
  same colour wherever it sits.
- **Light is the default.** Dark is the same design with the values inverted
  around the same structure, not a second look.

#### 2. Disciplined Purple Palette & WCAG AA Compliance
- One accent, `#7250e6`, with hover and pressed steps.
- Reserved amber and rose for warning and error states.
- All text surfaces meet WCAG AA (>= 4.5:1 primary, >= 3.0:1 muted) against
  every surface they can land on.
- The contrast tests survived the entire palette being replaced, because they
  were written against the **roles** — text on a card, muted on an inset, accent
  on a selected row — rather than against the colours. That is the whole
  argument for testing a palette this way.
- One value differs from the design draft deliberately: `text_faint` is
  `#777888` rather than the mockup's `#9a9bab`, which measured 2.9:1 on white
  and would have been the only token below AA.

#### 3. Component Refinements
- **Sidebar**: brand mark on a purple gradient chip; small-caps section labels;
  the active row takes a soft accent fill with accent glyph and label, and its
  tick sits flush against the card's inner edge rather than inset into the row.
  Segmented storage bar in a violet-to-purple ramp.
- **Action rail**: a fixed 64px strip of verbs between the sidebar and the
  panes, grouped by rhythm rather than by rules, on a faint vertical wash. See
  section 20.
- **Panes & tab strip**: opaque card, active tab underlined in the accent. Focus
  is carried by that underline; an accent ring around the largest element on
  screen read as a selection box and framed the whole window when only one pane
  was open.
- **Header**: a chevron breadcrumb, and under it the location's own name set
  large. Not redundant — one is a path you navigate, the other is a label that
  says at a glance which pane you are looking at.
- **File list**: selection is a tint plus a 2.5px left edge rather than a
  bordered pill, and filenames carry their extension in the faint tone.
- **Grid**: selection adds an accent border and a filled accent disc with a tick
  in the tile's top-right. A tile has no left edge to carry an accent the way a
  row does.
- **Window frame**: Neutron's own — title bar, drag, three buttons, and the
  eight resize regions an undecorated window has to put back.

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
  else. Its visibility is remembered across sessions. It worked only in a real
  folder until section 17.
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
  - **Now 19.01 MiB**, re-measured 2026-08-29 on the current tree, same
    `lto = "fat"` profile. An earlier note here said 18.8 MiB; that was read
    from a three-day-old binary, because waiting for a release build by testing
    for the file's existence returns at once when a stale one is already there.
    Corrected, and the trap is written down in `docs/perf.md`.
  - Pinning `russh` to `ring` (section 14) took one of two complete crypto
    libraries out. How much that saved cannot be measured here: the commit
    before it *cannot be built on this machine*, which is the finding that led
    to the change.
- **The drifting ground cost 1.69% of a core, and no longer exists.** This
  bullet has now been wrong twice, which is worth leaving on the record. It was
  written at 30fps (**8.4%**, or **17.7%** at 60fps) and stayed that way through
  four milestones after the rate dropped to 10fps (**1.69%** focused, **0.52%**
  unfocused). Section 20 removed the fields altogether: **0.26%** focused,
  **0.00%** unfocused. `docs/perf.md` had the intermediate figure right the
  whole time, which is the argument for one file owning the measurements.

#### 14. Three Things That Were Not Finished

Found by reading the tree against what the documentation claimed, rather than by
using the application — which is why they had survived. None of them showed up
as a failing test, because in each case the test that would have failed did not
exist.

**The build needed NASM, and nobody knew.** `russh` was declared at default
features, and its default crypto backend is `aws-lc-rs` — whose `aws-lc-sys` is
C and hand-written assembly that will not build for the MSVC target unless NASM
is on `PATH`. So a clean checkout on a machine with the documented prerequisites
(rustup, VS Build Tools, the Windows SDK) failed to compile.

That is exactly the dependency `russh` was chosen to avoid, three lines under
the comment saying so. `suppaftp` right above it had been pinned to
`rustls-ring` deliberately; `russh` had not, and its default pulled the C
library in through the back door. It also meant the binary linked **two**
complete crypto libraries, since `ring` was already there via rustls.

Pinned to `ring`. `russh` implements the same cipher set on either backend —
the gating in its `cipher` module is symmetric — so this is one crypto library
instead of two, not a reduction in what can be negotiated. `aws-lc-rs`,
`aws-lc-sys`, `cmake`, `fs_extra` and `dunce` left the lockfile with it, so the
build no longer needs CMake either.

**Silent refusals in two of the four namespaces.** Section 12 above ends eleven
silent failures. It ended them for *servers*: the guard tested
`matches!(location, NodeId::Remote { .. })`, and a shell folder and a Drive
folder are the other two locations with no filesystem path. So `F2` inside a
zip, `Delete` in This PC, `Ctrl+V` in Google Drive — every one of them reached
for a path, found none, and returned. The same bug the section was written to
fix, one namespace over.

The guard now tests for the absence of a path rather than for one variant, and
three more operations that were never guarded at all joined it: dragging out,
right-clicking empty space, and running a plugin. Two of those had grown their
own private wording for the same condition, which is the "one surface, not
three" problem from section 12 recurring inside the fix for section 12.

The sentence names the place, and distinguishes two answers that are not the
same. A server and a Drive folder are places Neutron *will* write to, so the
refusal ends in "yet" and promises something. Control Panel is not — there is no
folder to create in it — so it gets a flat statement. A promise about something
that is not coming is worse than no promise.

**Two features were written and never reachable.** `GoogleDrive::sign_out` and
`neutron_remote::forget_password` both existed, both worked, and both had zero
call sites. The consequence in each case was a credential the application would
store and would not let go of: no way to disconnect Drive, and no way to remove
a saved server without hand-editing `sites.json` and leaving its password in
Credential Manager for good.

Both are palette commands now. Disconnecting Drive needed more than the call —
the loader thread holds a `GoogleDrive` with a cached *access* token good for
most of an hour, so clearing the refresh token alone would have left Drive
folders listing happily after the user disconnected. It is told through the same
channel that carries `ReloadSites`.

Neither was findable by a compiler. `dead_code` does not fire on a `pub` item in
a library crate, so an unreachable feature in `neutron-cloud` looks exactly like
one the binary has not got round to using. Worth knowing about as a class: the
warning that would have caught this does not exist.

#### 15. Google Drive: Browsing Past The Root

Drive browsing had never worked below the top folder, since M7. `activate` has a
branch for shell targets, one for remote paths and one for local paths, and
there was no cloud branch at all — so a Drive listing's target, which is an
object id, fell through to the branch that treats a non-path target as a shell
parsing name. A Drive folder id was being handed to `SHParseDisplayName`.

Four things had to be true before it worked:

- **A folder had to be able to say what it was.** `NodeId::Cloud` carried
  `{provider, id}` and `display_name` matched on the provider, so every folder
  in Drive at every depth was titled "Google Drive". It now carries a name, for
  the same reason `Shell` carries a display name: the id is opaque, and
  resolving it would be an API call on the paint thread.
- **A file had to go somewhere.** Activating one downloads it, reusing the
  server download's job — same progress, same Stop, same *opened a local copy*
  receipt, because from the user's side it is the same thing happening. The
  README had claimed downloads since M7; this is the first build where that is
  true.
- **A Google Doc has no bytes.** It is a server-side object, and `alt=media` on
  one fails. Docs, Sheets and Slides export to `docx` / `xlsx` / `pptx` rather
  than to PDF, because a spreadsheet exported as PDF is a picture of a
  spreadsheet and the reason to pull a Sheet out of Drive is usually to work on
  it. A Form, a Site and a Map have no export at all and are refused by name:
  Drive answers an export request for those with an *error document* and HTTP
  403, so a client that streamed before checking the status would save that
  document under the file's own name. The status is checked before the first
  byte is written.
- **A Drive name is not a filename.** Drive names are not paths, so Drive barely
  restricts them: `Q3/Q4 forecast`, `..`, `notes.txt:hidden` and a name with a
  newline in it are all names the API will hand back, and joining one onto a
  directory writes outside it. Zip Slip with a third transport, after the
  archive extractor and the FTP listing.

  Unlike those two, refusing is the wrong answer here. A slash in a Drive name
  is ordinary and legal, and refusing would leave a real file unopenable. So the
  name is rewritten into something that is certainly one safe component, and the
  result is then handed to the same rejector the listing side uses — as a proof
  rather than a second opinion. Eleven tests cover traversal, the ADS colon,
  `..`, control characters, device names, trailing dots (Windows strips them
  silently, so `evil. ` and `evil` are one file) and a 5,000-character name.

**Up still does not work inside Drive, deliberately.** For a shell folder the
parent is merely unresolvable without COM. For Drive it is not well defined: an
object can sit in several folders at once — which is the first thing `drive.rs`
says about the format — so "the" parent has more than one answer, and picking
one would send Up somewhere the user never came from. Back walks the trail that
actually happened, and the Up button greys out, exactly as it does in This PC.

Two smaller things found while writing it. The file branch was calling the Drive
downloader for any cloud provider; `OneDrive` is unreachable there today because
it syncs to a real directory and is a `Path` node, but a wildcard would have
fetched a OneDrive file through the Drive API the day that changed, so the match
is exhaustive. And Drive's API errors nest — `{"error":{"code":403,"message":…}}`
— where OAuth's are flat, so every one of them was collapsing to a bare
"HTTP 403" and discarding the message. The useful failures all live in that
message: an export over Drive's 10MB cap, an exhausted quota, sharing revoked.


#### 16. The One Rule, Actually Held

The rule this project is built on is that the UI thread only ever touches plain
memory. It was being broken in six places, all of them added one at a time by
someone who knew the rule — which is the way this particular rule always goes,
because the cost is invisible on the machine you are writing it on.

Every one of these is instant on a local SSD. That is exactly why they survived.
The stall they cause needs a sleeping external drive, a disconnected network
share, or a `%APPDATA%` redirected to a roaming profile over a VPN — and on a
file manager whose entire pitch is that it does not stall, those are not
edge cases, they are the cases.

| Where | What it was doing |
|---|---|
| `commit_rename` | one `stat` to answer "is that name taken?" |
| `extract_selection` | `destination_for`, a `stat` per candidate |
| `compress_selection` | `next_available`, a `stat` per candidate, in a loop |
| `plugins_folder` | `create_dir_all` under `%APPDATA%` |
| `submit_connect` | writing `sites.json` and a Credential Manager call |
| `forget_server` | the same, and this one was mine, added in section 14 |

The two archive ones are the worst of them. `destination_for` walks
`photos`, `photos (2)`, `photos (3)`… up to 9,998 candidates, and stats every
one, so extracting into a folder that already holds a run of them was a
four-figure number of blocking calls on the thread that draws.

**The fix is the same shape in every case: the call moves to the worker that was
already going to do the work.** The rename's collision check now happens beside
the rename it exists to explain. The archive destination is chosen inside the
job rather than being handed to it, which also shortens both signatures — there
was never a reason for the caller to pick the name.

Two of them needed somewhere to put the answer. Creating the plugins folder ends
in navigating to it, and saving a server ends in opening it, so there is now one
channel for *a worker decided the pane should go here* — because navigating
before the folder exists lists a folder that is not there yet.

The connect dialog is the one with a wrinkle worth stating: it stays open until
the save is known to have worked. The typed password exists nowhere else, and a
dialog that closes on submit and reports the failure somewhere else has thrown
away the thing the user would need to try again.

**A bug fell out of it.** `forget_server` was writing back the copy of
`sites.json` held in the UI. `connect::save` deliberately re-reads the file
first, and the comment there says why: the loader thread records SSH host keys
into that same file, so writing a stale copy over the top silently un-learns
one — and a forgotten host key is the check that exists to catch a swapped
server, quietly turned off. Doing the removal on the worker made re-reading the
natural thing to write, and it now does.

One deliberate exception, in `NeutronApp::new`: `sites.json` is read
synchronously there. That runs once, during construction, before there is a
window to paint or a frame to drop.


#### 17. The Preview Pane Everywhere Else

The pane worked in a folder and nowhere else. Inside a zip, on a server and in
Drive it went blank — not "no preview", not a reason, just an empty rectangle
under the file's name, which is the failure mode section 12 exists to end.

The cause was one line: the pane asked the tab for `as_path()`, and three of the
four kinds of location a pane can be showing do not have one.

The header was never the problem. It is built from the entry list — name, kind,
size, modified — and has worked in every namespace all along. What was missing
was the body, so the fix is about *where the bytes come from*, which is now a
`Source` rather than a `PathBuf`.

**Servers and Drive are fetched.** Both already had exactly the right shape of
API: a download that writes into any sink and asks a callback between chunks
whether to keep going. So a preview reads into memory instead of a file, and the
callback is where both limits live.

- **The size is checked against the listing before anything is fetched.** The
  cheapest round trip is the one not made, and a 400MB video does not need a
  connection opened to know it is not going to be previewed. The cap is 2MB: a
  preview is a glance, and this one costs bandwidth.
- **And checked again during the transfer**, because the size came from the
  server's own listing and a server that understated it would otherwise pull an
  unbounded file into memory.
- **A superseded preview stops mid-transfer.** The generation check already
  abandoned queued work before it started; a network fetch can outlive the
  selection it was made for, so the same check runs between chunks. Arrowing
  past a file stops its download rather than finishing it into a buffer nobody
  will read.

**One connection, kept.** The first version opened one per preview, which for
SFTP is a full key exchange every time the cursor rests on a different row. The
worker now holds one and reuses it, with the same single-retry rule `Sessions`
uses — servers close idle connections, and retrying more than once against one
that is rate-limiting logins is how an account gets locked. It is its own
connection rather than the listing's, because FTP carries one command at a time
and previewing over the listing connection would make every folder wait behind
whatever the cursor last touched. It is dropped as soon as the cursor leaves
that server, rather than held open against a server nobody is looking at.

**A Google Doc is refused without fetching it.** Docs, Sheets and Slides have no
bytes; they export to `docx`, which this pane cannot render either — so
fetching one would spend a round trip to arrive at the same answer. The listing
cannot tell a Doc from an empty file, since both report zero bytes, so the MIME
type is read first and settles both that question and the real size.

**The shell namespace says so instead.** Inside a zip the bytes exist only
through the shell's own extraction, and a Control Panel entry is not a file at
all. There is now a sentence rather than a blank pane. That is the whole
improvement there, and it is the right one: extracting an archive member to
answer "is this the file I meant" is more than the question is worth.

Not changed: `IPreviewHandler` is still absent, for the reasons in section 7.

#### 18. Test Suite
- Pure-logic unit tests (`neutron-core`, `neutron-ui`, `neutron-fuzzy`,
  `neutron-index`, `neutron-archive`, `neutron-plugin`) passing on Linux.
- `neutron-plugin` is tested against **real child processes** — plugins that
  hang, crash, print nonsense, never read their input, print forever, or emit
  one endless line. Those are the cases a mock cannot check, because the failure
  being guarded against is the pipe and the process.
  - **These run on Linux only.** The file is `#![cfg(unix)]`, so on Windows it
    reports zero tests and the suite is silently skipped. Until section 19 it
    was running nowhere at all, which is worth stating beside a claim about
    what is tested.
- `neutron-remote` is tested against **real servers over real sockets**, both
  written in the test files: an FTP server that can be told to lie, and an SSH
  server built from `russh`'s own server side. The second exists for one test in
  particular — a server presenting a *different host key* than the one recorded
  is the case host key checking exists for, and no real server can be asked to
  do that on demand.
- `neutron-app` is tested for the two things that are easy to get wrong and
  invisible when they are: what a location is *called* when an operation cannot
  run there (`place.rs`), and turning a name the provider chose into a local
  filename (`cloud.rs`). The second is the security-relevant one — its cases are
  traversal, the NTFS stream colon, bare dots, control characters, device names,
  trailing dots and a 5,000-character name.
- Full suite on the Windows target: **658 tests**, and clippy clean under
  `--all-targets -- -D warnings`, which is what CI runs. The FTP and SFTP
  suites run there — the fake servers are ordinary std sockets — but the
  plugin suite does not; see above.

#### 19. Continuous Integration

There was none. Section 14's first item — a clean checkout that would not
compile, because `russh` pulled in a C library needing NASM — sat in the tree
undetected, and a `windows-latest` runner doing nothing more than
`cargo build` would have said so the day it landed. That is the argument for
this, and it is the whole argument.

Three jobs, each earning its place:

**Windows**, because that is what ships. `neutron.exe` is an MSVC-target binary
and half the tree is Win32, so a green Linux build proves little on its own.
Build, the full test suite, and clippy at `-D warnings`.

**Linux**, and *not* merely because it is fast. `neutron-plugin`'s integration
tests are `#![cfg(unix)]`, so on Windows they run **zero tests** — the suite
that drives real child processes which hang, crash, print nonsense, never read
their input or print forever is skipped entirely on the shipped target. It has
been running nowhere at all. This job is the only place it executes, which
makes it load-bearing rather than a convenience.

**The stated minimum Rust.** `rust-version = "1.85"` is a promise to anyone who
takes the project at its word, and an unenforced one drifts — a pattern
compiles fine on the toolchain it was written with and fails for them. Checked
against a real 1.85 toolchain before the job was written, so a red run there
means a dependency raised its own floor rather than that the job was
aspirational.

**One check is specifically a regression guard.** It asserts `aws-lc-rs` is
absent from the dependency tree, rather than asserting that the build works
without NASM. The second would be the obvious test and is the wrong one: a
runner image that happens to ship NASM would pass it while the machine of
anyone without NASM still failed. The dependency is the thing to pin, not its
symptom.

**Deliberately absent: `cargo fmt --check`.** This tree is hand-formatted and
does not match rustfmt's output — 126 diffs in `neutron-app` alone. A check that
fails on every commit gets switched off within a week, and a switched-off check
is worse than an absent one because it looks like coverage.

**The ten warnings.** `-D warnings` needs a green baseline, and
`--all-targets` had ten it had never been run against: two `clone`s that wanted
`slice::from_ref`, four compile-time assertions that wanted `const` blocks, one
`Default` field reassignment, and two files whose tests sit above their drawing
half. The first seven are now fixed. The last two carry a targeted `allow` with
a reason, because those tests are about what a preview *is* and belong beside
that code rather than after several hundred lines of egui calls they say nothing
about. "Clippy clean" was true of the default invocation and is now true of the
one CI runs.

#### 20. The Redesign

Two concept images in `assets/` — a light cloud-storage dashboard and a light
file explorer — share a language: near-white surfaces, large radii, hairline
borders instead of shadows, a page title above a chevron breadcrumb, small-caps
section labels, and selection carried by a coloured badge. This is that language
applied to what Neutron actually shows.

The visible result is in sections 1–3. What follows is the part worth keeping:
the decisions, and the four bugs that only appeared by using it.

**The drifting ground is gone, and both reasons it existed went with it.** The
colour fields were there to give translucent glass something to refract. The
cards are opaque now, so there is nothing to refract *through*; and on a light
palette a moving ground stops reading as depth and starts reading as a smudge
travelling under the text.

It was also the only continuous repaint in the program. Measured on the same
machine: **1.69%** of a core with the drift, **0.26%** without. `ambient::paint`
takes no clock parameter any more, deliberately — a signature that accepts one
invites something to start moving again.

**The rail is against the project's own pitch, and stays.** The README lists
"mouse-centric" as *Explorer's* problem, and every verb on the rail already has
a keystroke and a palette entry. But a palette only helps someone who already
knows the command exists, and a visible row of verbs is how a person finds out
what a file manager can do. Each row's hover text names its shortcut, so the
rail teaches its own way out. It dims on the *selection*, never on whether the
location can be written to: those go through the same guard as the keyboard and
answer by naming the place, and a rail that silently greys itself out in a zip
teaches nothing.

**The dashboard is a location, not a mode.** `NodeId::Home` is a variant rather
than a flag on a path, because it is genuinely a different kind of place:
nothing to enumerate, no parent, nothing in it to rename. The compiler found
every site that had to decide, which was five — most matches already had the
catch-all the three virtual variants need. It draws only what the sidebar scan
already found, so it starts no work of its own and never waits on a sleeping
disk.

**Pins are a file, not session state.** The saved workspace already remembers
which folders were *open*. A pin is one you go back to whether or not it was
open when you last closed the window, so keeping them in the session would lose
them the first time a tab was closed.

**The icon is generated, not rasterised.** There is no SVG rasteriser among the
build requirements, and rendering each `.ico` size from the geometry beats
downscaling one bitmap — 16px is where an icon is judged and it fails first at
the corners. Worth recording separately: the documented way to rebuild it was
`convert -background none …`, and on Windows a bare `convert` resolves to
`C:\Windows\System32\convert.exe`, which converts a FAT volume to NTFS. The
instructions named a filesystem tool.

##### Four bugs that only showed up by using it

- **A wash painted outside its card.** `vertical_wash` was drawing
  `full.expand(12.0)` — twelve pixels beyond the frame on every side. One cause,
  three symptoms: it squared off the rounded corners, left a card-coloured band
  down each side, and made correctly-centred glyphs look off-centre against the
  wider painted band. A rectangular mesh cannot have rounded corners, so it is
  clipped to the card and inset by the radius.
- **egui's automatic spacing, twice.** `item_spacing.x` is inserted between
  widgets whether or not you are also spacing them by hand. In the tile grid
  that made every tile cost `width + 8 + 12` and pushed the last one in each row
  off the end; in the dashboard's centring it landed entirely on the left and
  made that gap wider than the right by exactly one unit. The second instance
  should have been caught when the first was fixed.
- **Two bindings for one key.** `(D, TogglePin)` and `(D, ToggleTheme)` were
  both in the table, so `Ctrl+Shift+D` pinned *and* flipped the theme. One
  binding chosen by Shift, the way `Ctrl+F` and `Ctrl+P` already were.
- **A menu row that was never written.** Right-clicking to pin did nothing
  because there was no such item — the keystroke and the palette entry existed
  and the menu had been forgotten.

##### Cost

- Idle CPU **1.69% → 0.26%** of one core focused, **0.52% → 0.00%**
  unfocused. Both sampled over 12s with the foreground window confirmed at
  each end, because that is the reading that is easy to get wrong.
- Binary **19.02 MiB**, unchanged within measurement noise: the redesign added
  code but no dependency.
- Cold start and memory unchanged at ~416ms and ~499 MB private, both of which
  are GPU bring-up and neither of which this touched.
