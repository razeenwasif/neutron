//! FTP and FTPS, over `suppaftp`'s blocking client.
//!
//! # Listing: MLSD first, LIST only as a fallback
//!
//! `LIST` has no specification. What comes back is whatever the server thought
//! a human would like to read, which in practice is one of two shapes — Unix
//! `ls -l`, or the DOS format IIS emits — and neither is unambiguous. Unix
//! listings give a year for old files and a clock time for recent ones, so a
//! file's year is inferred from today's date. Filenames may contain the same
//! spaces the columns are separated by. Some servers localise the month.
//!
//! `MLSD` (RFC 3659) exists precisely because of this: it returns typed facts,
//! `type=file;size=1234;modify=20201019151930; notes.txt`. Almost every server
//! written this century supports it. So Neutron asks `FEAT` once per
//! connection, uses `MLSD` when it is offered, and falls back to parsing `LIST`
//! only when it is not.
//!
//! # Binary mode, always
//!
//! FTP's ASCII mode rewrites line endings *inside the transfer*. Downloading a
//! zip or an executable in ASCII mode produces a corrupt file that is subtly
//! the wrong length, and the transfer reports success. `TYPE I` is set once,
//! immediately after login, and nothing here ever leaves it.
//!
//! # Passive mode
//!
//! Active mode asks the server to open a connection back to the client, which
//! has not worked through a home router without deliberate configuration for
//! twenty years. `suppaftp` defaults to passive; this leaves it there.

use std::io::{Read, Write};

use neutron_core::entry::EntryKind;
use suppaftp::list::{File, ListParser};
use suppaftp::{ImplFtpStream, RustlsConnector, RustlsFtpStream, TlsStream};

use crate::path;
use crate::site::{Protocol, Site};
use crate::transport::{CHUNK, CONNECT_TIMEOUT, Continue, RemoteEntry, RemoteError, Transport};

/// A connected FTP or FTPS session.
///
/// The two are different Rust types — `suppaftp` parameterises the stream by
/// its TLS implementation, and the plain variant uses a placeholder that has no
/// TCP stream at all — so they cannot be one value. Everything below is written
/// once, generically, and dispatched through this enum in two lines.
pub struct FtpTransport {
    stream: Stream,
    /// Whether the server offered MLSD, asked once per connection.
    mlsd: bool,
}

enum Stream {
    Plain(suppaftp::FtpStream),
    Tls(Box<RustlsFtpStream>),
}

impl FtpTransport {
    /// Connects, logs in, and switches to binary mode.
    pub fn connect(site: &Site, password: &str) -> Result<FtpTransport, RemoteError> {
        let address = site.address();
        let endpoint = resolve(&site.host, site.port(), &address)?;

        // An empty username means anonymous FTP, which is a real and common
        // configuration. The convention is any e-mail-shaped string as the
        // password, and servers that check want *something* rather than
        // nothing.
        let (user, password) = if site.username.is_empty() {
            ("anonymous", "neutron@example.invalid")
        } else {
            (site.username.as_str(), password)
        };

        // FTPS is not a plain connection that is later upgraded in the type
        // system: `suppaftp` parameterises the whole stream by its TLS
        // implementation, so a secure session has to be built as one from the
        // start. It is still plain TCP on the wire until `AUTH TLS`.
        let (stream, mlsd) = match site.protocol {
            Protocol::Ftps => {
                let plain = RustlsFtpStream::connect_timeout(endpoint, CONNECT_TIMEOUT)
                    .map_err(|e| RemoteError::Connect {
                        address: address.clone(),
                        reason: e.to_string(),
                    })?;
                let connector = RustlsConnector::from(std::sync::Arc::new(tls_config()));
                let mut secure =
                    plain
                        .into_secure(connector, &site.host)
                        .map_err(|e| RemoteError::Connect {
                            address: address.clone(),
                            reason: format!("TLS could not be established: {e}"),
                        })?;
                let mlsd = prepare(&mut secure, user, password, &address)?;
                (Stream::Tls(Box::new(secure)), mlsd)
            }
            // Sftp never reaches here; the dispatcher sends it elsewhere.
            _ => {
                let mut plain = suppaftp::FtpStream::connect_timeout(endpoint, CONNECT_TIMEOUT)
                    .map_err(|e| RemoteError::Connect {
                        address: address.clone(),
                        reason: e.to_string(),
                    })?;
                let mlsd = prepare(&mut plain, user, password, &address)?;
                (Stream::Plain(plain), mlsd)
            }
        };

        tracing::debug!(%address, mlsd, "ftp connected");
        Ok(FtpTransport { stream, mlsd })
    }
}

