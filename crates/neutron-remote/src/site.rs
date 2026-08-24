//! Saved servers: `%APPDATA%\Neutron\sites.json`.
//!
//! ```json
//! {
//!   "sites": [
//!     {
//!       "id": "nas",
//!       "label": "Home NAS",
//!       "protocol": "sftp",
//!       "host": "nas.local",
//!       "port": 22,
//!       "username": "razeen",
//!       "path": "/volume1/media"
//!     }
//!   ]
//! }
//! ```
//!
//! # No passwords here
//!
//! The file holds everything except the secret. Passwords live in Windows
//! Credential Manager under `Neutron/Site/<id>`, for the same reasons the Drive
//! refresh token does: a password in a config file is readable by every process
//! running as the user, ends up in backups, and outlives whatever anyone
//! remembers about it.
//!
//! This is not a strong boundary — Neutron can read the credential back, so
//! anything running as the user can too. It is the protection every other
//! desktop client gets, and it is markedly better than plaintext.
//!
//! # Why a file rather than a dialog first
//!
//! Both, eventually; the file is what makes the dialog testable and what lets a
//! site be inspected, edited, diffed and copied between machines. A connection
//! that exists only inside a GUI is one nobody can back up.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How to talk to a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// Plain FTP. Everything, including the password, crosses the network in
    /// the clear — offered because it is what a great many appliances speak,
    /// and named plainly so nobody chooses it by accident.
    Ftp,
    /// FTP over TLS (explicit `AUTH TLS`), which is what "FTPS" means in
    /// practice.
    Ftps,
    /// SFTP: a subsystem of SSH, and not related to FTP at all despite the name.
    Sftp,
}

impl Protocol {
    /// The port used when the site does not name one.
    pub fn default_port(self) -> u16 {
        match self {
            // FTPS here is *explicit* — it starts as FTP on 21 and upgrades.
            // Implicit FTPS on 990 is a different, deprecated thing.
            Protocol::Ftp | Protocol::Ftps => 21,
            Protocol::Sftp => 22,
        }
    }

    /// Whether the password crosses the network in the clear.
    ///
    /// Used to warn, not to refuse. Plenty of these servers are on a LAN and
    /// the user knows exactly what they have.
    pub fn is_cleartext(self) -> bool {
        matches!(self, Protocol::Ftp)
    }

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Ftp => "FTP",
            Protocol::Ftps => "FTPS",
            Protocol::Sftp => "SFTP",
        }
    }
}

/// One saved server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Site {
    /// Slug, unique across sites. Names the credential and identifies the site
    /// in a saved workspace, so it must survive being renamed in the UI —
    /// which is why the label is separate.
    pub id: String,
    /// What the sidebar shows. Defaults to the host when absent.
    #[serde(default)]
    pub label: String,
    pub protocol: Protocol,
    pub host: String,
    /// Absent means [`Protocol::default_port`].
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub username: String,
    /// Where browsing starts. Defaults to the root.
    #[serde(default)]
    pub path: Option<String>,
    /// The server's SSH host key fingerprint, once it has been seen.
    ///
    /// Recorded on the first connection and checked on every one after, which
    /// is what makes a swapped server a hard failure rather than a silent
    /// success. SFTP only. See [`crate::known_hosts`].
    #[serde(default)]
    pub fingerprint: Option<String>,
}

impl Site {
    pub fn port(&self) -> u16 {
        self.port.unwrap_or_else(|| self.protocol.default_port())
    }

    /// Where browsing starts.
    pub fn start_path(&self) -> String {
        crate::path::normalize(self.path.as_deref().unwrap_or("/"))
    }

    /// `user@host:port`, for logs and error messages. Never a password.
    pub fn address(&self) -> String {
        let port = self.port();
        if self.username.is_empty() {
            format!("{}:{port}", self.host)
        } else {
            format!("{}@{}:{port}", self.username, self.host)
        }
    }

