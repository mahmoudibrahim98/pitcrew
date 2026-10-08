//! `POST /v1/machines/{id}/scan` in `pitcrewd serve`, end to end: the real binary over temporary
//! agent homes filled from `crates/fixtures` (Claude, Codex and OpenCode), never the machine's own.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, home_of, id};
use serde_json::{Value, json};
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long a test waits for a frame. Generous: CI machines are busy.
const WAIT: Duration = Duration::from_secs(30);

/// 2026-09-29T08:00:00Z: the Codex fixture's start, the earliest of the three.
const CODEX_START: i64 = 1_790_668_800_000;

fn state_dir() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    (tmp, state)
}

fn now_ms() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(since.as_millis()).unwrap()
}

/// `path` as the JSON string a CLI would write for it (backslashes escaped on Windows).
fn json_string(path: &Path) -> String {
    serde_json::to_string(path.to_str().unwrap()).unwrap()
}

/// A fixture transcript with its folder (`"/home/sam/…"`, quoted as in the file) moved to `to`.
fn fixture(name: &str, from: &str, to: &Path) -> String {
    let path = pitcrew_fixtures::data_dir().join("transcripts").join(name);
    let text = std::fs::read_to_string(path).unwrap();
    let quoted = format!("\"{from}\"");
    assert!(text.contains(&quoted), "{name} names {from}");
    text.replace(&quoted, &json_string(to))
}

/// Where the fixtures' sessions ran, under `root`: two git repositories and a folder without one.
struct Work {
    paper: PathBuf,
    runs: PathBuf,
    tools: PathBuf,
}

impl Work {
    fn under(root: &Path) -> Self {
        let work = root.join("work");
        let paper_repo = work.join("diffusion-paper");
        let tools = work.join("lab-tools");
        let runs = work.join("diffusion-runs");
        std::fs::create_dir_all(paper_repo.join(".git")).unwrap();
        std::fs::create_dir_all(tools.join(".git")).unwrap();
        std::fs::create_dir_all(&runs).unwrap();
        let paper = paper_repo.join("paper");
        std::fs::create_dir_all(&paper).unwrap();
        Self { paper, runs, tools }
    }
}

/// Lays the three fixture sessions out in `home` as each CLI would: the Claude session in the
/// paper's `paper` folder, the Codex rollout in the runs folder, the OpenCode session in the lab's
/// tools.
fn agent_homes(home: &Path, work: &Work) {
    let claude = home
        .join(".claude")
        .join("projects")
        .join("-work-diffusion-paper-paper");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(
        claude.join("2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b.jsonl"),
        fixture(
            "claude/demo-session.jsonl",
            "/home/sam/work/diffusion-paper/paper",
            &work.paper,
        ),
    )
    .unwrap();

    let codex = home.join(".codex").join("sessions/2026/09/29");
    std::fs::create_dir_all(&codex).unwrap();
    std::fs::write(
        codex.join("rollout-2026-09-29T08-00-00-7c1e9d2a-0b3f-4e6a-8d5c-1f2e3a4b5c6d.jsonl"),
        fixture(
            "codex/rollout-demo.jsonl",
            "/scratch/sam/diffusion-runs",
            &work.runs,
        ),
    )
    .unwrap();

    let opencode = home.join(".local").join("share").join("opencode");
    std::fs::create_dir_all(&opencode).unwrap();
    let sql = pitcrew_fixtures::data_dir()
        .join("transcripts")
        .join("opencode");
    let db = pitcrew_store::sql::Connection::open(opencode.join("opencode.db")).unwrap();
    for file in ["schema.sql", "seed.sql"] {
        db.execute_batch(&std::fs::read_to_string(sql.join(file)).unwrap())
            .unwrap();
    }
    // Its folder moves with the others; it was active just now, as the other two files were.
    db.execute(
        "UPDATE session SET directory = ?1, time_updated = ?2",
        pitcrew_store::sql::params![work.tools.to_str().unwrap(), now_ms()],
    )
    .unwrap();
}

// ─── A streamed answer, read as it comes ──────────────────────────────────────────────────────────

/// `POST /v1/machines/{machine}/scan`, its head read, its body read frame by frame.
struct Scan {
    stream: TcpStream,
    status: u16,
    headers: Vec<(String, String)>,
    /// Bytes read but not yet decoded from chunks.
    raw: Vec<u8>,
    /// Decoded body not yet split into lines.
    body: Vec<u8>,
    ended: bool,
}

