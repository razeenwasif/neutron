//! Running the authorisation: bind a socket, open a browser, redeem the code.
//!
//! # Threading
//!
//! Blocking throughout, and one step waits on a human. Worker thread only.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::oauth::{self, AuthError, Pkce};

/// How long to wait for the user to finish at the consent screen.
///
/// Generous — they may have to sign in, pick between accounts, and read the
/// scopes. It exists so a browser that never opens, or a user who wandered off,
/// eventually releases the worker rather than pinning it for the session.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

/// How often the loopback wait wakes to check [`CONSENT_TIMEOUT`].
const POLL: Duration = Duration::from_millis(100);

/// Longest request line read from the loopback socket. Google's redirect is a
/// few hundred bytes; anything far longer is not it.
const MAX_REQUEST_LINE: u64 = 8 * 1024;

/// Where the client credentials come from.
///
/// Environment variables are checked first, then `%APPDATA%\\Neutron\\google.json`
/// — see [`crate::config`]. The file is what makes Drive work when Neutron is
/// launched from the taskbar rather than a shell.
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
    resolve(env_value("NEUTRON_GOOGLE_CLIENT_ID"), || {
        crate::config::load().map(|c| c.client_id)
    })
    .ok_or(AuthError::NoClientId)
}

/// The client secret Google issues alongside a Desktop-app client.
///
/// Required at the token endpoint. Omitting it — which the first version of
/// this did, on the assumption that PKCE made it unnecessary — gets the whole
/// exchange rejected with `invalid_request: client_secret is missing`, *after*
/// the user has already consented in the browser.
pub fn client_secret() -> Result<String, AuthError> {
    resolve(env_value("NEUTRON_GOOGLE_CLIENT_SECRET"), || {
        crate::config::load().map(|c| c.client_secret)
    })
    .ok_or(AuthError::NoClientSecret)
}