    /// The Credential Manager target holding this site's password.
    pub fn credential_target(&self) -> String {
        format!("Neutron/Site/{}", self.id)
    }

    /// What the sidebar shows.
    pub fn display(&self) -> &str {
        if self.label.is_empty() {
            &self.host
        } else {
            &self.label
        }
    }
}

/// Why a site was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error("{field} is empty")]
    Empty { field: &'static str },
    #[error("id may only contain a-z, 0-9, '-', '_' and '.'")]
    IdNotASlug,
    #[error("{field} contains a space or a control character")]
    NotAHostname { field: &'static str },
    #[error("port 0 is not a port")]
    ZeroPort,
    #[error("two sites share the id {id:?}")]
    DuplicateId { id: String },
}

/// Longest id accepted. It ends up in a credential name and in a saved layout.
const MAX_ID: usize = 40;

/// Everything saved, as read from disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sites {
    #[serde(default)]
    pub sites: Vec<Site>,
}

impl Sites {
    pub fn get(&self, id: &str) -> Option<&Site> {
        self.sites.iter().find(|s| s.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }
}

/// `%APPDATA%\Neutron\sites.json`, beside the application's other state.
///
/// The fallbacks match the rest of the application's config handling and are
/// what let this be exercised on Linux.
pub fn sites_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("Neutron").join("sites.json"))
}

/// Reads and validates the saved sites.
///
/// A missing file is the normal case for someone with no servers, and is not an
/// error. A file that will not parse *is*, and the reason is returned so it can
/// be shown — a site that silently fails to appear is unanswerable.
pub fn load() -> Result<Sites, String> {
    let Some(path) = sites_path() else {
        return Ok(Sites::default());
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Sites::default()),
        Err(e) => Err(format!("{} could not be read: {e}", path.display())),
    }
}

