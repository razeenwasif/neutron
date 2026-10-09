//! The FTP client against a real FTP server, spoken over real sockets.
//!
//! The server is in this file. That is deliberate: the interesting failures in
//! an FTP client are not in the parsing, they are in the conversation — a data
//! connection left open desynchronising the control channel, a listing format
//! the server chose, a transfer mode that silently corrupts bytes. A mock of
//! the client's own abstraction cannot exercise any of that, and a public test
//! server cannot be made to misbehave on demand.
//!
//! So [`FakeFtp`] speaks enough of RFC 959 and RFC 3659 to be a server, records
//! every command it receives, and can be told to lie.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use neutron_remote::site::{Protocol, Site};
use neutron_remote::transport::{Continue, RemoteError, Transport};
use parking_lot::Mutex;

/// How the fake server should behave.
#[derive(Clone)]
struct Config {
    /// Whether `FEAT` advertises MLSD. Decides which listing the client asks
    /// for, which is the branch worth testing both sides of.
    mlsd: bool,
    /// Lines the listing command answers with, verbatim.
    listing: Vec<String>,
    /// Bytes `RETR` answers with.
    file: Vec<u8>,
    /// Whether `PASS` succeeds.
    password_ok: bool,
    /// A verb the server answers `550` to, so the client's handling of a
    /// refusal can be tested without a real permission problem.
    refuse: Option<String>,
    /// Whether `FEAT` advertises EPSV and the server answers it.
    epsv: bool,
    /// The host the `PASV` reply advertises, in place of the address the
    /// server is really listening on — what a server behind NAT sends, and
    /// what a hostile one would send to aim the data connection elsewhere.
    pasv_host: Option<[u8; 4]>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mlsd: true,
            listing: Vec::new(),
            file: Vec::new(),
            password_ok: true,
            refuse: None,
            epsv: false,
            pasv_host: None,
        }
    }
}

struct FakeFtp {
    addr: SocketAddr,
    /// Every command line the server received, in order.
    log: Arc<Mutex<Vec<String>>>,
    /// Whatever `STOR` received, so an upload can be checked byte for byte.
    stored: Arc<Mutex<Vec<u8>>>,
}

impl FakeFtp {
    /// Starts a server that serves exactly one client, then exits.
    fn start(config: Config) -> FakeFtp {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let log = Arc::new(Mutex::new(Vec::new()));

        let stored = Arc::new(Mutex::new(Vec::new()));
        let worker_log = Arc::clone(&log);
        let worker_stored = Arc::clone(&stored);
        std::thread::spawn(move || {
            if let Ok((control, _)) = listener.accept() {
                serve(control, config, worker_log, worker_stored);
            }
        });

        FakeFtp { addr, log, stored }
    }

    fn commands(&self) -> Vec<String> {
        self.log.lock().clone()
    }

    fn stored(&self) -> Vec<u8> {
        self.stored.lock().clone()
    }

    fn site(&self, username: &str) -> Site {
        Site {
            id: "fake".into(),
            label: String::new(),
            protocol: Protocol::Ftp,
            host: self.addr.ip().to_string(),
            port: Some(self.addr.port()),
            username: username.into(),
            path: None,
            fingerprint: None,
            auth: Default::default(),
            trusted_cert: None,
        }
    }
}

