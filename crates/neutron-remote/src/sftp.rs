//! SFTP, over `russh`.
//!
//! Despite the name, SFTP has nothing to do with FTP: it is a subsystem of SSH,
//! carried inside one already-authenticated, already-encrypted channel. There
//! is no second connection per transfer, no ASCII mode, no `LIST` format to
//! guess at, and the listing carries real POSIX metadata. Almost everything
//! that makes `ftp.rs` careful does not apply here.
//!
//! What does apply is the host key. See [`crate::hostkey`] — encryption without
//! knowing who is on the far end protects against nobody in particular.
//!
//! # Async, in one thread, on purpose
//!
//! `russh` is async and there is no maintained pure-Rust SSH client that is
//! not. The alternative is libssh2 through a C build, and this project
//! cross-compiles from WSL to the MSVC toolchain, where every C dependency is a
//! new way for the build to break at a bad moment.
//!
//! So this module owns a **current-thread** Tokio runtime and blocks on it. The
//! runtime never leaves this struct, no other crate learns about async, and the
//! `Transport` methods stay ordinary blocking functions like the FTP ones. The
//! cost is one extra thread-local executor per connection, which is nothing
//! next to a TCP round trip.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use neutron_core::entry::EntryKind;
use russh::keys::PublicKeyOrCertificate;
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg};
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::hostkey::{self, Decision};
use crate::path;
use crate::site::{Auth, Site};
use crate::transport::{CHUNK, CONNECT_TIMEOUT, Continue, RemoteEntry, RemoteError, RemoteVolume, Transport};

/// A connected SFTP session, and the runtime driving it.
pub struct SftpTransport {
    runtime: tokio::runtime::Runtime,
    session: SftpSession,
    /// Kept alive for the session's sake: dropping the SSH handle closes the
    /// channel underneath the SFTP session.
    _ssh: russh::client::Handle<Client>,
    address: String,
    /// Set when this connection was the first to see the server's key, so the
    /// caller can record it against the site.
    new_fingerprint: Option<String>,
}

/// The SSH client callbacks.
///
/// The only one that matters is [`Client::check_server_key`], which is where an
/// SSH client is usually got wrong.
struct Client {
    /// `user@host:port`, so a refusal can name the server it was about.
    address: String,
    /// What the site has recorded, if anything.
    expected: Option<String>,
    /// What the server presented. Written during the handshake and read after.
    presented: Arc<parking_lot::Mutex<Option<String>>>,
    /// Whether the key was accepted, so the failure can be reported as a host
    /// key problem rather than as a generic handshake error.
    mismatch: Arc<parking_lot::Mutex<Option<String>>>,
    /// Whether this was a first sighting, to be recorded afterwards.
    first_sight: Arc<AtomicBool>,
}

impl russh::client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // `SHA256:` base64, byte-for-byte what OpenSSH prints, so a user can
        // compare it with `ssh-keyscan host | ssh-keygen -lf -`. A format only
        // Neutron understands would be unverifiable, which defeats the point of
        // showing it at all.
        //
        // A host certificate is fingerprinted by the key inside it. Neutron
        // does not check certificate authorities — almost nothing uses SSH host
        // certificates — so this treats one as the key it carries rather than
        // pretending to a validation it does not do.
        let fingerprint = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                key.fingerprint(HashAlg::Sha256).to_string()
            }
            PublicKeyOrCertificate::Certificate(certificate) => certificate
                .public_key()
                .fingerprint(HashAlg::Sha256)
                .to_string(),
        };
        *self.presented.lock() = Some(fingerprint.clone());

        match hostkey::decide(self.expected.as_deref(), &fingerprint) {
            Decision::Known => Ok(true),
            Decision::Record { .. } => {
                self.first_sight.store(true, Ordering::Relaxed);
                Ok(true)
            }
            Decision::Changed { expected, actual } => {
                // Refused here, and the reason stashed so the caller can say
                // something better than "handshake failed".
                *self.mismatch.lock() =
                    Some(hostkey::changed_message(&self.address, &expected, &actual));
                Ok(false)
            }
        }
    }
}

impl SftpTransport {
    /// Connects, checks the host key, authenticates, and opens the subsystem.
    pub fn connect(site: &Site, password: &str) -> Result<SftpTransport, RemoteError> {
        let address = site.address();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|e| RemoteError::Connect {
                address: address.clone(),
                reason: format!("could not start the network runtime: {e}"),
            })?;