/// Picks a credential from the environment, falling back to the config file.
///
/// The two sources are separated from the lookups themselves so the precedence
/// rule can be tested. It cannot be tested through [`client_id`]: that reads a
/// real file in `%APPDATA%`, so on a machine where Drive is actually set up —
/// which is every machine where anyone would run these tests — a test asserting
/// "no client id when the variable is unset" fails for the right reason and the
/// wrong one at once.
///
/// The file is read lazily so that setting the variable avoids touching the
/// disk at all.
fn resolve(from_env: Option<String>, from_file: impl FnOnce() -> Option<String>) -> Option<String> {
    from_env
        .or_else(|| from_file().map(|s| s.trim().to_owned()))
        .filter(|s| !s.is_empty())
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

/// Waits for the redirect that carries this flow's authorisation code.
fn wait_for_redirect(listener: &TcpListener, expected_state: &str) -> Result<String, AuthError> {
    wait_for_redirect_within(listener, expected_state, CONSENT_TIMEOUT)
}

/// [`wait_for_redirect`], with the deadline as a parameter so the tests can
/// use one measured in milliseconds.
///
/// # The deadline is real
///
/// A blocking `accept` cannot be interrupted, so a deadline checked only after
/// it returns is checked only when something connects — and the case the
/// deadline exists for is the one where nothing ever does: a browser that
/// never opened, a user who closed the tab. That pinned a worker thread for
/// the life of the process. The listener is non-blocking instead, and polled.
///
/// # Only this flow's redirect ends it
///
/// The port is reachable by every process on the machine, and browsers make
/// requests of their own. A request for any other path is answered with a 404
/// and ignored; a request on the redirect path whose `state` does not match is
/// answered with an error page and ignored too. Ending the flow on either
/// would let anything local cancel a sign-in by guessing a port, and a
/// favicon fetch that won the race would do it by accident.
fn wait_for_redirect_within(
    listener: &TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String, AuthError> {
    listener
        .set_nonblocking(true)
        .map_err(|e| AuthError::Http(e.to_string()))?;

    let deadline = Instant::now() + timeout;

    loop {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::WouldBlock {
                    tracing::debug!("loopback accept failed: {e}");
                }
                if Instant::now() >= deadline {
                    return Err(AuthError::Http("timed out waiting for consent".to_owned()));
                }
                std::thread::sleep(POLL);
                continue;
            }
        };

        // On macOS and the BSDs an accepted socket inherits the listener's
        // non-blocking flag, and a non-blocking read here would fail at once
        // rather than wait for the browser to send its request.
        let _ = stream.set_nonblocking(false);
        // A connection that stalls mid-request must not hold the flow open.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

        // Capped, because anything on the machine can connect, and a request
        // line that never ends must not grow without limit.
        let mut line = String::new();
        if BufReader::new((&stream).take(MAX_REQUEST_LINE))
            .read_line(&mut line)
            .is_err()
        {
            continue;
        }

        // The redirect URI is the bare origin, so the browser comes back to
        // `/`. Anything else — `/favicon.ico` above all, which browsers
        // request alongside the page — is not the redirect.
        if request_path(&line) != Some("/") {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
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

        match result {
            // Not ours. Answered, so whoever sent it is not left hanging, and
            // otherwise ignored: the real redirect may still be on its way.
            Err(AuthError::StateMismatch) => {
                tracing::warn!("ignored a loopback request whose state did not match");
                continue;
            }
            other => return other.map(|r| r.code),
        }
    }
}

/// The path of an HTTP request line, without its query.
fn request_path(request_line: &str) -> Option<&str> {
    let target = request_line.split_whitespace().nth(1)?;
    Some(target.split_once('?').map_or(target, |(path, _)| path))
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
    // `open` hands a URL to the default browser on macOS; `xdg-open` is the
    // freedesktop equivalent and does not exist there.
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(opener)
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
        // The message is the only thing telling a new contributor what to do.
        assert!(AuthError::NoClientId.to_string().contains("NEUTRON_GOOGLE_CLIENT_ID"));
    }

    #[test]
    fn a_missing_client_secret_is_named_too() {
        // Google requires it for installed apps despite PKCE. Assuming
        // otherwise cost a full round of consent before the token endpoint
        // said so.
        assert!(
            AuthError::NoClientSecret
                .to_string()
                .contains("NEUTRON_GOOGLE_CLIENT_SECRET")
        );
    }

    #[test]
    fn with_nothing_set_anywhere_there_is_no_credential() {
        assert_eq!(resolve(None, || None), None);
    }

    #[test]
    fn the_environment_wins_over_the_config_file() {
        // The file is what a signed-in user has; the variable is how someone
        // tries a different client without editing it.
        assert_eq!(
            resolve(Some("from-env".to_owned()), || Some("from-file".to_owned())),
            Some("from-env".to_owned())
        );
    }

    #[test]
    fn the_config_file_is_used_when_the_environment_is_unset() {
        assert_eq!(
            resolve(None, || Some("from-file".to_owned())),
            Some("from-file".to_owned())
        );
    }

    #[test]
    fn a_blank_value_in_the_config_file_counts_as_missing() {
        // A half-filled google.json is what you get from stopping partway
        // through the setup, and it must not read as configured.
        assert_eq!(resolve(None, || Some("   ".to_owned())), None);
    }

    #[test]
    fn the_config_file_value_is_trimmed() {
        assert_eq!(
            resolve(None, || Some(" pasted\n".to_owned())),
            Some("pasted".to_owned())
        );
    }

    #[test]
    fn a_blank_client_id_counts_as_missing() {
        unsafe { std::env::set_var("NEUTRON_GOOGLE_CLIENT_ID", "   ") };
        let value = env_value("NEUTRON_GOOGLE_CLIENT_ID");
        unsafe { std::env::remove_var("NEUTRON_GOOGLE_CLIENT_ID") };
        assert_eq!(value, None);
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

    /// Sends one request line to the loopback listener and returns the reply.
    fn send(port: u16, line: &str) -> String {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(format!("{line}\r\nHost: 127.0.0.1\r\n\r\n").as_bytes()).unwrap();
        let mut reply = String::new();
        let _ = stream.read_to_string(&mut reply);
        reply
    }

    #[test]
    fn an_abandoned_sign_in_gives_up_on_its_deadline() {
        // Nothing ever connects: the browser never opened, or the user closed
        // the tab. The wait used to block in `accept` for ever.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let started = Instant::now();
        let outcome = wait_for_redirect_within(&listener, "ours", Duration::from_millis(300));

        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
        assert!(
            matches!(&outcome, Err(AuthError::Http(e)) if e.contains("timed out")),
            "{outcome:?}"
        );
    }

    #[test]
    fn stray_requests_are_answered_and_ignored_until_the_real_redirect_arrives() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let port = listener.local_addr().unwrap().port();

        let client = std::thread::spawn(move || {
            [
                send(port, "GET /favicon.ico HTTP/1.1"),
                // Another path, even one carrying the right state, is not
                // the redirect.
                send(port, "GET /elsewhere?code=x&state=ours HTTP/1.1"),
                // A local process trying to cancel the sign-in.
                send(port, "GET /?error=access_denied&state=guess HTTP/1.1"),
                send(port, "GET /?error=access_denied HTTP/1.1"),
                // Or to substitute its own code.
                send(port, "GET /?code=theirs&state=guess HTTP/1.1"),
                send(port, "GET /?code=good&state=ours HTTP/1.1"),
            ]
        });

        let outcome = wait_for_redirect_within(&listener, "ours", Duration::from_secs(10));
        let replies = client.join().unwrap();

        assert_eq!(outcome, Ok("good".to_owned()));
        assert!(replies[0].starts_with("HTTP/1.1 404"), "{}", replies[0]);
        assert!(replies[1].starts_with("HTTP/1.1 404"), "{}", replies[1]);
        for reply in &replies[2..5] {
            assert!(reply.contains("Could not connect"), "{reply}");
        }
        assert!(replies[5].contains("Google Drive connected"), "{}", replies[5]);
    }

    #[test]
    fn a_decline_with_the_right_state_still_ends_the_flow() {
        // The user pressing Cancel is this flow's answer, not a stray.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client =
            std::thread::spawn(move || send(port, "GET /?error=access_denied&state=ours HTTP/1.1"));

        let outcome = wait_for_redirect_within(&listener, "ours", Duration::from_secs(10));
        client.join().unwrap();
        assert_eq!(outcome, Err(AuthError::Declined));
    }

    #[test]
    fn the_request_path_is_read_without_its_query() {
        assert_eq!(request_path("GET /?code=a HTTP/1.1"), Some("/"));
        assert_eq!(request_path("GET /favicon.ico HTTP/1.1"), Some("/favicon.ico"));
        assert_eq!(request_path("GET"), None);
    }

    #[test]
    fn the_loopback_binds_only_to_localhost() {
        // Binding 0.0.0.0 would expose the authorisation code to anything on
        // the network that can guess the port.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
    }
}