/// One client's whole session.
fn serve(
    control: TcpStream,
    config: Config,
    log: Arc<Mutex<Vec<String>>>,
    stored: Arc<Mutex<Vec<u8>>>,
) {
    let mut writer = control.try_clone().expect("clone");
    let mut reader = BufReader::new(control);

    let say = |w: &mut TcpStream, line: &str| {
        let _ = w.write_all(format!("{line}\r\n").as_bytes());
        let _ = w.flush();
    };

    say(&mut writer, "220 Fake FTP ready");

    // Opened by PASV, consumed by the next transfer command.
    let mut pending_data: Option<TcpListener> = None;

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let line = line.trim_end_matches(['\r', '\n']).to_owned();
        if line.is_empty() {
            continue;
        }
        log.lock().push(line.clone());

        let (verb, argument) = match line.split_once(' ') {
            Some((verb, rest)) => (verb.to_ascii_uppercase(), rest.to_owned()),
            None => (line.to_ascii_uppercase(), String::new()),
        };

        if config.refuse.as_deref() == Some(verb.as_str()) {
            say(&mut writer, "550 Permission denied");
            continue;
        }

        match verb.as_str() {
            "USER" => say(&mut writer, "331 Password required"),
            "PASS" => {
                if config.password_ok {
                    say(&mut writer, "230 Logged in");
                } else {
                    say(&mut writer, "530 Login incorrect");
                }
            }
            "TYPE" => say(&mut writer, "200 Type set"),
            "FEAT" => {
                let _ = writer.write_all(b"211-Features:\r\n");
                if config.mlsd {
                    let _ = writer.write_all(b" MLST type*;size*;modify*;\r\n");
                    let _ = writer.write_all(b" MLSD\r\n");
                } else {
                    let _ = writer.write_all(b" UTF8\r\n");
                }
                if config.epsv {
                    let _ = writer.write_all(b" EPSV\r\n");
                }
                say(&mut writer, "211 End");
            }
            "PWD" => say(&mut writer, "257 \"/\" is the current directory"),
            "CWD" => say(&mut writer, "250 Directory changed"),
            "PASV" => {
                let data = TcpListener::bind(("127.0.0.1", 0)).expect("bind data");
                let port = data.local_addr().expect("addr").port();
                pending_data = Some(data);
                let [a, b, c, d] = config.pasv_host.unwrap_or([127, 0, 0, 1]);
                say(
                    &mut writer,
                    &format!(
                        "227 Entering Passive Mode ({a},{b},{c},{d},{},{})",
                        port >> 8,
                        port & 0xFF
                    ),
                );
            }
            "EPSV" if config.epsv => {
                let data = TcpListener::bind(("127.0.0.1", 0)).expect("bind data");
                let port = data.local_addr().expect("addr").port();
                pending_data = Some(data);
                say(
                    &mut writer,
                    &format!("229 Entering Extended Passive Mode (|||{port}|)"),
                );
            }
            "LIST" | "MLSD" => {
                let payload = config
                    .listing
                    .iter()
                    .map(|l| format!("{l}\r\n"))
                    .collect::<String>();
                transfer(&mut writer, &mut pending_data, payload.as_bytes());
            }
            "RETR" => {
                let _ = argument;
                transfer(&mut writer, &mut pending_data, &config.file);
            }
            // The write half. Answers are the ones a real server gives, because
            // the client branches on the reply code.
            "MKD" => say(&mut writer, "257 Directory created"),
            "DELE" | "RMD" => say(&mut writer, "250 Deleted"),
            "RNFR" => say(&mut writer, "350 Ready for destination"),
            "RNTO" => say(&mut writer, "250 Rename successful"),
            "STOR" => receive(&mut writer, &mut pending_data, &stored),
            "QUIT" => {
                say(&mut writer, "221 Goodbye");
                return;
            }
            _ => say(&mut writer, "502 Command not implemented"),
        }
    }
}