        let presented = Arc::new(parking_lot::Mutex::new(None));
        let mismatch = Arc::new(parking_lot::Mutex::new(None));
        let first_sight = Arc::new(AtomicBool::new(false));

        let client = Client {
            address: address.clone(),
            expected: site.fingerprint.clone(),
            presented: Arc::clone(&presented),
            mismatch: Arc::clone(&mismatch),
            first_sight: Arc::clone(&first_sight),
        };

        let host = site.host.clone();
        let port = site.port();
        let username = if site.username.is_empty() {
            // SSH has no anonymous mode; a site with no username is a
            // misconfiguration worth naming rather than a login to attempt.
            return Err(RemoteError::Auth { address });
        } else {
            site.username.clone()
        };
        // Read before anything is dialled: a missing key file or a forgotten
        // passphrase is a problem on this machine, and finding out about it
        // should not cost a round trip and a key exchange.
        let login = Login::prepare(site, password, &address)?;
        let address_for_task = address.clone();

        let (ssh, session) = runtime.block_on(async move {
            let config = Arc::new(russh::client::Config {
                inactivity_timeout: Some(std::time::Duration::from_secs(300)),
                ..Default::default()
            });

            let connect = tokio::time::timeout(
                CONNECT_TIMEOUT,
                russh::client::connect(config, (host.as_str(), port), client),
            );
            let mut ssh = match connect.await {
                Ok(Ok(ssh)) => ssh,
                Ok(Err(e)) => {
                    // A refused key surfaces here as an ordinary handshake
                    // error, so the stashed reason is what gets reported.
                    if let Some(reason) = mismatch.lock().take() {
                        return Err(RemoteError::HostKeyChanged(reason));
                    }
                    return Err(RemoteError::Connect {
                        address: address_for_task.clone(),
                        reason: e.to_string(),
                    });
                }
                Err(_) => {
                    return Err(RemoteError::Connect {
                        address: address_for_task.clone(),
                        reason: format!("no answer within {}s", CONNECT_TIMEOUT.as_secs()),
                    });
                }
            };

            login.authenticate(&mut ssh, &username, &address_for_task).await?;

            let channel = ssh
                .channel_open_session()
                .await
                .map_err(|e| RemoteError::Connect {
                    address: address_for_task.clone(),
                    reason: e.to_string(),
                })?;
            channel
                .request_subsystem(true, "sftp")
                .await
                .map_err(|e| RemoteError::Connect {
                    address: address_for_task.clone(),
                    // The usual cause, and worth naming: SSH is fine and the
                    // server simply has no SFTP subsystem configured.
                    reason: format!("the server would not start the sftp subsystem: {e}"),
                })?;

            let session = SftpSession::new(channel.into_stream()).await.map_err(|e| {
                RemoteError::Connect {
                    address: address_for_task.clone(),
                    reason: e.to_string(),
                }
            })?;

            Ok((ssh, session))
        })?;

        let new_fingerprint = first_sight
            .load(Ordering::Relaxed)
            .then(|| presented.lock().clone())
            .flatten();
        if let Some(fingerprint) = &new_fingerprint {
            tracing::info!(%address, %fingerprint, "recording host key on first connection");
        }

        Ok(SftpTransport {
            runtime,
            session,
            _ssh: ssh,
            address,
            new_fingerprint,
        })
    }
}

/// How this connection will prove who it is, resolved before dialling.
enum Login {
    Password(String),
    Key(Arc<PrivateKey>),
    Agent,
}

impl Login {
    fn prepare(site: &Site, secret: &str, address: &str) -> Result<Login, RemoteError> {
        Ok(match &site.auth {
            Auth::Password => Login::Password(secret.to_owned()),
            Auth::Key { path } => Login::Key(Arc::new(load_key(path, secret, address)?)),
            Auth::Agent => Login::Agent,
        })
    }

