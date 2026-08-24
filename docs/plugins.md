# Writing a Neutron plugin

A plugin adds commands to the command palette and, if it asks, to the
right-click menu. Choosing one runs a program with the current folder and
selection on its standard input; what it prints back tells Neutron what to do.

That is the whole design. A plugin is a program that reads a line of JSON and
prints some lines of JSON, so it can be written in anything — the example that
ships with Neutron is twenty lines of PowerShell.

## Installing one

```
%APPDATA%\Neutron\plugins\
  hashes\
    plugin.json     what the plugin declares
    run.cmd         the program plugin.json names
    hashes.ps1
```

One folder per plugin. **Open the plugins folder** in the command palette
(`Ctrl+Shift+P`) creates it and takes you there; **Reload plugins** re-reads it
without restarting.

Copy `examples/plugins/hashes` from the repository into that folder to try the
example. It writes `SHA256SUMS.txt` for whatever you have selected.

## plugin.json

```json
{
  "protocol": 1,
  "id": "hashes",
  "name": "Hashes",
  "description": "Writes SHA-256 checksums for the selected files",
  "run": "run.cmd",
  "args": [],
  "commands": [
    {
      "id": "sha256",
      "title": "Write SHA-256 checksums",
      "needs": "selection",
      "menu": true,
      "timeout_ms": 20000
    }
  ]
}
```

| Field | | |
|---|---|---|
| `protocol` | required | `1`. A manifest declaring anything else is refused by name, rather than half-working. |
| `id` | required | Lowercase slug, unique across installed plugins. |
| `name` | required | Shown beside your commands, so two plugins offering "Copy hash" can be told apart. |
| `description` | optional | Not shown yet. Write it anyway. |
| `run` | required | **A file name beside `plugin.json`.** Not a path — see below. |
| `args` | optional | Fixed arguments, before anything Neutron adds. |
| `commands` | required | At least one, at most 32. |

And per command:

| Field | | |
|---|---|---|
| `id` | required | Lowercase slug, unique within the plugin. Sent back to you as `command`. |
| `title` | required | What the user reads. Imperative — it is an instruction, not a label. |
| `needs` | required | `"selection"` or `"folder"`. A `selection` command is hidden from the right-click menu when nothing is selected. |
| `menu` | optional | Also show it in the right-click menu. Defaults to off; that menu is long enough already. |
| `timeout_ms` | optional | Default 20 000, clamped to 250–120 000. |

### `run` is a file name, not a path

`"run": "..\\..\\anything.exe"` is refused, as are absolute paths, subdirectories,
anything containing `:` (which covers both `C:\` and the quieter
`plugin.exe:stream`), and the Windows device names. The plugin folder is what
the user installed and can look inside; a manifest that could point `run`
somewhere else would mean the thing that runs is not the thing they installed.

`CreateProcess` cannot start a `.ps1` or a `.py`, so a scripted plugin ships a
one-line `.cmd` launcher — which is also where it declares its interpreter. See
`run.cmd` in the example.

## The request

One line of JSON on standard input, then end-of-file:

```json
{"protocol":1,"command":"sha256","folder":"C:\\notes","selection":["C:\\notes\\a.txt"]}
```

- `command` — the `id` of the command chosen, from your manifest.
- `folder` — the folder on screen.
- `selection` — absolute paths, empty for a `folder` command, at most 1024.

## The responses

Newline-delimited JSON on standard output. Print as many as you like, in any
order, up to 64.

| | |
|---|---|
| `{"type":"status","text":"3 files hashed"}` | Say something in the status bar. The last one wins. |
| `{"type":"refresh"}` | Re-list the folder, because you changed what is in it. |
| `{"type":"navigate","path":"C:\\somewhere"}` | Show a folder in the pane the command was run from. |
| `{"type":"open","path":"C:\\file.txt"}` | Hand a path to the shell, as a double-click would. At most 8. |
| `{"type":"error","message":"that is not an image"}` | The command failed. Your message is what the user reads. |

Anything else on a line is ignored with a warning — but if a run prints *only*
unreadable lines, the user is told the plugin printed something Neutron cannot
read, rather than being left with a command that appears to do nothing.

Use `stderr` for your own diagnostics. Neutron logs it, and shows the tail of it
when a plugin exits non-zero without an `error` response — which means a plugin
that crashes on a missing interpreter says so in the status bar.

## What Neutron guarantees, and what it does not

**Not a sandbox.** A plugin is a program you installed and it runs with your
rights: it can read and write everything you can. `open` grants it nothing extra
— it could launch anything itself — it just saves it linking a Windows API.

What the design does guarantee:

- **Listing a plugin never runs it.** Discovery reads manifests and stops.
  Nothing executes until a person picks a command, so a plugin cannot run at
  startup, and a malformed one cannot run at all.
- **Nothing appears that is not written in a file you can read.** Every title in
  the palette comes from a manifest on disk, never from a program's output.
- **A plugin cannot take the window down or hang it.** It is a separate process
  with a deadline, three drained pipes, and a Stop button.
- **Output is treated as untrusted.** Text is flattened to one line, stripped of
  control and bidi characters, and length-capped before it is drawn.

## When a plugin does not appear

Every rejected folder keeps its reason. **Reload plugins** reports the first one
in the status bar, and all of them go to the log:

```
$env:RUST_LOG = "neutron_plugin=debug,neutron_app=debug"; neutron.exe
```

The usual causes are a `plugin.json` that is not valid JSON, a `run` naming a
file that is not there, and two plugins claiming the same `id`.