impl Transport for FtpTransport {
    fn list(
        &mut self,
        path: &str,
        refused: &mut Vec<String>,
    ) -> Result<Vec<RemoteEntry>, RemoteError> {
        let mlsd = self.mlsd;
        match &mut self.stream {
            Stream::Plain(s) => list_on(s, path, mlsd, refused),
            Stream::Tls(s) => list_on(s, path, mlsd, refused),
        }
    }

    fn download(
        &mut self,
        path: &str,
        sink: &mut dyn Write,
        on_progress: &mut dyn FnMut(u64) -> Continue,
    ) -> Result<u64, RemoteError> {
        match &mut self.stream {
            Stream::Plain(s) => download_on(s, path, sink, on_progress),
            Stream::Tls(s) => download_on(s, path, sink, on_progress),
        }
    }
}

/// Logs in, switches to binary mode, and asks what the server can do.
///
/// Generic over the stream's TLS type so the FTP and FTPS paths are the same
/// code rather than two copies that drift.
fn prepare<T: TlsStream>(
    stream: &mut ImplFtpStream<T>,
    user: &str,
    password: &str,
    address: &str,
) -> Result<bool, RemoteError> {
    stream
        .login(user, password)
        .map_err(|e| classify_login(address, &e.to_string()))?;

    // Before anything is transferred. See the module docs — ASCII mode
    // silently corrupts every non-text file.
    stream
        .transfer_type(suppaftp::types::FileType::Binary)
        .map_err(|e| RemoteError::Connect {
            address: address.to_owned(),
            reason: format!("could not switch to binary mode: {e}"),
        })?;

    // One FEAT per connection rather than one per listing.
    let mlsd = stream
        .feat()
        .map(|features| features.contains_key("MLST") || features.contains_key("MLSD"))
        .unwrap_or(false);

    Ok(mlsd)
}

fn list_on<T: TlsStream>(
    stream: &mut ImplFtpStream<T>,
    path: &str,
    mlsd: bool,
    refused: &mut Vec<String>,
) -> Result<Vec<RemoteEntry>, RemoteError> {
    let path = path::normalize(path);

    let lines = if mlsd {
        stream.mlsd(Some(&path))
    } else {
        stream.list(Some(&path))
    }
    .map_err(|e| RemoteError::Server {
        path: path.clone(),
        reason: e.to_string(),
    })?;

    let mut entries = Vec::with_capacity(lines.len());
    for line in &lines {
        match parse_line(line, mlsd) {
            // A line that is deliberately not an entry: `total 48`, the
            // cdir/pdir facts, a blank. Not worth reporting.
            Parsed::Skip => continue,
            Parsed::Entry(file) => match path::safe_name(file.name()) {
                Ok(_) => entries.push(to_remote(&file)),
                Err(refusal) => {
                    tracing::warn!(%path, "{refusal}");
                    refused.push(refusal.to_string());
                }
            },
            Parsed::Unreadable => {
                // Logged rather than reported: an unparseable LIST line is a
                // shape this build does not know, not an attack, and a listing
                // full of complaints helps nobody.
                tracing::debug!(%path, "unreadable listing line: {line:?}");
            }
        }
    }

    Ok(entries)
}