    async fn authenticate(
        self,
        ssh: &mut russh::client::Handle<Client>,
        username: &str,
        address: &str,
    ) -> Result<(), RemoteError> {
        let failed = |e: &dyn std::fmt::Display| RemoteError::Connect {
            address: address.to_owned(),
            reason: e.to_string(),
        };

        let accepted = match self {
            Login::Password(password) => ssh
                .authenticate_password(username, password)
                .await
                .map_err(|e| failed(&e))?
                .success(),
            Login::Key(key) => {
                let hash = rsa_hash(ssh, key.public_key().algorithm()).await;
                ssh.authenticate_publickey(username, PrivateKeyWithHashAlg::new(key, hash))
                    .await
                    .map_err(|e| failed(&e))?
                    .success()
            }
            Login::Agent => {
                let mut agent = agent().await.map_err(|reason| RemoteError::Connect {
                    address: address.to_owned(),
                    reason,
                })?;
                let identities = agent
                    .request_identities()
                    .await
                    .map_err(|e| failed(&format!("the SSH agent did not answer: {e}")))?;
                if identities.is_empty() {
                    return Err(failed(&"the SSH agent holds no keys — add one with ssh-add"));
                }

                // Offered one at a time, as OpenSSH does. A server counts each
                // as an attempt, so an agent holding more keys than the
                // server's MaxAuthTries can be cut off before reaching the
                // right one — the same limit `ssh` itself runs into, and the
                // reason a key file is the better choice for that setup.
                let mut accepted = false;
                for identity in identities {
                    let outcome = match identity {
                        AgentIdentity::PublicKey { key, .. } => {
                            let hash = rsa_hash(ssh, key.algorithm()).await;
                            ssh.authenticate_publickey_with(username, key, hash, &mut agent)
                                .await
                        }
                        AgentIdentity::Certificate { certificate, .. } => {
                            let hash = rsa_hash(ssh, certificate.algorithm()).await;
                            ssh.authenticate_certificate_with(
                                username,
                                certificate,
                                hash,
                                &mut agent,
                            )
                            .await
                        }
                    };
                    match outcome {
                        Ok(result) if result.success() => {
                            accepted = true;
                            break;
                        }
                        Ok(_) => {}
                        // An agent that refuses to sign — a hardware key nobody
                        // touched, a confirmation declined — is one identity
                        // failing, not the whole login.
                        Err(e) => tracing::debug!(%address, "agent identity not usable: {e}"),
                    }
                }
                accepted
            }
        };

        if accepted {
            Ok(())
        } else {
            Err(RemoteError::Auth { address: address.to_owned() })
        }
    }
}

/// Which signature hash to use with an RSA key, or `None` for every other kind.
///
/// Only asked for RSA, because asking can wait up to a second for the server's
/// extension list. A server that sends no list is offered SHA-256 anyway:
/// OpenSSH has refused SHA-1 RSA signatures by default since 8.8, so falling
/// back to it would fail against exactly the servers most likely to be running.
async fn rsa_hash(
    ssh: &russh::client::Handle<Client>,
    algorithm: russh::keys::Algorithm,
) -> Option<HashAlg> {
    if !algorithm.is_rsa() {
        return None;
    }
    match ssh.best_supported_rsa_hash().await {
        Ok(Some(hash)) => hash,
        Ok(None) | Err(_) => Some(HashAlg::Sha256),
    }
}

type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

/// The running SSH agent, wherever this platform keeps it.
#[cfg(unix)]
async fn agent() -> Result<Agent, String> {
    match AgentClient::connect_env().await {
        Ok(agent) => Ok(agent.dynamic()),
        Err(russh::keys::Error::EnvVar(_)) => {
            Err("no SSH agent is running (SSH_AUTH_SOCK is not set)".to_owned())
        }
        Err(e) => Err(format!("could not reach the SSH agent: {e}")),
    }
}

/// The running SSH agent, wherever this platform keeps it.
///
/// Windows has two in common use: the OpenSSH agent service, on a named pipe,
/// and PuTTY's Pageant. `SSH_AUTH_SOCK` is honoured first for anyone who has
/// pointed it at a pipe of their own, then OpenSSH's well-known pipe, then
/// Pageant.
#[cfg(windows)]
async fn agent() -> Result<Agent, String> {
    if let Ok(pipe) = std::env::var("SSH_AUTH_SOCK") {
        if let Ok(agent) = AgentClient::connect_named_pipe(pipe).await {
            return Ok(agent.dynamic());
        }
    }
    if let Ok(agent) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        return Ok(agent.dynamic());
    }
    match AgentClient::connect_pageant().await {
        Ok(agent) => Ok(agent.dynamic()),
        Err(_) => Err(
            "no SSH agent is running — start the OpenSSH Authentication Agent service or Pageant"
                .to_owned(),
        ),
    }
}

