# Neutron

A fast file explorer for Windows and macOS. Rust, `egui`/`wgpu`, single binary.

**Status: M7 complete, plus everything since.** Tabs and arbitrarily nested
split panes over a virtualized listing, with sortable columns, keyboard
navigation, a live sidebar of drives, known folders, OneDrive and WSL
distributions, and a layout that survives restart. Real shell icons, a
themed right-click menu built from the real shell one, Recycle Bin deletes, and
drag-and-drop in *and* out. Everything-style search over 3.3M files across every
NTFS volume, a scoped fuzzy finder, and a command palette. This PC, Network,
Control Panel, the Recycle Bin and the inside of a zip all browse like folders.

Since M7: cut/copy/paste, rename, new folders and rubber-band selection; a grid
view; a preview pane; listings that update themselves; zip and tar archives;
plugins as separate processes; FTP, FTPS and SFTP servers; Google Drive over the
API; and one status line that says when any of it fails.

Redesigned throughout: light by default, opaque cards on a still ground, a fixed
action rail, Neutron's own window frame, and a dashboard to open on. See
[Design](#design).

A 100k-entry folder opens in 52ms and scrolls at 0.21ms per frame.

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

On macOS, install stable Rust and the Xcode command line tools (`xcode-select --install`), then:

```sh
cargo xtask bundle             # release build: target/release/Neutron.app
open target/release/Neutron.app
cargo xtask install            # optional: build and install to ~/Applications
cargo test --workspace --locked
```

The bundle is built for the current Mac's architecture (Apple Silicon or Intel)
and requires macOS 12 or later. It contains the Rust app and a small native Swift
helper for file operations, file URLs on the clipboard, Trash, and Keychain.
`cargo run -p neutron-app -- /some/folder` also works during development.
Configuration files live in `~/Library/Application Support/Neutron`.

macOS supports local browsing, copy/cut/paste, safe copy and move without
replacing existing files, rename, new folders, Trash, previews, archives, native
file launching, Terminal, and saved remote/cloud credentials in Keychain.
Command works for the existing Ctrl shortcuts; Command+Backspace moves the
selection to Trash. The Finder's own keys work too: Command+Up and Command+Down
to leave and enter folders, Command+[ and Command+] for back and forward, and
Command+Shift+. for hidden files (Command+H belongs to Hide). The terminal opens
with Control+`, since Command+` is the system's window switcher. Global search uses Spotlight (and its indexing/privacy
settings). Folder search scans at most 100,000 entries or three seconds and
marks partial results. Visible folders refresh within roughly one second.

The Mac window uses native rounded corners, shadow, and traffic-light controls.
The action rail runs Copy, Cut, Paste, Rename, Trash, New Folder, Extract, and
Zip against the focused local pane. Buttons explain unavailable actions, and
the rail scrolls in shorter windows. Archive jobs refresh their source folder
when they finish, including when you have switched to another pane.

The icon source is `assets/icon.svg`. To regenerate the bundled PNG, Windows
ICO, and in-app RGBA assets, install Node.js and ImageMagick 7, then run:

```sh
npm ci --prefix scripts
scripts/render-icon.sh
```

The macOS bundle generates its ICNS from the 1024-pixel PNG. The window and
sidebar use a separate 256-pixel render made directly from the vector for
Retina displays.

On macOS Neutron uses the system's own services: native file icons and Quick
Look previews, dragging files out to other apps, the keychain for server
passwords, and `copyfile(3)` for copies (cloned on APFS, metadata and tags
kept). Windows shell extensions and NTFS indexing have no Mac equivalent and are
not offered there. Settings, pins and saved servers live under
`~/Library/Application Support/Neutron`. The local bundle is ad-hoc signed;
public distribution requires Developer ID signing and notarization.

### On the Mac

The menu bar has File, Edit, View, Go and Window menus laid out as the
Finder's, with every command below in them. The keys follow the Finder's:

