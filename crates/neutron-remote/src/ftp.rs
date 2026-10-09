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
//!
//! Within passive mode, `EPSV` (RFC 2428) is used whenever `FEAT` offers it,
//! and `PASV` otherwise. `EPSV` answers with a port only, so the data
//! connection necessarily goes to the host the control connection reached —
//! and it is the only form that works over IPv6 at all. When the server only
//! speaks `PASV`, the address in its reply is ignored and the port is used
//! against the control connection's peer instead. That is the usual NAT
//! workaround — a server behind a router routinely advertises its private
//! `192.168.x.y` address, which is unreachable from here — and it also closes
//! the old FTP bounce trick, where a hostile server's `PASV` reply points the
//! client's data connection at some third machine of the server's choosing.
//!
//! # Whose certificates FTPS trusts
//!
//! The bundled Mozilla roots from `webpki-roots` — not the platform's trust
//! store, so a corporate root installed in Keychain or the Windows store is not
//! honoured here — plus, per site, one exact certificate the user chose to
//! trust. See [`crate::certificate`] for why that is a pin the user sets rather
//! than something recorded on first contact.
//!
//! # Every socket has a deadline
//!
//! The connect timeout covers only the handshake. A server that accepts and
//! then stalls would leave a blocking read waiting forever, and every remote
//! tab shares one loader thread, so the whole feature would freeze behind it.
//! The control connection and every data connection therefore carry read and
//! write timeouts — see [`IO_TIMEOUT`].

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use neutron_core::entry::EntryKind;
use suppaftp::list::{File, ListParser};
use suppaftp::{ImplFtpStream, RustlsConnector, RustlsFtpStream, TlsStream};

use crate::certificate;
use crate::path;
use crate::site::{Protocol, Site};
use crate::transport::{
    CHUNK, CONNECT_TIMEOUT, Continue, IO_TIMEOUT, RemoteEntry, RemoteError, Transport,
};

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

/// The deadlines one session runs under.
///
/// A value rather than the two constants used directly, so the tests can make
/// a stalled server fail in a fraction of a second instead of a minute.
#[derive(Debug, Clone, Copy)]
struct Timeouts {
    connect: Duration,
    io: Duration,
}

impl Timeouts {
    const DEFAULT: Timeouts = Timeouts {
        connect: CONNECT_TIMEOUT,
        io: IO_TIMEOUT,
    };
}

impl FtpTransport {
    /// Connects, logs in, and switches to binary mode.
    pub fn connect(site: &Site, password: &str) -> Result<FtpTransport, RemoteError> {
        connect_with(site, password, Timeouts::DEFAULT)
    }
}

fn connect_with(
    site: &Site,
    password: &str,
    timeouts: Timeouts,
) -> Result<FtpTransport, RemoteError> {
    let address = site.address();
    let control = open_control(&site.host, site.port(), &address, timeouts)?;

    // An empty username means anonymous FTP, which is a real and common
    // configuration. The convention is any e-mail-shaped string as the
    // password, and servers that check want *something* rather than
    // nothing.
    let (user, password) = if site.username.is_empty() {
        ("anonymous", "neutron@example.invalid")
    } else {
        (site.username.as_str(), password)
    };

    let unreachable = |e: suppaftp::FtpError| RemoteError::Connect {
        address: address.clone(),
        reason: describe_connect_error(&e, timeouts),
    };

    // FTPS is not a plain connection that is later upgraded in the type
    // system: `suppaftp` parameterises the whole stream by its TLS
    // implementation, so a secure session has to be built as one from the
    // start. It is still plain TCP on the wire until `AUTH TLS`.
    //
    // Either way the session is built on a socket opened here rather than by
    // `suppaftp`, because that is the only way to put a read timeout on it
    // before the first read — the server's greeting, which is exactly where a
    // half-dead server stalls.
    let (stream, features) = match site.protocol {
        Protocol::Ftps => {
            let mut plain = RustlsFtpStream::connect_with_stream(control)
                .map_err(unreachable)?
                .passive_stream_builder(data_connector(timeouts));
            plain.set_passive_nat_workaround(true);
            let trusted = site.trusted_cert.as_deref();
            let (config, refused) =
                certificate::client_config(trusted).map_err(|reason| RemoteError::Connect {
                    address: address.clone(),
                    reason: format!("TLS could not be set up: {reason}"),
                })?;
            let connector = RustlsConnector::from(std::sync::Arc::new(config));
            let mut secure = plain.into_secure(connector, &site.host).map_err(|e| {
                // rustls has reduced a refused certificate to a generic
                // handshake failure by the time it gets here. The verifier
                // kept what the server presented, which is what the user
                // needs in order to decide whether to trust it.
                match refused.lock().take() {
                    Some(refusal) => RemoteError::UntrustedCertificate {
                        message: certificate::untrusted_message(&address, trusted, &refusal),
                        fingerprint: refusal.fingerprint,
                    },
                    None => RemoteError::Connect {
                        address: address.clone(),
                        reason: format!("TLS could not be established: {e}"),
                    },
                }
            })?;
            let features = prepare(&mut secure, user, password, &address)?;
            (Stream::Tls(Box::new(secure)), features)
        }
        // Sftp never reaches here; the dispatcher sends it elsewhere.
        _ => {
            let mut plain = suppaftp::FtpStream::connect_with_stream(control)
                .map_err(unreachable)?
                .passive_stream_builder(data_connector(timeouts));
            plain.set_passive_nat_workaround(true);
            let features = prepare(&mut plain, user, password, &address)?;
            (Stream::Plain(plain), features)
        }
    };

    tracing::debug!(%address, mlsd = features.mlsd, epsv = features.epsv, "ftp connected");
    Ok(FtpTransport {
        stream,
        mlsd: features.mlsd,
    })
}