/// Reads a private key file, decrypting it with `passphrase` if it needs one.
///
/// An unencrypted key is tried without the passphrase first: a site that
/// switched from a password to a key may still have the old password saved,
/// and some key formats refuse to parse when handed a passphrase they do not
/// need.
fn load_key(path: &str, passphrase: &str, address: &str) -> Result<PrivateKey, RemoteError> {
    let unusable = |reason: String| RemoteError::Key {
        path: path.to_owned(),
        reason,
    };
    let text = std::fs::read_to_string(expand_home(path)).map_err(|e| unusable(e.to_string()))?;

    let first = match russh::keys::decode_secret_key(&text, None) {
        Ok(key) => return Ok(key),
        Err(e) => e,
    };
    let encrypted = matches!(first, russh::keys::Error::KeyIsEncrypted)
        || text.contains("ENCRYPTED")
        || text.contains("Encryption: aes");

    if passphrase.is_empty() {
        return Err(if encrypted {
            // The same error as a missing password, because the fix is the
            // same: open the site and type the secret in.
            RemoteError::NoPassword { address: address.to_owned() }
        } else {
            unusable(first.to_string())
        });
    }
    russh::keys::decode_secret_key(&text, Some(passphrase)).map_err(|e| {
        unusable(if encrypted {
            "the saved passphrase does not decrypt it".to_owned()
        } else {
            e.to_string()
        })
    })
}

/// `~/.ssh/id_ed25519` → the real path. Key paths are written this way by
/// hand far more often than not, and the OS will not expand them.
fn expand_home(path: &str) -> std::path::PathBuf {
    let rest = match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => rest,
        _ => return path.into(),
    };
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    match home {
        Some(home) => {
            let mut full = std::path::PathBuf::from(home);
            let rest = rest.trim_start_matches(['/', '\\']);
            if !rest.is_empty() {
                full.push(rest);
            }
            full
        }
        None => path.into(),
    }
}

impl Transport for SftpTransport {
    fn list(
        &mut self,
        path: &str,
        refused: &mut Vec<String>,
    ) -> Result<Vec<RemoteEntry>, RemoteError> {
        let path = path::normalize(path);
        let dir_path = sftp_dir_path(&path);
        let address = self.address.clone();

        let read = self
            .runtime
            .block_on(self.session.read_dir(dir_path))
            .map_err(|e| RemoteError::Server {
                path: path.clone(),
                reason: describe(&address, &e),
            })?;

        let mut entries = Vec::new();
        for entry in read {
            let name = entry.file_name();
            // Every directory has these and they are not interesting. Dropped
            // before the name check so they are not reported as refusals.
            if name == "." || name == ".." {
                continue;
            }
            if let Err(refusal) = path::safe_name(&name) {
                tracing::warn!(%path, "{refusal}");
                refused.push(refusal.to_string());
                continue;
            }

            let metadata = entry.metadata();
            let kind = if metadata.is_dir() {
                EntryKind::Directory
            } else if metadata.is_symlink() {
                EntryKind::Symlink
            } else {
                EntryKind::File
            };

            entries.push(RemoteEntry {
                name,
                kind,
                size: metadata.size.unwrap_or(0),
                // SFTP reports whole seconds; Neutron stores milliseconds.
                modified: metadata.mtime.map(|t| t as i64 * 1000).unwrap_or(0),
            });
        }

        Ok(entries)
    }


    fn create_dir(&mut self, path: &str) -> Result<(), RemoteError> {
        let path = path::normalize(path);
        let address = self.address.clone();
        self.runtime
            .block_on(self.session.create_dir(path.clone()))
            .map_err(|e| RemoteError::Server {
                path,
                reason: describe(&address, &e),
            })
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), RemoteError> {
        let (from, to) = (path::normalize(from), path::normalize(to));
        let address = self.address.clone();
        self.runtime
            .block_on(self.session.rename(from, to.clone()))
            .map_err(|e| RemoteError::Server {
                // The destination: a rename fails because of where it was
                // going far more often than because of where it came from.
                path: to,
                reason: describe(&address, &e),
            })
    }