/// Reads the pending data connection to its end, with the two responses a
/// client expects around it.
///
/// The closing `226` is what `finalize_put_stream` waits for. A fake that
/// skipped it would leave the client blocked and the test would hang rather
/// than fail, so it is sent even when nothing arrived.
fn receive(
    control: &mut TcpStream,
    pending: &mut Option<TcpListener>,
    stored: &Mutex<Vec<u8>>,
) {
    let Some(listener) = pending.take() else {
        let _ = control.write_all(b"425 Use PASV first
");
        return;
    };
    let _ = control.write_all(b"150 Opening data connection
");

    let mut sink = Vec::new();
    if let Ok((mut data, _)) = listener.accept() {
        use std::io::Read;
        let _ = data.read_to_end(&mut sink);
    }

    // Stored *before* the completion reply, not after. The client returns as
    // soon as it reads 226, so storing afterwards leaves a window in which the
    // test can read an empty buffer — which it did, but only under load, which
    // is the worst kind of flake to leave in a suite.
    *stored.lock() = sink;
    let _ = control.write_all(b"226 Transfer complete
");
}

/// Sends `payload` over the pending data connection, with the two responses a
/// client expects around it.
fn transfer(control: &mut TcpStream, pending: &mut Option<TcpListener>, payload: &[u8]) {
    let Some(listener) = pending.take() else {
        let _ = control.write_all(b"425 Use PASV first\r\n");
        return;
    };

    let _ = control.write_all(b"150 Opening data connection\r\n");
    let _ = control.flush();

    if let Ok((mut data, _)) = listener.accept() {
        let _ = data.write_all(payload);
        let _ = data.flush();
        // Closing is what tells the client the transfer ended.
        drop(data);
    }

    let _ = control.write_all(b"226 Transfer complete\r\n");
    let _ = control.flush();
}

/// Connects to the fake server and lists `/`.
fn list(server: &FakeFtp, refused: &mut Vec<String>) -> Vec<neutron_remote::RemoteEntry> {
    let site = server.site("razeen");
    let mut transport =
        neutron_remote::ftp::FtpTransport::connect(&site, "hunter2").expect("should connect");
    transport.list("/", refused).expect("should list")
}

#[test]
fn an_mlsd_listing_becomes_entries() {
    let server = FakeFtp::start(Config {
        mlsd: true,
        listing: vec![
            "type=cdir;modify=20201019151930; /".into(),
            "type=pdir;modify=20201019151930; /".into(),
            "type=dir;modify=20201019151930; photos".into(),
            "type=file;size=1234;modify=20201019151930; notes.txt".into(),
        ],
        ..Config::default()
    });

    let mut refused = Vec::new();
    let entries = list(&server, &mut refused);

    assert!(refused.is_empty(), "{refused:?}");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    // cdir and pdir are this directory and its parent, and are not entries.
    assert_eq!(names, ["photos", "notes.txt"]);
    assert_eq!(entries[0].kind, neutron_core::EntryKind::Directory);
    assert_eq!(entries[1].size, 1234);
    // MLSD carries an exact time rather than one inferred from today's date.
    assert!(entries[1].modified > 0);
}

#[test]
fn a_server_without_mlsd_falls_back_to_parsing_list() {
    let server = FakeFtp::start(Config {
        mlsd: false,
        listing: vec![
            "total 48".into(),
            "drwxr-xr-x 2 user group 4096 Nov  5 13:46 .".into(),
            "drwxr-xr-x 2 user group 4096 Nov  5 13:46 ..".into(),
            "drwxr-xr-x 2 user group 4096 Nov  5 13:46 photos".into(),
            "-rw-r--r-- 1 user group 1234 Nov  5 13:46 notes.txt".into(),
        ],
        ..Config::default()
    });

    let mut refused = Vec::new();
    let entries = list(&server, &mut refused);

    assert!(refused.is_empty(), "{refused:?}");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["photos", "notes.txt"]);

    // And it really did use LIST rather than MLSD.
    let commands = server.commands();
    assert!(commands.iter().any(|c| c.starts_with("LIST")), "{commands:?}");
    assert!(!commands.iter().any(|c| c.starts_with("MLSD")), "{commands:?}");
}

#[test]
fn mlsd_is_preferred_when_the_server_offers_it() {
    let server = FakeFtp::start(Config {
        mlsd: true,
        listing: vec!["type=file;size=1;modify=20201019151930; a.txt".into()],
        ..Config::default()
    });

    list(&server, &mut Vec::new());

    let commands = server.commands();
    assert!(commands.iter().any(|c| c == "FEAT"), "{commands:?}");
    assert!(commands.iter().any(|c| c.starts_with("MLSD")), "{commands:?}");
    assert!(!commands.iter().any(|c| c.starts_with("LIST")), "{commands:?}");
}

#[test]
fn a_listing_entry_that_climbs_out_of_the_download_folder_is_refused() {
    // The attack this client has to survive: a server answers a listing with a
    // name that is a path, and a client that joins it onto a local directory
    // writes wherever the server said.
    let server = FakeFtp::start(Config {
        mlsd: true,
        listing: vec![
            "type=file;size=1;modify=20201019151930; ../../../../Windows/System32/evil.dll".into(),
            "type=file;size=1;modify=20201019151930; ..\\..\\evil.exe".into(),
            "type=file;size=2;modify=20201019151930; innocent.txt".into(),
        ],
        ..Config::default()
    });

    let mut refused = Vec::new();
    let entries = list(&server, &mut refused);

    // Both hostile names refused...
    assert_eq!(refused.len(), 2, "{refused:?}");
    // ...and the rest of the folder still opened, because a directory that
    // will not list because one file in it is hostile is a denial of service.
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["innocent.txt"]);
}

#[test]
fn a_download_arrives_byte_for_byte() {
    // Bytes chosen to catch ASCII mode: a NUL, a bare CR, a CRLF and a lone LF
    // all survive a binary transfer and none survive a text one.
    let payload: Vec<u8> = vec![0x00, 0xFF, b'\r', b'\n', b'a', b'\n', b'\r', 0x1A, 0x00];
    let server = FakeFtp::start(Config {
        file: payload.clone(),
        ..Config::default()
    });

    let site = server.site("razeen");
    let mut transport =
        neutron_remote::ftp::FtpTransport::connect(&site, "hunter2").expect("should connect");

    let mut sink = Vec::new();
    let written = transport
        .download("/notes.bin", &mut sink, &mut |_| Continue::Yes)
        .expect("should download");

    assert_eq!(sink, payload);
    assert_eq!(written as usize, payload.len());
}

#[test]
fn the_connection_is_put_into_binary_mode_before_anything_is_transferred() {
    // FTP's ASCII mode rewrites line endings inside the transfer, so a zip or
    // an executable arrives corrupt and the transfer reports success.
    let server = FakeFtp::start(Config {
        file: vec![1, 2, 3],
        ..Config::default()
    });

    let site = server.site("razeen");
    let mut transport =
        neutron_remote::ftp::FtpTransport::connect(&site, "hunter2").expect("should connect");
    transport
        .download("/a.bin", &mut Vec::new(), &mut |_| Continue::Yes)
        .expect("should download");

    let commands = server.commands();
    let type_at = commands.iter().position(|c| c.eq_ignore_ascii_case("TYPE I"));
    let retr_at = commands.iter().position(|c| c.starts_with("RETR"));
    assert!(type_at.is_some(), "binary mode was never requested: {commands:?}");
    assert!(type_at < retr_at, "binary mode was set after the transfer: {commands:?}");
}

#[test]
fn progress_is_reported_and_a_download_can_be_stopped() {
    let server = FakeFtp::start(Config {
        // Larger than one chunk, so the callback fires more than once and
        // stopping has something to interrupt.
        file: vec![7u8; neutron_remote::transport::CHUNK * 3],
        ..Config::default()
    });

    let site = server.site("razeen");
    let mut transport =
        neutron_remote::ftp::FtpTransport::connect(&site, "hunter2").expect("should connect");

    let mut seen = Vec::new();
    let mut sink = Vec::new();
    let outcome = transport.download("/big.bin", &mut sink, &mut |done| {
        seen.push(done);
        // Stop after the first chunk.
        Continue::Stop
    });

    assert!(matches!(outcome, Err(RemoteError::Cancelled)), "{outcome:?}");
    assert_eq!(seen.len(), 1, "the callback should have stopped the transfer");
    // What arrived before the stop is kept, exactly as the archive jobs do.
    assert!(!sink.is_empty());
    assert!(sink.len() < neutron_remote::transport::CHUNK * 3);
}

#[test]
fn epsv_is_used_when_the_server_offers_it() {
    // EPSV names only a port, so the data connection cannot be aimed anywhere
    // but the server already connected to, and it is the only passive form
    // that works over IPv6.
    let server = FakeFtp::start(Config {
        epsv: true,
        file: vec![4, 5, 6],
        ..Config::default()
    });

    let site = server.site("razeen");
    let mut transport =
        neutron_remote::ftp::FtpTransport::connect(&site, "hunter2").expect("should connect");
    let mut sink = Vec::new();
    transport
        .download("/a.bin", &mut sink, &mut |_| Continue::Yes)
        .expect("should download over EPSV");
    assert_eq!(sink, [4, 5, 6]);

    let commands = server.commands();
    assert!(commands.iter().any(|c| c == "EPSV"), "{commands:?}");
    assert!(!commands.iter().any(|c| c == "PASV"), "{commands:?}");
}

#[test]
fn pasv_is_used_when_epsv_is_not_offered() {
    let server = FakeFtp::start(Config {
        file: vec![1],
        ..Config::default()
    });

    let site = server.site("razeen");
    let mut transport =
        neutron_remote::ftp::FtpTransport::connect(&site, "hunter2").expect("should connect");
    transport
        .download("/a.bin", &mut Vec::new(), &mut |_| Continue::Yes)
        .expect("should download over PASV");

    let commands = server.commands();
    assert!(commands.iter().any(|c| c == "PASV"), "{commands:?}");
    assert!(!commands.iter().any(|c| c == "EPSV"), "{commands:?}");
}

#[test]
fn the_address_in_a_pasv_reply_is_ignored_in_favour_of_the_server_itself() {
    // 192.0.2.1 is TEST-NET-1: reserved for documentation, routed nowhere. A
    // server behind NAT advertises its private address the same way, and a
    // hostile one could advertise a third party's. Either way the only
    // address worth connecting to is the one the control connection reached.
    let server = FakeFtp::start(Config {
        pasv_host: Some([192, 0, 2, 1]),
        file: vec![9, 9, 9],
        ..Config::default()
    });

    let site = server.site("razeen");
    let mut transport =
        neutron_remote::ftp::FtpTransport::connect(&site, "hunter2").expect("should connect");
    let mut sink = Vec::new();
    transport
        .download("/a.bin", &mut sink, &mut |_| Continue::Yes)
        .expect("the data connection should have gone to the server, not the advertised host");
    assert_eq!(sink, [9, 9, 9]);
}

#[test]
fn a_rejected_password_is_reported_as_an_authentication_failure() {
    let server = FakeFtp::start(Config {
        password_ok: false,
        ..Config::default()
    });

    let site = server.site("razeen");
    let Err(error) = neutron_remote::ftp::FtpTransport::connect(&site, "wrong") else {
        panic!("a rejected password should not connect");
    };
    assert!(matches!(error, RemoteError::Auth { .. }), "{error:?}");
}

#[test]
fn an_empty_username_logs_in_anonymously() {
    // A real and common configuration for public servers.
    let server = FakeFtp::start(Config::default());

    let site = server.site("");
    neutron_remote::ftp::FtpTransport::connect(&site, "").expect("should connect");

    let commands = server.commands();
    assert!(
        commands.iter().any(|c| c.eq_ignore_ascii_case("USER anonymous")),
        "{commands:?}"
    );
}

#[test]
fn a_listing_line_in_no_known_format_is_skipped_rather_than_failing_the_folder() {
    let server = FakeFtp::start(Config {
        mlsd: false,
        listing: vec![
            "this is not a listing line".into(),
            "-rw-r--r-- 1 user group 12 Nov  5 13:46 real.txt".into(),
        ],
        ..Config::default()
    });

    let mut refused = Vec::new();
    let entries = list(&server, &mut refused);

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["real.txt"]);
    // Not reported: an unreadable line is a format this build does not know,
    // not something hostile, and a listing full of complaints helps nobody.
    assert!(refused.is_empty(), "{refused:?}");
}

