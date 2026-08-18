//! Storing the Google refresh token in Windows Credential Manager.
//!
//! # Why not a file
//!
//! A refresh token is a long-lived key to the user's Drive. On disk it is
//! readable by every process running as that user, ends up in backups, and
//! survives in a config file nobody remembers exists. Credential Manager keeps
//! it encrypted under the user's login and scopes it to this account.
//!
//! This is not a strong boundary — anything running as the user can ask for it
//! back, which is exactly what Neutron does. It is the same protection every
//! other desktop client gets, and it is markedly better than plaintext.
//!
//! # Never logged
//!
//! Nothing here writes a token to `tracing`, including in errors. A secret in a
//! log file has escaped whatever protected it.

use crate::oauth::AuthError;

/// Credential Manager target name. Namespaced so it is identifiable in the
/// user's credential list rather than an anonymous blob.
const TARGET: &str = "Neutron/GoogleDrive";

#[cfg(windows)]
mod win32 {
    use super::{AuthError, TARGET};

    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    };
    use windows::core::PWSTR;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Stores the refresh token, replacing any previous one.
    pub fn store(token: &str) -> Result<(), AuthError> {
        let mut target = wide(TARGET);
        let mut blob = token.as_bytes().to_vec();

        let credential = CREDENTIALW {
            Flags: Default::default(),
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            Comment: PWSTR::null(),
            LastWritten: FILETIME::default(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            // LOCAL_MACHINE rather than SESSION: a session-scoped credential is
            // gone at logout, which would mean re-consenting every morning.
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: PWSTR::null(),
            UserName: PWSTR::null(),
        };

        // SAFETY: every pointer in `credential` addresses a buffer that
        // outlives the call, and the sizes match those buffers.
        unsafe { CredWriteW(&credential, 0) }
            .map_err(|e| AuthError::Http(format!("could not save the credential: {}", e.message())))
    }

    /// Reads the stored refresh token, if there is one.
    pub fn load() -> Option<String> {
        let target = wide(TARGET);
        let mut raw = std::ptr::null_mut();

        // SAFETY: `target` is NUL-terminated; on success the out-param owns an
        // allocation freed by CredFree below on every path.
        unsafe { CredReadW(windows::core::PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut raw) }
            .ok()?;
        if raw.is_null() {
            return None;
        }

        // SAFETY: `raw` points at a CREDENTIALW the API just filled in, and the
        // blob is described by its own size field.
        let token = unsafe {
            let credential = &*raw;
            if credential.CredentialBlob.is_null() || credential.CredentialBlobSize == 0 {
                None
            } else {
                let bytes = std::slice::from_raw_parts(
                    credential.CredentialBlob,
                    credential.CredentialBlobSize as usize,
                );
                // Lossy rather than failing: a corrupt blob should send the
                // user through consent again, not return an error they cannot
                // act on. The exchange will reject it either way.
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
        };

        // SAFETY: allocated by CredReadW and not used afterwards.
        unsafe { CredFree(raw as *const _) };
        token.filter(|t| !t.is_empty())
    }

    /// Removes the stored token — signing out.
    pub fn clear() -> Result<(), AuthError> {
        let target = wide(TARGET);
        // SAFETY: `target` is NUL-terminated.
        unsafe { CredDeleteW(windows::core::PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) }
            .map_err(|e| {
                AuthError::Http(format!("could not remove the credential: {}", e.message()))
            })
    }
}

#[cfg(not(windows))]
mod win32 {
    use super::AuthError;

    pub fn store(_token: &str) -> Result<(), AuthError> {
        Err(AuthError::Http(
            "credential storage is Windows-only".to_owned(),
        ))
    }
    pub fn load() -> Option<String> {
        None
    }
    pub fn clear() -> Result<(), AuthError> {
        Ok(())
    }
}

pub use win32::{clear, load, store};

#[cfg(test)]
mod tests {
    #[test]
    fn the_target_name_is_identifiable_in_the_credential_list() {
        // Users do look through Credential Manager. An anonymous entry is
        // something they cannot reason about and may delete at random.
        assert!(super::TARGET.starts_with("Neutron/"));
    }
}