| Key | Does |
|---|---|
| `Space`, `⌘Y` | Quick Look the selection; arrows move through the folder with it open |
| `↩` | Rename |
| `⌘O`, `⌘↓` | Open |
| `⌘↑`, `⌘[`, `⌘]` | Enclosing folder, back, forward |
| `⌘1` `⌘2` `⌘3` | Grid, list, columns |
| `⌘D` | Duplicate |
| `⌘Z`, `⇧⌘Z` | Undo and redo copies, moves, renames, new folders and Move to Trash |
| `⌘I` | Get Info (the Finder's window) |
| `⌘⌫`, `⌘⌥⌫` | Move to Trash, delete immediately (asks first) |
| `⌘⇧H` `⌘⇧D` `⌘⇧O` `⌘⌥L` `⌘⇧A` | Home, Desktop, Documents, Downloads, Applications |
| `⌘⇧G` | Go to a folder by path |
| `⌘⇧]`, `⌘⇧[`, `⌃⇥` | Next and previous tab |
| `⌘E` | Eject the disk this folder is on |
| `⌘K` | Connect to a server |
| `⌘N`, `⌘W` | New window; close the tab, or the window on its last tab |
| `⌃⌘T` | Add the folder to the sidebar |
| `⌘⇧.` | Show hidden files |

Copies and moves run in the background with progress and a Stop that works
partway through a file; when names are already taken you are asked whether to
keep both, skip, or replace (the replaced items go to the Trash). Disks appear
and disappear in the sidebar as they are mounted and ejected, with an eject
button beside the ones that can be. A `.zip`, `.tar` or `.tar.gz` opens as a
read-only folder you can browse, preview, Quick Look and copy out of;
**Extract** in its menu unpacks the whole thing beside itself.

Windows, natively:

```powershell
cargo build --release -p neutron-app
cargo test --workspace
cargo run -p neutron-app -- 'C:\some\folder'
```

Requires `rustup` with `x86_64-pc-windows-msvc`, and Visual Studio Build Tools
with the Windows SDK. Nothing else — in particular **no NASM and no CMake**,
which is deliberate and is checked by CI: the crypto backend is pinned to
`ring`, and the C library that would otherwise arrive through `russh` is the
one dependency that has already broken a clean checkout once.

> Earlier versions of this file described building from a WSL shell through a
> `cargo xtask` wrapper, which mapped the WSL share to a drive letter because
> much of the MSVC toolchain refuses UNC working directories. That setup is
> gone, and so is the wrapper. `xtask` is back, but as a task runner rather
> than a build driver — see below. If you go back to developing on a WSL
> filesystem, the UNC problem is real and that is what you will be solving.

### Installing it

```powershell
cargo xtask install      # build, then copy into %LOCALAPPDATA%\Programs\Neutron
cargo xtask uninstall    # remove that folder
cargo xtask where        # print the paths, and what is currently installed
```

Its own folder rather than loose files in `Programs`, so uninstalling is one
directory rather than a list that has to stay in step with what was copied.

Both executables are copied, and that is not incidental: `neutron.exe` looks for
`neutron-indexer.exe` *beside itself* and refuses to raise a UAC prompt for a
file that is not there. Installing the main binary alone gives you a program
whose search is permanently broken and which only says so when you use it.

Windows holds an executable open while it runs, so installing over a copy you
have open fails. It says which file and why, rather than passing on the bare
"the process cannot access the file".

### Fast inner loop

The crates that do not depend on `windows` build and test anywhere, which is
quicker than building the shell and the renderer:

```bash
cargo test -p neutron-core -p neutron-ui -p neutron-fuzzy -p neutron-archive -p neutron-plugin
```

`neutron-plugin`'s integration tests are the reason to run this on Linux rather
than Windows: they drive real child processes and are `#![cfg(unix)]`, so on
Windows they are skipped and report nothing. CI runs them on Linux for exactly
that reason.

`neutron-shell` and `neutron-app` include native macOS backends. The NTFS USN indexer remains Windows-only; macOS uses Spotlight and scoped filesystem search.

---

## Layout

```
crates/
  neutron-app/      window, event loop, tabs, panes         (bin: neutron)
     chrome.rs      the title bar, its buttons, and the resize edges
     rail.rs        the fixed strip of verbs
     dashboard.rs   what a window opens on
     pins.rs        folders pinned to Quick access
  neutron-ui/       theme, ambient layer, virtualized list, formatting
  neutron-core/     domain types, sorting, history, traits  ← no Win32, tests on Linux
  neutron-fuzzy/    fzf-style matching with highlight spans ← no Win32, tests on Linux
  neutron-shell/    Win32 + COM: enumeration, icons, context menus
  neutron-index/    NTFS journal indexing, query engine, wire protocol
  neutron-cloud/    OneDrive and Google Drive providers
  neutron-archive/  zip and tar: extract, compress         ← no Win32, tests on Linux
  neutron-plugin/   manifests, child processes, protocol   ← no Win32, tests on Linux
  neutron-remote/   FTP, FTPS and SFTP clients
  neutron-indexer/  elevated helper process                (bin: neutron-indexer)
xtask/              project tasks: install, uninstall, where
assets/             app icon (SVG source, .ico, raw RGBA)
scripts/            benchmark directory generator
.github/workflows/  CI: Windows, Linux, and the stated minimum Rust
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

Neither of those catches a bare `std::fs` call in `neutron-app`, and an audit
found six — a `stat` to check a rename collision, two more choosing a
non-colliding archive name (one of them in a loop up to 9,998 times), and three
writes to `%APPDATA%` and Credential Manager. Every one is instant on a local
SSD, which is precisely why each survived being written by someone who knew the
rule. They are all on workers now, but the honest summary is that this rule has
no compiler behind it in the crate where it matters most, and holding it means
looking.

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

**Dragging out** was the last piece and needed its own answer. `DoDragDrop` is
modal on the thread holding the mouse capture, so it runs on the STA pool with
the input queues joined — the window keeps painting for the whole drag. The data
object comes from `SHCreateDataObject` rather than being hand-rolled, so every
format a drop target might ask for is answered.

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

**Archives.** A `.zip`, `.tar` or `.tar.gz` opens as a folder, on every
platform, read by Neutron itself rather than through the shell: a zip's
directory is read without decompressing anything, a tar is read through once,
and either is kept while the archive is unchanged, so moving between its folders
costs nothing. It is read-only. Opening a file, previewing it, Quick Look and
Copy take the entries out into the cache (`~/Library/Caches/Neutron/archives` on
the Mac) and work from there; **Extract to the Archive's Folder** copies them out
beside the archive, with the usual question when a name is taken. Entry names
are an archive's own strings, so they are normalised for display and go through
the same checks as a full extraction before anything is written.

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

An app opened from the Dock or the Finder never sees environment variables, so
the values can also go in a file, which is read when the environment has none:
`~/Library/Application Support/Neutron/google.json` on the Mac,
`%APPDATA%\Neutron\google.json` on Windows.

```json
{ "client_id": "<id>.apps.googleusercontent.com", "client_secret": "<secret>" }
```

Until one or the other is there, the sidebar offers **Set up Google Drive**,
which creates that file with empty fields and shows it in the Finder; choosing
it again once the fields are filled in brings up **Connect Google Drive**. The
rest of Neutron is unaffected.

**Security.** The flow is PKCE with a loopback redirect. A desktop app cannot
keep a secret — anything compiled in is readable by whoever has the binary, which
is exactly why Google's own client secret for installed apps protects nothing —
so PKCE generates a fresh verifier per attempt and an intercepted authorisation
code is useless without it. The redirect binds `127.0.0.1:0` rather than a
custom URI scheme, which any other program on the machine could register and
intercept. The `state` parameter is checked before the code is even read.

The refresh token lives in the keychain on the Mac and in Windows Credential
Manager, not on disk. That is not
a strong boundary — anything running as the user can read it back, which is what
Neutron does — but it is encrypted under the login, scoped to the account, and
markedly better than a config file that ends up in backups. Only
`drive.readonly` is requested: Neutron browses and downloads, and asking for
write access it never uses is a worse consent screen and a larger blast radius.

**Drive is not a filesystem**, and three differences shape the code: objects are
addressed by id rather than path (a file can live in several folders), names are
not unique within a folder, and native Google formats have no bytes to download
without an export format.

Each of the three shows up somewhere visible:

**Folders are navigated by id.** A Drive folder id is not a path and not a shell
parsing name, so opening one is its own branch — and the folder's *name* travels
with the id, because an id says nothing about which folder it is and resolving
one would be an API call on the paint thread.

**Opening a file fetches a copy** to `%TEMP%\Neutron\GoogleDrive\` and opens
that, the same way a file on a server does, with the same progress and Stop in
the status bar. The status line says **opened a local copy**, because editing it
does not write back — the scope is read-only and there is nowhere for the edit
to go.

**A Google Doc has no bytes at all.** Docs, Sheets and Slides are exported to
`docx`, `xlsx` and `pptx` rather than to PDF: a spreadsheet exported as PDF is a
picture of a spreadsheet, and the reason to pull a Sheet out of Drive is usually
to work on it. A Form, a Site or a Map has no export, and is refused by name —
Drive answers an export request for those with an error *document*, which a
client that streamed before checking the status would save under the file's own
name.

**A Drive name is not a filename**, and this is the part that needed care. Drive
names are not paths, so Drive barely restricts them: `Q3/Q4 forecast`, `..`,
`notes.txt:hidden` and a name containing a newline are all names the API will
hand back, and joining one onto a directory writes outside it. Refusing them —
which is the right answer for an FTP listing, where a server sending such a name
is misbehaving — would be wrong here, because a slash in a Drive name is
ordinary and the file would simply become unopenable. So the name is rewritten
into one certainly-safe component, and the result is then put through the same
rejector the listing side uses, as a proof rather than a second opinion.

**Up does not work inside Drive, on purpose.** A Drive object can sit in several
folders at once, so "the" parent of a folder is a question with more than one
answer, and picking one would send Up somewhere you never came from. The button
greys out, as it does in This PC; Back walks the trail that actually happened.

**Disconnect Google Drive** in the palette clears the stored credential and
drops the cached token, so a Drive folder stops listing the moment you
disconnect rather than most of an hour later.

> If you install Google Drive for Desktop, Drive becomes a drive letter and a
> shell namespace extension — which M6 already handles, for free. This path
> exists for machines without it, and is the one that can reach a file the
> local client has not synced.

---

## When something fails

Every file operation that can fail says so, in one line at the right of the
status bar. An **error stays there** until it is replaced or clicked away; a
receipt — "3 files hashed" — clears itself after a few seconds. The log gets
everything in full, because some failures have paragraphs behind them and the
status bar has one line.

This is not a small thing. Before it existed, eleven operations — rename, new
folder, paste, delete, drop, drag, clipboard, open, context menu — ended in a
log line and nothing on screen, so a rename that failed looked exactly like a
file manager that had ignored the keystroke. A tool that fails silently is one
you check with a second tool afterwards, and then you are using the second one.

The same applies to something Neutron cannot do *here*. Four kinds of place can
be open in a pane and only one of them — a real folder — has a filesystem path,
so pressing `F2` inside a zip or `Delete` in Google Drive reaches for a path
that does not exist. Each of those now says which place it is looking at, and
distinguishes two answers that are not the same: a server and a Drive folder are
places Neutron *will* write to, so the sentence ends in "yet"; Control Panel is
not, so it does not get a promise about something that is not coming.

---

## Servers

FTP, FTPS and SFTP servers appear in the sidebar under **Servers** and open like
folders. **Connect to a server** in the palette adds one; everything but the
password is written to `sites.json` in Neutron's settings folder
(`~/Library/Application Support/Neutron` on the Mac, `%APPDATA%\Neutron` on
Windows), and the password goes into the keychain on the Mac or Windows
Credential Manager under `Neutron/Site/<id>`.

Both clients are pure Rust — `suppaftp` and `russh` — because the alternative
for SSH is libssh2 through a C build, and this project cross-compiles from WSL
to the MSVC toolchain, where every C dependency is a new way for the build to
break at a bad moment.

Two things it takes seriously. **SSH host keys are checked**: recorded on the
first connection and enforced after it, with a changed key refused outright and
both fingerprints shown, in the same `SHA256:` format OpenSSH prints so you can
compare them. And **a directory listing is data the server chooses** — a name
like `../../../../Windows/System32/drivers/etc/hosts` is a valid listing entry,
and a client that joins it onto a download folder writes exactly there, so any
listed name carrying path structure is refused and the folder opens without it.

Opening a remote file fetches a copy to `%TEMP%` and opens that; editing it does
not write back, and the status bar says so.

A server folder takes **new folders, renames, deletes and uploads** — paste local
files or drop them onto the pane, and folders go up with everything in them.
Each write runs on a connection of its own, so browsing stays responsive, and
the status bar shows progress with a Stop button. Uploads never replace: a name
the server folder already has is refused before anything is sent. Deleting asks
first, says when folders will go with their contents, and never follows a
symbolic link into somewhere else. A cut onto a server copies; the originals
stay. Copying files *off* a server, other than opening one, is not done yet.

**Forget this server** in the palette removes the one you are looking at, and
takes its password out of Credential Manager with it. Until that existed a saved
server could only be removed by editing `sites.json` by hand, which left the
password behind — a credential the application would store and would not let go
of. `docs/servers.md` has the manifest reference and the rest.

---

## Plugins

A plugin is a folder under `plugins` in Neutron's settings folder
(`~/Library/Application Support/Neutron/plugins` on the Mac,
`%APPDATA%\Neutron\plugins` on Windows) holding a `plugin.json`
and a program. Its commands appear in the command palette and, if the manifest
asks, in the right-click menu. Choosing one runs the program with the current
folder and selection as one line of JSON on standard input, and acts on the
lines it prints back.

```json
{"protocol":1,"command":"sha256","folder":"C:\\notes","selection":["C:\\notes\\a.txt"]}
```
```json
{"type":"status","text":"1 file hashed"}
{"type":"refresh"}
```

That is the whole interface, so a plugin can be written in anything that can
read a line and print one — the example that ships in `examples/plugins/hashes`
is twenty lines of PowerShell. **Open the plugins folder** in the palette
creates the directory and takes you there; **Reload plugins** re-reads it
without a restart, and reports the reason for anything it refused.

Plugins are **separate processes**, not loaded libraries. A release build is
`panic = "abort"`, so an in-process plugin that panics would take the window
down with it; a child process that dies is an exit status. The host holds it to
a deadline, drains all three of its pipes, and offers a Stop.

This is **not a sandbox** — a plugin runs with your rights. What it does
guarantee is that listing a plugin never runs it, that nothing appears in the
palette that is not written in a file you can read, and that a plugin's output
is treated as untrusted before it is drawn. `docs/plugins.md` has the full
manifest reference and the rest of the protocol.

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
| *press and drag* | Rubber-band select (hold `Ctrl` to add) |
| `Ctrl+C` `Ctrl+X` `Ctrl+V` | Copy / cut / paste, through the real Windows clipboard |
| `F2` | Rename in place, with the stem selected |
| `Ctrl+Shift+N` | New folder, ready to be named |
| `Ctrl+N` | Open another window here — in the same Neutron, sharing its caches, jobs and undo history |
| ``Ctrl+` `` | Open a terminal here — see `docs/terminal.md` |
| `Ctrl+H` | Show hidden files |
| `Ctrl+Shift+L` | Cycle the view: list, grid, columns |
| `←` `→` *(column view)* | Back out to the parent folder / into the selected folder |
| `Alt+P` | Show the preview pane — works on servers and in Drive too |
| `Ctrl+F` | Filter the current listing (`Esc` clears it) |
| `Ctrl+Shift+F` | Search every volume (arrows move, `Enter` opens, `Esc` closes) |
| `Ctrl+P` | Fuzzy-find a file in this folder and below |
| `Ctrl+Shift+P` | Command palette |
| *double-click a `.zip`* | Browse inside it (also `.tar` and `.tar.gz`); read-only |
| `Delete` | Send the selection to the Recycle Bin |
| `Shift+Delete` | Delete permanently (the shell still confirms) |
| *right-click* | The shell's own menu, drawn in Neutron's glass |
| `F5`, `Ctrl+R` | Refresh |
| `Esc` | Clear selection |
| `Ctrl+D` | Toggle light/dark theme |
| `Ctrl+Shift+D` | Pin or unpin this folder in Quick access |

The column view works like the Finder's. Each folder on the way to the
current one gets a column, with the next step highlighted, and selecting a
folder shows its contents in a column to the right. Columns start at your home
folder for anything inside it, and at the volume root otherwise. Clicking any
row in any column makes that folder current. Drag the edge between two columns
to resize the one on its left, hold `Option` (`Alt`) while dragging to resize
them all, and double-click an edge to fit that column to its longest name.
Servers, Drive and This PC have no path to build columns from, so they stay a
list.

The filter narrows the listing already in memory — a case-insensitive substring
over names, applied without touching the disk, so it stays instant while typing
even in a 100k-entry directory. It clears on navigation, because a filter
describing the folder you left would silently empty the one you entered. Fuzzy
matching across the whole index is a different thing, and lives in the finder
overlay above.

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

**Cards on a still ground.** A near-white ground with a soft overhead wash and
two static colour washes far off-centre, so the window is tinted rather than
flat. On it, the sidebar, the action rail and each pane are opaque white cards
with a hairline border, a large radius and a shadow you have to look for.
Depth comes from elevation and from that hairline, not from ruled lines through
the content.

**Light is the default**; dark is `Ctrl+D`, and it is the same design with the
values inverted rather than a second look — opaque cards, a still ground, one
accent.

**Nothing moves.** The ground used to drift: two colour fields on 41- and
57-second loops, there to give translucent glass something to refract. Both
premises are gone. The cards are opaque, so there is nothing to refract
*through*, and on a light palette a moving ground stops reading as depth and
starts reading as a smudge travelling under the text.

It was also the only continuous repaint in the program — `request_repaint_after`
running forever so that a soft gradient could move by under a pixel per frame.
Measured: **1.69%** of a core with it, **0.26%** without. A file manager that is
open all day should cost nothing while nobody is touching it, and
`ambient::paint` now takes no clock at all, because a signature that accepts one
invites something to start moving again.

**Colour is scarce.** Every surface that carries content is near-neutral. The
accent appears in about five places — the selected row's left edge, the active
sidebar row, the active tab's underline, the focus ring, and the grid's
selection badge — and file icons are grey, not purple. Saturated colour spread
across every row destroys its usefulness as a signal: if everything is purple,
purple stops meaning "this is selected".

**The location is named twice, on purpose.** A chevron breadcrumb you can
navigate, and under it the folder's own name set large. They are not redundant:
one is a path, the other is a label that says at a glance which pane you are
looking at. Both reference designs lead their content area this way.

**A filename is two things.** The stem carries the text colour and the
extension is dimmed. In a folder of forty photographs the extension is the same
forty times and costs the same reading weight as the part that differs.

**Navigation lives in the pane, not in a toolbar.** Back/forward/up, the
breadcrumb, the title and the filter are all inside each pane's card. With split
panes there is no single "current location", so a window-wide breadcrumb would
have to pick one pane and ignore the other — every control it held was really a
control on the focused pane wearing a costume.

**The rail is the exception to keyboard-first.** A fixed strip of verbs beside
the sidebar: copy, cut, paste, rename, delete, new folder, extract, zip. Every
one already has a keystroke and a palette entry, and the pitch above lists
"mouse-centric" as *Explorer's* problem — but a palette only helps someone who
already knows the command exists, and a visible row of verbs is how you find out
what a file manager can do. Each row's hover text names its shortcut, so the
rail teaches its own way out.

**The window frame is Neutron's own.** A system title bar sits above the layout
as a separate strip in the OS's colours and corner radius, and nothing done
inside the window makes the two read as one object. The cost is that everything
the OS gave away for free has to be put back: dragging, the three buttons,
double-click to maximise, and the resize border, which an undecorated window
does not have.

All colour lives in `neutron-ui/src/theme.rs`, with tests asserting WCAG contrast
for text, muted text, accents and selected rows against every surface they can
land on. Those tests survived the whole palette being replaced, because they
were written against the *roles* rather than the colours.

> Neutron originally asked Windows for a transparent window and used the DWM
> acrylic backdrop. Measurement showed wgpu cannot supply a surface with a
> transparent `CompositeAlphaMode` on any backend available here, so the ground
> is painted by Neutron itself and the effect is self-contained.

## Performance

See [`docs/perf.md`](docs/perf.md) for measured numbers.

| Metric | Target | Actual |
|---|---:|---:|
| Enumerate + sort 100k entries | <200 ms | **52 ms** |
| Frame time p99 while scrolling | <8 ms | **0.21 ms** |
| Index 3.3M files, all volumes | <10 s | **2.6 s** (warm cache) |
| Search, incremental keystroke | <1 ms | **0.003 ms** |
| Search, first character | <1 ms | 6–9 ms ❌ |
| Cold start, repeat launch | <100 ms | 410 ms ❌ |
| Cold start, first after a build | <100 ms | 801 ms ❌ |
| Idle CPU, focused | ~0% | 0.26% |
| Idle CPU, unfocused | ~0% | 0.00% |
| Idle memory, private bytes | <60 MB | 499 MB ❌ |
| Binary size | ~15 MB | 19.0 MiB ❌ |

Re-measured 2026-08-29; [`docs/perf.md`](docs/perf.md) is the measurement of
record and carries the method. Quote **private bytes** for memory, not working
set — Windows trims a working set at will, and across identical runs it varied
from 256 to 448 MB while private bytes held to a few percent.

Cold start and idle memory share one cause — GPU device creation, which costs
the best part of a second and a few hundred megabytes before any of our code
runs. The search miss is different: 6–9ms is half a frame, so it is below the
threshold where a keystroke feels delayed, but the target says <1ms and is not
met. Closing it means an n-gram or suffix structure over the name arena, which
is real memory for a latency nobody can perceive. That file records the
measurements, the causes, and the options.

The binary misses its target because of the server clients — `russh`, `rustls`,
`tokio` and `suppaftp` are most of the gap. Nothing is loaded until a server is
opened, so start-up and idle memory are unaffected by it.
