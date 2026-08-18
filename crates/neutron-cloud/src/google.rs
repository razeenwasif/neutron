//! The Google Drive provider: authentication state, listing, and caching.
//!
//! # Access tokens are short-lived, refresh tokens are not
//!
//! An access token lasts about an hour. The refresh token that mints new ones
//! lasts until revoked, which is why it is the one thing persisted — and why it
//! lives in Credential Manager rather than on disk. A user who authorised last
//! month opens Neutron and sees their Drive with no browser and no consent
//! screen, because the refresh happens silently on the first listing.
//!
//! # Threading
//!
//! Every method here does network I/O. Worker thread only.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use neutron_core::{EntryList, NamespaceError};

use crate::credentials;
use crate::drive::{self, FileList};
use crate::flow::{self, Tokens};
use crate::oauth::AuthError;

/// Refresh this long before the token actually expires.
///
/// A listing that starts valid and expires mid-pagination fails halfway with a
/// 401, which surfaces as a partial folder. Renewing early costs one extra
/// request an hour.
const REFRESH_MARGIN: Duration = Duration::from_secs(120);

/// A live access token and when it stops being one.
struct Access {
    token: String,
    expires_at: Instant,
}

impl Access {
    fn from(tokens: &Tokens) -> Self {
        // Default to Google's usual hour when the field is absent, minus the
        // margin — never treat a missing expiry as "forever".
        let lifetime = Duration::from_secs(tokens.expires_in.unwrap_or(3600));
        Self {
            token: tokens.access_token.clone(),
            expires_at: Instant::now() + lifetime.saturating_sub(REFRESH_MARGIN),
        }
    }

    fn is_usable(&self) -> bool {
        Instant::now() < self.expires_at
    }
}

/// Whether Drive can be browsed, and why not when it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveState {
    /// No client id configured — nothing can be attempted.
    NotConfigured,
    /// Configured, but nobody has signed in.
    SignedOut,
    SignedIn,
    Error(String),
}

pub struct GoogleDrive {
    client: reqwest::blocking::Client,
    access: Mutex<Option<Access>>,
}

impl Default for GoogleDrive {
    fn default() -> Self {
        Self::new()
    }
}

