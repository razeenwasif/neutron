//! Deciding whether to trust an SSH server's host key.
//!
//! # The problem this solves
//!
//! SSH encrypts the connection, but encryption alone says nothing about *who*
//! is on the other end. Without checking the host key, a machine on the path
//! can present its own key, accept the password, and forward everything on —
//! and the client shows a perfectly ordinary successful login. Verifying the
//! key is the entire defence, which is why every SSH client that skips it is
//! broken no matter how good its cryptography is.
//!
//! Plenty of libraries' example code answers `check_server_key` with
//! `Ok(true)`. That is the bug, written down.
//!
//! # What Neutron does
//!
//! Trust on first use, recorded, and enforced afterwards — the same model
//! OpenSSH uses:
//!
//! * **First connection**: no fingerprint saved. Accept, record it in the site,
//!   and say so, naming the fingerprint. A user who has one to compare against
//!   can compare it; one who does not is no worse off than with `ssh`.
//! * **Every connection after**: the fingerprint must match. It usually will.
//! * **Mismatch**: refuse, and show both fingerprints. This is the case the
//!   whole mechanism exists for, and it is the one case that must never be a
//!   warning the user can click past without noticing.
//!
//! Fingerprints are `SHA256:` base64, byte-for-byte what OpenSSH prints, so
//! `ssh-keyscan host | ssh-keygen -lf -` produces something directly
//! comparable. A format only Neutron understands would make the value
//! unverifiable, which defeats the point of showing it.

/// What to do about a host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Known and unchanged.
    Known,
    /// Never seen before. Connect, and record this fingerprint against the site.
    Record { fingerprint: String },
    /// The server is presenting a different key to the one recorded.
    Changed { expected: String, actual: String },
}

/// Compares a presented fingerprint against what was recorded for the site.
///
/// `expected` is `None` for a site never connected to before.
pub fn decide(expected: Option<&str>, presented: &str) -> Decision {
    match expected {
        None => Decision::Record { fingerprint: presented.to_owned() },
        // Trimmed because these get pasted in by hand from `ssh-keygen -lf`,
        // which prints the fingerprint in a line with other fields around it.
        Some(known) if known.trim() == presented.trim() => Decision::Known,
        Some(known) => Decision::Changed {
            expected: known.trim().to_owned(),
            actual: presented.to_owned(),
        },
    }
}

/// The message shown when a key does not match.
///
/// Deliberately blunt and deliberately long. Every other message in Neutron is
/// one line; this one is the only place where the right outcome is that the
/// user stops and thinks.
pub fn changed_message(address: &str, expected: &str, actual: &str) -> String {
    format!(
        "The host key for {address} has changed. \
         Expected {expected}, got {actual}. \
         This is what a machine impersonating the server looks like — \
         it is also what a rebuilt server looks like. \
         If you know the server was rebuilt, clear the fingerprint for this \
         site in sites.json; otherwise do not connect."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "SHA256:LP3Ek7Ur2eSm8YyPuT6BpYNSDe9jsi0DmvT6qJHfKcU";
    const B: &str = "SHA256:9dEYVmZ0uW1FaBiLp2HxKq7TnGcRsVjXoM4eZ8yQwAs";

    #[test]
    fn a_first_connection_records_the_key() {
        assert_eq!(decide(None, A), Decision::Record { fingerprint: A.to_owned() });
    }

    #[test]
    fn a_matching_key_is_known() {
        assert_eq!(decide(Some(A), A), Decision::Known);
    }

    #[test]
    fn a_changed_key_is_refused_and_names_both() {
        // The one case the mechanism exists for. Both fingerprints are carried
        // because a user can only judge this by comparing them.
        let decision = decide(Some(A), B);
        assert_eq!(
            decision,
            Decision::Changed { expected: A.to_owned(), actual: B.to_owned() }
        );
    }

    #[test]
    fn whitespace_around_a_pasted_fingerprint_does_not_cause_a_false_alarm() {
        // These are pasted by hand out of `ssh-keygen -lf` output. A trailing
        // space must not read as an impersonated server — crying wolf here is
        // how a real warning gets ignored.
        assert_eq!(decide(Some("  "), "  "), Decision::Known);
        assert_eq!(decide(Some(&format!(" {A} ")), A), Decision::Known);
    }

    #[test]
    fn the_comparison_is_exact_rather_than_a_prefix() {
        // A prefix match would accept a key whose fingerprint merely starts the
        // same way, which is exactly the thing an attacker would grind for.
        let truncated = &A[..A.len() - 4];
        assert!(matches!(decide(Some(A), truncated), Decision::Changed { .. }));
        assert!(matches!(decide(Some(truncated), A), Decision::Changed { .. }));
    }

    #[test]
    fn the_mismatch_message_names_the_server_and_both_keys() {
        let message = changed_message("nas.local:22", A, B);
        assert!(message.contains("nas.local:22"));
        assert!(message.contains(A));
        assert!(message.contains(B));
        // And says what to do about it, because "host key mismatch" alone
        // leaves a user with no next step but to give up or ignore it.
        assert!(message.contains("sites.json"));
    }
}