    fn remove_file(&mut self, path: &str) -> Result<(), RemoteError> {
        let path = path::normalize(path);
        let address = self.address.clone();
        self.runtime
            .block_on(self.session.remove_file(path.clone()))
            .map_err(|e| RemoteError::Server {
                path,
                reason: describe(&address, &e),
            })
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), RemoteError> {
        let path = path::normalize(path);
        let address = self.address.clone();
        self.runtime
            .block_on(self.session.remove_dir(path.clone()))
            .map_err(|e| RemoteError::Server {
                path,
                reason: describe(&address, &e),
            })
    }

    fn upload(
        &mut self,
        path: &str,
        source: &mut dyn Read,
        on_progress: &mut dyn FnMut(u64) -> Continue,
    ) -> Result<u64, RemoteError> {
        let path = path::normalize(path);
        let address = self.address.clone();

        // `create` truncates, which is what replacing a file means. Opening for
        // append and hoping would leave the tail of whatever was there before
        // stuck on the end of the new file.
        let mut file = self
            .runtime
            .block_on(self.session.create(path.clone()))
            .map_err(|e| RemoteError::Server {
                path: path.clone(),
                reason: describe(&address, &e),
            })?;

        let mut buffer = vec![0u8; CHUNK];
        let mut total = 0u64;

        loop {
            let read = source.read(&mut buffer).map_err(RemoteError::Io)?;
            if read == 0 {
                break;
            }
            self.runtime
                .block_on(file.write_all(&buffer[..read]))
                .map_err(RemoteError::Io)?;
            total += read as u64;
            if on_progress(total) == Continue::Stop {
                return Err(RemoteError::Cancelled);
            }
        }

        // Flushed before success is reported, so a write error on the last
        // buffer is not announced as a completed upload.
        self.runtime
            .block_on(file.flush())
            .map_err(RemoteError::Io)?;
        Ok(total)
    }

    fn download(
        &mut self,
        path: &str,
        sink: &mut dyn Write,
        on_progress: &mut dyn FnMut(u64) -> Continue,
    ) -> Result<u64, RemoteError> {
        let path = path::normalize(path);
        let address = self.address.clone();

        let mut file = self
            .runtime
            .block_on(self.session.open(path.clone()))
            .map_err(|e| RemoteError::Server {
                path: path.clone(),
                reason: describe(&address, &e),
            })?;

        // Rewind rather than trusting the handle's initial position: an
        // implementation that opened for append would otherwise read nothing
        // and report a zero-byte success.
        self.runtime
            .block_on(file.seek(std::io::SeekFrom::Start(0)))
            .map_err(RemoteError::Io)?;

        let mut buffer = vec![0u8; CHUNK];
        let mut total = 0u64;

        loop {
            let read = self
                .runtime
                .block_on(file.read(&mut buffer))
                .map_err(RemoteError::Io)?;
            if read == 0 {
                break;
            }
            sink.write_all(&buffer[..read])?;
            total += read as u64;
            if on_progress(total) == Continue::Stop {
                return Err(RemoteError::Cancelled);
            }
        }

        Ok(total)
    }

    fn new_fingerprint(&self) -> Option<&str> {
        self.new_fingerprint.as_deref()
    }

    fn volumes(&mut self) -> Result<Vec<RemoteVolume>, RemoteError> {
        let posix_cmd = r#"sh -c 'sys=$(uname -s 2>/dev/null); if [ "$sys" = "Darwin" ] || [ "$sys" = "Linux" ]; then df -kP / 2>/dev/null; for v in /Volumes/* /mnt/* /media/*/*; do [ -d "$v" ] && [ ! -L "$v" ] && df -kP "$v" 2>/dev/null; done; fi; true'"#;
        let stdout = self
            .exec_command(posix_cmd, std::time::Duration::from_secs(6))
            .unwrap_or_default();
        let posix_vols = parse_df_volumes(&stdout);
        if !posix_vols.is_empty() {
            return Ok(posix_vols);
        }

        // Windows OpenSSH hosts default to cmd.exe and report drive letters via PowerShell.
        let win_cmd = r#"powershell -NoProfile -NonInteractive -Command "Get-CimInstance Win32_LogicalDisk | Where-Object { $_.Size -gt 0 } | ForEach-Object { '{0}|{1}|{2}|{3}|{4}' -f $_.DeviceID, $_.VolumeName, $_.DriveType, $_.Size, $_.FreeSpace }""#;
        let win_out = self.exec_command(win_cmd, std::time::Duration::from_secs(10))?;
        Ok(parse_windows_volumes(&win_out))
    }
}