#[test]
fn two_transfers_run_in_sequence_on_one_connection() {
    // The classic FTP client bug: a data connection that is not finalised
    // leaves the control channel one response behind, so the *next* command
    // reads the previous command's reply and everything after it is wrong.
    let server = FakeFtp::start(Config {
        mlsd: true,
        listing: vec!["type=file;size=3;modify=20201019151930; a.txt".into()],
        file: vec![1, 2, 3],
        ..Config::default()
    });

    let site = server.site("razeen");
    let mut transport =
        neutron_remote::ftp::FtpTransport::connect(&site, "hunter2").expect("should connect");

    let mut first = Vec::new();
    transport
        .download("/a.txt", &mut first, &mut |_| Continue::Yes)
        .expect("first download");

    // If the control channel had desynchronised, this is where it would show.
    let entries = transport.list("/", &mut Vec::new()).expect("list after a download");
    assert_eq!(entries.len(), 1);

    let mut second = Vec::new();
    transport
        .download("/a.txt", &mut second, &mut |_| Continue::Yes)
        .expect("second download");
    assert_eq!(first, second);
}

// --- writing ---------------------------------------------------------------
//
// These act on someone else's machine and have no undo, so what is asserted is
// the exact command and the exact path. A rename that sends the right verbs to
// the wrong path is the failure that matters, and it is invisible from the
// client's own return value.

