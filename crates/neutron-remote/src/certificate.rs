//! Deciding whether to trust an FTPS server's certificate.
//!
//! # Two ways in
//!
//! An FTPS certificate is accepted if either of these holds:
//!
//! * **A recognised authority signed it for this host name.** Checked against
//!   the bundled Mozilla roots (`webpki-roots`), exactly as before this module
//!   existed. A server with a real certificate never notices any of this.
//! * **It is byte-for-byte the certificate the user said to trust.** The
//!   SHA-256 fingerprint of the server's certificate equals
//!   [`Site::trusted_cert`](crate::site::Site::trusted_cert).
//!
//! The second exists because the FTPS servers people actually run at home — a
//! NAS, a router, a Raspberry Pi with vsftpd — present a certificate the box
//! generated for itself, which no authority signed and which therefore fails
//! the first check every time. Without a way to trust one, FTPS against such a
//! server is impossible, and the only alternative on offer is plain FTP, which
//! sends the password in the clear. That is a worse outcome than a pinned
//! self-signed certificate by any measure.
//!
//! # Why not trust on first use, as SFTP does
//!
//! SSH has no authorities, so recording the first key seen is the best anyone
//! can do — see [`crate::hostkey`]. TLS does have them, and a certificate that
//! fails verification may be failing for a reason worth hearing: expired,
//! issued for some other name, or presented by a machine in the middle. So the
//! first connection to such a server is *refused*, with an error that names the
//! fingerprint and says how to trust it. The user decides; Neutron never pins
//! anything by itself.
//!
//! # What a pin accepts
//!
//! Exactly one certificate. Not its issuer, not anything else with the same
//! name — a match on the whole certificate's hash. Hostname and expiry are not
//! checked for a pinned certificate, because the self-signed certificates this
//! is for routinely name `localhost` and last a year, and a pin of the exact
//! certificate is a stronger statement than either check would add. When the
//! server's certificate changes the pin stops matching and the connection is
//! refused again, naming both fingerprints.
//!
//! Fingerprints are written as colon-separated uppercase hex, which is what
//! `openssl x509 -noout -fingerprint -sha256` and every browser's certificate
//! viewer print, so the value can be checked against the server itself.

use std::sync::Arc;

use suppaftp::rustls;
use suppaftp::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use suppaftp::rustls::client::WebPkiServerVerifier;
use suppaftp::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use suppaftp::rustls::{DigitallySignedStruct, SignatureScheme};

/// The SHA-256 fingerprint of a DER-encoded certificate, as `AB:CD:…`.
pub fn fingerprint(der: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, der);
    digest
        .as_ref()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Reads a fingerprint the way people paste them, and returns it in the one
/// spelling Neutron stores and compares.
///
/// Accepts the bare hex, with or without colons or spaces, in either case, and
/// with the labels the usual tools put in front of it: `SHA256:`,
/// `SHA-256:` and OpenSSL's `sha256 Fingerprint=`. Anything that is not
/// exactly 32 bytes of hex once those are removed is refused rather than
/// guessed at — a truncated fingerprint is not a weaker pin, it is a mistake.
pub fn normalize(text: &str) -> Option<String> {
    let mut text = text.trim();
    // OpenSSL: "sha256 Fingerprint=AB:CD:…". Everything before the `=` is the
    // label; nothing after it can contain one.
    if let Some((_, rest)) = text.rsplit_once('=') {
        text = rest.trim();
    }
    for label in ["sha256:", "sha-256:"] {
        if text.len() >= label.len() && text[..label.len()].eq_ignore_ascii_case(label) {
            text = text[label.len()..].trim();
        }
    }

    let hex: String = text
        .chars()
        .filter(|c| *c != ':' && !c.is_whitespace())
        .collect();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let upper = hex.to_ascii_uppercase();
    Some(
        upper
            .as_bytes()
            .chunks(2)
            .map(|pair| String::from_utf8_lossy(pair).into_owned())
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// What the server presented, when it was refused. Written by the verifier
/// during the handshake and read by the caller afterwards, because rustls
/// reports a refusal as an opaque handshake error by the time it reaches
/// `suppaftp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub fingerprint: String,
    /// Why the authority check failed, in rustls's words.
    pub reason: String,
}

pub type RefusalSlot = Arc<parking_lot::Mutex<Option<Refusal>>>;

/// The certificate check for one FTPS connection.
#[derive(Debug)]
pub struct Verifier {
    authorities: Arc<WebPkiServerVerifier>,
    /// The site's pin, already in canonical form.
    trusted: Option<String>,
    refused: RefusalSlot,
}

impl Verifier {
    /// A verifier that trusts the bundled roots, plus `trusted` if given.
    pub fn new(trusted: Option<&str>) -> Result<(Verifier, RefusalSlot), String> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let authorities =
            WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider())
                .build()
                .map_err(|e| e.to_string())?;
        let refused = RefusalSlot::default();
        Ok((
            Verifier {
                authorities,
                // A pin that does not parse can never match, which is the
                // safe reading of a corrupted file.
                trusted: trusted.and_then(normalize),
                refused: Arc::clone(&refused),
            },
            refused,
        ))
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let presented = fingerprint(end_entity.as_ref());
        if self.trusted.as_deref() == Some(presented.as_str()) {
            return Ok(ServerCertVerified::assertion());
        }

        match self.authorities.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(verified) => Ok(verified),
            Err(e) => {
                *self.refused.lock() = Some(Refusal {
                    fingerprint: presented,
                    reason: e.to_string(),
                });
                Err(e)
            }
        }
    }

    // The handshake signatures are checked against the certificate's own key
    // whichever way the certificate was accepted. A pin says which key the
    // server must hold; this is what proves it actually holds it.
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.authorities.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.authorities.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.authorities.supported_verify_schemes()
    }
}

