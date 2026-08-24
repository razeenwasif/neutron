//! Secrets in Windows Credential Manager, keyed by target name.
//!
//! # Why not a file
//!
//! Everything stored through here is a long-lived key to something of the
//! user's — a Drive refresh token, an FTP password. On disk it is readable by
//! every process running as that user, ends up in backups, and survives in a
//! config file nobody remembers exists. Credential Manager keeps it encrypted
//! under the user's login and scoped to this account.
//!
//! This is not a strong boundary: anything running as the user can ask for it
//! back, which is exactly what Neutron does. It is the same protection every
//! other desktop client gets, and markedly better than plaintext.
//!
//! # Target names
//!
//! Callers pass their own, and every one starts `Neutron/` so it is
//! identifiable in the user's credential list rather than an anonymous blob.
//! Users do look through that list, and an entry they cannot reason about is
//! one they may delete at random.
//!
//! # Never logged
//!
//! Nothing here writes a secret to `tracing`, including in errors. A secret in
//! a log file has escaped whatever was protecting it.

/// Why a credential operation failed. A string, because there is nothing a
/// caller can do differently for one Win32 error over another — it is shown.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CredentialError(pub String);

#[cfg(windows)]
mod win32 {
    use super::CredentialError;

    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    };
    use windows::core::PWSTR;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Stores `secret` under `target`, replacing any previous value.
    pub fn store(target: &str, secret: &str) -> Result<(), CredentialError> {
        let mut target = wide(target);
        let mut blob = secret.as_bytes().to_vec();

        let credential = CREDENTIALW {
            Flags: Default::default(),
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            Comment: PWSTR::null(),
            LastWritten: FILETIME::default(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            // LOCAL_MACHINE rather than SESSION: a session-scoped credential is
            // gone at logout, which would mean re-entering it every morning.
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: PWSTR::null(),
            UserName: PWSTR::null(),
        };

        // SAFETY: every pointer in `credential` addresses a buffer that
        // outlives the call, and the sizes match those buffers.
        unsafe { CredWriteW(&credential, 0) }
            .map_err(|e| CredentialError(format!("could not save the credential: {}", e.message())))
    }

    /// Reads the secret stored under `target`, if there is one.
    pub fn load(target: &str) -> Option<String> {
        let target = wide(target);
        let mut raw = std::ptr::null_mut();

        // SAFETY: `target` is NUL-terminated; on success the out-param owns an
        // allocation freed by CredFree below on every path.
        unsafe {
            CredReadW(
                windows::core::PCWSTR(target.as_ptr()),
                CRED_TYPE_GENERIC,
                None,
                &mut raw,
            )
        }
        .ok()?;
        if raw.is_null() {
            return None;
        }

        // SAFETY: `raw` points at a CREDENTIALW the API just filled in, and the
        // blob is described by its own size field.
        let secret = unsafe {
            let credential = &*raw;
            if credential.CredentialBlob.is_null() || credential.CredentialBlobSize == 0 {
                None
            } else {
                let bytes = std::slice::from_raw_parts(
                    credential.CredentialBlob,
                    credential.CredentialBlobSize as usize,
                );
                // Lossy rather than failing: a corrupt blob should send the
                // user through re-entering the secret, not return an error
                // they cannot act on. Whatever consumes it will reject it.
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
        };

        // SAFETY: allocated by CredReadW and not used afterwards.
        unsafe { CredFree(raw as *const _) };
        secret.filter(|s| !s.is_empty())
    }

    /// Removes the stored secret — signing out, or forgetting a server.
    pub fn clear(target: &str) -> Result<(), CredentialError> {
        let target = wide(target);
        // SAFETY: `target` is NUL-terminated.
        unsafe {
            CredDeleteW(
                windows::core::PCWSTR(target.as_ptr()),
                CRED_TYPE_GENERIC,
                None,
            )
        }
        .map_err(|e| {
            CredentialError(format!("could not remove the credential: {}", e.message()))
        })
    }
}

#[cfg(not(windows))]
mod win32 {
    use super::CredentialError;

    pub fn store(_target: &str, _secret: &str) -> Result<(), CredentialError> {
        Err(CredentialError(
            "credential storage is Windows-only".to_owned(),
        ))
    }
    pub fn load(_target: &str) -> Option<String> {
        None
    }
    pub fn clear(_target: &str) -> Result<(), CredentialError> {
        Ok(())
    }
}

pub use win32::{clear, load, store};

#[cfg(test)]
mod tests {
    #[test]
    fn a_missing_credential_reads_as_absent_rather_than_failing() {
        // Callers branch on Option, and a first run has nothing stored. On
        // Linux this is the stub, which is the same answer.
        assert_eq!(super::load("Neutron/Test/definitely-not-stored"), None);
    }
}