/// Anonymous, like the other tests here: a named user would need a password in
/// Credential Manager, and none of this is about authentication.
fn connect(server: &FakeFtp) -> Box<dyn neutron_remote::Transport> {
    neutron_remote::connect(&server.site("")).expect("connect")
}

#[test]
fn creating_a_directory_sends_mkd_with_the_full_path() {
    let server = FakeFtp::start(Config::default());
    let mut t = connect(&server);
    t.create_dir("/pub/new folder").expect("mkd");
    assert!(
        server.commands().contains(&"MKD /pub/new folder".to_owned()),
        "{:?}",
        server.commands()
    );
}

/// FTP renames in two commands, and a client that sent only one would leave the
/// server waiting mid-sequence with the file untouched.
#[test]
fn a_rename_sends_rnfr_then_rnto_in_that_order() {
    let server = FakeFtp::start(Config::default());
    let mut t = connect(&server);
    t.rename("/pub/old.txt", "/pub/new.txt").expect("rename");

    let log = server.commands();
    let from = log.iter().position(|c| c == "RNFR /pub/old.txt");
    let to = log.iter().position(|c| c == "RNTO /pub/new.txt");
    assert!(from.is_some() && to.is_some(), "{log:?}");
    assert!(from < to, "RNTO must follow RNFR: {log:?}");
}

