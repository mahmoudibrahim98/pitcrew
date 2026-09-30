//! A fake daemon: a thread answering canned responses over loopback TCP or a unix socket, and
//! recording every request. Synthetic data only.

#![allow(clippy::unwrap_used, dead_code)]

use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

pub const TOKEN: &str = "pca_test-token";
pub const SAM: &str = "01JB0000000000000000000001";
pub const WRITER: &str = "01JB0000000000000000000002";
pub const TASK1: &str = "01JB0000000000000000000011";
pub const TASK2: &str = "01JB0000000000000000000012";
pub const ASK1: &str = "01JB0000000000000000000021";
pub const ASK2: &str = "01JB0000000000000000000022";
pub const ASK3: &str = "01JB0000000000000000000023";
pub const ASK4: &str = "01JB0000000000000000000024";
pub const SUB1: &str = "01JB0000000000000000000031";
pub const SUB2: &str = "01JB0000000000000000000032";
pub const WORKSPACE: &str = "01JB0000000000000000000041";
pub const PROJECT: &str = "01JB0000000000000000000051";
pub const EVENT1: &str = "01JB0000000000000000000061";

/// One request as the server saw it.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    /// `METHOD /path` without the query.
    pub fn route(&self) -> String {
        let path = self.target.split('?').next().unwrap();
        format!("{} {path}", self.method)
    }
}

/// `(status, body)` for a request.
pub type Handler = Arc<dyn Fn(&Recorded) -> (u16, String) + Send + Sync>;

/// Answers `routes` (`"METHOD /path"` → status and JSON), `GET /v1/host/info` by default, and
/// 404 for anything else.
pub fn routes(routes: &[(&str, u16, Value)]) -> Handler {
    let mut table: HashMap<String, (u16, String)> = routes
        .iter()
        .map(|(route, status, body)| ((*route).to_owned(), (*status, body.to_string())))
        .collect();
    table
        .entry("GET /v1/host/info".into())
        .or_insert_with(|| (200, host_info(1, 1).to_string()));
    Arc::new(move |req| {
        table.get(&req.route()).cloned().unwrap_or_else(|| {
            (
                404,
                json!({"code": "not_found", "message": format!("No route for {}.", req.route())})
                    .to_string(),
            )
        })
    })
}

pub fn host_info(protocol_min: u32, protocol: u32) -> Value {
    json!({
        "name": "pitcrewd",
        "version": "0.0.0-test",
        "protocol": protocol,
        "protocol_min": protocol_min,
        "roles": ["hub", "runner"],
        "machine": {"hostname": "test-host", "os": "linux", "arch": "x86_64", "has_tmux": false},
        "capabilities": []
    })
}

pub fn members() -> Value {
    json!([
        {"id": SAM, "kind": "human", "handle": "@sam", "name": "Sam Example"},
        {"id": WRITER, "kind": "agent", "handle": "@writer", "name": "Writer", "owner": SAM}
    ])
}

pub fn writer() -> Value {
    members()[1].clone()
}

pub fn task(id: &str, key: &str, status: &str) -> Value {
    json!({
        "id": id,
        "key": key,
        "project": PROJECT,
        "title": format!("Synthetic task {key}"),
        "description": "Write the synthetic section.\nKeep it short.",
        "status": status,
        "priority": "high",
        "assignee": WRITER,
        "labels": ["writing"],
        "blocked_by": [],
        "accept_auto": false,
        "subtasks": [
            {"id": SUB1, "text": "Outline", "done": true,
             "source": {"kind": "agent_plan", "agent": WRITER}},
            {"id": SUB2, "text": "Ask Sam about scope", "done": false,
             "source": {"kind": "human"}}
        ]
    })
}

pub fn comment_event(text: &str) -> Value {
    json!({
        "id": EVENT1,
        "at": 1_700_000_000_000_i64,
        "workspace": WORKSPACE,
        "author": WRITER,
        "on_behalf_of": SAM,
        "body": {"type": "comment_posted", "data": {"task": TASK1, "text": text, "mentions": []}}
    })
}

pub fn api_error(code: &str, message: &str) -> Value {
    json!({"code": code, "message": message})
}

/// A running fake daemon.
pub struct FakeServer {
    /// `http://127.0.0.1:<port>` for TCP servers.
    pub url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    /// Keeps a unix socket's directory alive.
    _dir: Option<tempfile::TempDir>,
}

impl FakeServer {
    /// Over loopback TCP.
    pub fn tcp(handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (handler, log) = (Arc::clone(&handler), Arc::clone(&log));
                std::thread::spawn(move || serve(stream, &handler, &log));
            }
        });
        Self {
            url,
            requests,
            _dir: None,
        }
    }

    /// Over a unix socket named `pitcrewd.sock` in a new directory with `mode`.
    #[cfg(unix)]
    pub fn unix(handler: Handler, mode: u32) -> (Self, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("run");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        let socket = dir.join("pitcrewd.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (handler, log) = (Arc::clone(&handler), Arc::clone(&log));
                std::thread::spawn(move || serve(stream, &handler, &log));
            }
        });
        let server = Self {
            url: String::new(),
            requests,
            _dir: Some(tmp),
        };
        (server, socket)
    }

    /// Every complete request so far.
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    /// Requests other than the version check.
    pub fn api_requests(&self) -> Vec<Recorded> {
        self.requests()
            .into_iter()
            .filter(|r| r.target != "/v1/host/info")
            .collect()
    }
}

fn serve<S: Read + Write>(mut stream: S, handler: &Handler, log: &Mutex<Vec<Recorded>>) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    log.lock().unwrap().push(request.clone());
    let (status, body) = handler(&request);
    if status == 0 {
        // Hang: never answer.
        std::thread::sleep(std::time::Duration::from_secs(30));
        return;
    }
    let _ = stream.write_all(&reply_bytes(status, &body));
}

fn read_request<S: Read>(stream: &mut S) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(request) = parse_request(&buf) {
            return Some(request);
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// A whole request (head and `Content-Length` body), or `None` if more bytes are needed.
pub fn parse_request(buf: &[u8]) -> Option<Recorded> {
    let head_end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8(buf[..head_end].to_vec()).ok()?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let target = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let body = buf.get(head_end + 4..head_end + 4 + len)?.to_vec();
    Some(Recorded {
        method,
        target,
        headers,
        body,
    })
}

/// The bytes of a response.
pub fn reply_bytes(status: u16, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// A URL where nothing listens.
pub fn dead_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    url
}

/// Runs the library's `run` with only the given environment. Returns (exit code, stdout, stderr).
pub fn run(args: &[&str], env: &[(&str, &str)], stdin: &str) -> (i32, String, String) {
    let env: HashMap<String, OsString> = env
        .iter()
        .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
        .collect();
    let lookup = move |name: &str| env.get(name).cloned();
    let mut stdin = stdin.as_bytes();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut io = pitcrew_cli::Io {
        stdin: &mut stdin,
        stdin_is_terminal: false,
        stdout: &mut stdout,
        stderr: &mut stderr,
    };
    let args: Vec<OsString> = std::iter::once("pitcrew")
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect();
    let code = pitcrew_cli::run(args, &lookup, &mut io);
    (
        code,
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

/// `run` against a TCP server with the test token.
pub fn run_on(server: &FakeServer, args: &[&str], stdin: &str) -> (i32, String, String) {
    run(
        args,
        &[("PITCREW_URL", &server.url), ("PITCREW_TOKEN", TOKEN)],
        stdin,
    )
}
