//! An SSH server with an SFTP subsystem, for the integration tests.
//!
//! Shared by `sftp.rs` and `agent.rs`. The agent test lives in a binary of
//! its own because it has to point `SSH_AUTH_SOCK` at a test agent, and
//! changing the environment is only sound while no other thread is running.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use neutron_remote::site::{Protocol, Site};
use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    File, FileAttributes, Handle, Name, Status, StatusCode, Version,
};
use tokio::sync::Mutex;

/// One file or directory the fake server serves.
#[derive(Clone)]
pub struct Item {
    name: String,
    contents: Vec<u8>,
    is_dir: bool,
}

impl Item {
    pub fn file(name: &str, contents: &[u8]) -> Item {
        Item {
            name: name.to_owned(),
            contents: contents.to_vec(),
            is_dir: false,
        }
    }

    pub fn dir(name: &str) -> Item {
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
pub struct FakeSftp {
    pub addr: SocketAddr,
    /// What the client should see, so a test can assert on the recorded value.
    pub fingerprint: String,
    /// Write requests the server received, as `"verb path"`.
    writes: Arc<parking_lot::Mutex<Vec<String>>>,
    /// Whatever `write` received, byte for byte.
    written: Arc<parking_lot::Mutex<Vec<u8>>>,
}

impl FakeSftp {
    /// Starts a server using `key` as its host key, serving `items` in `/`.
    pub fn start(key: PrivateKey, items: Vec<Item>, password_ok: bool) -> FakeSftp {
        FakeSftp::start_accepting(key, items, password_ok, None)
    }

    /// As [`FakeSftp::start`], and also lets in a client that proves it holds
    /// the private half of `client_key`.
    pub fn start_accepting(
        key: PrivateKey,
        items: Vec<Item>,
        password_ok: bool,
        client_key: Option<PublicKey>,
    ) -> FakeSftp {
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
                    client_key,
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
    pub fn writes(&self) -> Vec<String> {
        self.writes.lock().clone()
    }

    pub fn written(&self) -> Vec<u8> {
        self.written.lock().clone()
    }

    pub fn site(&self, fingerprint: Option<&str>) -> Site {
        Site {
            id: "fake".into(),
            label: String::new(),
            protocol: Protocol::Sftp,
            host: self.addr.ip().to_string(),
            port: Some(self.addr.port()),
            username: "razeen".into(),
            path: None,
            fingerprint: fingerprint.map(str::to_owned),
            auth: Default::default(),
            trusted_cert: None,
        }
    }
}

#[derive(Clone)]
struct Server {
    items: Arc<Vec<Item>>,
    password_ok: bool,
    client_key: Option<PublicKey>,
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
            client_key: self.client_key.clone(),
            writes: Arc::clone(&self.writes),
            written: Arc::clone(&self.written),
        }
    }
}

struct SshSession {
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    items: Arc<Vec<Item>>,
    password_ok: bool,
    client_key: Option<PublicKey>,
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

    // russh checks the signature before calling this, so a match here means
    // the client holds the private key rather than merely naming the public
    // one.
    async fn auth_publickey(&mut self, _user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        if self.client_key.as_ref().is_some_and(|k| k.key_data() == key.key_data()) {
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

pub fn key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("key")
}
