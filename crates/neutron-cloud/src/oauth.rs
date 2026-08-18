//! OAuth 2.0 for an installed application: PKCE, a loopback redirect, and the
//! token exchange.
//!
//! # Why PKCE and a loopback address
//!
//! A desktop application cannot keep a secret. Anything compiled into the
//! binary — including the "client secret" Google issues for installed apps — is
//! readable by anyone who has the binary, so it authenticates nothing. PKCE
//! replaces that with a secret generated fresh per authorisation attempt: the
//! app sends a hash of a random verifier up front and the verifier itself at
//! redemption, so an authorisation code stolen in transit is useless without
//! the verifier that never left this process.
//!
//! The redirect goes to `http://127.0.0.1:<port>` rather than a custom URI
//! scheme. A custom scheme is registered machine-wide and any other application
//! can claim it, which turns the redirect — carrying the authorisation code —
//! into something a hostile program can intercept. A loopback socket is bound
//! by this process and cannot be.
//!
//! # What is checked on the way back
//!
//! Two things, both of which are security properties rather than politeness:
//!
//! * The `state` parameter must match what was sent. Without it, an attacker
//!   can feed the app *their* authorisation code and have the user's session
//!   silently attached to the attacker's account.
//! * Only the loopback interface is bound, so nothing off-machine can reach it.

use std::fmt;

use base64::Engine;
use sha2::{Digest, Sha256};

/// Scope requested. `drive.readonly` rather than full `drive`: Neutron browses
/// and downloads, and asking for write access it never uses is both a worse
/// consent screen and a larger blast radius if the token leaks.
pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";

pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// A PKCE verifier and its challenge.
#[derive(Debug, Clone)]
pub struct Pkce {
    /// Never leaves this process until the code is redeemed.
    pub verifier: String,
    /// `base64url(sha256(verifier))`, sent with the authorisation request.
    pub challenge: String,
}

impl Pkce {
    /// Generates a fresh pair from the OS random source.
    ///
    /// 32 bytes of entropy, encoded — inside the 43..=128 character range the
    /// spec requires, and well past guessable.
    pub fn generate() -> Result<Self, AuthError> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| AuthError::NoEntropy)?;

        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        Ok(Self::from_verifier(verifier))
    }

    /// Derives the challenge for a given verifier. Split out so the derivation
    /// can be tested against the RFC 7636 worked example.
    pub fn from_verifier(verifier: String) -> Self {
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        Self {
            verifier,
            challenge,
        }
    }
}

