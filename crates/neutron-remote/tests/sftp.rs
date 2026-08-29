//! The SFTP client against a real SSH server, over a real socket.
//!
//! The server is in this file, for the same reason the FTP one is: the failures
//! worth catching are in the conversation, not in the parsing. And one of them
//! cannot be tested any other way — a server that presents a *different host
//! key* than the one recorded is the exact case host key checking exists for,
//! and you cannot ask a real server to do that on demand.
//!
//! So [`FakeSftp`] is an SSH server whose host key the test chooses.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use neutron_remote::site::{Protocol, Site};
use neutron_remote::transport::{Continue, RemoteError, Transport};
use russh::keys::{Algorithm, HashAlg, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    File, FileAttributes, Handle, Name, Status, StatusCode, Version,
};
use tokio::sync::Mutex;

/// One file or directory the fake server serves.
#[derive(Clone)]
struct Item {
    name: String,
    contents: Vec<u8>,
    is_dir: bool,
}

impl Item {
    fn file(name: &str, contents: &[u8]) -> Item {
        Item {
            name: name.to_owned(),
            contents: contents.to_vec(),
            is_dir: false,
        }
    }

    fn dir(name: &str) -> Item {
        Item {
            name: name.to_owned(),
            contents: Vec::new(),
            is_dir: true,
        }
    }

    fn attributes(&self) -> FileAttributes {
        let mut attrs = FileAttributes {
            size: Some(self.contents.len() as u64),
            mtime: Some(1_600_000_000),
            ..FileAttributes::default()
        };
        if self.is_dir {
            attrs.set_dir(true);
        } else {
            attrs.set_regular(true);
        }
        attrs
    }
}

/// A running SSH server with an SFTP subsystem.
struct FakeSftp {
    addr: SocketAddr,
    /// What the client should see, so a test can assert on the recorded value.
    fingerprint: String,
    /// Write requests the server received, as `"verb path"`.
    writes: Arc<parking_lot::Mutex<Vec<String>>>,
    /// Whatever `write` received, byte for byte.
    written: Arc<parking_lot::Mutex<Vec<u8>>>,
}

impl FakeSftp {
    /// Starts a server using `key` as its host key, serving `items` in `/`.
    fn start(key: PrivateKey, items: Vec<Item>, password_ok: bool) -> FakeSftp {
        let fingerprint = key
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string();

        // Bound here rather than inside the server so the port is known before
        // this function returns and the test never races the listener.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        listener.set_nonblocking(true).expect("nonblocking");

        let writes: Arc<parking_lot::Mutex<Vec<String>>> = Arc::new(Default::default());
        let written: Arc<parking_lot::Mutex<Vec<u8>>> = Arc::new(Default::default());
        let server_writes = Arc::clone(&writes);
        let server_written = Arc::clone(&written);

        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");

            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).expect("from_std");
                let config = Arc::new(russh::server::Config {
                    keys: vec![key],
                    auth_rejection_time: std::time::Duration::from_millis(50),
                    auth_rejection_time_initial: Some(std::time::Duration::ZERO),
                    ..Default::default()
                });
                let mut server = Server {
                    items: Arc::new(items),
                    password_ok,
                    writes: server_writes,
                    written: server_written,
                };
                let running = server.run_on_socket(config, &listener);
                let _ = running.await;
            });
        });

        FakeSftp {
            addr,
            fingerprint,
            writes,
            written,
        }
    }

    /// Every write request the server received, as `"verb path"`.
    fn writes(&self) -> Vec<String> {
        self.writes.lock().clone()
    }

    fn written(&self) -> Vec<u8> {
        self.written.lock().clone()
    }

    fn site(&self, fingerprint: Option<&str>) -> Site {
        Site {
            id: "fake".into(),
            label: String::new(),
            protocol: Protocol::Sftp,
            host: self.addr.ip().to_string(),
            port: Some(self.addr.port()),
            username: "razeen".into(),
            path: None,
            fingerprint: fingerprint.map(str::to_owned),
        }
    }
}

#[derive(Clone)]
struct Server {
    items: Arc<Vec<Item>>,
    password_ok: bool,
    writes: Arc<parking_lot::Mutex<Vec<String>>>,
    written: Arc<parking_lot::Mutex<Vec<u8>>>,
}

impl russh::server::Server for Server {
    type Handler = SshSession;

    fn new_client(&mut self, _: Option<SocketAddr>) -> SshSession {
        SshSession {
            channels: Arc::new(Mutex::new(HashMap::new())),
            items: Arc::clone(&self.items),
            password_ok: self.password_ok,
            writes: Arc::clone(&self.writes),
            written: Arc::clone(&self.written),
        }
    }
}

struct SshSession {
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    items: Arc<Vec<Item>>,
    password_ok: bool,
    writes: Arc<parking_lot::Mutex<Vec<String>>>,
    written: Arc<parking_lot::Mutex<Vec<u8>>>,
}

