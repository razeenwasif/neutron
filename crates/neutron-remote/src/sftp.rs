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

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use neutron_core::entry::EntryKind;
use russh::keys::PublicKeyOrCertificate;
use russh::keys::HashAlg;
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::hostkey::{self, Decision};
use crate::path;
use crate::site::Site;
use crate::transport::{CHUNK, CONNECT_TIMEOUT, Continue, RemoteEntry, RemoteError, Transport};

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
        let password = password.to_owned();
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

            let authenticated = ssh
                .authenticate_password(&username, &password)
                .await
                .map_err(|e| RemoteError::Connect {
                    address: address_for_task.clone(),
                    reason: e.to_string(),
                })?;
            if !authenticated.success() {
                return Err(RemoteError::Auth { address: address_for_task.clone() });
            }

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

impl Transport for SftpTransport {
    fn list(
        &mut self,
        path: &str,
        refused: &mut Vec<String>,
    ) -> Result<Vec<RemoteEntry>, RemoteError> {
        let path = path::normalize(path);
        let address = self.address.clone();

        let read = self
            .runtime
            .block_on(self.session.read_dir(path.clone()))
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
}
