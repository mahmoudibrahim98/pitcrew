//! Test support: the real `pitcrewd` binary on a temp state directory and a free port, and a
//! small HTTP and WebSocket client over plain TCP.

#![allow(dead_code, clippy::unwrap_used)]

use serde_json::Value;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// The binary under test.
pub const PITCREWD: &str = env!("CARGO_BIN_EXE_pitcrewd");

/// How long a daemon may take to say it is listening. Generous: CI machines are busy.
const READY: Duration = Duration::from_secs(60);

/// Demo fixture ids.
pub mod id {
    pub const SAM: &str = "01JB000000000000000MEM0001";
    pub const WRITER: &str = "01JB000000000000000MEM0002";
    /// The back office's agent.
    pub const OFFICE: &str = "01JB000000000000000MEM0006";
    pub const PAPER: &str = "01JB000000000000000PRJ0001";
    pub const TOOLING: &str = "01JB000000000000000PRJ0002";
    pub const SUBMISSION: &str = "01JB000000000000000WST0001";
    pub const PARSERS: &str = "01JB000000000000000WST0003";
    pub const PAP1: &str = "01JB000000000000000TSK0001";
    pub const PAP2: &str = "01JB000000000000000TSK0002";
    pub const PAP3: &str = "01JB000000000000000TSK0003";
    pub const PAP7: &str = "01JB000000000000000TSK0007";
    pub const TL1: &str = "01JB000000000000000TSK0008";
    pub const SES1: &str = "01JB000000000000000SES0001";
    /// On the demo's `cluster`, a machine this hub cannot reach.
    pub const SES2: &str = "01JB000000000000000SES0002";
    /// The demo's own machine, "This laptop": the runner's.
    pub const LAPTOP: &str = "01JB000000000000000MCH0001";
    pub const REVIEWER: &str = "01JB000000000000000MEM0004";
    pub const SES_UNKNOWN: &str = "01JB000000000000000SES0099";
    /// PAP-1's active dispatch (@writer, session 1).
    pub const DSP1: &str = "01JB000000000000000DSP0001";
    /// PAP-3's finished dispatch.
    pub const DSP4: &str = "01JB000000000000000DSP0004";
    pub const WORKSPACE: &str = "01JB000000000000000WSP0001";
}

/// A running daemon. Killed when dropped, unless it was stopped.
#[derive(Debug)]
pub struct Daemon {
    child: Child,
    /// The TCP port; 0 when it listens on a unix socket.
    pub port: u16,
    /// Where it listens, from its ready line: `http://…` or a socket path.
    pub at: String,
    pub state: PathBuf,
    /// How long it took to print its ready line.
    pub ready_in: Duration,
    stderr: Arc<Mutex<String>>,
}

/// A daemon that exited instead of becoming ready.
#[derive(Debug)]
pub struct Refused {
    pub status: ExitStatus,
    pub stderr: String,
}

impl Daemon {
    /// Starts `pitcrewd --state-dir <state> serve --listen tcp:127.0.0.1:0 <extra>` and waits for
    /// its ready line. Panics if it does not become ready.
    pub fn start(state: &Path, extra: &[&str]) -> Self {
        match Self::try_start(state, extra) {
            Ok(daemon) => daemon,
            Err(refused) => panic!(
                "pitcrewd did not start ({}):\n{}",
                refused.status, refused.stderr
            ),
        }
    }

    /// As [`Daemon::start`], but returns how it failed.
    pub fn try_start(state: &Path, extra: &[&str]) -> Result<Self, Refused> {
        Self::try_start_on(state, "tcp:127.0.0.1:0", extra)
    }

    /// As [`Daemon::start`], listening on `listen` (a `--listen` value).
    pub fn start_on(state: &Path, listen: &str, extra: &[&str]) -> Self {
        Self::try_start_on(state, listen, extra).unwrap_or_else(|refused| {
            panic!(
                "pitcrewd did not start ({}):\n{}",
                refused.status, refused.stderr
            )
        })
    }