impl Scan {
    fn start(port: u16, machine: &str, token: Option<&str>) -> Self {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let mut head = format!(
            "POST /v1/machines/{machine}/scan HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
             Connection: close\r\nContent-Length: 0\r\n"
        );
        if let Some(token) = token {
            head.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        let mut raw = Vec::new();
        let split = loop {
            if let Some(at) = find(&raw, b"\r\n\r\n") {
                break at;
            }
            let mut chunk = [0u8; 4096];
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0, "the connection closed before the head");
            raw.extend_from_slice(&chunk[..n]);
        };
        let text = String::from_utf8_lossy(&raw[..split]).into_owned();
        let mut lines = text.split("\r\n");
        let status = lines
            .next()
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
            .collect();
        let raw = raw[split + 4..].to_vec();
        Self {
            stream,
            status,
            headers,
            raw,
            body: Vec::new(),
            ended: false,
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// A refusal's `ApiError` body (its length is in `Content-Length`).
    fn error(mut self) -> Value {
        assert_ne!(self.status, 200);
        let _ = self.stream.read_to_end(&mut self.raw);
        serde_json::from_slice(&self.raw).unwrap()
    }

    /// The next frame, or `None` once the answer has ended.
    fn next(&mut self) -> Option<Value> {
        assert_eq!(self.status, 200, "not a stream");
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(at) = self.body.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.body.drain(..=at).collect();
                return Some(serde_json::from_slice(&line).unwrap());
            }
            if self.ended {
                assert!(self.body.is_empty(), "a frame without its newline");
                return None;
            }
            if self.dechunk() {
                continue;
            }
            assert!(Instant::now() < deadline, "no frame within {WAIT:?}");
            let mut chunk = [0u8; 8192];
            let n = self.stream.read(&mut chunk).unwrap();
            assert!(n > 0 || self.ended, "the connection closed mid-answer");
            self.raw.extend_from_slice(&chunk[..n]);
        }
    }

    /// Moves every whole chunk from `raw` to `body`; true if it moved anything.
    fn dechunk(&mut self) -> bool {
        let mut moved = false;
        while let Some(line_end) = find(&self.raw, b"\r\n") {
            let size = usize::from_str_radix(
                String::from_utf8_lossy(&self.raw[..line_end])
                    .split(';')
                    .next()
                    .unwrap()
                    .trim(),
                16,
            )
            .unwrap();
            if size == 0 {
                self.ended = true;
                self.raw.clear();
                return true;
            }
            if self.raw.len() < line_end + 2 + size + 2 {
                break;
            }
            let data = self.raw[line_end + 2..line_end + 2 + size].to_vec();
            self.body.extend_from_slice(&data);
            self.raw.drain(..line_end + 2 + size + 2);
            moved = true;
        }
        moved
    }

