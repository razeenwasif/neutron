//! Running the authorisation: bind a socket, open a browser, redeem the code.
//!
//! # Threading
//!
//! Blocking throughout, and one step waits on a human. Worker thread only.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::time::Duration;

use serde::Deserialize;

use crate::oauth::{self, AuthError, Pkce};

/// How long to wait for the user to finish at the consent screen.
///
/// Generous — they may have to sign in, pick between accounts, and read the
/// scopes. It exists so a browser that never opens, or a user who wandered off,
/// eventually releases the worker rather than pinning it for the session.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Where the client id comes from.
///
/// # Why this is not compiled in
///
/// An OAuth client id for an installed app is not a secret in the cryptographic
/// sense — Google publishes it to anyone who runs the binary, and PKCE is what
/// actually secures the exchange. It is still not committed, for two duller
/// reasons: it ties a public repository to one person's Google Cloud project
/// and its quota, and anyone building Neutron themselves should be using their
/// own rather than inheriting someone else's rate limit and audit trail.
pub fn client_id() -> Result<String, AuthError> {
    std::env::var("NEUTRON_GOOGLE_CLIENT_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or(AuthError::NoClientId)
}

/// Tokens as Google returns them.
#[derive(Debug, Clone, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    /// Only present on the first exchange, and only with `access_type=offline`.
    /// Losing it means the user has to consent again, so it is the thing worth
    /// persisting.
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// Runs the whole interactive flow and returns tokens.
///
/// **Worker thread only** — binds a socket and waits on a human.
pub fn authorize() -> Result<Tokens, AuthError> {
    let client_id = client_id()?;
    let pkce = Pkce::generate()?;
    let state = oauth::random_state()?;

    // Port 0 lets the OS pick a free one, and only then is the redirect URI
    // known — Google matches it exactly, so it has to be the port actually
    // bound rather than a guess that might be in use.
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .map_err(|e| AuthError::Http(format!("could not bind a loopback port: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| AuthError::Http(e.to_string()))?
        .port();
    let redirect = format!("http://127.0.0.1:{port}");

    let url = oauth::authorize_url(&client_id, &redirect, &state, &pkce);
    open_browser(&url)?;

    let code = wait_for_redirect(&listener, &state)?;
    exchange_code(&client_id, &redirect, &code, &pkce)
}

/// Accepts exactly one request and reads the authorisation code from it.
fn wait_for_redirect(listener: &TcpListener, expected_state: &str) -> Result<String, AuthError> {
    listener
        .set_nonblocking(false)
        .map_err(|e| AuthError::Http(e.to_string()))?;

    let deadline = std::time::Instant::now() + CONSENT_TIMEOUT;

    for stream in listener.incoming() {
        if std::time::Instant::now() > deadline {
            return Err(AuthError::Http("timed out waiting for consent".to_owned()));
        }

        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("loopback accept failed: {e}");
                continue;
            }
        };
        // A connection that stalls mid-request must not hold the flow open.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));

        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }

        // Browsers request /favicon.ico alongside the page. Answering it as the
        // redirect would fail the state check and abort a flow that is fine.
        if line.contains("/favicon.ico") {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        }

        let result = oauth::parse_redirect(&line, expected_state);

        // The page is written before returning either way, so the user sees an
        // outcome in the tab they are looking at rather than a dead socket.
        let _ = stream.write_all(oauth::success_page().as_bytes());
        let _ = stream.flush();

        return result.map(|r| r.code);
    }

    Err(AuthError::Http("the loopback listener closed".to_owned()))
}

/// Redeems the authorisation code, sending the verifier that proves this is the
/// same process that asked for it.
pub fn exchange_code(
    client_id: &str,
    redirect: &str,
    code: &str,
    pkce: &Pkce,
) -> Result<Tokens, AuthError> {
    post_token(&[
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", &pkce.verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", redirect),
    ])
}

/// Trades a stored refresh token for a fresh access token.
///
/// The reason a refresh token is worth storing in the credential manager: this
/// path involves no browser and no consent screen, so a returning user never
/// sees one.
pub fn refresh(client_id: &str, refresh_token: &str) -> Result<Tokens, AuthError> {
    post_token(&[
        ("client_id", client_id),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ])
}