impl russh::server::Handler for SshSession {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<Auth, Self::Error> {
        if self.password_ok {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(channel_id)?;
            return Ok(());
        }

        let channel = self
            .channels
            .lock()
            .await
            .remove(&channel_id)
            .expect("the channel should have been opened");
        session.channel_success(channel_id)?;
        russh_sftp::server::run(
            channel.into_stream(),
            SftpHandler {
                items: Arc::clone(&self.items),
                writes: Arc::clone(&self.writes),
                written: Arc::clone(&self.written),
                read_dirs: HashSet::new(),
                read_files: HashSet::new(),
            },
        )
        .await;
        Ok(())
    }
}

/// An SFTP server over a flat list of items in `/`.
struct SftpHandler {
    items: Arc<Vec<Item>>,
    /// Every write request the server was asked to perform, as
    /// `"verb path"`, in order. What a write test actually asserts is that the
    /// right request reached the server with the right path — the client's own
    /// return value cannot tell you that.
    writes: Arc<parking_lot::Mutex<Vec<String>>>,
    /// Whatever `write` received, so an upload can be checked byte for byte.
    written: Arc<parking_lot::Mutex<Vec<u8>>>,
    /// Directory handles already drained, so the second `readdir` says EOF.
    read_dirs: HashSet<String>,
    /// File handles already drained, likewise.
    read_files: HashSet<String>,
}

impl SftpHandler {
    fn find(&self, path: &str) -> Option<&Item> {
        let name = path.trim_start_matches('/');
        self.items.iter().find(|i| i.name == name)
    }

    fn note(&self, verb: &str, path: &str) {
        self.writes.lock().push(format!("{verb} {path}"));
    }

    fn ok(&self, id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_owned(),
            language_tag: "en-US".to_owned(),
        }
    }
}

impl russh_sftp::server::Handler for SftpHandler {
    type Error = StatusCode;

    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn close(&mut self, id: u32, _handle: String) -> Result<Status, Self::Error> {
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_owned(),
            language_tag: "en-US".to_owned(),
        })
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        Ok(Name {
            id,
            files: vec![File::dummy(&path)],
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        self.read_dirs.remove(&path);
        Ok(Handle { id, handle: path })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        if !self.read_dirs.insert(handle) {
            // Everything has been sent; EOF is how a listing ends.
            return Err(StatusCode::Eof);
        }
        Ok(Name {
            id,
            files: self
                .items
                .iter()
                .map(|item| File::new(&item.name, item.attributes()))
                .collect(),
        })
    }


    // --- the write half -----------------------------------------------------
    //
    // Each one records the request and succeeds. What these tests are for is
    // the path: a client that sends the right verb to the wrong place has no
    // way of noticing, because the server answers Ok either way.

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        self.note("mkdir", &path);
        Ok(self.ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        self.note("rmdir", &path);
        Ok(self.ok(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        self.note("remove", &filename);
        Ok(self.ok(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        self.note("rename", &format!("{oldpath} -> {newpath}"));
        Ok(self.ok(id))
    }

    async fn write(
        &mut self,
        id: u32,
        _handle: String,
        _offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        self.written.lock().extend_from_slice(&data);
        Ok(self.ok(id))
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: russh_sftp::protocol::OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        // An upload opens a file that does not exist yet, which is exactly the
        // case a read-only fake refuses. The flags are what distinguishes the
        // two, and getting this wrong here would make every upload test fail
        // for a reason that has nothing to do with the client.
        if flags.contains(russh_sftp::protocol::OpenFlags::WRITE)
            || flags.contains(russh_sftp::protocol::OpenFlags::CREATE)
        {
            self.note("open-write", &filename);
            return Ok(Handle { id, handle: filename });
        }
        match self.find(&filename) {
            Some(_) => {
                self.read_files.remove(&filename);
                Ok(Handle { id, handle: filename })
            }
            None => Err(StatusCode::NoSuchFile),
        }
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<russh_sftp::protocol::Data, Self::Error> {
        let item = self.find(&handle).ok_or(StatusCode::NoSuchFile)?;
        let start = offset as usize;
        if start >= item.contents.len() {
            return Err(StatusCode::Eof);
        }
        let end = (start + len as usize).min(item.contents.len());
        Ok(russh_sftp::protocol::Data {
            id,
            data: item.contents[start..end].to_vec(),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        let item = self.find(&handle).ok_or(StatusCode::NoSuchFile)?;
        Ok(russh_sftp::protocol::Attrs {
            id,
            attrs: item.attributes(),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        let item = self.find(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(russh_sftp::protocol::Attrs {
            id,
            attrs: item.attributes(),
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        self.stat(id, path).await
    }
}

fn key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("key")
}

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
