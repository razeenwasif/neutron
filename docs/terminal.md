# Opening a terminal at a folder

On the Mac, **Open in Terminal**, `Control+\``, and **Open a terminal here** in
the command palette open Neutron's own terminal in a tab, running your login
shell in the folder. What follows is about Windows, where Neutron opens an
external terminal instead.

`Ctrl+\`` — or **Open a terminal here** in the command palette — opens a
terminal with the current folder as its working directory.

Without configuration Neutron runs `wt.exe -d <folder>`, then falls back to
`cmd.exe` if Windows Terminal is not installed.

## When the terminal opens in the wrong place

`wt -d` sets the directory for a profile whose command line Windows Terminal
controls. A **WSL profile installed from the Store is not one of those**: the
profile launches the distribution's own launcher, which starts in the Linux home
directory and ignores the working directory it was handed. Measured on a machine
with Ubuntu as the default profile:

| | lands in |
|---|---|
| `wt -d C:\notes cmd.exe` | `C:\notes` |
| `wt -d C:\notes` (Store WSL profile) | `~` |
| `wsl.exe --cd C:\notes` | `/mnt/c/notes` |

Nothing Neutron can pass fixes that, because the profile decides. Naming the
command yourself does. Put this in `%APPDATA%\Neutron\terminal.json`:

```json
{
  "command": "wt.exe",
  "args": ["-d", "{dir}", "wsl.exe", "--cd", "{dir}"]
}
```

Every `{dir}` is replaced with the folder's path — twice here, once for Windows
Terminal's own working directory and once for `wsl --cd`.

| Field | | |
|---|---|---|
| `command` | required | The program to run. |
| `args` | optional | Its arguments. `{dir}` is substituted everywhere it appears. |

The folder is *also* set as the launched process's working directory, so a
terminal that inherits it needs no arguments at all:

```json
{ "command": "alacritty.exe" }
```

A file rather than an environment variable, because a file manager is started
from the taskbar, the Start menu, or by Explorer opening a folder — none of
which inherit an environment.

A configured command that fails to start is reported rather than quietly falling
back: you wrote it down, so being silently ignored would be worse than an error.