/// Writes the sites file, creating the directory if needed.
pub fn save(sites: &Sites) -> Result<PathBuf, String> {
    let path = sites_path().ok_or_else(|| "no application data directory".to_owned())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(sites).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Parses and validates a sites file.
pub fn parse(text: &str) -> Result<Sites, String> {
    let mut sites: Sites = serde_json::from_str(text).map_err(|e| e.to_string())?;
    for i in 0..sites.sites.len() {
        validate(&mut sites.sites[i]).map_err(|e| format!("{}: {e}", sites.sites[i].id))?;
        if let Some(dup) = sites.sites[..i].iter().find(|s| s.id == sites.sites[i].id) {
            return Err(Invalid::DuplicateId { id: dup.id.clone() }.to_string());
        }
    }
    Ok(sites)
}

/// Checks and normalises one site.
pub fn validate(site: &mut Site) -> Result<(), Invalid> {
    if site.id.is_empty() {
        return Err(Invalid::Empty { field: "id" });
    }
    if site.id.len() > MAX_ID
        || !site
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
    {
        return Err(Invalid::IdNotASlug);
    }

    site.host = site.host.trim().to_owned();
    if site.host.is_empty() {
        return Err(Invalid::Empty { field: "host" });
    }
    // A space or a control character in a hostname is either a typo or an
    // attempt to smuggle something into a command line or a log.
    if site
        .host
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(Invalid::NotAHostname { field: "host" });
    }
    if site.username.chars().any(|c| c.is_control()) {
        return Err(Invalid::NotAHostname { field: "username" });
    }
    if site.port == Some(0) {
        return Err(Invalid::ZeroPort);
    }

    site.label = site.label.trim().to_owned();
    site.path = Some(site.start_path());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site() -> Site {
        Site {
            id: "nas".into(),
            label: "Home NAS".into(),
            protocol: Protocol::Sftp,
            host: "nas.local".into(),
            port: None,
            username: "razeen".into(),
            path: Some("/volume1/media".into()),
            fingerprint: None,
        }
    }

    #[test]
    fn a_well_formed_file_parses() {
        let text = r#"{"sites":[{"id":"nas","label":"Home NAS","protocol":"sftp",
                      "host":"nas.local","username":"razeen","path":"/volume1"}]}"#;
        let sites = parse(text).expect("should parse");
        assert_eq!(sites.sites.len(), 1);
        assert_eq!(sites.sites[0].protocol, Protocol::Sftp);
        assert_eq!(sites.sites[0].port(), 22);
    }

    #[test]
    fn an_absent_file_is_an_empty_list_not_an_error() {
        // Someone with no servers is the normal case.
        assert!(parse(r#"{}"#).expect("should parse").is_empty());
    }

    #[test]
    fn default_ports_follow_the_protocol() {
        assert_eq!(Protocol::Ftp.default_port(), 21);
        // Explicit FTPS starts on the FTP port and upgrades; 990 is the
        // separate, deprecated implicit variant.
        assert_eq!(Protocol::Ftps.default_port(), 21);
        assert_eq!(Protocol::Sftp.default_port(), 22);
    }

    #[test]
    fn only_plain_ftp_is_cleartext() {
        assert!(Protocol::Ftp.is_cleartext());
        assert!(!Protocol::Ftps.is_cleartext());
        assert!(!Protocol::Sftp.is_cleartext());
    }

    #[test]
    fn the_credential_target_is_identifiable_and_per_site() {
        // Users do look through Credential Manager, and an anonymous blob is
        // something they cannot reason about and may delete at random.
        let a = site();
        let mut b = site();
        b.id = "other".into();
        assert!(a.credential_target().starts_with("Neutron/Site/"));
        assert_ne!(a.credential_target(), b.credential_target());
    }

    #[test]
    fn the_address_never_contains_a_password() {
        // It goes in logs and in error messages shown on screen. There is no
        // password field on Site at all, which is the real guarantee; this
        // pins the format.
        assert_eq!(site().address(), "razeen@nas.local:22");
    }

    #[test]
    fn a_site_with_no_username_still_has_a_readable_address() {
        let mut s = site();
        s.username.clear();
        assert_eq!(s.address(), "nas.local:22");
    }

    #[test]
    fn the_label_falls_back_to_the_host() {
        let mut s = site();
        s.label.clear();
        assert_eq!(s.display(), "nas.local");
    }

    #[test]
    fn ids_must_be_slugs() {
        let mut s = site();
        s.id = "Home NAS".into();
        assert_eq!(validate(&mut s), Err(Invalid::IdNotASlug));
    }

    #[test]
    fn a_hostname_may_not_contain_a_space_or_a_control_character() {
        let mut s = site();
        s.host = "nas.local evil".into();
        assert!(validate(&mut s).is_err());

        let mut s = site();
        s.host = "nas\nlocal".into();
        assert!(validate(&mut s).is_err());
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let text = r#"{"sites":[
            {"id":"a","protocol":"ftp","host":"one.example"},
            {"id":"a","protocol":"ftp","host":"two.example"}]}"#;
        let err = parse(text).expect_err("should refuse");
        assert!(err.contains('a'), "{err}");
    }

    #[test]
    fn the_start_path_is_normalised() {
        let mut s = site();
        s.path = Some("//volume1//media/".into());
        validate(&mut s).unwrap();
        assert_eq!(s.start_path(), "/volume1/media");
    }

    #[test]
    fn a_site_with_no_path_starts_at_the_root() {
        let mut s = site();
        s.path = None;
        assert_eq!(s.start_path(), "/");
    }

    #[test]
    fn port_zero_is_refused() {
        let mut s = site();
        s.port = Some(0);
        assert_eq!(validate(&mut s), Err(Invalid::ZeroPort));
    }

    #[test]
    fn a_site_round_trips_through_the_file_format() {
        let sites = Sites { sites: vec![site()] };
        let text = serde_json::to_string(&sites).unwrap();
        let back = parse(&text).expect("should parse");
        assert_eq!(back.sites[0], sites.sites[0]);
    }
}
