# Saved servers: FTP, FTPS and SFTP

Neutron browses remote servers as ordinary panes. A server appears in the
sidebar under **Servers**, opens like a folder, and its files sort, filter, and
browse in list, grid, and column views the way local ones do.

## Adding one

**Connect to a server** (`⌘K` on macOS, or via the command palette), or **Add a
server…** at the bottom of the Servers section.

| | |
|---|---|
| **SFTP** | SSH's file transfer subsystem. Encrypted, and the default. Not related to FTP. |
| **FTPS** | FTP over TLS (`AUTH TLS`). Encrypted. |
| **FTP** | Plain FTP. **The password crosses the network in the clear.** Offered because a great many appliances speak nothing else. |

Everything except the password is written to `sites.json`
(`~/Library/Application Support/Neutron/sites.json` on macOS, or
`%APPDATA%\Neutron\sites.json` on Windows), which you can also edit by hand:

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
      "path": "/volume1/media",
      "auth": { "method": "key", "path": "~/.ssh/id_ed25519" }
    }
  ]
}
```

| Field | | |
|---|---|---|
| `id` | required | Lowercase slug, unique. Names the stored credential and identifies the site in a saved session, so changing it orphans both — rename the `label` instead. |
| `label` | optional | What the sidebar shows. Defaults to the host. |
| `protocol` | required | `"ftp"`, `"ftps"` or `"sftp"`. |
| `host` | required | Name or address. |
| `port` | optional | Defaults to 21 for FTP and FTPS, 22 for SFTP. |
| `username` | optional | Empty means anonymous, for FTP and FTPS. SFTP requires one — SSH has no anonymous mode. |
| `path` | optional | Where browsing starts. Defaults to `/`. |
| `auth` | optional (SFTP) | Omit for password auth, `{ "method": "key", "path": "..." }` for a private key file (with any passphrase in the credential store), or `{ "method": "agent" }` for the running SSH agent (`SSH_AUTH_SOCK`). |
| `fingerprint` | written by Neutron | The server's SSH host key. See below. |
| `trusted_cert` | optional (FTPS) | SHA-256 fingerprint of a self-signed FTPS certificate you have chosen to trust. |

**Reload saved servers** in the palette re-reads the file and drops every open
connection, so an edit takes effect without a restart.

## Passwords and key passphrases

Secrets are **not** stored in `sites.json`. They go into the system credential
store (macOS Keychain / Windows Credential Manager) under `Neutron/Site/<id>`,
where they are protected under your login rather than sitting in a config file
that ends up in backups.

A password site with no saved password is **not** dialled with an empty one.
Some servers count that as a failed login attempt, and a file manager should
not spend your lockout budget guessing.

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

- **Browsing:** List, grid, and column views, sorting, and filtering.
- **Opening and editing files:** Opening a file downloads it to the temp cache
  under `Neutron/<site>/` and opens it in its associated app. While Neutron is
  running, saving changes to that opened file automatically uploads the updated
  file back to the server and refreshes the folder listing.
- **Creating, renaming, and deleting:** **New Folder** (`⇧⌘N`), **New File**
  (`⌥⌘N`), inline **Rename** (`↩`), and permanent **Delete** (`⌘⌫`, after
  confirmation — recursive folder deletes never follow symbolic links).
- **Uploading:** Paste (`⌘V`) or drag-and-drop local files and folders into a
  server pane. Existing names at the destination are refused up front so
  uploads never overwrite files unexpectedly, and failed or stopped uploads
  remove partial files.

Downloads and uploads show a running byte count and a **Stop** button in the
status bar. Stopping removes any partial file.

## Limitations

There is no offline sync queue, no resumable transfer, and no server-side search
or Finder Quick Look on remote items. Writing to a server executes directly over
the connection (there is no remote Trash).

## A note on names

A directory listing is data the *server* chooses. A server can answer with an
entry called `../../../../Windows/System32/drivers/etc/hosts`, and a client that
joins that name onto a download folder writes exactly there. Neutron refuses any
listed name containing a path separator, `..`, a colon, a control character, or a
Windows device name; the entry is dropped, the reason is logged, and the rest of
the folder still opens.

## Diagnosing a connection

```sh
RUST_LOG=neutron_remote=debug,neutron_app=debug neutron
```

The FTP client logs whether the server offered `MLSD` — the machine-readable
listing — and falls back to parsing `LIST` when it does not. `LIST` has no
specification, so an entry that looks wrong on an unusual server is worth
reporting with the log line beside it.

## Remote storage & dual-boot hosts

Connected SFTP servers automatically report their internal and external storage
volumes in the sidebar and on the Dashboard (`<Server> · Storage`):

- **macOS & Linux hosts:** Probed via POSIX `df -kP` across `/`, `/Volumes/*`,
  `/mnt/*`, `/media/*/*`, and `/run/media/*/*` (skipping system/recovery mounts
  such as `/System/Volumes`, `/Volumes/Recovery`, `/boot`, `/snap`, and
  `/run/user`).
- **Windows hosts:** Enumerated quickly with `fsutil fsinfo drives`, then probed
  per drive (`fsutil fsinfo drivetype`, `vol`, `fsutil volume diskfree`) under
  an independent 2-second timeout per drive so an unresponsive removable reader
  never stalls `C:` or healthy external SSDs.
- **Pinning & live refresh:** Remote folders can be pinned to the sidebar
  **Pinned** section (`⌥⌘T` / context menu). Neutron re-probes SFTP storage
  every 60 seconds in the background and immediately on **Refresh** (`⌘R` /
  `F5`) from the Dashboard or a remote pane, clearing stale volumes whenever a
  host goes offline — so a dual-boot machine configured with separate Windows and
  Linux entries in `sites.json` displays only whichever OS is currently booted.