/// Opens the control connection, with its deadlines already in place.
///
/// Every address the name resolves to is tried in turn, each with its own
/// connect timeout. A host with both an IPv6 and an IPv4 address, where only
/// one of the two actually routes from here, is common — keeping only the
/// first address made such a server unreachable for no reason the user could
/// see.
fn open_control(
    host: &str,
    port: u16,
    address: &str,
    timeouts: Timeouts,
) -> Result<TcpStream, RemoteError> {
    let mut last = None;
    for endpoint in resolve(host, port, address)? {
        match TcpStream::connect_timeout(&endpoint, timeouts.connect) {
            Ok(stream) => {
                set_deadlines(&stream, timeouts.io).map_err(|e| RemoteError::Connect {
                    address: address.to_owned(),
                    reason: e.to_string(),
                })?;
                return Ok(stream);
            }
            Err(e) => {
                tracing::debug!(%address, %endpoint, "ftp connect failed: {e}");
                last = Some(e);
            }
        }
    }
    Err(RemoteError::Connect {
        address: address.to_owned(),
        reason: last.map_or_else(|| "resolved to no addresses".to_owned(), |e| e.to_string()),
    })
}

/// How data connections are opened: with a connect timeout, and with the same
/// read and write deadlines as the control connection.
///
/// `suppaftp`'s default builder is a bare `TcpStream::connect`, which has
/// neither — a data port that never answers, or a server that stops sending
/// halfway through a file, would block the transfer for as long as the OS
/// cares to wait, which for a read is forever. TLS, when there is any, wraps
/// this same socket, so the deadlines hold for FTPS data too.
fn data_connector(
    timeouts: Timeouts,
) -> impl Fn(SocketAddr) -> Result<TcpStream, suppaftp::FtpError> + Send + Sync + 'static {
    move |endpoint| {
        let stream = TcpStream::connect_timeout(&endpoint, timeouts.connect)
            .map_err(suppaftp::FtpError::ConnectionError)?;
        set_deadlines(&stream, timeouts.io).map_err(suppaftp::FtpError::ConnectionError)?;
        Ok(stream)
    }
}

fn set_deadlines(stream: &TcpStream, io: Duration) -> std::io::Result<()> {
    stream.set_read_timeout(Some(io))?;
    stream.set_write_timeout(Some(io))
}