/// The crypto backend, named rather than taken from rustls's process default:
/// which default is installed depends on what else in the binary enabled which
/// rustls feature, and this crate deliberately links only `ring`.
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The TLS settings for one FTPS connection, and where a refusal is reported.
pub fn client_config(
    trusted: Option<&str>,
) -> Result<(rustls::ClientConfig, RefusalSlot), String> {
    let (verifier, refused) = Verifier::new(trusted)?;
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok((config, refused))
}

/// The message for a certificate that failed verification.
///
/// Names the fingerprint, because the user can only decide by comparing it
/// with what the server itself reports, and says where to put it, because an
/// error with no next step is one people work around by switching to plain
/// FTP.
pub fn untrusted_message(address: &str, trusted: Option<&str>, refusal: &Refusal) -> String {
    match trusted.and_then(normalize) {
        Some(expected) => format!(
            "The FTPS certificate for {address} has changed. \
             Expected {expected}, got {}. \
             A renewed self-signed certificate looks like this — so does a \
             machine impersonating the server. If you know the certificate was \
             replaced, edit the server and trust the new fingerprint; otherwise \
             do not connect.",
            refusal.fingerprint
        ),
        None => format!(
            "{address} presented a certificate no recognised authority vouches \
             for ({}). Its SHA-256 fingerprint is {}. If that matches the \
             certificate on the server — a NAS or router that made its own will \
             look like this — edit the server and trust this certificate.",
            refusal.reason, refusal.fingerprint
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "4f2a9c0e3b7d1856a0c4e2f19b3d7a6c5e8f0a1b2c3d4e5f60718293a4b5c6d7";

    fn canonical() -> String {
        normalize(HEX).unwrap()
    }

    #[test]
    fn a_fingerprint_looks_like_what_openssl_prints() {
        let fp = fingerprint(b"anything");
        assert_eq!(fp.len(), 32 * 3 - 1);
        assert!(fp.split(':').all(|pair| pair.len() == 2
            && pair.chars().all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())));
    }

    #[test]
    fn the_usual_ways_of_writing_a_fingerprint_all_read_the_same() {
        let colons = canonical();
        for pasted in [
            HEX.to_owned(),
            HEX.to_ascii_uppercase(),
            colons.clone(),
            colons.to_ascii_lowercase(),
            format!("sha256 Fingerprint={colons}"),
            format!("SHA256:{HEX}"),
            format!("  SHA-256: {colons}  "),
            colons.replace(':', " "),
        ] {
            assert_eq!(normalize(&pasted).as_deref(), Some(colons.as_str()), "{pasted}");
        }
    }

    #[test]
    fn a_truncated_or_garbled_fingerprint_is_refused_rather_than_guessed_at() {
        assert_eq!(normalize(&HEX[..62]), None);
        assert_eq!(normalize(&format!("{HEX}00")), None);
        assert_eq!(normalize(&HEX.replace('a', "g")), None);
        assert_eq!(normalize(""), None);
    }

    fn refusal() -> Refusal {
        Refusal { fingerprint: canonical(), reason: "UnknownIssuer".into() }
    }

    #[test]
    fn a_first_refusal_names_the_fingerprint_and_the_way_to_trust_it() {
        let message = untrusted_message("nas.local:21", None, &refusal());
        assert!(message.contains("nas.local:21"), "{message}");
        assert!(message.contains(&canonical()), "{message}");
        assert!(message.contains("trust"), "{message}");
    }

    #[test]
    fn a_changed_certificate_names_both_fingerprints() {
        let old = fingerprint(b"old");
        let message = untrusted_message("nas.local:21", Some(&old), &refusal());
        assert!(message.contains(&old), "{message}");
        assert!(message.contains(&canonical()), "{message}");
        assert!(message.contains("changed"), "{message}");
    }

    #[test]
    fn a_configuration_can_be_built_with_and_without_a_pin() {
        assert!(client_config(None).is_ok());
        assert!(client_config(Some(HEX)).is_ok());
    }
}