/// Random value tying the redirect back to the request that started it.
pub fn random_state() -> Result<String, AuthError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| AuthError::NoEntropy)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// Builds the URL the user's browser is sent to.
pub fn authorize_url(client_id: &str, redirect: &str, state: &str, pkce: &Pkce) -> String {
    let q = [
        ("client_id", client_id),
        ("redirect_uri", redirect),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("state", state),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        // Without this Google only returns a refresh token on the *first*
        // authorisation ever granted to this client, so a user who has
        // authorised before and then cleared local state can never get one
        // again — the app would silently re-prompt on every launch.
        ("access_type", "offline"),
        ("prompt", "consent"),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", percent_encode(v)))
    .collect::<Vec<_>>()
    .join("&");

    format!("{AUTH_ENDPOINT}?{q}")
}

/// Percent-encodes everything outside the unreserved set.
///
/// Deliberately conservative rather than using a URL library: the values here
/// include a full URL (the redirect) and a base64url challenge, and encoding
/// too much is harmless while encoding too little silently truncates a
/// parameter at the first `&`.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// What came back on the loopback redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub code: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The OS random source failed. Continuing without it would mean a
    /// guessable verifier, which is worse than not authenticating at all.
    NoEntropy,
    /// The user pressed Cancel on the consent screen.
    Declined,
    /// The redirect carried an error, or no code at all.
    Provider(String),
    /// The `state` did not match. Treated as hostile, not as a glitch.
    StateMismatch,
    Malformed(String),
    NoClientId,
    Http(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::NoEntropy => write!(f, "the system random source is unavailable"),
            AuthError::Declined => write!(f, "access was declined"),
            AuthError::Provider(e) => write!(f, "Google returned an error: {e}"),
            AuthError::StateMismatch => {
                write!(f, "the redirect did not match this request and was rejected")
            }
            AuthError::Malformed(e) => write!(f, "malformed redirect: {e}"),
            AuthError::NoClientId => write!(
                f,
                "no Google OAuth client id configured — set NEUTRON_GOOGLE_CLIENT_ID"
            ),
            AuthError::Http(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// Parses the request line of the loopback redirect, e.g.
/// `GET /?code=abc&state=xyz HTTP/1.1`.
///
/// `expected_state` is compared in full; a mismatch is an error rather than a
/// warning, because the whole point of the parameter is to reject a redirect
/// this process did not initiate.
pub fn parse_redirect(request_line: &str, expected_state: &str) -> Result<Redirect, AuthError> {
    let target = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| AuthError::Malformed("no request target".to_owned()))?;

    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");

    let mut code = None;
    let mut state = None;
    let mut error = None;

    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = percent_decode(value);
        match key {
            "code" => code = Some(value),
            "state" => state = Some(value),
            "error" => error = Some(value),
            _ => {}
        }
    }

    if let Some(error) = error {
        // Google's spelling for "the user pressed Cancel". Distinguished so the
        // UI can say so plainly instead of reporting a failure.
        return Err(if error == "access_denied" {
            AuthError::Declined
        } else {
            AuthError::Provider(error)
        });
    }

    let state = state.ok_or_else(|| AuthError::Malformed("no state".to_owned()))?;
    // Compared before the code is even read: a redirect that fails this is not
    // ours, and nothing in it should be used.
    if state != expected_state {
        return Err(AuthError::StateMismatch);
    }

    let code = code.ok_or_else(|| AuthError::Malformed("no code".to_owned()))?;
    if code.is_empty() {
        return Err(AuthError::Malformed("empty code".to_owned()));
    }

    Ok(Redirect { code, state })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            // `+` means space in a form-encoded query, which is what a redirect
            // query is.
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    // A stray `%` is data, not an escape.
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// The page the browser lands on after the redirect.
///
/// Served from memory with an explicit `Connection: close`, so the browser does
/// not hold the socket open waiting for more — the listener accepts exactly one
/// request and a keep-alive connection would make it hang.
pub fn success_page() -> String {
    let body = "<!doctype html><meta charset=\"utf-8\">\
        <title>Neutron</title>\
        <body style=\"font-family:Segoe UI,system-ui,sans-serif;background:#0c0714;\
        color:#fbf7ff;display:grid;place-items:center;height:100vh;margin:0\">\
        <div style=\"text-align:center\">\
        <h1 style=\"font-weight:600\">Google Drive connected</h1>\
        <p style=\"color:#b8adcc\">You can close this tab and return to Neutron.</p>\
        </div>";

    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_matches_the_rfc_worked_example() {
        // RFC 7636 appendix B. Getting this wrong produces a challenge Google
        // rejects only at redemption, long after the consent screen — which is
        // a maddening thing to debug from the error alone.
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_owned());
        assert_eq!(pkce.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn a_generated_verifier_is_within_the_spec_length() {
        let pkce = Pkce::generate().expect("entropy");
        assert!(
            (43..=128).contains(&pkce.verifier.len()),
            "verifier is {} characters",
            pkce.verifier.len()
        );
        // Unreserved characters only, or it has to be percent-encoded on the
        // way out and matched byte-for-byte on the way back.
        assert!(
            pkce.verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')),
            "verifier contains characters needing encoding: {}",
            pkce.verifier
        );
    }

    #[test]
    fn two_generated_verifiers_differ() {
        // A fixed verifier would make the whole exchange forgeable.
        let a = Pkce::generate().unwrap().verifier;
        let b = Pkce::generate().unwrap().verifier;
        assert_ne!(a, b);
    }

    #[test]
    fn the_authorize_url_encodes_its_parameters() {
        let pkce = Pkce::from_verifier("v".repeat(43));
        let url = authorize_url("client.apps.googleusercontent.com", "http://127.0.0.1:1234", "st", &pkce);

        // The redirect is itself a URL; left raw its `://` and `/` would end
        // the parameter early and Google would reject the request.
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1234"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge)));
        assert!(!url.contains(&pkce.verifier), "the verifier must never be sent");
    }

    #[test]
    fn a_good_redirect_parses() {
        let got = parse_redirect("GET /?code=4/abc-DEF_123&state=xyz HTTP/1.1", "xyz").unwrap();
        assert_eq!(got.code, "4/abc-DEF_123");
        assert_eq!(got.state, "xyz");
    }

    #[test]
    fn a_mismatched_state_is_rejected() {
        // The attack this prevents: an attacker completes their own
        // authorisation, feeds the app the resulting code, and the user's
        // Neutron ends up reading the attacker's Drive.
        let got = parse_redirect("GET /?code=abc&state=attacker HTTP/1.1", "ours");
        assert_eq!(got, Err(AuthError::StateMismatch));
    }

    #[test]
    fn the_state_is_checked_before_the_code_is_used() {
        // Even with no code at all, a wrong state must report the mismatch
        // rather than a missing parameter — otherwise the error message
        // reveals which check failed first.
        let got = parse_redirect("GET /?state=attacker HTTP/1.1", "ours");
        assert_eq!(got, Err(AuthError::StateMismatch));
    }

    #[test]
    fn declining_is_distinguished_from_failing() {
        let got = parse_redirect("GET /?error=access_denied&state=xyz HTTP/1.1", "xyz");
        assert_eq!(got, Err(AuthError::Declined));

        let got = parse_redirect("GET /?error=invalid_scope&state=xyz HTTP/1.1", "xyz");
        assert_eq!(got, Err(AuthError::Provider("invalid_scope".to_owned())));
    }

    #[test]
    fn a_redirect_without_a_code_is_malformed() {
        assert!(matches!(
            parse_redirect("GET /?state=xyz HTTP/1.1", "xyz"),
            Err(AuthError::Malformed(_))
        ));
        assert!(matches!(
            parse_redirect("GET /?code=&state=xyz HTTP/1.1", "xyz"),
            Err(AuthError::Malformed(_))
        ));
    }

    #[test]
    fn a_garbage_request_line_does_not_panic() {
        // This socket is reachable by anything on the machine, so it will be
        // sent junk. Every branch must return an error rather than index off
        // the end of a slice.
        for line in ["", "GET", "\n", "GET /", "GET /?%", "GET /?code=%ZZ&state=xyz", "%"] {
            let _ = parse_redirect(line, "xyz");
        }
    }

    #[test]
    fn percent_escapes_decode() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("a+b"), "a b");
        // A trailing, incomplete escape is data rather than a panic.
        assert_eq!(percent_decode("a%"), "a%");
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    #[test]
    fn the_success_page_closes_the_connection() {
        // The listener accepts one request. With keep-alive the browser holds
        // the socket and the flow appears to hang after consent.
        let page = success_page();
        assert!(page.contains("Connection: close"));
        assert!(page.contains("Content-Length:"));
    }
}
