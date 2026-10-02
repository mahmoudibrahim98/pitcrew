//! A transcript swapped after discovery for a link (to a file or a folder), a folder or a named
//! pipe is not read, by any adapter; a transcript under a linked home still is.

use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_ingest::codex::CodexAdapter;
use pitcrew_ingest::opencode::OpenCodeAdapter;
use pitcrew_ingest::{FileKind, refusal};
use pitcrew_interfaces::source::{
    Cursor, SourceAdapter, SourceError, TranscriptItem, TranscriptRef,
};
use rusqlite::{Connection, params_from_iter};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
enum Engine {
    Claude,
    Codex,
    OpenCode,
}

const ENGINES: [Engine; 3] = [Engine::Claude, Engine::Codex, Engine::OpenCode];

impl Engine {
    fn adapter(self) -> Box<dyn SourceAdapter> {
        match self {
            Self::Claude => Box::new(ClaudeAdapter::new()),
            Self::Codex => Box::new(CodexAdapter::new()),
            Self::OpenCode => Box::new(OpenCodeAdapter::new()),
        }
    }

    /// Where this engine's one transcript lives in a home.
    fn transcript(self, home: &Path) -> PathBuf {
        match self {
            Self::Claude => home.join("projects/-w-demo/sess-1.jsonl"),
            Self::Codex => home.join(
                "sessions/2026/09/30/rollout-2026-09-30T08-00-00-7c1e9d2a-0b3f-4e6a-8d5c-1f2e3a4b5c6d.jsonl",
            ),
            Self::OpenCode => home.join("opencode.db"),
        }
    }

    /// Writes a whole transcript of this engine at `path`, with a prompt saying `text`. Everything
    /// it needs is compiled in, so the test binary runs anywhere (a Windows build included).
    fn write(self, path: &Path, text: &str) {
        fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        let at = "2026-09-30T08:00:00Z";
        let line = match self {
            Self::Claude => json!({"type": "user", "sessionId": "sess-1", "cwd": "/w",
                                   "timestamp": at,
                                   "message": {"role": "user", "content": text}}),
            Self::Codex => json!({"timestamp": at, "type": "response_item",
                                  "payload": {"type": "message", "role": "user",
                                              "content": [{"type": "input_text", "text": text}]}}),
            Self::OpenCode => return opencode_store(path),
        };
        fs::write(path, format!("{line}\n{line}\n")).expect("write");
    }

    /// A home with one transcript in it.
    fn home(self, home: &Path) {
        self.write(&self.transcript(home), "hello");
    }
}

/// An OpenCode store with the adapter tests' synthetic history (`tests/data/opencode/`).
fn opencode_store(path: &Path) {
    let schema = include_str!("data/opencode/schema.sql");
    let history = include_str!("data/opencode/history.jsonl");
    let conn = Connection::open(path).expect("open");
    conn.execute_batch("PRAGMA synchronous = OFF")
        .expect("pragma");
    conn.execute_batch(schema).expect("schema");
    conn.execute_batch("BEGIN").expect("begin");
    for line in history.lines() {
        let op: Value = serde_json::from_str(line).expect("json");
        let table = op["table"].as_str().expect("table");
        let row = op["row"].as_object().expect("row");
        let cols: Vec<&str> = row.keys().map(String::as_str).collect();
        let marks = vec!["?"; cols.len()].join(", ");
        let set: Vec<String> = cols.iter().map(|c| format!("{c} = excluded.{c}")).collect();
        let sql = format!(
            "INSERT INTO {table} ({}) VALUES ({marks}) ON CONFLICT(id) DO UPDATE SET {}",
            cols.join(", "),
            set.join(", ")
        );
        conn.execute(&sql, params_from_iter(row.values().map(sql_value)))
            .expect("write");
    }
    conn.execute_batch("COMMIT").expect("commit");
}

fn sql_value(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match v {
        Value::Null => V::Null,
        Value::Bool(b) => V::Integer(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .map_or_else(|| V::Real(n.as_f64().unwrap_or(0.0)), V::Integer),
        Value::String(s) => V::Text(s.clone()),
        other => V::Text(other.to_string()),
    }
}

/// Retries once on an I/O error that is not a refusal (WSL's reads of `/mnt/c` can glitch under
/// load).
fn retry<T>(mut attempt: impl FnMut() -> Result<T, SourceError>) -> Result<T, SourceError> {
    match attempt() {
        Err(e @ SourceError::Io(_)) if refusal(&e).is_none() => attempt(),
        other => other,
    }
}

/// Discovers `home` and checks that its transcripts read, with items.
fn discover_and_read(engine: Engine, home: &Path) -> Vec<TranscriptRef> {
    let adapter = engine.adapter();
    let refs = retry(|| adapter.discover(home)).expect("discover");
    assert!(!refs.is_empty(), "{engine:?}: nothing discovered");
    let (mut read, mut paged) = (0, 0);
    for t in &refs {
        read += retry(|| adapter.read_from(t, &Cursor::default()))
            .expect("read")
            .items
            .len();
        paged += retry(|| adapter.read_page(t, None, 50))
            .expect("page")
            .items
            .len();
    }
    assert!(
        read > 0 && paged > 0,
        "{engine:?}: {read} items read, {paged} paged"
    );
    refs
}

/// Both reads of every reference fail, refused as `kind`, and nothing of the file is read.
fn assert_refused(engine: Engine, refs: &[TranscriptRef], kind: FileKind) {
    let adapter = engine.adapter();
    for t in refs {
        let errors = [
            adapter.read_from(t, &Cursor::default()).map(|c| c.items),
            adapter.read_page(t, None, 50).map(|p| p.items),
        ];
        for result in errors {
            let err = expect_err(engine, result);
            let found = refusal(&err).unwrap_or_else(|| panic!("{engine:?}: not a refusal: {err}"));
            assert_eq!(found.kind, kind, "{engine:?}: {err}");
            assert_eq!(found.path, t.path, "{engine:?}");
        }
    }
}

fn expect_err(engine: Engine, result: Result<Vec<TranscriptItem>, SourceError>) -> SourceError {
    match result {
        Ok(items) => panic!(
            "{engine:?}: read {} items through a swapped file",
            items.len()
        ),
        Err(e) => e,
    }
}

/// Replaces the transcript at `path` (and an OpenCode store's side files) with nothing.
fn remove(path: &Path) {
    fs::remove_file(path).expect("remove");
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut side = path.as_os_str().to_owned();
        side.push(suffix);
        let _ = fs::remove_file(PathBuf::from(side));
    }
}