impl SftpTransport {
    fn exec_command(&mut self, cmd: &str, timeout: std::time::Duration) -> Result<String, RemoteError> {
        let address = self.address.clone();
        let timeout_address = self.address.clone();
        let ssh = &self._ssh;
        let cmd = cmd.to_owned();

        self.runtime.block_on(async move {
            tokio::time::timeout(timeout, async move {
                let mut channel = ssh.channel_open_session().await.map_err(|e| RemoteError::Connect {
                    address: address.clone(),
                    reason: e.to_string(),
                })?;
                channel.exec(true, cmd.as_bytes()).await.map_err(|e| RemoteError::Connect {
                    address: address.clone(),
                    reason: e.to_string(),
                })?;
                let mut out = Vec::new();
                while let Some(msg) = channel.wait().await {
                    match msg {
                        russh::ChannelMsg::Data { data } => out.extend_from_slice(&data),
                        russh::ChannelMsg::Eof | russh::ChannelMsg::Close => break,
                        _ => {}
                    }
                }
                Ok::<String, RemoteError>(String::from_utf8_lossy(&out).into_owned())
            })
            .await
            .map_err(|_| RemoteError::Connect {
                address: timeout_address,
                reason: "timed out querying remote host".to_owned(),
            })?
        })
    }
}

/// Appends `/` to normalized Windows SFTP drive roots like `"/C:"` -> `"/C:/"`
/// because Windows OpenSSH expects a trailing slash to list a drive's root.
pub(crate) fn sftp_dir_path(normalized: &str) -> String {
    let bytes = normalized.as_bytes();
    if bytes.len() == 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
    {
        format!("{normalized}/")
    } else {
        normalized.to_owned()
    }
}

/// Parses POSIX `df -kP` lines into sorted, deduplicated remote volumes,
/// skipping OS virtual/recovery mounts.
pub(crate) fn parse_df_volumes(stdout: &str) -> Vec<RemoteVolume> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("Filesystem") {
            continue;
        }
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        let Some(pct_idx) = tokens.iter().position(|t| t.ends_with('%')) else {
            continue;
        };
        if pct_idx < 4 || pct_idx + 1 >= tokens.len() {
            continue;
        }
        let Ok(blocks_1k) = tokens[pct_idx - 3].parse::<u64>() else {
            continue;
        };
        let Ok(avail_1k) = tokens[pct_idx - 1].parse::<u64>() else {
            continue;
        };
        // Rejoin mount path from the original slice so spaces inside volume
        // names are preserved without collapsing or losing trailing parts.
        let mount = tokens[pct_idx + 1..].join(" ");
        if mount.is_empty() || !seen.insert(mount.clone()) {
            continue;
        }
        if mount.starts_with("/System/Volumes")
            || mount.starts_with("/Volumes/Recovery")
            || mount.starts_with("/dev")
            || mount.starts_with("/private")
            || mount.starts_with("/mnt/wsl")
        {
            continue;
        }
        if blocks_1k == 0 {
            continue;
        }

        let name = if mount == "/" {
            "Internal SSD".to_owned()
        } else if let Some(rest) = mount.strip_prefix("/Volumes/") {
            if rest.is_empty() {
                continue;
            }
            rest.to_owned()
        } else {
            mount
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or(&mount)
                .to_owned()
        };

        out.push(RemoteVolume {
            name,
            path: mount,
            total: blocks_1k.saturating_mul(1024),
            free: avail_1k.saturating_mul(1024),
        });
    }

    out.sort_by(|a, b| match (a.path == "/", b.path == "/") {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    out
}

/// Parses pipe-delimited `Win32_LogicalDisk` lines into sorted remote volumes.
pub(crate) fn parse_windows_volumes(stdout: &str) -> Vec<RemoteVolume> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.split('|').collect();
        if parts.len() != 5 {
            continue;
        }
        let device = parts[0].trim();
        let bytes = device.as_bytes();
        if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
            continue;
        }
        let upper = device.to_ascii_uppercase();
        if !seen.insert(upper.clone()) {
            continue;
        }
        let label = parts[1].trim();
        let dtype = parts[2].trim().parse::<u32>().unwrap_or(0);
        if dtype == 5 {
            // Optical / CD-ROM drive
            continue;
        }
        let Ok(total) = parts[3].trim().parse::<u64>() else {
            continue;
        };
        if total == 0 {
            continue;
        }
        let free = parts[4].trim().parse::<u64>().unwrap_or(0);

        let name = if !label.is_empty() {
            format!("{label} ({upper})")
        } else if upper == "C:" {
            "Local Disk (C:)".to_owned()
        } else if dtype == 2 {
            format!("External Drive ({upper})")
        } else {
            format!("Drive ({upper})")
        };

        out.push(RemoteVolume {
            name,
            path: format!("/{upper}/"),
            total,
            free,
        });
    }

    out.sort_by(|a, b| match (a.path == "/C:/", b.path == "/C:/") {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.path.cmp(&b.path),
    });
    out
}