fn download_on<T: TlsStream>(
    stream: &mut ImplFtpStream<T>,
    path: &str,
    sink: &mut dyn Write,
    on_progress: &mut dyn FnMut(u64) -> Continue,
) -> Result<u64, RemoteError> {
    let path = path::normalize(path);

    let mut data = stream
        .retr_as_stream(&path)
        .map_err(|e| RemoteError::Server {
            path: path.clone(),
            reason: e.to_string(),
        })?;

    let mut buffer = vec![0u8; CHUNK];
    let mut total = 0u64;
    let mut stopped = false;
    let mut failure = None;

    loop {
        let read = match data.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                failure = Some(RemoteError::Io(e));
                break;
            }
        };
        if let Err(e) = sink.write_all(&buffer[..read]) {
            failure = Some(RemoteError::Io(e));
            break;
        }
        total += read as u64;
        if on_progress(total) == Continue::Stop {
            stopped = true;
            break;
        }
    }

    // On every path, including the failures above. Leaving the data connection
    // open desynchronises the control connection, and every command after it
    // then reads the *previous* command's response — which is the classic FTP
    // client bug and is maddening to diagnose from the far end.
    let finalize = stream.finalize_retr_stream(data);

    if let Some(failure) = failure {
        return Err(failure);
    }
    if stopped {
        return Err(RemoteError::Cancelled);
    }
    finalize.map_err(|e| RemoteError::Server {
        path,
        reason: e.to_string(),
    })?;

    Ok(total)
}

/// Resolves the host to one address, so the connect timeout applies to a single
/// attempt rather than to a list of them.
fn resolve(host: &str, port: u16, address: &str) -> Result<std::net::SocketAddr, RemoteError> {
    use std::net::ToSocketAddrs;
    (host, port)
        .to_socket_addrs()
        .map_err(|e| RemoteError::Connect {
            address: address.to_owned(),
            reason: format!("could not be resolved: {e}"),
        })?
        .next()
        .ok_or_else(|| RemoteError::Connect {
            address: address.to_owned(),
            reason: "resolved to no addresses".to_owned(),
        })
}

