# Saved servers: FTP, FTPS and SFTP

Neutron browses remote servers as ordinary panes. A server appears in the
sidebar under **Servers**, opens like a folder, and its files sort, filter and
preview the way local ones do.

## Adding one

**Connect to a server** in the command palette (`Ctrl+Shift+P`), or **Add a
server…** at the bottom of the Servers section.

| | |
|---|---|
| **SFTP** | SSH's file transfer subsystem. Encrypted, and the default. Not related to FTP. |
| **FTPS** | FTP over TLS (`AUTH TLS`). Encrypted. |
| **FTP** | Plain FTP. **The password crosses the network in the clear.** Offered because a great many appliances speak nothing else. |

Everything except the password is written to `%APPDATA%\Neutron\sites.json`,
which you can also edit by hand:

```json
{
  "sites": [
    {
      "id": "nas",
      "label": "Home NAS",
      "protocol": "sftp",
      "host": "nas.local",
      "port": 22,
      "username": "razeen",
      "path": "/volume1/media"
    }
  ]
}
```

| Field | | |
|---|---|---|
| `id` | required | Lowercase slug, unique. Names the stored password and identifies the site in a saved session, so changing it orphans both — rename the `label` instead. |
| `label` | optional | What the sidebar shows. Defaults to the host. |
| `protocol` | required | `"ftp"`, `"ftps"` or `"sftp"`. |
| `host` | required | Name or address. |
| `port` | optional | Defaults to 21 for FTP and FTPS, 22 for SFTP. |
| `username` | optional | Empty means anonymous, for FTP and FTPS. SFTP requires one — SSH has no anonymous mode. |
| `path` | optional | Where browsing starts. Defaults to `/`. |
| `fingerprint` | written by Neutron | The server's SSH host key. See below. |

**Reload saved servers** in the palette re-reads the file and drops every open
connection, so an edit takes effect without a restart.

## Passwords

Passwords are **not** in `sites.json`. They go into Windows Credential Manager
under `Neutron/Site/<id>`, where they are encrypted under your login rather than
sitting in a file that ends up in a backup. You can see and remove them in
Credential Manager like any other stored credential.

This is not a strong boundary — anything running as you can read one back, which
is exactly what Neutron does — but it is what every other desktop client gets,
and it beats plaintext.

A site with no saved password is **not** dialled with an empty one. Some servers
count that as a failed login attempt, and a file manager should not spend your
lockout budget guessing.

## SSH host keys

Encryption says nothing about *who* is on the other end. A machine on the path
can present its own key, take your password, and forward everything on, and the
login looks perfectly ordinary. Checking the host key is the whole defence.

Neutron does what OpenSSH does:

- **First connection** — the key is recorded into the site's `fingerprint` and
  the connection proceeds. The fingerprint is `SHA256:` base64, byte-for-byte
  what OpenSSH prints, so `ssh-keyscan host | ssh-keygen -lf -` gives you
  something to compare it against.
- **Afterwards** — it must match.
- **If it changes** — the connection is refused, and both fingerprints are
  shown. That is what an impersonated server looks like. It is also what a
  rebuilt server looks like: if you know the server was rebuilt, clear
  `fingerprint` for that site in `sites.json`; otherwise do not connect.

## What works

Browsing, sorting, filtering, and opening files. Opening a file downloads it to
`%TEMP%\Neutron\<site>\` and hands the copy to whatever normally opens that file
type — the status bar says **opened a local copy**, because editing it does not
write back.

Downloads show a running byte count and a **Stop** in the status bar. Stopping
removes the partial file: unlike an interrupted extraction, this one is about to
be handed to an application, and half a PDF is a crash report rather than a
document.

## What does not work yet

**Uploading, renaming, deleting and creating folders on the server.** Nothing
here writes to a server. Drag-and-drop into a remote pane does nothing, and the
usual keys report that they cannot.

There is also no background sync, no resumable transfer, and no key-based SSH
authentication — passwords only.

## A note on names

A directory listing is data the *server* chooses. A server can answer with an
entry called `../../../../Windows/System32/drivers/etc/hosts`, and a client that
joins that name onto a download folder writes exactly there. Neutron refuses any
listed name containing a path separator, `..`, a colon, a control character, or a
Windows device name; the entry is dropped, the reason is logged, and the rest of
the folder still opens.

## Diagnosing a connection

```powershell
$env:RUST_LOG = "neutron_remote=debug,neutron_app=debug"; neutron.exe
```

The FTP client logs whether the server offered `MLSD` — the machine-readable
listing — and falls back to parsing `LIST` when it does not. `LIST` has no
specification, so an entry that looks wrong on an unusual server is worth
reporting with the log line beside it.
