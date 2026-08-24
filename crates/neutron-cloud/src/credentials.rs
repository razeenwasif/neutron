//! The Google refresh token, in Windows Credential Manager.
//!
//! The storage itself lives in [`neutron_shell::credentials`], which is shared
//! with the saved-server passwords in `neutron-remote`; this module is only the
//! Drive-specific target name and error type.
//!
//! # Never logged
//!
//! Nothing here writes a token to `tracing`, including in errors. A secret in a
//! log file has escaped whatever protected it.

use crate::oauth::AuthError;

/// Credential Manager target name. Namespaced so it is identifiable in the
/// user's credential list rather than an anonymous blob.
const TARGET: &str = "Neutron/GoogleDrive";

/// Stores the refresh token, replacing any previous one.
pub fn store(token: &str) -> Result<(), AuthError> {
    neutron_shell::credentials::store(TARGET, token).map_err(|e| AuthError::Http(e.0))
}

/// Reads the stored refresh token, if there is one.
pub fn load() -> Option<String> {
    neutron_shell::credentials::load(TARGET)
}

/// Removes the stored token — signing out.
pub fn clear() -> Result<(), AuthError> {
    neutron_shell::credentials::clear(TARGET).map_err(|e| AuthError::Http(e.0))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_target_name_is_identifiable_in_the_credential_list() {
        // Users do look through Credential Manager. An anonymous entry is
        // something they cannot reason about and may delete at random.
        assert!(super::TARGET.starts_with("Neutron/"));
    }
}