/// The TLS settings for FTPS: the platform's own trust store where it can be
/// read, and the bundled roots otherwise.
fn tls_config() -> suppaftp::rustls::ClientConfig {
    let mut roots = suppaftp::rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    suppaftp::rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// Whether a login failure was the credentials or something else.
///
/// Worth separating because the two need different things from the user: one is
/// "type it again", the other is "the server is not where you think it is".
/// Takes the message rather than the error so the classification can be tested
/// without constructing one of `suppaftp`'s error values.
fn classify_login(address: &str, text: &str) -> RemoteError {
    // 530 is the FTP code for "not logged in", which is what a server answers
    // to a wrong username or password.
    if text.contains("530") || text.to_lowercase().contains("login") {
        RemoteError::Auth { address: address.to_owned() }
    } else {
        RemoteError::Connect {
            address: address.to_owned(),
            reason: text.to_owned(),
        }
    }
}

/// What one line of a listing turned out to be.
enum Parsed {
    Entry(File),
    /// A line that is deliberately not an entry.
    Skip,
    /// A line this build could not read.
    Unreadable,
}

/// Reads one listing line.
///
/// `.` and `..` are dropped here rather than by the name check, so they do not
/// show up as refusals. Every directory has them and they are not interesting.
fn parse_line(line: &str, mlsd: bool) -> Parsed {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() {
        return Parsed::Skip;
    }

    if mlsd {
        // `type=cdir` and `type=pdir` are this directory and its parent. Their
        // names are full paths rather than single names, so they would be
        // refused as containing separators — accurate, but noise.
        let facts = line.to_ascii_lowercase();
        if facts.contains("type=cdir") || facts.contains("type=pdir") {
            return Parsed::Skip;
        }
        return match ListParser::parse_mlsd(line) {
            Ok(file) => Parsed::Entry(file),
            Err(_) => Parsed::Unreadable,
        };
    }

    // `ls -l` prefaces the listing with a block count.
    if line.starts_with("total ") {
        return Parsed::Skip;
    }

    // Unix first because it is far more common, then the IIS format.
    let parsed = ListParser::parse_posix(line).or_else(|_| ListParser::parse_dos(line));
    match parsed {
        Ok(file) if file.name() == "." || file.name() == ".." => Parsed::Skip,
        Ok(file) => Parsed::Entry(file),
        Err(_) => Parsed::Unreadable,
    }
}

fn to_remote(file: &File) -> RemoteEntry {
    // Symlinks are checked before directories: a link *to* a directory reports
    // both, and showing it as a link is the honest answer — following it is the
    // server's job, not ours.
    let kind = if file.is_symlink() {
        EntryKind::Symlink
    } else if file.is_directory() {
        EntryKind::Directory
    } else {
        EntryKind::File
    };

    RemoteEntry {
        name: file.name().to_owned(),
        kind,
        size: file.size() as u64,
        modified: file
            .modified()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unix_listing_line_is_read() {
        let line = "-rw-r--r-- 1 user group 1234 Nov  5 13:46 notes.txt";
        let Parsed::Entry(file) = parse_line(line, false) else {
            panic!("should have parsed");
        };
        assert_eq!(file.name(), "notes.txt");
        assert_eq!(file.size(), 1234);
        assert!(file.is_file());
    }

    #[test]
    fn a_unix_directory_line_is_read_as_a_directory() {
        let line = "drwxr-xr-x 2 user group 4096 Nov  5 13:46 photos";
        let Parsed::Entry(file) = parse_line(line, false) else {
            panic!("should have parsed");
        };
        assert!(file.is_directory());
        assert_eq!(to_remote(&file).kind, EntryKind::Directory);
    }

    #[test]
    fn the_ls_block_count_is_not_an_entry() {
        // Every `ls -l` listing starts with this, and a client that reports it
        // shows a file called "total".
        assert!(matches!(parse_line("total 48", false), Parsed::Skip));
    }

    #[test]
    fn dot_and_dot_dot_are_dropped_quietly() {
        let dot = "drwxr-xr-x 2 user group 4096 Nov  5 13:46 .";
        let dotdot = "drwxr-xr-x 2 user group 4096 Nov  5 13:46 ..";
        assert!(matches!(parse_line(dot, false), Parsed::Skip));
        assert!(matches!(parse_line(dotdot, false), Parsed::Skip));
    }

    #[test]
    fn an_mlsd_line_is_read_with_its_facts() {
        let line = "type=file;size=1234;modify=20201019151930; notes.txt";
        let Parsed::Entry(file) = parse_line(line, true) else {
            panic!("should have parsed");
        };
        assert_eq!(file.name(), "notes.txt");
        assert_eq!(file.size(), 1234);
        // The point of MLSD: the time is exact rather than inferred from
        // today's date the way a Unix listing's is.
        assert!(to_remote(&file).modified > 0);
    }

    #[test]
    fn mlsd_entries_for_this_directory_and_its_parent_are_dropped() {
        // Their names are full paths, so they would otherwise be reported as
        // refused names — accurate and useless.
        assert!(matches!(
            parse_line("type=cdir;modify=20201019151930; /pub", true),
            Parsed::Skip
        ));
        assert!(matches!(
            parse_line("type=pdir;modify=20201019151930; /", true),
            Parsed::Skip
        ));
    }

    #[test]
    fn a_dos_listing_line_is_read() {
        // What IIS emits. Different enough from the Unix format that a single
        // parser cannot cover both.
        let line = "12-05-20  02:38PM              1234 notes.txt";
        let Parsed::Entry(file) = parse_line(line, false) else {
            panic!("should have parsed the IIS format");
        };
        assert_eq!(file.name(), "notes.txt");
        assert_eq!(file.size(), 1234);
    }

    #[test]
    fn a_dos_directory_line_is_read_as_a_directory() {
        let line = "12-05-20  02:38PM       <DIR>          photos";
        let Parsed::Entry(file) = parse_line(line, false) else {
            panic!("should have parsed");
        };
        assert!(file.is_directory());
    }

    #[test]
    fn a_line_in_no_known_format_is_unreadable_rather_than_an_entry() {
        // Better an entry that does not appear than one whose name is a
        // fragment of a line.
        assert!(matches!(parse_line("what is this", false), Parsed::Unreadable));
    }

    #[test]
    fn a_name_with_spaces_survives_a_unix_listing() {
        let line = "-rw-r--r-- 1 user group 12 Nov  5 13:46 my holiday photos.tar.gz";
        let Parsed::Entry(file) = parse_line(line, false) else {
            panic!("should have parsed");
        };
        assert_eq!(file.name(), "my holiday photos.tar.gz");
    }

    #[test]
    fn a_wrong_password_is_told_apart_from_an_unreachable_server() {
        // They need different things from the user: one is "type it again",
        // the other is "the server is not where you think it is".
        let auth = classify_login("u@h:21", "unexpected response: 530 Login incorrect");
        assert!(matches!(auth, RemoteError::Auth { .. }), "{auth:?}");

        let unreachable = classify_login("u@h:21", "connection refused");
        assert!(matches!(unreachable, RemoteError::Connect { .. }), "{unreachable:?}");
    }
}