impl GoogleDrive {
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            access: Mutex::new(None),
        }
    }

    /// What the sidebar should show, without performing any network I/O.
    pub fn state(&self) -> DriveState {
        // Both, because the secret is required at redemption and finding that
        // out only after consent is a poor trade for one extra check here.
        if flow::client_id().is_err() || flow::client_secret().is_err() {
            return DriveState::NotConfigured;
        }
        if credentials::load().is_none() {
            return DriveState::SignedOut;
        }
        DriveState::SignedIn
    }

    /// Runs the interactive flow and stores the refresh token.
    ///
    /// **Worker thread only** — opens a browser and waits for consent.
    pub fn sign_in(&self) -> Result<(), AuthError> {
        let tokens = flow::authorize()?;

        // Without a refresh token the session dies in an hour and the user is
        // sent back through consent, which looks like the sign-in silently
        // failing. Better to say so now.
        let refresh = tokens.refresh_token.clone().ok_or_else(|| {
            AuthError::Provider(
                "Google did not return a refresh token — try removing Neutron's access \
                 in your Google account and signing in again"
                    .to_owned(),
            )
        })?;

        credentials::store(&refresh)?;
        *self.access.lock().expect("access lock") = Some(Access::from(&tokens));
        Ok(())
    }

    /// Forgets the stored credential.
    pub fn sign_out(&self) -> Result<(), AuthError> {
        *self.access.lock().expect("access lock") = None;
        credentials::clear()
    }

    /// A usable access token, refreshing silently if the current one is stale.
    fn access_token(&self) -> Result<String, AuthError> {
        {
            let cached = self.access.lock().expect("access lock");
            if let Some(access) = cached.as_ref() {
                if access.is_usable() {
                    return Ok(access.token.clone());
                }
            }
        }

        let client_id = flow::client_id()?;
        let refresh_token = credentials::load().ok_or(AuthError::Declined)?;
        let tokens = flow::refresh(&client_id, &refresh_token)?;

        let token = tokens.access_token.clone();
        *self.access.lock().expect("access lock") = Some(Access::from(&tokens));
        Ok(token)
    }

    /// Lists one Drive folder, following pagination to the end.
    ///
    /// **Worker thread only.**
    pub fn list(&self, folder_id: &str) -> Result<EntryList, NamespaceError> {
        let token = self
            .access_token()
            .map_err(|e| NamespaceError::Other(e.to_string()))?;

        let mut list = EntryList::with_capacity(64);
        let mut page_token: Option<String> = None;

        loop {
            let page = self
                .fetch_page(&token, folder_id, page_token.as_deref())
                .map_err(|e| NamespaceError::Other(e.to_string()))?;

            for file in &page.files {
                list.push(&file.to_entry());
                // Drive children are addressed by id, never by joining a name
                // to the parent — two files in one folder can share a name
                // exactly. A shortcut records its *target*, so opening one
                // behaves like opening what it points at.
                list.push_target(file.target_id(), false);
            }

            match page.next_page_token {
                Some(next) => page_token = Some(next),
                None => break,
            }
        }

        list.reset_order();
        Ok(list)
    }

    fn fetch_page(
        &self,
        token: &str,
        folder_id: &str,
        page_token: Option<&str>,
    ) -> Result<FileList, AuthError> {
        let response = self
            .client
            .get("https://www.googleapis.com/drive/v3/files")
            .bearer_auth(token)
            .query(&drive::list_query(folder_id, page_token))
            .send()
            .map_err(|e| AuthError::Http(format!("could not reach Drive: {e}")))?;

        let status = response.status();
        let body = response.text().map_err(|e| AuthError::Http(e.to_string()))?;

        if !status.is_success() {
            return Err(AuthError::Provider(flow::describe_token_error(
                &body,
                status.as_u16(),
            )));
        }

        serde_json::from_str(&body)
            .map_err(|e| AuthError::Malformed(format!("Drive listing: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_reports_missing_configuration_before_missing_credentials() {
        // Two different problems with two different remedies: one needs a
        // client id, the other needs a sign-in. Reporting "signed out" when
        // there is no client id sends the user to a button that cannot work.
        unsafe {
            std::env::remove_var("NEUTRON_GOOGLE_CLIENT_ID");
            std::env::remove_var("NEUTRON_GOOGLE_CLIENT_SECRET");
        }
        assert_eq!(GoogleDrive::new().state(), DriveState::NotConfigured);
    }

    #[test]
    fn a_token_is_treated_as_expired_before_it_actually_is() {
        // A listing that starts valid and expires during pagination returns a
        // partial folder, which looks like missing files rather than an error.
        let tokens = Tokens {
            access_token: "a".to_owned(),
            refresh_token: None,
            // Shorter than the margin: already unusable.
            expires_in: Some(60),
        };
        assert!(!Access::from(&tokens).is_usable());
    }

    #[test]
    fn a_fresh_token_is_usable() {
        let tokens = Tokens {
            access_token: "a".to_owned(),
            refresh_token: None,
            expires_in: Some(3600),
        };
        assert!(Access::from(&tokens).is_usable());
    }

    #[test]
    fn a_missing_expiry_does_not_mean_forever() {
        // Defaulting to "never expires" produces a client that works for an
        // hour and then fails every request until restarted.
        let tokens = Tokens {
            access_token: "a".to_owned(),
            refresh_token: None,
            expires_in: None,
        };
        let access = Access::from(&tokens);
        assert!(access.is_usable());
        assert!(access.expires_at < Instant::now() + Duration::from_secs(3600));
    }
}