/// A file and a directory take different verbs. Sending DELE for a directory
/// fails on a real server, so the two are not interchangeable.
#[test]
fn a_file_and_a_directory_are_removed_with_different_commands() {
    let server = FakeFtp::start(Config::default());
    let mut t = connect(&server);
    t.remove_file("/pub/notes.txt").expect("dele");
    t.remove_dir("/pub/empty").expect("rmd");

    let log = server.commands();
    assert!(log.contains(&"DELE /pub/notes.txt".to_owned()), "{log:?}");
    assert!(log.contains(&"RMD /pub/empty".to_owned()), "{log:?}");
}

#[test]
fn an_upload_arrives_byte_for_byte() {
    let server = FakeFtp::start(Config::default());
    let mut t = connect(&server);

    // Includes a NUL and a high byte: an upload that went through ASCII mode
    // would mangle these and still report success.
    let payload: Vec<u8> = vec![0x00, 0x0D, 0x0A, 0xFF, b'h', b'i', 0x1A];
    let mut source = std::io::Cursor::new(payload.clone());
    let written = t
        .upload("/pub/blob.bin", &mut source, &mut |_| {
            neutron_remote::Continue::Yes
        })
        .expect("upload");

    assert_eq!(written, payload.len() as u64);
    assert_eq!(server.stored(), payload);
    assert!(
        server.commands().contains(&"STOR /pub/blob.bin".to_owned()),
        "{:?}",
        server.commands()
    );
}

/// The reason `finalize_put_stream` is called even on a stopped transfer.
///
/// Dropping the data connection without it leaves the control connection
/// waiting for a response nobody read, and every later command then reads the
/// *previous* reply — so the session does not fail, it silently answers the
/// wrong questions. This asserts the connection is still usable afterwards.
#[test]
fn a_stopped_upload_leaves_the_connection_usable() {
    let server = FakeFtp::start(Config::default());
    let mut t = connect(&server);

    let big = vec![b'x'; 512 * 1024];
    let mut source = std::io::Cursor::new(big);
    let outcome = t.upload("/pub/big.bin", &mut source, &mut |_| {
        neutron_remote::Continue::Stop
    });
    assert!(
        matches!(outcome, Err(neutron_remote::RemoteError::Cancelled)),
        "{outcome:?}"
    );

    // The next command has to work. If the control stream were out of step this
    // would read the upload's leftover reply and fail, or hang.
    t.create_dir("/pub/after").expect("the session survived the stop");
    assert!(
        server.commands().contains(&"MKD /pub/after".to_owned()),
        "{:?}",
        server.commands()
    );
}

/// A refusal is the server's own words, not a generic failure. "Permission
/// denied" is actionable; "could not delete" is not.
#[test]
fn a_refused_write_reports_what_the_server_said() {
    let server = FakeFtp::start(Config {
        refuse: Some("DELE".into()),
        ..Config::default()
    });
    let mut t = connect(&server);

    let error = t.remove_file("/pub/locked.txt").expect_err("should refuse");
    let text = error.to_string();
    assert!(text.contains("/pub/locked.txt"), "{text}");
    assert!(text.contains("550") || text.contains("Permission"), "{text}");
}