#[test]
fn a_transcript_swapped_for_a_link_to_another_file_is_not_read() {
    for engine in ENGINES {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        engine.home(&home);
        let refs = discover_and_read(engine, &home);
        // What the link points at is a whole, readable transcript: were it followed, items would
        // come back.
        let secret = dir
            .path()
            .join("elsewhere")
            .join(format!("secret-{engine:?}"));
        engine.write(&secret, "SECRET");
        let path = engine.transcript(&home);
        remove(&path);
        if !platform::link_file(&secret, &path) {
            return;
        }
        assert_refused(engine, &refs, FileKind::Link);
        // Discovery does not list the link either.
        let found = engine.adapter().discover(&home).expect("discover");
        assert!(found.is_empty(), "{engine:?}: discovered {found:?}");
    }
}

#[test]
fn a_transcript_swapped_for_a_link_to_a_folder_is_not_read() {
    for engine in ENGINES {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        engine.home(&home);
        let refs = discover_and_read(engine, &home);
        let folder = dir.path().join("folder");
        fs::create_dir(&folder).expect("mkdir");
        let path = engine.transcript(&home);
        remove(&path);
        platform::link_dir(&folder, &path);
        assert_refused(engine, &refs, FileKind::Link);
    }
}

#[test]
fn a_transcript_swapped_for_a_folder_is_not_read() {
    for engine in ENGINES {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        engine.home(&home);
        let refs = discover_and_read(engine, &home);
        let path = engine.transcript(&home);
        remove(&path);
        fs::create_dir(&path).expect("mkdir");
        assert_refused(engine, &refs, FileKind::Directory);
    }
}

#[cfg(unix)]
#[test]
fn a_transcript_swapped_for_a_named_pipe_is_not_read_and_nothing_hangs() {
    use std::sync::mpsc;
    use std::time::Duration;
    for engine in ENGINES {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        engine.home(&home);
        let refs = discover_and_read(engine, &home);
        let path = engine.transcript(&home);
        remove(&path);
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo");
        assert!(made.success(), "mkfifo failed");
        let (tx, rx) = mpsc::channel();
        let refs_for_reads = refs.clone();
        std::thread::spawn(move || {
            assert_refused(engine, &refs_for_reads, FileKind::Fifo);
            let _ = tx.send(());
        });
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{engine:?}: the reads failed"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Unblock a reader stuck on the pipe before failing, so nothing is left behind.
                let _ = fs::OpenOptions::new().write(true).open(&path);
                panic!("{engine:?}: a read blocked on a named pipe");
            }
        }
    }
}

#[test]
fn a_transcript_under_a_linked_home_is_still_read() {
    for engine in ENGINES {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real-home");
        engine.home(&real);
        let home = dir.path().join("home");
        platform::link_dir(&real, &home);
        let refs = discover_and_read(engine, &home);
        assert!(
            refs.iter().all(|t| t.path.starts_with(&home)),
            "{engine:?}: {refs:?}"
        );
    }
}

#[cfg(unix)]
mod platform {
    use std::path::Path;

    pub(crate) fn link_file(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).expect("link");
        true
    }

    pub(crate) fn link_dir(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).expect("link");
    }
}

#[cfg(windows)]
mod platform {
    use std::path::Path;

    /// A file symlink needs Developer Mode or an elevated process: `false`, with a message, where
    /// this account may not create one.
    pub(crate) fn link_file(target: &Path, link: &Path) -> bool {
        match std::os::windows::fs::symlink_file(target, link) {
            Ok(()) => true,
            // ERROR_PRIVILEGE_NOT_HELD
            Err(e) if e.raw_os_error() == Some(1314) => {
                eprintln!("skipped: this account may not create a file symlink ({e})");
                false
            }
            Err(e) => panic!("cannot create a file symlink: {e}"),
        }
    }

    /// A junction: a link to a folder any account may create (std cannot make one).
    pub(crate) fn link_dir(target: &Path, link: &Path) {
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("mklink");
        assert!(status.success(), "mklink /J failed");
    }
}