/// Turns an SFTP status into something worth reading.
///
/// The protocol's own codes are the useful part — "no such file" and
/// "permission denied" are different problems with different fixes — and the
/// library's Display is already those words.
fn describe(address: &str, error: &russh_sftp::client::error::Error) -> String {
    let text = error.to_string();
    if text.trim().is_empty() {
        format!("{address} refused the request")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use crate::site::{Protocol, Site};

    fn site() -> Site {
        Site {
            id: "nas".into(),
            label: String::new(),
            protocol: Protocol::Sftp,
            host: "127.0.0.1".into(),
            port: Some(1),
            username: String::new(),
            path: None,
            fingerprint: None,
            auth: Default::default(),
            trusted_cert: None,
        }
    }

    #[test]
    fn a_site_with_no_username_is_refused_before_a_connection_is_attempted() {
        // SSH has no anonymous mode. Dialling out to fail is slower and tells
        // the user less than saying so.
        let Err(error) = super::SftpTransport::connect(&site(), "") else {
            panic!("SSH has no anonymous mode");
        };
        assert!(
            matches!(error, crate::transport::RemoteError::Auth { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn parse_df_volumes_extracts_internal_and_external_ssds_and_skips_recovery() {
        let sample = "Filesystem     1024-blocks      Used Available Capacity  Mounted on
/dev/disk3s1s1   239311296  13433660  33790060    29%    /
Filesystem     1024-blocks      Used Available Capacity  Mounted on
/dev/disk4s2     234216576 128555520 105661056    55%    /Volumes/MAC-ARCHIVE
/dev/disk3s3     239311296   1938692  33790060     6%    /Volumes/Recovery
/dev/disk5s1     100000000  20000000  80000000    20%    /Volumes/My Backup Drive
";
        let vols = super::parse_df_volumes(sample);
        assert_eq!(vols.len(), 3);
        assert_eq!(vols[0].name, "Internal SSD");
        assert_eq!(vols[0].path, "/");
        assert_eq!(vols[0].total, 239311296 * 1024);
        assert_eq!(vols[0].free, 33790060 * 1024);

        assert_eq!(vols[1].name, "MAC-ARCHIVE");
        assert_eq!(vols[1].path, "/Volumes/MAC-ARCHIVE");

        assert_eq!(vols[2].name, "My Backup Drive");
        assert_eq!(vols[2].path, "/Volumes/My Backup Drive");
    }

    #[test]
    fn parse_windows_volumes_formats_internal_and_external_drives() {
        let sample = "D:|FastArchive|3|4000787030016|2100000000000
C:||3|1999299932160|199216619520
E:||2|500107862016|310000000000
F:|Disc|5|700000000|0
";
        let vols = super::parse_windows_volumes(sample);
        assert_eq!(vols.len(), 3);
        assert_eq!(vols[0].name, "Local Disk (C:)");
        assert_eq!(vols[0].path, "/C:/");
        assert_eq!(vols[0].total, 1999299932160);
        assert_eq!(vols[0].free, 199216619520);

        assert_eq!(vols[1].name, "FastArchive (D:)");
        assert_eq!(vols[1].path, "/D:/");

        assert_eq!(vols[2].name, "External Drive (E:)");
        assert_eq!(vols[2].path, "/E:/");
    }

    #[test]
    fn sftp_dir_path_appends_slash_only_for_windows_drive_roots() {
        assert_eq!(super::sftp_dir_path("/C:"), "/C:/");
        assert_eq!(super::sftp_dir_path("/d:"), "/d:/");
        assert_eq!(super::sftp_dir_path("/C:/Users"), "/C:/Users");
        assert_eq!(super::sftp_dir_path("/Users/razeen"), "/Users/razeen");
        assert_eq!(super::sftp_dir_path("/"), "/");
    }
}