// --- FTPS and self-signed certificates ---------------------------------------
//
// The certificate a NAS makes for itself on first boot: self-signed, valid for
// 127.0.0.1, signed by nobody the bundled roots know. Generated once with
//
//     openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
//         -nodes -days 36500 -subj "/CN=neutron-test-nas" \
//         -addext "subjectAltName=IP:127.0.0.1,DNS:localhost"
//
// and embedded so the test needs no certificate generator. The fingerprint is
// what `openssl x509 -noout -fingerprint -sha256` prints for it, which is the
// point: the value Neutron shows must be checkable against the server.

const SELF_SIGNED_CERT: &str = "-----BEGIN CERTIFICATE-----
MIIBqjCCAU+gAwIBAgIUIja8tx2eiMDhs0JE0VNeGjCLA1UwCgYIKoZIzj0EAwIw
GzEZMBcGA1UEAwwQbmV1dHJvbi10ZXN0LW5hczAgFw0yNjEwMDkwMTAzNTRaGA8y
MTI2MDkxNTAxMDM1NFowGzEZMBcGA1UEAwwQbmV1dHJvbi10ZXN0LW5hczBZMBMG
ByqGSM49AgEGCCqGSM49AwEHA0IABJjcQT+QW4P7VXqNWQoRT+9u1uxunj170CiU
vi5kOXXoMdCKfuwvuM4vtD1awssAbrLnW3gYF3wPawDs2bhOZdijbzBtMB0GA1Ud
DgQWBBS+dBGk9lkEEgBYBQPTj/AMUSRl7DAfBgNVHSMEGDAWgBS+dBGk9lkEEgBY
BQPTj/AMUSRl7DAPBgNVHRMBAf8EBTADAQH/MBoGA1UdEQQTMBGHBH8AAAGCCWxv
Y2FsaG9zdDAKBggqhkjOPQQDAgNJADBGAiEAxHOeOKPtUzbSc3aWUPmfFPiqLHFi
GVJ7uXzmTvUpRZsCIQCKO8FcNUyAz1hJIDloKd2AMmHlRO2IAnAWB0d/qVbhJg==
-----END CERTIFICATE-----
";

const SELF_SIGNED_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgvUx57v5qif5PMQM9
GtCusUgL5uPqCs0PYZYVLQ2u3RyhRANCAASY3EE/kFuD+1V6jVkKEU/vbtbsbp49
e9AolL4uZDl16DHQin7sL7jOL7Q9WsLLAG6y51t4GBd8D2sA7Nm4TmXY
-----END PRIVATE KEY-----
";

const SELF_SIGNED_FINGERPRINT: &str = "4A:E4:0B:3F:5D:87:1C:12:B1:0F:FE:A8:51:D7:9C:0B:\
EE:BB:28:87:8B:66:B2:3C:C8:C0:25:AF:07:DA:B2:50";

/// An explicit-FTPS server presenting [`SELF_SIGNED_CERT`]. Speaks only as much
/// as a login needs, and records every command it reads *after* the handshake —
/// so a `PASS` in the log is proof the password crossed an encrypted channel,
/// and its absence is proof a refused certificate kept the password at home.
struct FakeFtps {
    addr: SocketAddr,
    log: Arc<Mutex<Vec<String>>>,
}

impl FakeFtps {
    fn start() -> FakeFtps {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let log = Arc::new(Mutex::new(Vec::new()));
        let worker_log = Arc::clone(&log);
        std::thread::spawn(move || {
            if let Ok((control, _)) = listener.accept() {
                serve_tls(control, &worker_log);
            }
        });
        FakeFtps { addr, log }
    }

    fn site(&self, trusted_cert: Option<&str>) -> Site {
        Site {
            id: "nas".into(),
            label: String::new(),
            protocol: Protocol::Ftps,
            host: self.addr.ip().to_string(),
            port: Some(self.addr.port()),
            username: "razeen".into(),
            path: None,
            fingerprint: None,
            auth: Default::default(),
            trusted_cert: trusted_cert.map(str::to_owned),
        }
    }

    fn saw_password(&self) -> bool {
        self.log.lock().iter().any(|line| line.starts_with("PASS"))
    }
}

