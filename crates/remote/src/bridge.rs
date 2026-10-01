//! `pitcrewd connect`: the remote end of the stdio bridge.
//!
//! Where a site forbids forwarding unix sockets (`AllowStreamLocalForwarding no`), or the laptop
//! cannot forward one (Windows), the tunnel ([`crate::tunnel`]) runs
//! `ssh <host> exec pitcrewd connect --socket <path> --nonce <hex>` for each connection and
//! speaks to the daemon through ssh's stdin and stdout. This module is that command; the
//! daemon's CLI calls [`main`] with the arguments after `connect` (or [`connect_stdio`]).
//!
//! **Before any byte is passed on**, it makes the checks a client of the daemon makes:
//! - the socket's directory is a real directory (not a symbolic link) owned by this user, with
//!   no access for the group or others;
//! - the socket is a socket (not a link) owned by this user;
//! - the process listening on it runs as this user (`SO_PEERCRED`, `getpeereid`).
//!
//! Then it prints its ready mark on stdout ([`ready_mark`]: [`READY`], with the caller's nonce
//! when it gave one), and copies stdin to the socket and the socket to stdout. The laptop
//! discards whatever comes before the mark (a start-up file's chatter). Either side may
//! half-close: end of file on stdin shuts down the socket's write side, and the daemon's end of
//! file closes stdout, while the other direction goes on. It returns once the client has
//! finished (end of file on stdin) and the daemon has too, or as soon as the daemon has closed
//! its side completely, or the client went away.
//!
//! **Framed** (`--framed`, for `srun`, whose output forwarding may hold back a line until it
//! ends): what goes to stdout is cut into frames, `<length: 8 hex digits>:<bytes>\n`, each
//! ending a line; `00000000:\n` marks the daemon's end of file. stdin stays plain.
//!
//! **The command line** ([`main`]): `--socket <path> [--framed] [--nonce <hex>]`, the
//! arguments after `pitcrewd connect`.
//!
//! **Blocking:** [`connect_stdio`] blocks its thread with plain reads and writes. It works
//! inside or outside a tokio runtime (the one async call it needs runs on a thread of its own),
//! but from async code call it with `spawn_blocking`.
//!
//! Errors name what is wrong, never the path (which carries the user's name). Unix only; on
//! other systems [`connect_stdio`] fails with [`BridgeError::Unsupported`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The ready mark without a nonce: what the bridge prints once its checks have passed, before
/// the daemon's first byte. The NUL keeps any start-up file's text from looking like it.
pub const READY: &[u8] = b"\0pitcrew-bridge 1 ready\n";

/// The longest nonce accepted (hex digits).
pub const MAX_NONCE: usize = 64;

/// The length of a frame's header: 8 hex digits and `:`.
pub const FRAME_HEADER: usize = 9;

/// The mark the bridge prints for `nonce`: `\0pitcrew-bridge 1 ready <nonce>\n`, or [`READY`]
/// without one.
#[must_use]
pub fn ready_mark(nonce: Option<&str>) -> Vec<u8> {
    match nonce {
        Some(nonce) => {
            let mut mark = READY.strip_suffix(b"\n").unwrap_or(READY).to_vec();
            mark.push(b' ');
            mark.extend_from_slice(nonce.as_bytes());
            mark.push(b'\n');
            mark
        }
        None => READY.to_vec(),
    }
}

/// What the bridge is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The daemon's socket.
    pub socket: PathBuf,
    /// Frame what goes to stdout (see the module docs).
    pub framed: bool,
    /// Put this in the ready mark: 1 to [`MAX_NONCE`] lower-case hex digits.
    pub nonce: Option<String>,
}

impl Options {
    /// Plain, for `socket`.
    #[must_use]
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            framed: false,
            nonce: None,
        }
    }

    /// Reads `--socket <path> [--framed] [--nonce <hex>]`, strictly.
    ///
    /// # Errors
    /// [`BridgeError::Usage`] naming the problem.
    pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Result<Self, BridgeError> {
        let usage = |why: &str| {
            Err(BridgeError::Usage(format!(
                "{why}; usage: pitcrewd connect --socket <path> [--framed] [--nonce <hex>]"
            )))
        };
        let (mut socket, mut framed, mut nonce) = (None, false, None);
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--socket") if socket.is_none() => match args.next() {
                    Some(path) => socket = Some(PathBuf::from(path)),
                    None => return usage("--socket needs a path"),
                },
                Some("--framed") if !framed => framed = true,
                Some("--nonce") if nonce.is_none() => {
                    let value = args.next().and_then(|v| v.into_string().ok());
                    match value {
                        Some(v) if valid_nonce(&v) => nonce = Some(v),
                        _ => return usage("--nonce needs 1 to 64 lower-case hex digits"),
                    }
                }
                _ => return usage("an unknown or repeated argument"),
            }
        }
        let Some(socket) = socket else {
            return usage("--socket is missing");
        };
        Ok(Self {
            socket,
            framed,
            nonce,
        })
    }
}

