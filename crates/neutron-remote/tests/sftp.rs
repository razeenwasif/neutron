//! The SFTP client against a real SSH server, over a real socket.
//!
//! The server is in `support/sftp_server.rs`, written by hand for the same
//! reason the FTP one is: the failures
//! worth catching are in the conversation, not in the parsing. And one of them
//! cannot be tested any other way — a server that presents a *different host
//! key* than the one recorded is the exact case host key checking exists for,
//! and you cannot ask a real server to do that on demand.
//!
//! So [`FakeSftp`] is an SSH server whose host key the test chooses.

#[path = "support/sftp_server.rs"]
mod support;

use neutron_remote::site::Auth;
use neutron_remote::transport::{Continue, RemoteError, Transport};
use russh::keys::{Algorithm, HashAlg, PrivateKey};
use support::{FakeSftp, Item, key};

// ---------------------------------------------------------------------------

#[test]
fn a_listing_becomes_entries_with_their_kinds_sizes_and_times() {
    let server = FakeSftp::start(
        key(),
        vec![Item::dir("photos"), Item::file("notes.txt", b"hello")],
        true,
    );

    let mut transport =
        neutron_remote::sftp::SftpTransport::connect(&server.site(None), "hunter2")
            .expect("should connect");

    let mut refused = Vec::new();
    let mut entries = transport.list("/", &mut refused).expect("should list");
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "notes.txt");
    assert_eq!(entries[0].kind, neutron_core::EntryKind::File);
    assert_eq!(entries[0].size, 5);
    // Seconds on the wire, milliseconds in Neutron.
    assert_eq!(entries[0].modified, 1_600_000_000_000);
    assert_eq!(entries[1].name, "photos");
    assert_eq!(entries[1].kind, neutron_core::EntryKind::Directory);
}

#[test]
fn a_listing_entry_that_climbs_out_of_the_download_folder_is_refused() {
    // The same attack as over FTP, and it has to be refused on both paths —
    // a check on one protocol is not a check.
    let server = FakeSftp::start(
        key(),
        vec![
            Item::file("../../../../etc/passwd", b"root:x:0:0"),
            Item::file("..\\..\\evil.exe", b"MZ"),
            Item::file("innocent.txt", b"ok"),
        ],
        true,
    );

    let mut transport =
        neutron_remote::sftp::SftpTransport::connect(&server.site(None), "hunter2")
            .expect("should connect");

    let mut refused = Vec::new();
    let entries = transport.list("/", &mut refused).expect("should list");

    assert_eq!(refused.len(), 2, "{refused:?}");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["innocent.txt"]);
}

#[test]
fn a_download_arrives_byte_for_byte() {
    let payload: Vec<u8> = (0..=255u8).cycle().take(200_000).collect();
    let server = FakeSftp::start(key(), vec![Item::file("big.bin", &payload)], true);

    let mut transport =
        neutron_remote::sftp::SftpTransport::connect(&server.site(None), "hunter2")
            .expect("should connect");

    let mut sink = Vec::new();
    let written = transport
        .download("/big.bin", &mut sink, &mut |_| Continue::Yes)
        .expect("should download");

    assert_eq!(written as usize, payload.len());
    assert_eq!(sink, payload);
}

#[test]
fn a_download_can_be_stopped() {
    let payload: Vec<u8> = vec![9u8; 300_000];
    let server = FakeSftp::start(key(), vec![Item::file("big.bin", &payload)], true);

    let mut transport =
        neutron_remote::sftp::SftpTransport::connect(&server.site(None), "hunter2")
            .expect("should connect");

    let mut sink = Vec::new();
    let outcome = transport.download("/big.bin", &mut sink, &mut |_| Continue::Stop);

    assert!(matches!(outcome, Err(RemoteError::Cancelled)), "{outcome:?}");
    assert!(sink.len() < payload.len());
}

#[test]
fn a_first_connection_records_the_host_key() {
    let server = FakeSftp::start(key(), vec![Item::file("a.txt", b"a")], true);

    let transport = neutron_remote::sftp::SftpTransport::connect(&server.site(None), "hunter2")
        .expect("should connect");

    // Reported so the caller can save it against the site, and it is the
    // server's actual key rather than something invented.
    assert_eq!(transport.new_fingerprint(), Some(server.fingerprint.as_str()));
}

#[test]
fn a_known_host_key_connects_and_is_not_recorded_again() {
    let server = FakeSftp::start(key(), vec![Item::file("a.txt", b"a")], true);

    let site = server.site(Some(&server.fingerprint));
    let transport =
        neutron_remote::sftp::SftpTransport::connect(&site, "hunter2").expect("should connect");

    assert_eq!(transport.new_fingerprint(), None);
}