/// Why a connection did not come up, in words.
///
/// A read that times out surfaces as `WouldBlock` on Unix ("Resource
/// temporarily unavailable") and `TimedOut` on Windows, neither of which says
/// what actually happened: the server let us in and then went quiet.
fn describe_connect_error(error: &suppaftp::FtpError, timeouts: Timeouts) -> String {
    match error {
        suppaftp::FtpError::ConnectionError(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            format!(
                "the server accepted the connection but did not answer within {}s",
                timeouts.io.as_secs_f32()
            )
        }
        other => other.to_string(),
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
    fn create_dir(&mut self, path: &str) -> Result<(), RemoteError> {
        match &mut self.stream {
            Stream::Plain(s) => simple(s, path, |s, p| s.mkdir(p)),
            Stream::Tls(s) => simple(s, path, |s, p| s.mkdir(p)),
        }
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), RemoteError> {
        let (from, to) = (path::normalize(from), path::normalize(to));
        match &mut self.stream {
            Stream::Plain(s) => rename_on(s, &from, &to),
            Stream::Tls(s) => rename_on(s, &from, &to),
        }
    }

    fn remove_file(&mut self, path: &str) -> Result<(), RemoteError> {
        match &mut self.stream {
            Stream::Plain(s) => simple(s, path, |s, p| s.rm(p)),
            Stream::Tls(s) => simple(s, path, |s, p| s.rm(p)),
        }
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), RemoteError> {
        match &mut self.stream {
            Stream::Plain(s) => simple(s, path, |s, p| s.rmdir(p)),
            Stream::Tls(s) => simple(s, path, |s, p| s.rmdir(p)),
        }
    }

    fn upload(
        &mut self,
        path: &str,
        source: &mut dyn Read,
        on_progress: &mut dyn FnMut(u64) -> Continue,
    ) -> Result<u64, RemoteError> {
        match &mut self.stream {
            Stream::Plain(s) => upload_on(s, path, source, on_progress),
            Stream::Tls(s) => upload_on(s, path, source, on_progress),
        }
    }

}

/// What `FEAT` said the server can do, as far as this client cares.
#[derive(Debug, Clone, Copy, Default)]
struct Features {
    /// `MLSD` listings — see the module docs.
    mlsd: bool,
    /// `EPSV` data connections — see the module docs.
    epsv: bool,
}

impl Features {
    fn from_feat(features: &suppaftp::types::Features) -> Features {
        // `FEAT` keys are the verbs as the server spelled them. RFC 2389 says
        // they are case-insensitive, and a few servers do answer in lower case.
        let offers = |verb: &str| features.keys().any(|k| k.trim().eq_ignore_ascii_case(verb));
        Features {
            mlsd: offers("MLST") || offers("MLSD"),
            epsv: offers("EPSV"),
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
) -> Result<Features, RemoteError> {
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

    // One FEAT per connection rather than one per listing. A server that does
    // not understand FEAT at all is old enough to be assumed to offer neither.
    let features = stream
        .feat()
        .map(|features| Features::from_feat(&features))
        .unwrap_or_default();

    // Decided from FEAT rather than by trying EPSV and falling back on a
    // refusal: a failed EPSV happens in the middle of a transfer, where
    // retrying means repeating whatever command it was attached to. Every
    // mainstream server that implements EPSV lists it.
    stream.set_mode(if features.epsv {
        suppaftp::Mode::ExtendedPassive
    } else {
        suppaftp::Mode::Passive
    });

    Ok(features)
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


/// One command that either works or reports the server's own words.
///
/// The three removals and the directory create are all this shape, and writing
/// them out four times invites one of them to report a different path than the
/// one it acted on.
fn simple<T: TlsStream, F>(
    stream: &mut ImplFtpStream<T>,
    path: &str,
    call: F,
) -> Result<(), RemoteError>
where
    F: FnOnce(&mut ImplFtpStream<T>, &str) -> Result<(), suppaftp::FtpError>,
{
    let path = path::normalize(path);
    call(stream, &path).map_err(|e| RemoteError::Server {
        path,
        reason: e.to_string(),
    })
}

fn rename_on<T: TlsStream>(
    stream: &mut ImplFtpStream<T>,
    from: &str,
    to: &str,
) -> Result<(), RemoteError> {
    stream.rename(from, to).map_err(|e| RemoteError::Server {
        // The destination, not the source: a rename fails because of where it
        // was going — a name already taken, a directory that is not writable —
        // far more often than because of where it came from.
        path: to.to_owned(),
        reason: e.to_string(),
    })
}

fn upload_on<T: TlsStream>(
    stream: &mut ImplFtpStream<T>,
    path: &str,
    source: &mut dyn Read,
    on_progress: &mut dyn FnMut(u64) -> Continue,
) -> Result<u64, RemoteError> {
    let path = path::normalize(path);

    let mut data = stream.put_with_stream(&path).map_err(|e| RemoteError::Server {
        path: path.clone(),
        reason: e.to_string(),
    })?;

    let mut buffer = vec![0u8; CHUNK];
    let mut total = 0u64;
    let mut stopped = false;
    let mut failure = None;

    loop {
        let read = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                failure = Some(RemoteError::Io(e));
                break;
            }
        };
        if let Err(e) = data.write_all(&buffer[..read]) {
            failure = Some(RemoteError::Io(e));
            break;
        }
        total += read as u64;
        if on_progress(total) == Continue::Stop {
            stopped = true;
            break;
        }
    }

    // The data connection is finalised either way. Dropping it without
    // `finalize_put_stream` leaves the control connection waiting for a
    // response that never comes, and every later command on this session then
    // reads the wrong reply — a transfer the user stopped would break the
    // connection rather than just ending.
    let finished = stream.finalize_put_stream(data);

    if let Some(e) = failure {
        return Err(e);
    }
    if stopped {
        return Err(RemoteError::Cancelled);
    }
    finished.map_err(|e| RemoteError::Server {
        path,
        reason: e.to_string(),
    })?;
    Ok(total)
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

/// Resolves the host to every address it has, in the resolver's order.
///
/// Returned as a list so [`open_control`] can try each in turn, each attempt
/// with its own connect timeout.
fn resolve(host: &str, port: u16, address: &str) -> Result<Vec<SocketAddr>, RemoteError> {
    use std::net::ToSocketAddrs;
    let endpoints: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| RemoteError::Connect {
            address: address.to_owned(),
            reason: format!("could not be resolved: {e}"),
        })?
        .collect();
    if endpoints.is_empty() {
        return Err(RemoteError::Connect {
            address: address.to_owned(),
            reason: "resolved to no addresses".to_owned(),
        });
    }
    Ok(endpoints)
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

    fn site_at(addr: SocketAddr) -> Site {
        Site {
            id: "stall".into(),
            label: String::new(),
            protocol: Protocol::Ftp,
            host: addr.ip().to_string(),
            port: Some(addr.port()),
            username: String::new(),
            path: None,
            fingerprint: None,
            auth: Default::default(),
            trusted_cert: None,
        }
    }

    #[test]
    fn a_server_that_accepts_and_then_says_nothing_is_given_up_on() {
        // The failure the read deadline exists for: the TCP handshake
        // succeeds, so the connect timeout is satisfied, and then the greeting
        // never comes. Without a read timeout this blocked forever.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        // Detached: it outlives the assertion on purpose, holding the socket
        // open and silent for longer than the client is willing to wait.
        std::thread::spawn(move || {
            let held = listener.accept();
            std::thread::sleep(Duration::from_secs(3));
            drop(held);
        });

        let timeouts = Timeouts {
            connect: Duration::from_secs(2),
            io: Duration::from_millis(300),
        };
        let started = std::time::Instant::now();
        let outcome = connect_with(&site_at(addr), "", timeouts);

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
        let Err(RemoteError::Connect { reason, .. }) = outcome else {
            panic!("a silent server should fail to connect: {:?}", outcome.err());
        };
        assert!(reason.contains("did not answer"), "{reason}");
    }

    #[test]
    fn data_connections_carry_the_same_deadlines_as_the_control_connection() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let timeouts = Timeouts {
            connect: Duration::from_secs(2),
            io: Duration::from_millis(1234),
        };

        let stream = data_connector(timeouts)(addr).expect("should connect");
        assert_eq!(stream.read_timeout().unwrap(), Some(timeouts.io));
        assert_eq!(stream.write_timeout().unwrap(), Some(timeouts.io));
    }

    #[test]
    fn every_resolved_address_is_kept() {
        // `localhost` commonly resolves to both ::1 and 127.0.0.1; whatever it
        // resolves to here, nothing may be dropped on the way through.
        use std::net::ToSocketAddrs;
        let expected: Vec<SocketAddr> = ("localhost", 21).to_socket_addrs().unwrap().collect();
        assert_eq!(resolve("localhost", 21, "localhost:21").unwrap(), expected);
    }

    #[test]
    fn feat_verbs_are_matched_without_regard_to_case() {
        let mut feat = suppaftp::types::Features::new();
        feat.insert("epsv".to_owned(), None);
        feat.insert("MLST".to_owned(), Some("type*;size*;".to_owned()));
        let features = Features::from_feat(&feat);
        assert!(features.epsv);
        assert!(features.mlsd);

        let none = Features::from_feat(&suppaftp::types::Features::new());
        assert!(!none.epsv && !none.mlsd);
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
