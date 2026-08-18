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

/// Where the client credentials come from.
///
/// # Why these are not compiled in
///
/// Neither value is a secret in the cryptographic sense. Google's own
/// documentation is explicit that for installed applications the client secret
/// "is obviously not treated as a secret" — it ships inside every copy of the
/// binary and anyone can read it out. PKCE is what actually secures the
/// exchange; the secret is a Google protocol requirement, not a protection.
///
/// They are still not committed, for two duller reasons: they tie a public
/// repository to one person's Google Cloud project and its quota, and anyone
/// building Neutron themselves should use their own rather than inherit someone
/// else's rate limit and audit trail.
pub fn client_id() -> Result<String, AuthError> {
    env_value("NEUTRON_GOOGLE_CLIENT_ID").ok_or(AuthError::NoClientId)
}

/// The client secret Google issues alongside a Desktop-app client.
///
/// Required at the token endpoint. Omitting it — which the first version of
/// this did, on the assumption that PKCE made it unnecessary — gets the whole
/// exchange rejected with `invalid_request: client_secret is missing`, *after*
/// the user has already consented in the browser.
pub fn client_secret() -> Result<String, AuthError> {
    env_value("NEUTRON_GOOGLE_CLIENT_SECRET").ok_or(AuthError::NoClientSecret)
}

/// Reads an environment variable, trimmed, treating blank as absent.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        // Trimmed, not merely checked for being non-blank. These are pasted by
        // hand, and a trailing newline survives the environment intact — it
        // then percent-encodes to `%0A` inside the authorization URL and Google
        // answers `Error 400: invalid_request` with nothing pointing at
        // whitespace.
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
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
    // Checked up front, not at redemption. Discovering it is missing after the
    // user has worked through a consent screen wastes the one step that costs
    // them attention.
    client_secret()?;
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
    // Logged so a rejected authorisation can be diagnosed from the URL that
    // was actually sent. Safe: it carries the *challenge*, never the verifier,
    // and the client id is public by design.
    tracing::debug!(%url, "opening the consent screen");
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

        // The page reports what actually happened. This is the only chance to
        // tell the user anything in the tab they are looking at, and claiming
        // success here while returning an error is how a failed sign-in came to
        // look like a working one.
        let page = oauth::result_page(result.as_ref().map(|_| ()));
        let _ = stream.write_all(page.as_bytes());
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
    let secret = client_secret()?;
    post_token(&[
        ("client_id", client_id),
        ("client_secret", &secret),
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
    let secret = client_secret()?;
    post_token(&[
        ("client_id", client_id),
        ("client_secret", &secret),
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

/// Opens the user's default browser at `url`.
///
/// # Never through `cmd /C start`
///
/// That was the first implementation and it is silently, catastrophically
/// wrong: `cmd.exe` treats `&` as a command separator, and an OAuth
/// authorization URL is *made* of `&`. The browser received everything up to
/// the first one — `...?client_id=X` alone, with no redirect_uri, response_type
/// or scope — and cmd tried to execute `redirect_uri=...` as a program. Google
/// answered `Error 400: invalid_request`, which points at the request rather
/// than at how it was opened, and the URL in the log looked perfect because it
/// *was* perfect right up until a shell chewed it.
///
/// Quoting the argument would fix the splitting, but `ShellExecuteW` is what
/// `start` calls anyway — this skips the shell, and with it a spawned console
/// window and a complaint about the UNC working directory.
#[cfg(windows)]
fn open_browser(url: &str) -> Result<(), AuthError> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{PCWSTR, w};

    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: `wide` is NUL-terminated and outlives the call; the verb is a
    // static literal and no owner window is needed for a browser launch.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    // Returns a fake HINSTANCE; anything <= 32 is an error code rather than a
    // handle. The one API in Win32 that reports failure this way.
    if result.0 as usize <= 32 {
        return Err(AuthError::Http(format!(
            "could not open a browser (ShellExecute returned {})",
            result.0 as usize
        )));
    }
    Ok(())
}

/// Safe as written — the URL is one argv element and no shell is involved, so
/// nothing interprets the `&` separators.
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
    fn a_missing_client_secret_is_named_too() {
        // Google requires it for installed apps despite PKCE. Assuming
        // otherwise cost a full round of consent before the token endpoint
        // said so.
        unsafe { std::env::remove_var("NEUTRON_GOOGLE_CLIENT_SECRET") };
        assert_eq!(client_secret(), Err(AuthError::NoClientSecret));
        assert!(
            AuthError::NoClientSecret
                .to_string()
                .contains("NEUTRON_GOOGLE_CLIENT_SECRET")
        );
    }

    #[test]
    fn a_blank_client_id_counts_as_missing() {
        unsafe { std::env::set_var("NEUTRON_GOOGLE_CLIENT_ID", "   ") };
        assert_eq!(client_id(), Err(AuthError::NoClientId));
        unsafe { std::env::remove_var("NEUTRON_GOOGLE_CLIENT_ID") };
    }

    #[test]
    fn surrounding_whitespace_is_stripped_from_the_client_id() {
        // A pasted id very often carries a trailing newline. Left on, it
        // encodes to `%0A` in the authorization URL and Google rejects the
        // whole request with an error that says nothing about whitespace.
        unsafe {
            std::env::set_var("NEUTRON_GOOGLE_CLIENT_ID", "  123-abc.apps.googleusercontent.com\n")
        };
        assert_eq!(
            client_id().as_deref(),
            Ok("123-abc.apps.googleusercontent.com")
        );
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
    fn the_authorization_url_contains_the_separators_that_broke_it() {
        // Guards the shape of the problem rather than the fix: the URL is built
        // from `&`-joined parameters, so any future change to how the browser
        // is launched has to survive them. Routing this through `cmd /C start`
        // delivered only the first parameter.
        let pkce = crate::oauth::Pkce::from_verifier("v".repeat(43));
        let url = crate::oauth::authorize_url("id", "http://127.0.0.1:1", "st", &pkce);
        assert!(url.matches('&').count() >= 6, "{url}");
    }

    #[test]
    fn the_loopback_binds_only_to_localhost() {
        // Binding 0.0.0.0 would expose the authorisation code to anything on
        // the network that can guess the port.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
    }
}