fn serve_tls(mut control: TcpStream, log: &Mutex<Vec<String>>) {
    use suppaftp::rustls;
    use suppaftp::rustls::pki_types::pem::PemObject;
    use suppaftp::rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let _ = control.write_all(b"220 Fake FTPS ready\r\n");
    let mut line = String::new();
    {
        let mut plain = BufReader::new(&control);
        if plain.read_line(&mut line).is_err() || !line.to_ascii_uppercase().starts_with("AUTH TLS")
        {
            return;
        }
    }
    let _ = control.write_all(b"234 Proceed with negotiation\r\n");

    let cert = CertificateDer::from_pem_slice(SELF_SIGNED_CERT.as_bytes()).expect("cert");
    let key = PrivateKeyDer::from_pem_slice(SELF_SIGNED_KEY.as_bytes()).expect("key");
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("protocol versions")
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .expect("server config");
    let connection = rustls::ServerConnection::new(Arc::new(config)).expect("connection");
    let mut tls = BufReader::new(rustls::StreamOwned::new(connection, control));

    loop {
        let mut line = String::new();
        // A client that refused the certificate aborts the handshake, which
        // surfaces here as an error on the first read.
        match tls.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let line = line.trim_end_matches(['\r', '\n']).to_owned();
        log.lock().push(line.clone());
        let verb = line.split(' ').next().unwrap_or("").to_ascii_uppercase();
        let reply: &[u8] = match verb.as_str() {
            "PBSZ" | "PROT" | "TYPE" => b"200 OK\r\n",
            "USER" => b"331 Password required\r\n",
            "PASS" => b"230 Logged in\r\n",
            "FEAT" => b"211-Features:\r\n UTF8\r\n211 End\r\n",
            "QUIT" => {
                let _ = tls.get_mut().write_all(b"221 Goodbye\r\n");
                return;
            }
            _ => b"502 Command not implemented\r\n",
        };
        let _ = tls.get_mut().write_all(reply);
        let _ = tls.get_mut().flush();
    }
}

#[test]
fn a_certificate_fingerprint_matches_what_openssl_prints() {
    use suppaftp::rustls::pki_types::CertificateDer;
    use suppaftp::rustls::pki_types::pem::PemObject;
    let der = CertificateDer::from_pem_slice(SELF_SIGNED_CERT.as_bytes()).expect("cert");
    assert_eq!(
        neutron_remote::certificate::fingerprint(der.as_ref()),
        SELF_SIGNED_FINGERPRINT
    );
}

#[test]
fn a_self_signed_certificate_is_refused_on_first_contact_naming_its_fingerprint() {
    let server = FakeFtps::start();

    let Err(error) = neutron_remote::ftp::FtpTransport::connect(&server.site(None), "hunter2")
    else {
        panic!("an unvouched-for certificate must not be accepted silently");
    };
    let RemoteError::UntrustedCertificate { fingerprint, message } = &error else {
        panic!("expected an untrusted certificate, got {error:?}");
    };
    assert_eq!(fingerprint, SELF_SIGNED_FINGERPRINT);
    assert!(message.contains(SELF_SIGNED_FINGERPRINT), "{message}");
    assert!(message.contains("trust"), "{message}");
    assert!(!server.saw_password(), "the password was sent to an untrusted server");
}

#[test]
fn a_pinned_self_signed_certificate_connects() {
    let server = FakeFtps::start();
    // Pasted the way OpenSSL prints it, label and all.
    let pin = format!("sha256 Fingerprint={SELF_SIGNED_FINGERPRINT}");

    neutron_remote::ftp::FtpTransport::connect(&server.site(Some(&pin)), "hunter2")
        .expect("the pinned certificate should be trusted");
    assert!(server.saw_password(), "the login did not happen over TLS");
}

#[test]
fn a_certificate_that_differs_from_the_pin_is_refused_naming_both() {
    let server = FakeFtps::start();
    let old = neutron_remote::certificate::fingerprint(b"the certificate it used to have");

    let Err(error) = neutron_remote::ftp::FtpTransport::connect(&server.site(Some(&old)), "hunter2")
    else {
        panic!("a changed certificate must not connect");
    };
    let RemoteError::UntrustedCertificate { message, .. } = &error else {
        panic!("expected an untrusted certificate, got {error:?}");
    };
    assert!(message.contains(&old), "{message}");
    assert!(message.contains(SELF_SIGNED_FINGERPRINT), "{message}");
    assert!(!server.saw_password(), "the password was sent to a changed server");
}
