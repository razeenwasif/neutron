//! Where Google client credentials come from when nobody set an environment
//! variable.
//!
//! # Why a file is needed at all
//!
//! Environment variables only reach a process that inherits them, which means a
//! shell. Neutron is normally started from the taskbar, the Start menu, or by
//! Explorer opening a folder — none of which inherit anything. So a build that
//! reads credentials only from the environment has a Drive integration that
//! works exclusively when launched from a terminal, which is nobody's normal
//! way of opening a file manager.
//!
//! # Precedence
//!
//! Environment first, then the file. That order matters for development: it
//! lets a different client be pointed at without editing the file the installed
//! application reads.
//!
//! # Not in the repository
//!
//! The file lives beside the application's other state in `%APPDATA%\Neutron`,
//! not in the source tree, so it cannot be committed by accident.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Google OAuth client credentials.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoogleConfig {
    pub client_id: String,
    /// Required by Google for installed-app clients even under PKCE. Not a
    /// secret in the cryptographic sense — it ships in every copy of any
    /// desktop application — but still worth keeping out of a repository.
    pub client_secret: String,
}

impl GoogleConfig {
    pub fn is_complete(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }
}

/// `%APPDATA%\Neutron\google.json`, beside the workspace state eframe writes.
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("Neutron").join("google.json"))
}

/// Reads the stored credentials, if the file exists and parses.
///
/// A malformed file is a warning rather than an error: the application still
/// starts, Drive reports itself unconfigured, and the log says why.
pub fn load() -> Option<GoogleConfig> {
    let path = config_path()?;
    let text = std::fs::read_to_string(&path).ok()?;

    match serde_json::from_str::<GoogleConfig>(&text) {
        Ok(config) if config.is_complete() => Some(config),
        Ok(_) => {
            tracing::warn!(path = %path.display(), "google.json is missing a field");
            None
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), "google.json could not be read: {e}");
            None
        }
    }
}

/// Writes the credentials, creating the directory if needed.
pub fn save(config: &GoogleConfig) -> std::io::Result<PathBuf> {
    let path = config_path().ok_or_else(|| {
        std::io::Error::other("no application data directory")
    })?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(config).map_err(std::io::Error::other)?;
    std::fs::write(&path, json)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_needs_both_halves() {
        // A half-filled file is the likely result of someone pasting one value
        // and meaning to come back. Treating it as configured sends them
        // through consent to a rejection.
        assert!(!GoogleConfig::default().is_complete());
        assert!(
            !GoogleConfig {
                client_id: "x".into(),
                client_secret: String::new(),
            }
            .is_complete()
        );
        assert!(
            GoogleConfig {
                client_id: "x".into(),
                client_secret: "y".into(),
            }
            .is_complete()
        );
    }

    #[test]
    fn whitespace_does_not_count_as_configured() {
        assert!(
            !GoogleConfig {
                client_id: "  ".into(),
                client_secret: "\n".into(),
            }
            .is_complete()
        );
    }

    #[test]
    fn the_config_lives_beside_the_other_application_state() {
        // Not in the source tree, so it cannot be committed by accident.
        let path = config_path().expect("a config path");
        assert!(path.ends_with("Neutron/google.json") || path.ends_with("Neutron\\google.json"));
    }

    #[test]
    fn a_config_round_trips_through_json() {
        let config = GoogleConfig {
            client_id: "123-abc.apps.googleusercontent.com".into(),
            client_secret: "GOCSPX-example".into(),
        };
        let text = serde_json::to_string(&config).unwrap();
        let back: GoogleConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back.client_id, config.client_id);
        assert_eq!(back.client_secret, config.client_secret);
    }
}