#[test]
fn a_server_presenting_a_different_host_key_is_refused() {
    // The case the whole mechanism exists for, and the one that cannot be
    // tested against a real server: same site, same credentials, different key.
    let server = FakeSftp::start(key(), vec![Item::file("a.txt", b"a")], true);

    let someone_else = key()
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();
    let site = server.site(Some(&someone_else));

    let Err(error) = neutron_remote::sftp::SftpTransport::connect(&site, "hunter2") else {
        panic!("a changed host key must not connect");
    };

    let RemoteError::HostKeyChanged(message) = &error else {
        panic!("expected a host key failure, got {error:?}");
    };
    // Both fingerprints, because a user can only judge this by comparing them.
    assert!(message.contains(&someone_else), "{message}");
    assert!(message.contains(&server.fingerprint), "{message}");
}

#[test]
fn a_rejected_password_is_reported_as_an_authentication_failure() {
    let server = FakeSftp::start(key(), vec![], false);

    let Err(error) = neutron_remote::sftp::SftpTransport::connect(&server.site(None), "wrong")
    else {
        panic!("a rejected password should not connect");
    };
    assert!(matches!(error, RemoteError::Auth { .. }), "{error:?}");
}

#[test]
fn one_connection_serves_several_operations() {
    let server = FakeSftp::start(
        key(),
        vec![Item::file("a.txt", b"aaa"), Item::file("b.txt", b"bbbb")],
        true,
    );

    let mut transport =
        neutron_remote::sftp::SftpTransport::connect(&server.site(None), "hunter2")
            .expect("should connect");

    // Opening a connection costs a key exchange, so it is made once and reused.
    for _ in 0..3 {
        let entries = transport.list("/", &mut Vec::new()).expect("list");
        assert_eq!(entries.len(), 2);
    }

    let mut a = Vec::new();
    transport
        .download("/a.txt", &mut a, &mut |_| Continue::Yes)
        .expect("download a");
    let mut b = Vec::new();
    transport
        .download("/b.txt", &mut b, &mut |_| Continue::Yes)
        .expect("download b");

    assert_eq!(a, b"aaa");
    assert_eq!(b, b"bbbb");
}

// --- writing ---------------------------------------------------------------
//
// The server answers Ok to every one of these, which is the point: what a
// client gets wrong is not the verb but the *path*, and its own return value
// cannot tell you about that. Each test asserts what actually reached the
// server.

fn writer(server: &FakeSftp) -> neutron_remote::sftp::SftpTransport {
    neutron_remote::sftp::SftpTransport::connect(&server.site(None), "hunter2")
        .expect("should connect")
}

#[test]
fn creating_a_directory_asks_for_the_full_path() {
    let server = FakeSftp::start(key(), Vec::new(), true);
    let mut t = writer(&server);
    t.create_dir("/pub/new folder").expect("mkdir");
    assert!(
        server.writes().contains(&"mkdir /pub/new folder".to_owned()),
        "{:?}",
        server.writes()
    );
}

/// A file and a directory take different requests over SFTP too, and sending
/// `remove` for a directory fails on a real server.
#[test]
fn a_file_and_a_directory_are_removed_with_different_requests() {
    let server = FakeSftp::start(key(), Vec::new(), true);
    let mut t = writer(&server);
    t.remove_file("/pub/notes.txt").expect("remove");
    t.remove_dir("/pub/empty").expect("rmdir");

    let w = server.writes();
    assert!(w.contains(&"remove /pub/notes.txt".to_owned()), "{w:?}");
    assert!(w.contains(&"rmdir /pub/empty".to_owned()), "{w:?}");
}

/// Both paths, in the right order. A rename that swapped them would move the
/// destination onto the source and destroy the file it was meant to keep.
#[test]
fn a_rename_carries_both_paths_the_right_way_round() {
    let server = FakeSftp::start(key(), Vec::new(), true);
    let mut t = writer(&server);
    t.rename("/pub/old.txt", "/pub/new.txt").expect("rename");
    assert!(
        server
            .writes()
            .contains(&"rename /pub/old.txt -> /pub/new.txt".to_owned()),
        "{:?}",
        server.writes()
    );
}

#[test]
fn an_upload_arrives_byte_for_byte() {
    let server = FakeSftp::start(key(), Vec::new(), true);
    let mut t = writer(&server);

    // Larger than one chunk, so the loop runs more than once and an
    // off-by-one in the offset would show up as a short or duplicated write.
    let payload: Vec<u8> = (0..=255u8).cycle().take(200_000).collect();
    let mut source = std::io::Cursor::new(payload.clone());
    let written = t
        .upload("/pub/blob.bin", &mut source, &mut |_| Continue::Yes)
        .expect("upload");

    assert_eq!(written, payload.len() as u64);
    assert_eq!(server.written(), payload);
}