    /// Every frame left.
    fn rest(&mut self) -> Vec<Value> {
        std::iter::from_fn(|| self.next()).collect()
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn refused(scan: Scan, status: u16, code: &str) -> String {
    assert_eq!(scan.status, status);
    let error = scan.error();
    assert_eq!(error["code"], code, "{error}");
    error["message"].as_str().unwrap().to_owned()
}

/// The report of a whole scan, after checking its frames: progress first (`scanned: 0`), ticks
/// that only grow to `scanned == total`, and `done` last.
fn report_of(mut scan: Scan) -> Value {
    assert_eq!(scan.status, 200);
    assert_eq!(scan.header("content-type"), Some("application/x-ndjson"));
    let frames = scan.rest();
    assert_eq!(
        frames.first(),
        Some(&json!({ "type": "progress", "scanned": 0 }))
    );
    let (last, ticks) = frames.split_last().unwrap();
    assert_eq!(last["type"], "done", "{frames:?}");
    assert!(ticks.iter().all(|f| f["type"] == "progress"), "{frames:?}");
    let scanned: Vec<u64> = ticks
        .iter()
        .map(|f| f["scanned"].as_u64().unwrap())
        .collect();
    assert!(scanned.windows(2).all(|w| w[0] <= w[1]), "{scanned:?}");
    let final_tick = ticks.last().unwrap();
    assert_eq!(final_tick["scanned"], final_tick["total"], "{frames:?}");
    last["report"].clone()
}

fn path(p: &Path) -> Value {
    Value::String(p.to_str().unwrap().to_owned())
}

/// The three fixture sessions are counted by engine, home, folder and month, and suggest the two
/// repositories and the folder without one, with the paper's `paper` folder as a workstream.
#[test]
fn a_scan_streams_progress_and_reports_the_fixtures() {
    let (tmp, state) = state_dir();
    let homes = tmp.path().join("homes");
    let work = Work::under(tmp.path());
    agent_homes(&homes, &work);
    let daemon = Daemon::start(&state, &["--demo", "--homes", homes.to_str().unwrap()]);
    let device = daemon.device_token();

    let report = report_of(Scan::start(daemon.port, id::LAPTOP, Some(&device)));
    let counts = &report["counts"];
    assert_eq!(counts["sessions"], 3, "{report:#}");
    assert_eq!(counts["subagent_sessions"], 0);
    assert_eq!(
        counts["by_engine"],
        json!([
            { "engine": "claude", "count": 1 },
            { "engine": "codex", "count": 1 },
            { "engine": "opencode", "count": 1 },
        ])
    );
    assert_eq!(
        counts["by_home"],
        json!([
            { "engine": "claude", "home": path(&homes.join(".claude")), "count": 1 },
            { "engine": "codex", "home": path(&homes.join(".codex")), "count": 1 },
            {
                "engine": "opencode",
                "home": path(&homes.join(".local").join("share").join("opencode")),
                "count": 1,
            },
        ])
    );
    let mut folders: Vec<&Value> = counts["by_folder"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| &f["path"])
        .collect();
    folders.sort_by_key(|p| p.as_str().unwrap().to_owned());
    assert_eq!(
        folders,
        [&path(&work.paper), &path(&work.runs), &path(&work.tools)]
    );
    assert_eq!(
        counts["by_month"],
        json!([{ "month": "2026-09", "count": 3 }])
    );
    assert_eq!(counts["first_activity"], CODEX_START);
    assert!(counts["last_activity"].as_i64().unwrap() >= CODEX_START);
    assert_eq!(report["unreadable"], 0);

    let repo = work.paper.parent().unwrap();
    assert_eq!(
        report["suggestions"],
        json!([
            {
                "id": path(repo),
                "name": "diffusion-paper",
                "path": path(repo),
                "is_git": true,
                "session_count": 1,
                "recent_30d": 1,
                "recent_90d": 1,
                "workstreams": [{
                    "id": path(&work.paper),
                    "name": "paper",
                    "session_count": 1,
                    "recent_30d": 1,
                    "recent_90d": 1,
                }],
            },
            {
                "id": path(&work.runs),
                "name": "diffusion-runs",
                "path": path(&work.runs),
                "is_git": false,
                "session_count": 1,
                "recent_30d": 1,
                "recent_90d": 1,
                "workstreams": [],
            },
            {
                "id": path(&work.tools),
                "name": "lab-tools",
                "path": path(&work.tools),
                "is_git": true,
                "session_count": 1,
                "recent_30d": 1,
                "recent_90d": 1,
                "workstreams": [],
            },
        ]),
        "{report:#}"
    );
    // The scan logs its counts (and no path).
    let deadline = Instant::now() + WAIT;
    let line = loop {
        let logs = daemon.stderr();
        if let Some(line) = logs
            .lines()
            .find(|l| l.contains("scanned this machine's agent homes"))
        {
            break line.to_owned();
        }
        assert!(Instant::now() < deadline, "no scan in the log:\n{logs}");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(line.contains("sessions=3"), "{line}");
    assert!(!line.contains(work.paper.to_str().unwrap()), "{line}");
}

/// One scan at a time: a second one while the first is held is 409, and so is one after the
/// first's client went away, until its walk has ended; then the machine may be scanned again.
#[test]
fn a_second_scan_meanwhile_is_a_conflict() {
    let (tmp, state) = state_dir();
    let homes = tmp.path().join("homes");
    agent_homes(&homes, &Work::under(tmp.path()));
    let daemon = Daemon::start(
        &state,
        &[
            "--demo",
            "--homes",
            homes.to_str().unwrap(),
            "--scan-hold-ms",
            "2000",
        ],
    );
    let device = daemon.device_token();

    let mut first = Scan::start(daemon.port, id::LAPTOP, Some(&device));
    assert_eq!(first.status, 200);
    assert_eq!(
        first.next().unwrap(),
        json!({ "type": "progress", "scanned": 0 })
    );
    let message = refused(
        Scan::start(daemon.port, id::LAPTOP, Some(&device)),
        409,
        "conflict",
    );
    assert!(message.contains("already running"), "{message}");
    // The first is unharmed, and ends with its report.
    let report = report_of_rest(&mut first);
    assert_eq!(report["counts"]["sessions"], 3);

    // A client that goes away does not give the machine back before the walk ends.
    let mut gone = Scan::start(daemon.port, id::LAPTOP, Some(&device));
    assert_eq!(gone.status, 200);
    gone.next().unwrap();
    drop(gone);
    refused(
        Scan::start(daemon.port, id::LAPTOP, Some(&device)),
        409,
        "conflict",
    );
    let deadline = Instant::now() + WAIT;
    let again = loop {
        let scan = Scan::start(daemon.port, id::LAPTOP, Some(&device));
        if scan.status == 200 {
            break scan;
        }
        refused(scan, 409, "conflict");
        assert!(
            Instant::now() < deadline,
            "the machine was never given back"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(report_of(again)["counts"]["sessions"], 3);
}

/// The frames after the first, which the caller has read: ticks, then `done`.
fn report_of_rest(scan: &mut Scan) -> Value {
    let frames = scan.rest();
    let (last, ticks) = frames.split_last().unwrap();
    assert!(ticks.iter().all(|f| f["type"] == "progress"), "{frames:?}");
    assert_eq!(last["type"], "done", "{frames:?}");
    last["report"].clone()
}

/// A person's route, for the hub's own machine: no token 401, an agent 403, an unknown or
/// malformed machine 404, another machine of the workspace 409.
#[test]
fn only_a_person_scans_and_only_this_machine() {
    let (tmp, state) = state_dir();
    let homes = tmp.path().join("empty-homes");
    let daemon = Daemon::start(&state, &["--demo", "--homes", homes.to_str().unwrap()]);
    let device = daemon.device_token();
    let agent = daemon.agent_token();

    refused(
        Scan::start(daemon.port, id::LAPTOP, None),
        401,
        "unauthorized",
    );
    refused(
        Scan::start(daemon.port, id::LAPTOP, Some(&agent)),
        403,
        "forbidden",
    );
    refused(
        Scan::start(daemon.port, "01JB000000000000000MCH0099", Some(&device)),
        404,
        "not_found",
    );
    refused(
        Scan::start(daemon.port, "not-a-machine", Some(&device)),
        404,
        "not_found",
    );
    let message = refused(
        Scan::start(daemon.port, id::CLUSTER, Some(&device)),
        409,
        "conflict",
    );
    assert!(message.contains("not supported yet"), "{message}");

    // Homes that hold nothing: an empty report, every list present.
    let report = report_of(Scan::start(daemon.port, id::LAPTOP, Some(&device)));
    assert_eq!(
        report,
        json!({
            "counts": {
                "sessions": 0,
                "subagent_sessions": 0,
                "by_engine": [],
                "by_home": [],
                "by_folder": [],
                "by_month": [],
            },
            "suggestions": [],
            "unreadable": 0,
        })
    );
}

/// Without its runner the hub reads no agent home, so it scans none either.
#[test]
fn a_hub_without_its_runner_scans_nothing() {
    let (_tmp, state) = state_dir();
    let daemon = Daemon::start(&state, &["--demo", "--no-runner"]);
    let device = daemon.device_token();
    let message = refused(
        Scan::start(daemon.port, id::LAPTOP, Some(&device)),
        409,
        "conflict",
    );
    assert!(message.contains("--no-runner"), "{message}");
}

/// A fresh hub has no machine to scan; once set up, it scans its new machine, in "this user's"
/// homes (here the test's own home folder, as every test daemon's is).
#[test]
fn a_fresh_hub_scans_its_machine_once_set_up() {
    let (tmp, state) = state_dir();
    let daemon_home = home_of(&state);
    agent_homes(&daemon_home, &Work::under(tmp.path()));
    // This test checks scan provisioning alone; the office starts asynchronously after setup
    // and can otherwise add its own member between the two idempotency snapshots.
    let daemon = Daemon::start(&state, &["--no-office"]);
    let device = daemon.device_token();

    refused(
        Scan::start(daemon.port, id::LAPTOP, Some(&device)),
        404,
        "not_found",
    );
    let setup = daemon.post(
        "/v1/setup",
        Some(&device),
        &json!({
            "workspace_name": "Demo Lab",
            "person": { "name": "Sam Rivera", "handle": "@sam" },
            "machine_name": "This laptop",
        }),
    );
    assert_eq!(setup.status, 200, "{}", setup.body);
    let machine = setup.json()["machine"]["id"].as_str().unwrap().to_owned();
    let report = report_of(Scan::start(daemon.port, &machine, Some(&device)));
    assert_eq!(report["counts"]["sessions"], 3, "{report:#}");
    assert_eq!(report["suggestions"].as_array().unwrap().len(), 3);
    let owner = setup.json()["me"]["id"].as_str().unwrap().to_owned();
    let members = daemon.get("/v1/members", Some(&device)).json();
    let personas = daemon.get("/v1/personas", Some(&device)).json();
    for engine in ["claude", "codex", "opencode"] {
        let matching: Vec<_> = members
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| {
                m["kind"] == "agent"
                    && m["owner"] == owner
                    && personas
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|p| p["id"] == m["persona"] && p["engine"] == engine)
            })
            .collect();
        assert_eq!(matching.len(), 1, "{engine}: {members:#}");
        assert_ne!(matching[0]["handle"], "@office");
    }
    report_of(Scan::start(daemon.port, &machine, Some(&device)));
    assert_eq!(daemon.get("/v1/members", Some(&device)).json(), members);
    assert_eq!(daemon.get("/v1/personas", Some(&device)).json(), personas);
}
