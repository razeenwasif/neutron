//! Browsing FTP, FTPS and SFTP servers.
//!
//! A *site* is a saved server ([`site::Site`], from `sites.json`). Connecting to
//! one yields a [`transport::Transport`] — a live session that can list a
//! directory and read a file. Everything blocks; nothing here may be called
//! from the UI thread.
//!
//! # Layout
//!
//! | | |
//! |---|---|
//! | [`site`] | saved servers, and where the password is not kept |
//! | [`path`] | POSIX remote paths, and the check that a listed name cannot escape a local folder |
//! | [`hostkey`] | whether to trust an SSH server's key |
//! | [`transport`] | what both protocols look like from outside |
//! | [`ftp`] | FTP and FTPS |
//! | [`sftp`] | SFTP, over SSH |
//!
//! # One connection per session, not per request
//!
//! Both protocols are expensive to open — FTP needs a TCP connection, a login
//! and a `FEAT`; SSH needs a key exchange — and cheap to keep. A connection is
//! made once and reused for every listing and transfer, which is the difference
//! between a folder opening instantly and a folder opening in a second.
//!
//! Reconnection is the caller's job: a session that has gone away comes back as
//! an ordinary error, and the caller decides whether to dial again. Automatic
//! silent reconnection is how a client ends up re-authenticating in a loop
//! against a server that is rate-limiting it.

pub mod ftp;
pub mod hostkey;
pub mod path;
pub mod sftp;
pub mod site;
pub mod transport;

pub use site::{Protocol, Site, Sites};
pub use transport::{Continue, RemoteEntry, RemoteError, Transport};

/// Reads the password saved for a site, if there is one.
///
/// Passwords live in Windows Credential Manager rather than in `sites.json` —
/// see [`site`] for why.
pub fn password_for(site: &Site) -> Option<String> {
    neutron_shell::credentials::load(&site.credential_target())
}

/// Saves a site's password.
pub fn remember_password(site: &Site, password: &str) -> Result<(), String> {
    neutron_shell::credentials::store(&site.credential_target(), password).map_err(|e| e.0)
}

/// Forgets a site's password.
pub fn forget_password(site: &Site) -> Result<(), String> {
    neutron_shell::credentials::clear(&site.credential_target()).map_err(|e| e.0)
}

/// Opens a connection to `site`.
///
/// The password comes from Credential Manager. A site whose protocol needs one
/// and has none saved fails with [`RemoteError::NoPassword`] rather than trying
/// an empty password — some servers count that as a failed login attempt, and a
/// file manager should not be spending the user's lockout budget guessing.
pub fn connect(site: &Site) -> Result<Box<dyn Transport>, RemoteError> {
    let password = password_for(site);

    // Anonymous FTP is a real configuration, and it is the one case where
    // having no password is correct rather than missing.
    let anonymous = site.username.is_empty() && !matches!(site.protocol, Protocol::Sftp);
    let password = match password {
        Some(password) => password,
        None if anonymous => String::new(),
        None => {
            return Err(RemoteError::NoPassword { address: site.address() });
        }
    };

    match site.protocol {
        Protocol::Ftp | Protocol::Ftps => {
            Ok(Box::new(ftp::FtpTransport::connect(site, &password)?))
        }
        Protocol::Sftp => Ok(Box::new(sftp::SftpTransport::connect(site, &password)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(protocol: Protocol, username: &str) -> Site {
        Site {
            id: "s".into(),
            label: String::new(),
            protocol,
            host: "127.0.0.1".into(),
            // Port 1 is not going to be listening, which is the point: these
            // tests are about what happens *before* a connection is attempted.
            port: Some(1),
            username: username.into(),
            path: None,
            fingerprint: None,
        }
    }

    #[test]
    fn a_site_with_no_saved_password_is_not_dialled() {
        // Trying an empty password spends a real login attempt against a server
        // that may be counting them.
        let Err(error) = connect(&site(Protocol::Ftps, "razeen")) else {
            panic!("a site with no saved password should not be dialled");
        };
        assert!(matches!(error, RemoteError::NoPassword { .. }), "{error:?}");
    }

    #[test]
    fn anonymous_ftp_needs_no_saved_password() {
        // No username means anonymous, which is a configuration rather than an
        // omission — so this must get as far as trying to connect and fail on
        // the network instead.
        let Err(error) = connect(&site(Protocol::Ftp, "")) else {
            panic!("nothing is listening on port 1");
        };
        assert!(matches!(error, RemoteError::Connect { .. }), "{error:?}");
    }

    #[test]
    fn sftp_without_a_username_is_never_anonymous() {
        // SSH has no anonymous mode, so an empty username is a mistake, not a
        // request to connect as nobody.
        let Err(error) = connect(&site(Protocol::Sftp, "")) else {
            panic!("SSH has no anonymous mode");
        };
        assert!(
            matches!(error, RemoteError::NoPassword { .. } | RemoteError::Auth { .. }),
            "{error:?}"
        );
    }
}