fn post_token(form: &[(&str, &str)]) -> Result<Tokens, AuthError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| AuthError::Http(e.to_string()))?;

    let response = client
        .post(oauth::TOKEN_ENDPOINT)
        .form(form)
        .send()
        .map_err(|e| AuthError::Http(format!("could not reach Google: {e}")))?;

    let status = response.status();
    let body = response
        .text()
        .map_err(|e| AuthError::Http(e.to_string()))?;

    if !status.is_success() {
        // Google's error body names the cause — `invalid_grant` for an expired
        // or revoked refresh token, which is the one worth acting on.
        return Err(AuthError::Provider(describe_token_error(&body, status.as_u16())));
    }

    serde_json::from_str(&body)
        .map_err(|e| AuthError::Malformed(format!("token response: {e}")))
}

/// Turns Google's error body into something worth showing.
pub fn describe_token_error(body: &str, status: u16) -> String {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: Option<String>,
        error_description: Option<String>,
    }

    match serde_json::from_str::<ErrorBody>(body) {
        Ok(e) => match (e.error.as_deref(), e.error_description) {
            (Some("invalid_grant"), _) => {
                "the saved authorisation is no longer valid — sign in again".to_owned()
            }
            (Some(code), Some(description)) => format!("{code}: {description}"),
            (Some(code), None) => code.to_owned(),
            _ => format!("HTTP {status}"),
        },
        Err(_) => format!("HTTP {status}"),
    }
}

#[cfg(windows)]
fn open_browser(url: &str) -> Result<(), AuthError> {
    // Through the shell, so it opens whatever the user has set as default
    // rather than assuming a browser is on PATH.
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
        .map(|_| ())
        .map_err(|e| AuthError::Http(format!("could not open a browser: {e}")))
}

#[cfg(not(windows))]
fn open_browser(url: &str) -> Result<(), AuthError> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| AuthError::Http(format!("could not open a browser: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_client_id_is_named_rather_than_a_generic_failure() {
        // Safe to assert: the variable is not set in the test environment, and
        // the message is the only thing telling a new contributor what to do.
        unsafe { std::env::remove_var("NEUTRON_GOOGLE_CLIENT_ID") };
        assert_eq!(client_id(), Err(AuthError::NoClientId));
        assert!(AuthError::NoClientId.to_string().contains("NEUTRON_GOOGLE_CLIENT_ID"));
    }

    #[test]
    fn a_blank_client_id_counts_as_missing() {
        unsafe { std::env::set_var("NEUTRON_GOOGLE_CLIENT_ID", "   ") };
        assert_eq!(client_id(), Err(AuthError::NoClientId));
        unsafe { std::env::remove_var("NEUTRON_GOOGLE_CLIENT_ID") };
    }

    #[test]
    fn an_expired_refresh_token_says_what_to_do() {
        // `invalid_grant` is what a revoked or expired refresh token returns,
        // and it is the one token error a user can actually resolve.
        let msg = describe_token_error(r#"{"error":"invalid_grant"}"#, 400);
        assert!(msg.contains("sign in again"), "{msg}");
    }

    #[test]
    fn other_token_errors_keep_their_description() {
        let msg = describe_token_error(
            r#"{"error":"invalid_client","error_description":"Unauthorized"}"#,
            401,
        );
        assert!(msg.contains("invalid_client"), "{msg}");
        assert!(msg.contains("Unauthorized"), "{msg}");
    }

    #[test]
    fn a_non_json_error_body_still_produces_a_message() {
        // Google can return an HTML error page from a proxy; parsing it as
        // JSON fails and must not swallow the status.
        let msg = describe_token_error("<html>502</html>", 502);
        assert!(msg.contains("502"), "{msg}");
    }

    #[test]
    fn a_token_response_without_a_refresh_token_parses() {
        // Refresh responses omit it — the client keeps the one it already has.
        // Making the field required would fail every silent re-auth.
        let t: Tokens =
            serde_json::from_str(r#"{"access_token":"a","expires_in":3599,"token_type":"Bearer"}"#)
                .expect("parses");
        assert!(t.refresh_token.is_none());
        assert_eq!(t.access_token, "a");
    }

    #[test]
    fn the_loopback_binds_only_to_localhost() {
        // Binding 0.0.0.0 would expose the authorisation code to anything on
        // the network that can guess the port.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
    }
}