fn valid_nonce(nonce: &str) -> bool {
    (1..=MAX_NONCE).contains(&nonce.len())
        && nonce
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `pitcrewd connect <args>`: reads the arguments ([`Options::parse`]), runs
/// [`connect_stdio`], says any error on stderr (as `pitcrewd connect: …`) and turns it into the
/// exit code. What the daemon's CLI calls with the arguments after `connect`.
#[must_use]
pub fn main<I: IntoIterator<Item = OsString>>(args: I) -> ExitCode {
    match Options::parse(args).and_then(|options| connect_stdio(&options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pitcrewd connect: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}

/// Exit code: the command line was wrong (also clap's, for an unknown subcommand).
pub const EXIT_USAGE: u8 = 2;
/// Exit code: the socket, its directory or the process behind it is not this user's.
pub const EXIT_UNSAFE: u8 = 3;
/// Exit code: no daemon listens there (no socket, or nothing accepting).
pub const EXIT_NO_DAEMON: u8 = 4;
/// Exit code: any other failure.
pub const EXIT_FAILED: u8 = 1;

/// Why the bridge did not start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BridgeError {
    /// The socket path cannot be used as given.
    #[error("{0}")]
    Usage(String),
    /// The socket, its directory or the process listening is not this user's alone.
    #[error("the daemon's socket is not safe to use: {0}")]
    Unsafe(String),
    /// Nothing listens there.
    #[error("no daemon listens on the socket: {0}")]
    NoDaemon(String),
    /// Reading, writing or connecting failed.
    #[error("{0}")]
    Io(#[source] std::io::Error),
    /// This system has no unix sockets for the bridge.
    #[error("the stdio bridge needs unix sockets")]
    Unsupported,
}

impl BridgeError {
    /// The exit code `pitcrewd connect` ends with.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => EXIT_USAGE,
            Self::Unsafe(_) => EXIT_UNSAFE,
            Self::NoDaemon(_) => EXIT_NO_DAEMON,
            Self::Io(_) | Self::Unsupported => EXIT_FAILED,
        }
    }
}

/// `pitcrewd connect --socket <socket>`, plain: [`main`] for code that has a path.
#[must_use]
pub fn run(socket: &Path) -> ExitCode {
    main([OsString::from("--socket"), socket.as_os_str().to_owned()])
}

/// Connects stdin and stdout to the daemon listening on `options.socket`, after the checks in
/// the module docs. Returns when the connection is over (see the module docs); a thread may
/// still be blocked reading stdin then, so the process should exit.
///
/// # Errors
/// A check fails ([`BridgeError::Unsafe`]), nothing listens ([`BridgeError::NoDaemon`]), the
/// nonce is not hex, or connecting or writing the ready mark fails. Once the mark is out, the
/// end of the connection, however it came, is not an error.
#[cfg(unix)]
pub fn connect_stdio(options: &Options) -> Result<(), BridgeError> {
    use std::io::Write as _;
    if options.nonce.as_deref().is_some_and(|n| !valid_nonce(n)) {
        return Err(BridgeError::Usage(
            "the nonce must be 1 to 64 lower-case hex digits".to_owned(),
        ));
    }
    let stream = unix::open(&options.socket)?;
    let mut out = std::io::stdout();
    out.write_all(&ready_mark(options.nonce.as_deref()))
        .and_then(|()| out.flush())
        .map_err(BridgeError::Io)?;
    unix::pump(
        stream,
        std::io::stdin(),
        out,
        options.framed,
        unix::close_stdout,
    );
    Ok(())
}

/// See the Unix version.
///
/// # Errors
/// Always [`BridgeError::Unsupported`].
#[cfg(not(unix))]
pub fn connect_stdio(options: &Options) -> Result<(), BridgeError> {
    let _ = options;
    Err(BridgeError::Unsupported)
}

#[cfg(unix)]
pub(crate) mod unix {
    use super::BridgeError;
    use std::io::{self, Read, Write};
    use std::net::Shutdown;
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    /// Checks `socket` and its directory, connects, and checks who listens.
    pub(crate) fn open(socket: &Path) -> Result<UnixStream, BridgeError> {
        check(socket)?;
        let stream = UnixStream::connect(socket).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
                BridgeError::NoDaemon("nothing accepts connections there".to_owned())
            }
            _ => BridgeError::Io(e),
        })?;
        let (uid, stream) = peer_uid(stream).map_err(BridgeError::Io)?;
        let me = crate::private::euid();
        if uid != me {
            return Err(BridgeError::Unsafe(format!(
                "the process listening on it runs as uid {uid}, not {me}"
            )));
        }
        Ok(stream)
    }

    /// The socket and its directory are this user's alone.
    pub(crate) fn check(socket: &Path) -> Result<(), BridgeError> {
        if !socket.is_absolute() {
            return Err(BridgeError::Usage(
                "the socket must be an absolute path".to_owned(),
            ));
        }
        let Some(dir) = socket.parent().filter(|d| !d.as_os_str().is_empty()) else {
            return Err(BridgeError::Usage("the socket has no directory".to_owned()));
        };
        let me = crate::private::euid();
        let unsafe_ = |why: String| Err(BridgeError::Unsafe(why));
        let meta = match std::fs::symlink_metadata(dir) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(BridgeError::NoDaemon(
                    "its directory does not exist".to_owned(),
                ));
            }
            Err(e) => return Err(BridgeError::Io(e)),
        };
        if meta.file_type().is_symlink() {
            return unsafe_("its directory is a symbolic link".to_owned());
        }
        if !meta.is_dir() {
            return unsafe_("its directory is not a directory".to_owned());
        }
        if meta.uid() != me {
            return unsafe_(format!("its directory belongs to uid {}", meta.uid()));
        }
        if meta.mode() & 0o077 != 0 {
            return unsafe_(format!(
                "its directory is open to others (mode {:o})",
                meta.mode() & 0o7777
            ));
        }
        let meta = match std::fs::symlink_metadata(socket) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(BridgeError::NoDaemon("there is no socket".to_owned()));
            }
            Err(e) => return Err(BridgeError::Io(e)),
        };
        if meta.file_type().is_symlink() {
            return unsafe_("it is a symbolic link".to_owned());
        }
        if !meta.file_type().is_socket() {
            return unsafe_("it is not a socket".to_owned());
        }
        if meta.uid() != me {
            return unsafe_(format!("it belongs to uid {}", meta.uid()));
        }
        Ok(())
    }

    /// The uid of the process that listens on `stream`'s other end. tokio has the portable
    /// call (`SO_PEERCRED` on Linux, `getpeereid` elsewhere); the stream goes in and comes back.
    /// It runs on a thread of its own with a runtime of its own, so the caller may be inside a
    /// runtime or not (a runtime cannot be dropped inside another's context).
    fn peer_uid(stream: UnixStream) -> io::Result<(u32, UnixStream)> {
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_io()
                        .build()?;
                    let _entered = runtime.enter();
                    stream.set_nonblocking(true)?;
                    let stream = tokio::net::UnixStream::from_std(stream)?;
                    let uid = stream.peer_cred()?.uid();
                    let stream = stream.into_std()?;
                    stream.set_nonblocking(false)?;
                    Ok((uid, stream))
                })
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("the peer check failed")))
        })
    }

    /// Ends this process's standard output: points it at `/dev/null`, so the client reads end
    /// of file (once nothing else holds the pipe) while it can still write.
    pub(crate) fn close_stdout(mut out: std::io::Stdout) {
        let _ = out.flush();
        if let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
            let _ = rustix::stdio::dup2_stdout(&null);
        }
    }

    /// Copies `input` to `socket` on a thread and `socket` to `output` here (in frames when
    /// `framed`), with half-closes passed on (see the module docs). `close_output` ends `output`
    /// once the daemon has finished sending.
    pub(crate) fn pump<R, W>(
        socket: UnixStream,
        input: R,
        mut output: W,
        framed: bool,
        close_output: impl FnOnce(W),
    ) where
        R: Read + Send + 'static,
        W: Write,
    {
        let Ok(up) = socket.try_clone() else {
            return;
        };
        let Ok(up) = std::thread::Builder::new()
            .name("bridge-up".to_owned())
            .spawn(move || copy_up(input, up))
        else {
            return;
        };
        let mut buf = vec![0u8; 64 * 1024];
        let mut from = &socket;
        let daemon_done = loop {
            match from.read(&mut buf) {
                Ok(0) => break true,
                Ok(n) => {
                    let Some(chunk) = buf.get(..n) else {
                        break false;
                    };
                    if send(&mut output, chunk, framed).is_err() {
                        // The client went away.
                        break false;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break false,
            }
        };
        if !daemon_done || (framed && send(&mut output, &[], true).is_err()) {
            let _ = socket.shutdown(Shutdown::Both);
            return;
        }
        // The daemon has nothing more to say: the client hears so, and may still talk, unless
        // the daemon has closed its side altogether.
        close_output(output);
        while !up.is_finished() && !hung_up(&socket) {}
    }

    /// Writes `chunk` (a frame of it when `framed`; an empty frame is the end mark) and flushes.
    fn send<W: Write>(output: &mut W, chunk: &[u8], framed: bool) -> io::Result<()> {
        if framed {
            let header = format!("{:08x}:", chunk.len());
            output.write_all(header.as_bytes())?;
            output.write_all(chunk)?;
            output.write_all(b"\n")?;
        } else {
            output.write_all(chunk)?;
        }
        output.flush()
    }

    /// stdin to the socket; at its end, the socket's write side is shut down.
    fn copy_up<R: Read>(mut input: R, mut socket: UnixStream) {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let Some(chunk) = buf.get(..n) else { break };
                    if socket.write_all(chunk).is_err() {
                        // The daemon stopped reading.
                        return;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = socket.shutdown(Shutdown::Write);
    }

    /// Waits up to 200 ms for the daemon to close its side of `socket` completely (both
    /// directions: `POLLHUP`), and says whether it has.
    fn hung_up(socket: &UnixStream) -> bool {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let mut fds = [PollFd::new(socket, PollFlags::empty())];
        let wait = Timespec {
            tv_sec: 0,
            tv_nsec: 200_000_000,
        };
        match poll(&mut fds, Some(&wait)) {
            Ok(_) => fds
                .first()
                .is_some_and(|fd| fd.revents().intersects(PollFlags::HUP | PollFlags::ERR)),
            Err(_) => {
                std::thread::sleep(Duration::from_millis(200));
                false
            }
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::unix::{check, open, pump};
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::{UnixListener, UnixStream};

    fn private(dir: &Path) {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn sockets_that_are_not_ours_alone_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let run = tmp.path().join("run");
        std::fs::create_dir(&run).unwrap();
        private(&run);
        let socket = run.join("pitcrewd.sock");
        assert!(matches!(check(&socket), Err(BridgeError::NoDaemon(_))));
        let _listener = UnixListener::bind(&socket).unwrap();
        check(&socket).unwrap();
        open(&socket).unwrap();

        // A directory others can enter.
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o750)).unwrap();
        let err = check(&socket).unwrap_err();
        assert!(
            matches!(&err, BridgeError::Unsafe(why) if why.contains("750")),
            "{err}"
        );
        assert_eq!(err.exit_code(), EXIT_UNSAFE);
        private(&run);

        // A link to the socket, or to its directory.
        let link = run.join("link.sock");
        std::os::unix::fs::symlink(&socket, &link).unwrap();
        assert!(matches!(check(&link), Err(BridgeError::Unsafe(_))));
        let dir_link = tmp.path().join("run-link");
        std::os::unix::fs::symlink(&run, &dir_link).unwrap();
        assert!(matches!(
            check(&dir_link.join("pitcrewd.sock")),
            Err(BridgeError::Unsafe(_))
        ));
        // A file that is not a socket.
        let file = run.join("file.sock");
        std::fs::write(&file, "").unwrap();
        assert!(matches!(check(&file), Err(BridgeError::Unsafe(_))));
        // Relative, or nothing listening.
        assert!(matches!(
            check(Path::new("run/pitcrewd.sock")),
            Err(BridgeError::Usage(_))
        ));
        drop(_listener);
        let err = open(&socket).unwrap_err();
        assert!(matches!(err, BridgeError::NoDaemon(_)), "{err}");
        // No path in any message: they carry the user's name.
        for err in [check(&link).unwrap_err(), check(&file).unwrap_err()] {
            assert!(!err.to_string().contains('/'), "{err}");
        }
    }

    /// Both half-closes, through `pump` with socket pairs standing in for stdin and stdout.
    #[test]
    fn half_closes_pass_through() {
        let (daemon, bridge_side) = UnixStream::pair().unwrap();
        let (mut client_in, bridge_in) = UnixStream::pair().unwrap();
        let (bridge_out, mut client_out) = UnixStream::pair().unwrap();
        let pumping = std::thread::spawn(move || {
            pump(bridge_side, bridge_in, bridge_out, false, |out| {
                let _ = out.shutdown(std::net::Shutdown::Write);
            });
        });
        // The daemon is done talking first; the client still talks.
        let mut daemon = daemon;
        daemon.write_all(b"bye").unwrap();
        daemon.shutdown(std::net::Shutdown::Write).unwrap();
        let mut got = Vec::new();
        client_out.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"bye");
        client_in.write_all(b"still here").unwrap();
        client_in.shutdown(std::net::Shutdown::Write).unwrap();
        let mut heard = Vec::new();
        daemon.read_to_end(&mut heard).unwrap();
        assert_eq!(heard, b"still here");
        pumping.join().unwrap();
    }

    /// A daemon that closes its side completely ends the bridge, though the client never
    /// closes stdin.
    #[test]
    fn a_closed_daemon_ends_the_bridge() {
        let (daemon, bridge_side) = UnixStream::pair().unwrap();
        let (_client_in, bridge_in) = UnixStream::pair().unwrap();
        let (bridge_out, mut client_out) = UnixStream::pair().unwrap();
        let pumping = std::thread::spawn(move || {
            pump(bridge_side, bridge_in, bridge_out, false, drop);
        });
        let mut daemon = daemon;
        daemon.write_all(b"done").unwrap();
        drop(daemon);
        let mut got = Vec::new();
        client_out.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"done");
        pumping.join().unwrap();
    }

    /// Framed: every frame ends a line, and an empty frame marks the end.
    #[test]
    fn frames_end_lines() {
        let (daemon, bridge_side) = UnixStream::pair().unwrap();
        let (_client_in, bridge_in) = UnixStream::pair().unwrap();
        let (bridge_out, mut client_out) = UnixStream::pair().unwrap();
        let pumping = std::thread::spawn(move || {
            pump(bridge_side, bridge_in, bridge_out, true, drop);
        });
        let mut daemon = daemon;
        daemon.write_all(b"no newline").unwrap();
        drop(daemon);
        let mut got = Vec::new();
        client_out.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"0000000a:no newline\n00000000:\n");
        pumping.join().unwrap();
    }

    #[test]
    fn arguments_are_read_strictly() {
        let parse = |args: &[&str]| Options::parse(args.iter().map(OsString::from));
        assert_eq!(
            parse(&["--socket", "/r/pitcrewd.sock"]).unwrap(),
            Options::new("/r/pitcrewd.sock")
        );
        let all = parse(&["--framed", "--nonce", "0a1b", "--socket", "/r/x.sock"]).unwrap();
        assert!(all.framed);
        assert_eq!(all.nonce.as_deref(), Some("0a1b"));
        for bad in [
            &[][..],
            &["--socket"],
            &["--socket", "/a", "--socket", "/b"],
            &["--socket", "/a", "--nonce", "XYZ"],
            &["--socket", "/a", "--nonce", ""],
            &["--socket", "/a", "--verbose"],
            &["/a"],
        ] {
            let err = parse(bad).unwrap_err();
            assert_eq!(err.exit_code(), EXIT_USAGE, "{bad:?}");
        }
        assert_eq!(ready_mark(None), READY);
        assert_eq!(ready_mark(Some("ab12")), b"\0pitcrew-bridge 1 ready ab12\n");
    }

    /// The peer check works from inside a runtime too (a daemon CLI with an async main).
    #[tokio::test(flavor = "multi_thread")]
    async fn the_peer_check_works_inside_a_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let run = tmp.path().join("run");
        std::fs::create_dir(&run).unwrap();
        private(&run);
        let socket = run.join("pitcrewd.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        open(&socket).unwrap();
    }
}