    /// As [`Daemon::start_on`], but returns how it failed.
    pub fn try_start_on(state: &Path, listen: &str, extra: &[&str]) -> Result<Self, Refused> {
        let started = Instant::now();
        let mut command = Command::new(PITCREWD);
        command
            .arg("--state-dir")
            .arg(state)
            .args(["serve", "--listen", listen])
            .args(extra)
            .env("PITCREW_LOG", "debug");
        let mut child = private_homes(&mut command, state)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn pitcrewd");

        let stderr = Arc::new(Mutex::new(String::new()));
        let mut err = child.stderr.take().expect("stderr");
        let sink = Arc::clone(&stderr);
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });

        let (lines, ready) = mpsc::channel();
        let out = child.stdout.take().expect("stdout");
        thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if lines.send(line).is_err() {
                    break;
                }
            }
        });

        // The ready line is the first thing on stdout.
        match ready.recv_timeout(READY) {
            Ok(line) => {
                let Some(at) = line.strip_prefix("pitcrewd listening on ") else {
                    panic!("unexpected stdout line: {line}");
                };
                let port = match at.strip_prefix("http://") {
                    Some(host) => host.rsplit(':').next().unwrap().parse().unwrap(),
                    None => 0,
                };
                Ok(Self {
                    child,
                    port,
                    at: at.to_owned(),
                    state: state.to_path_buf(),
                    ready_in: started.elapsed(),
                    stderr,
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let status = child.wait().expect("wait");
                // Let the stderr reader finish.
                thread::sleep(Duration::from_millis(100));
                let stderr = stderr.lock().unwrap().clone();
                Err(Refused { status, stderr })
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "pitcrewd was not ready within {READY:?}:\n{}",
                    stderr.lock().unwrap()
                );
            }
        }
    }

    /// Everything it wrote to stderr so far.
    pub fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// The device token `serve` wrote.
    pub fn device_token(&self) -> String {
        read_token(&self.state.join("device.token"))
    }

    /// The demo agent's token `serve --demo` wrote.
    pub fn agent_token(&self) -> String {
        read_token(&self.state.join("demo-agent.token"))
    }

    pub fn get(&self, path: &str, token: Option<&str>) -> Reply {
        request(self.port, "GET", path, token, None, &[])
    }

    pub fn post(&self, path: &str, token: Option<&str>, body: &Value) -> Reply {
        request(self.port, "POST", path, token, Some(body), &[])
    }

    /// Sends SIGTERM and waits for the process to exit.
    #[cfg(unix)]
    pub fn terminate(&mut self) -> ExitStatus {
        self.signal("TERM")
    }

    /// Sends `signal` (`TERM`, `HUP`, …) and waits for the process to exit.
    #[cfg(unix)]
    pub fn signal(&mut self, signal: &str) -> ExitStatus {
        let sent = Command::new("kill")
            .args([&format!("-{signal}"), &self.child.id().to_string()])
            .status()
            .expect("run kill");
        assert!(sent.success(), "kill -{signal} failed");
        self.wait_exit(Duration::from_secs(20))
    }

    /// Stops it: SIGTERM on Unix (and checks it stopped cleanly), killed elsewhere.
    pub fn stop(&mut self) {
        #[cfg(unix)]
        {
            let status = self.terminate();
            assert!(status.success(), "{status}:\n{}", self.stderr());
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Waits until its stderr contains `text`, at most `within`.
    pub fn wait_for_log(&self, text: &str, within: Duration) {
        let deadline = Instant::now() + within;
        while !self.stderr().contains(text) {
            assert!(
                Instant::now() < deadline,
                "no {text:?} in the log within {within:?}:\n{}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Every event of the log, oldest first, paged back through `GET /v1/events`.
    pub fn all_events(&self, token: &str) -> Vec<Value> {
        self.events_matching("", token)
    }

    /// Every event `GET /v1/events?<query>` matches, oldest first, paged back until `at_start`.
    pub fn events_matching(&self, query: &str, token: &str) -> Vec<Value> {
        self.events_paged(query, 500, token)
    }

    /// As [`Daemon::events_matching`], `limit` events a page.
    pub fn events_paged(&self, query: &str, limit: usize, token: &str) -> Vec<Value> {
        let mut events = Vec::new();
        let mut before: Option<u64> = None;
        for _ in 0..1000 {
            let mut path = format!("/v1/events?limit={limit}{query}");
            if let Some(before) = before {
                path.push_str(&format!("&before={before}"));
            }
            let reply = self.get(&path, Some(token));
            assert_eq!(reply.status, 200, "{path}: {}", reply.body);
            let page = reply.json();
            let mut older = page["events"].as_array().unwrap().clone();
            older.extend(events);
            events = older;
            if page["at_start"].as_bool().unwrap() {
                return events;
            }
            before = Some(page["from_rev"].as_u64().unwrap());
        }
        panic!("{query}: paging never reached the start");
    }

    /// Waits until the back office has looked at every revision of the log, and returns the
    /// newest. It knows from the daemon's debug log: a run that ended there, or a start with
    /// nothing to look at. Needs the back office on, and `PITCREW_LOG=debug` (as `start` sets).
    pub fn settle(&self, token: &str) -> u64 {
        let deadline = Instant::now() + READY;
        loop {
            let latest = self.latest_rev(token);
            let logs = self.stderr();
            let ran = logs.contains(&format!(" to={latest} applied="))
                || logs.contains(&format!(" from={} latest={latest}", latest + 1));
            if ran && self.latest_rev(token) == latest {
                return latest;
            }
            assert!(
                Instant::now() < deadline,
                "the back office did not reach revision {latest}:\n{logs}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// The newest revision, from `GET /v1/events`.
    pub fn latest_rev(&self, token: &str) -> u64 {
        let reply = self.get("/v1/events?limit=1", Some(token));
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()["to_rev"].as_u64().unwrap()
    }

    /// Waits for the process to exit, at most `within`.
    pub fn wait_exit(&mut self, within: Duration) -> ExitStatus {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                // Let the stderr reader finish.
                thread::sleep(Duration::from_millis(100));
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "pitcrewd did not exit within {within:?}:\n{}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Appends `bodies` to the demo workspace's store in `state` from this process, authored by
/// `author` on behalf of `owner`, as another writer would (here: in place of the runner link, which
/// is not wired in yet). A running daemon is not told: it looks at them with its next append, or at
/// its next start. Returns their revisions.
pub fn append_to_store(
    state: &Path,
    author: &str,
    owner: Option<&str>,
    bodies: Vec<pitcrew_protocol::events::EventBody>,
) -> pitcrew_store::RevRange {
    use pitcrew_protocol::events::Event;
    let store =
        pitcrew_store::Store::open(state.join("hub.db"), pitcrew_store::StoreOptions::default())
            .expect("open the store");
    let events: Vec<Event> = bodies
        .into_iter()
        .map(|body| Event {
            on_behalf_of: owner.map(|o| o.parse().unwrap()),
            ..Event::now(
                id::WORKSPACE.parse().unwrap(),
                author.parse().unwrap(),
                body,
            )
        })
        .collect();
    store.append(&events).expect("append")
}

pub fn read_token(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .trim()
        .to_owned()
}

/// The home folder a daemon on `state` takes for this user's: `<state>-home`, next to it in the
/// test's temporary folder. A test may lay out agent homes in it (`.claude/projects/…`) to stand
/// for the person's own.
pub fn home_of(state: &Path) -> PathBuf {
    let mut home = state.as_os_str().to_owned();
    home.push("-home");
    PathBuf::from(home)
}

/// **Never the real homes.** A daemon started without `--homes` (and without `--demo`) watches
/// this user's agent homes, which hold the person's private transcripts. Every daemon a test
/// starts therefore gets [`home_of`] as its home folder, and none of the variables that point
/// the adapters elsewhere.
fn private_homes<'a>(command: &'a mut Command, state: &Path) -> &'a mut Command {
    let home = home_of(state);
    command
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("XDG_DATA_HOME")
}

/// Runs `pitcrewd <args>` to completion, with a home folder of its own (see [`private_homes`]).
pub fn run(args: &[&std::ffi::OsStr]) -> std::process::Output {
    let home = tempfile::tempdir().expect("a temporary home");
    let mut command = Command::new(PITCREWD);
    command.args(args);
    private_homes(&mut command, &home.path().join("state"))
        .stdin(Stdio::null())
        .output()
        .expect("run pitcrewd")
}

// ─── HTTP ────────────────────────────────────────────────────────────────────────────────────────

/// A response.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {:?}", self.body))
    }

    /// The `code` of an `ApiError` body.
    pub fn code(&self) -> String {
        self.json()["code"].as_str().unwrap_or_default().to_owned()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// One HTTP/1.1 request on a fresh connection.
pub fn request(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&Value>,
    headers: &[(&str, &str)],
) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut head =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    let body = body.map(Value::to_string).unwrap_or_default();
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    if !body.is_empty() || method == "POST" || method == "PUT" || method == "PATCH" {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read the response");
    let split = find(&raw, b"\r\n\r\n").expect("a complete response head");
    let (status, headers) = parse_head(&raw[..split]);
    let mut body = raw[split + 4..].to_vec();
    let chunked = headers.iter().any(|(n, v)| {
        n.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked")
    });
    if chunked {
        body = dechunk(&body);
    }
    Reply {
        status,
        headers,
        body: String::from_utf8(body).expect("a UTF-8 body"),
    }
}

fn parse_head(head: &[u8]) -> (u16, Vec<(String, String)>) {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap();
    let status = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("bad status line {status_line:?}"));
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
        .collect();
    (status, headers)
}

fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line_end = find(body, b"\r\n").expect("a chunk size");
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&body[..line_end])
                .split(';')
                .next()
                .unwrap()
                .trim(),
            16,
        )
        .expect("a hex chunk size");
        body = &body[line_end + 2..];
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ─── WebSocket ───────────────────────────────────────────────────────────────────────────────────

/// A frame from the server.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Text(String),
    Binary(Vec<u8>),
    /// The close code, if any, and the reason.
    Close(Option<u16>, String),
    Ping,
    Pong,
}

/// A minimal WebSocket client (no fragmentation; the server's frames are small).
#[derive(Debug)]
pub struct Ws {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Ws {
    /// Opens `path` with the token as a subprotocol, as the browser does. `Err` holds the HTTP
    /// answer when the server does not switch protocols.
    pub fn connect(port: u16, path: &str, token: &str) -> Result<Self, Reply> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.{token}\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let split = loop {
            if let Some(split) = find(&buf, b"\r\n\r\n") {
                break split;
            }
            let n = stream.read(&mut chunk).expect("read the handshake");
            assert!(n > 0, "the server closed during the handshake");
            buf.extend_from_slice(&chunk[..n]);
        };
        let (status, headers) = parse_head(&buf[..split]);
        let rest = buf[split + 4..].to_vec();
        if status != 101 {
            // Read the rest of the error body; the connection may stay open, so not for long.
            let mut body = rest;
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let _ = stream.read_to_end(&mut body);
            return Err(Reply {
                status,
                headers,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        let protocol = headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("sec-websocket-protocol"))
            .map(|(_, v)| v.clone());
        assert_eq!(protocol.as_deref(), Some("pitcrew.v1"));
        Ok(Self { stream, buf: rest })
    }

    /// The next frame, waiting at most `within`. `Ok(None)` when the connection ended without a
    /// Close frame.
    pub fn next(&mut self, within: Duration) -> io::Result<Option<Frame>> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(frame) = self.parse() {
                return Ok(Some(frame));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no frame in time"));
            }
            self.stream.set_read_timeout(Some(left))?;
            let mut chunk = [0u8; 8192];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(None),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                    ) =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// The next text frame as JSON, skipping pings.
    pub fn next_json(&mut self, within: Duration) -> Value {
        loop {
            match self.next(within).expect("a frame") {
                Some(Frame::Text(text)) => return serde_json::from_str(&text).expect("JSON"),
                Some(Frame::Ping | Frame::Pong) => {}
                other => panic!("expected a text frame, got {other:?}"),
            }
        }
    }

    fn parse(&mut self) -> Option<Frame> {
        let buf = &self.buf;
        if buf.len() < 2 {
            return None;
        }
        let opcode = buf[0] & 0x0f;
        assert_eq!(buf[1] & 0x80, 0, "server frames are not masked");
        let (len, start) = match buf[1] & 0x7f {
            126 if buf.len() >= 4 => (usize::from(u16::from_be_bytes([buf[2], buf[3]])), 4),
            127 if buf.len() >= 10 => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&buf[2..10]);
                (usize::try_from(u64::from_be_bytes(bytes)).unwrap(), 10)
            }
            126 | 127 => return None,
            n => (usize::from(n), 2),
        };
        if buf.len() < start + len {
            return None;
        }
        let payload = buf[start..start + len].to_vec();
        self.buf.drain(..start + len);
        Some(match opcode {
            1 => Frame::Text(String::from_utf8(payload).expect("UTF-8 text")),
            2 => Frame::Binary(payload),
            8 => {
                let code =
                    (payload.len() >= 2).then(|| u16::from_be_bytes([payload[0], payload[1]]));
                let reason =
                    String::from_utf8_lossy(payload.get(2..).unwrap_or_default()).into_owned();
                Frame::Close(code, reason)
            }
            9 => Frame::Ping,
            10 => Frame::Pong,
            other => panic!("unexpected opcode {other}"),
        })
    }
}