#[test]
fn progress_is_reported_and_an_upload_can_be_stopped() {
    let server = FakeSftp::start(key(), Vec::new(), true);
    let mut t = writer(&server);

    let payload = vec![b'x'; 300_000];
    let mut source = std::io::Cursor::new(payload);
    let mut seen = Vec::new();
    let outcome = t.upload("/pub/big.bin", &mut source, &mut |done| {
        seen.push(done);
        // Stop after the first chunk: a cancel that only takes effect at the
        // end is not a cancel.
        Continue::Stop
    });

    assert!(
        matches!(outcome, Err(neutron_remote::RemoteError::Cancelled)),
        "{outcome:?}"
    );
    assert_eq!(seen.len(), 1, "the transfer kept going after Stop: {seen:?}");
    assert!(seen[0] > 0, "progress reported nothing");
    assert!(
        server.written().len() < 300_000,
        "a stopped upload sent the whole file"
    );
}

// --- key login ---------------------------------------------------------------
//
// Every one of these servers refuses all passwords, so a connection that gets
// in can only have done it with the key.

/// A private key written to a file of its own, removed when the test ends.
struct KeyFile(std::path::PathBuf);

impl KeyFile {
    fn write(key: &PrivateKey) -> KeyFile {
        let text = key
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("serialise key");
        let path = std::env::temp_dir().join(format!(
            "neutron-test-key-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::write(&path, text.as_bytes()).expect("write key");
        KeyFile(path)
    }

    fn site(&self, server: &FakeSftp) -> neutron_remote::Site {
        let mut site = server.site(None);
        site.auth = Auth::Key {
            path: self.0.to_string_lossy().into_owned(),
        };
        site
    }
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn client_key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("key")
}

#[test]
fn an_unencrypted_key_file_logs_in() {
    let client = client_key();
    let server = FakeSftp::start_accepting(
        key(),
        vec![Item::file("a.txt", b"a")],
        false,
        Some(client.public_key().clone()),
    );
    let file = KeyFile::write(&client);

    // No secret at all: an unencrypted key needs none.
    let mut transport = neutron_remote::sftp::SftpTransport::connect(&file.site(&server), "")
        .expect("the key should be accepted");
    assert_eq!(transport.list("/", &mut Vec::new()).expect("list").len(), 1);
}

#[test]
fn an_encrypted_key_file_is_opened_with_the_saved_secret() {
    let client = client_key();
    let server =
        FakeSftp::start_accepting(key(), Vec::new(), false, Some(client.public_key().clone()));
    let encrypted = client
        .encrypt(&mut rand::rng(), "correct horse")
        .expect("encrypt");
    let file = KeyFile::write(&encrypted);
    let site = file.site(&server);

    neutron_remote::sftp::SftpTransport::connect(&site, "correct horse")
        .expect("the passphrase should decrypt the key");

    // No passphrase saved: the same error as a missing password, so the app
    // sends the user to the same place to type one in — and nothing is dialled.
    let Err(error) = neutron_remote::sftp::SftpTransport::connect(&site, "") else {
        panic!("an encrypted key cannot be used without its passphrase");
    };
    assert!(matches!(error, RemoteError::NoPassword { .. }), "{error:?}");

    let Err(error) = neutron_remote::sftp::SftpTransport::connect(&site, "wrong") else {
        panic!("the wrong passphrase must not decrypt the key");
    };
    assert!(matches!(error, RemoteError::Key { .. }), "{error:?}");
}

#[test]
fn a_key_the_server_does_not_know_is_an_authentication_failure() {
    let server =
        FakeSftp::start_accepting(key(), Vec::new(), false, Some(client_key().public_key().clone()));
    let file = KeyFile::write(&client_key());

    let Err(error) = neutron_remote::sftp::SftpTransport::connect(&file.site(&server), "") else {
        panic!("an unknown key should not log in");
    };
    assert!(matches!(error, RemoteError::Auth { .. }), "{error:?}");
}

#[test]
fn a_missing_key_file_is_reported_by_path_without_dialling() {
    let server = FakeSftp::start(key(), Vec::new(), false);
    let mut site = server.site(None);
    site.auth = Auth::Key {
        path: "/nonexistent/neutron/id_ed25519".into(),
    };

    let Err(error) = neutron_remote::sftp::SftpTransport::connect(&site, "") else {
        panic!("there is no key to log in with");
    };
    let RemoteError::Key { path, .. } = &error else {
        panic!("expected a key file error, got {error:?}");
    };
    assert_eq!(path, "/nonexistent/neutron/id_ed25519");
}
