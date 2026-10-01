//! The machine scan: synthetic homes across all three engines, progress, warnings and timing.

use pitcrew_ingest::scan::{self, ScanHome, ScanOptions, ScanProgress};
use pitcrew_protocol::model::Engine;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// 2026-01-01T00:00:00Z, so test fixtures do not depend on the real clock.
const BASE_MS: i64 = 1_767_225_600_000;
const DAY_MS: i64 = 86_400_000;

fn claude_line(session_id: &str, cwd: &str, branch: &str, ts: &str, sidechain: bool) -> String {
    let mut v = json!({
        "type": "user", "sessionId": session_id, "cwd": cwd, "gitBranch": branch,
        "timestamp": ts, "message": {"role": "user", "content": "hello"}
    });
    if sidechain {
        v["isSidechain"] = Value::Bool(true);
        v["agentId"] = Value::String(format!("{session_id}-agent"));
    }
    v.to_string()
}

/// Writes `line` (plus a newline) to `path` and sets its modification time to `mtime_ms`, so the
/// scan's "last activity" (a transcript's real mtime) is deterministic rather than "whenever the
/// test happened to run".
fn write_line(path: &Path, line: &str, mtime_ms: i64) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, format!("{line}\n")).expect("write");
    let time = SystemTime::UNIX_EPOCH + Duration::from_millis(u64::try_from(mtime_ms).unwrap_or(0));
    fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(time))
        .expect("set mtime");
}

/// A Claude home with: two sessions directly in `proj-a` (one `main`, one `feat/x`), one in
/// `proj-a/apps/web`, one sub-agent session nested under the first, and an unrelated session in
/// `proj-a`'s sibling `notes/x` (no `.git` anywhere near it, so it later groups with OpenCode's
/// `notes/y`). `proj-a`'s sessions are recent; `notes/x`'s is made old on purpose.
fn claude_home(root: &Path) -> PathBuf {
    let home = root.join("claude-home");
    let proj = home.join("projects").join("enc-a");
    let proj_a = format!("{}", root.join("work/proj-a").display());
    let recent = BASE_MS + 5 * DAY_MS;
    write_line(
        &proj.join("s1.jsonl"),
        &claude_line("s1", &proj_a, "main", "2026-01-01T00:00:00Z", false),
        recent,
    );
    write_line(
        &proj.join("s1").join("subagents").join("agent-1.jsonl"),
        &claude_line("s1", &proj_a, "main", "2026-01-01T00:05:00Z", true),
        recent,
    );
    write_line(
        &proj.join("s2.jsonl"),
        &claude_line("s2", &proj_a, "feat/x", "2026-01-02T00:00:00Z", false),
        recent,
    );
    write_line(
        &proj.join("s3.jsonl"),
        &claude_line(
            "s3",
            &format!("{}", root.join("work/proj-a/apps/web").display()),
            "main",
            "2026-01-03T00:00:00Z",
            false,
        ),
        recent,
    );
    write_line(
        &proj.join("s4.jsonl"),
        &claude_line(
            "s4",
            &format!("{}", root.join("work/notes/x").display()),
            "",
            "2026-01-04T00:00:00Z",
            false,
        ),
        BASE_MS - 100 * DAY_MS, // old: outside the 90-day window
    );
    fs::create_dir_all(root.join("work/proj-a/.git")).expect("git dir");
    home
}

fn codex_line(id: &str, cwd: &str, branch: &str, ts: &str, subagent: bool) -> String {
    let source = if subagent {
        json!({"subagent": true})
    } else {
        json!("cli")
    };
    json!({
        "timestamp": ts, "type": "session_meta",
        "payload": {"id": id, "cwd": cwd, "git": {"branch": branch}, "source": source}
    })
    .to_string()
}

/// A Codex home with one recent session in a git-rooted `proj-b`.
fn codex_home(root: &Path) -> PathBuf {
    let home = root.join("codex-home");
    let path = home.join("sessions/2026/01/05/rollout-test-1.jsonl");
    write_line(
        &path,
        &codex_line(
            "codex-1",
            &format!("{}", root.join("work/proj-b").display()),
            "master",
            "2026-01-05T00:00:00Z",
            false,
        ),
        BASE_MS + 5 * DAY_MS,
    );
    fs::create_dir_all(root.join("work/proj-b/.git")).expect("git dir");
    home
}

/// An OpenCode home: one recent session in `work/notes/y` (no `.git`; groups with Claude's
/// `work/notes/x`), and one sub-session (`parent_id` set). OpenCode's "last activity" is the
/// `time_updated` column, not a file's mtime, so it is deterministic without any extra work.
fn opencode_home(root: &Path) -> PathBuf {
    let home = root.join("opencode-home");
    fs::create_dir_all(&home).expect("mkdir");
    let db = home.join("opencode.db");
    let conn = Connection::open(&db).expect("open");
    conn.execute_batch(
        "CREATE TABLE session (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL, parent_id TEXT,
            directory TEXT NOT NULL, title TEXT NOT NULL, version TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL
         );",
    )
    .expect("schema");
    let y = root.join("work/notes/y");
    let recent = BASE_MS + 5 * DAY_MS;
    conn.execute(
        "INSERT INTO session (id, project_id, parent_id, directory, title, version, time_created, time_updated)
         VALUES ('oc-1', 'p', NULL, ?1, 'New session - x', 'v', ?2, ?2)",
        params![y.to_string_lossy(), recent],
    )
    .expect("insert");
    conn.execute(
        "INSERT INTO session (id, project_id, parent_id, directory, title, version, time_created, time_updated)
         VALUES ('oc-2', 'p', 'oc-1', ?1, 'Child session - x', 'v', ?2, ?2)",
        params![y.to_string_lossy(), recent + 100_000],
    )
    .expect("insert");
    drop(conn);
    home
}

/// Strips `needle` out of every string in `value`, for snapshots that must not carry a tempdir's
/// non-deterministic path.
fn redact(value: &mut Value, needle: &str) {
    match value {
        Value::String(s) if s.contains(needle) => *s = s.replace(needle, "<ROOT>"),
        Value::Array(a) => a.iter_mut().for_each(|v| redact(v, needle)),
        Value::Object(o) => o.values_mut().for_each(|v| redact(v, needle)),
        _ => {}
    }
}

#[test]
fn golden_scan_across_all_three_engines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let homes = vec![
        ScanHome {
            engine: Engine::Claude,
            home: claude_home(dir.path()),
        },
        ScanHome {
            engine: Engine::Codex,
            home: codex_home(dir.path()),
        },
        ScanHome {
            engine: Engine::OpenCode,
            home: opencode_home(dir.path()),
        },
    ];
    // 10 days after the base: the proj-a/proj-b/notes-y sessions (base + 5 days) are "recent" in
    // both windows; notes/x (base - 100 days) is not.
    let options = ScanOptions {
        now: BASE_MS + 10 * DAY_MS,
        threads: Some(2),
    };

    let mut ticks: Vec<ScanProgress> = Vec::new();
    let report = scan::scan(&homes, &options, |p| ticks.push(p));

    assert_eq!(report.unreadable, 0);
    assert_eq!(
        report.counts.sessions, 6,
        "s1, s2, s3, s4 (claude), codex-1, oc-1"
    );
    assert_eq!(
        report.counts.subagent_sessions, 2,
        "claude agent-1 and opencode oc-2"
    );

    let mut v = serde_json::to_value(&report).expect("json");
    redact(&mut v, &dir.path().to_string_lossy());
    insta::assert_json_snapshot!("scan_all_engines", v);

    // Progress reached the discovered total, monotonically, with one consistent total throughout.
    assert!(!ticks.is_empty());
    assert!(ticks.windows(2).all(|w| w[0].scanned <= w[1].scanned));
    let total = ticks.last().expect("a tick").total.expect("total");
    assert_eq!(ticks.last().expect("a tick").scanned, total);
    assert!(ticks.iter().all(|t| t.total == Some(total)));
}

#[test]
fn progress_is_monotonic_and_reaches_the_total() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("claude-home");
    let proj = home.join("projects").join("p");
    for n in 0..300 {
        write_line(
            &proj.join(format!("s{n}.jsonl")),
            &claude_line(
                &format!("s{n}"),
                "/w/p",
                "main",
                "2026-01-01T00:00:00Z",
                false,
            ),
            BASE_MS,
        );
    }
    let homes = vec![ScanHome {
        engine: Engine::Claude,
        home,
    }];
    let mut ticks = Vec::new();
    let report = scan::scan(&homes, &ScanOptions::default(), |p| ticks.push(p));
    assert_eq!(report.counts.sessions, 300);
    assert!(ticks.windows(2).all(|w| w[0].scanned <= w[1].scanned));
    assert_eq!(ticks.last().expect("a tick").scanned, 300);
}

#[cfg(unix)]
#[test]
fn an_unreadable_folder_and_an_unreadable_file_are_warnings_not_failures() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");

    // A whole home whose `projects` folder cannot be listed.
    let locked_home = dir.path().join("locked-claude-home");
    let locked_projects = locked_home.join("projects");
    fs::create_dir_all(&locked_projects).expect("mkdir");
    fs::set_permissions(&locked_projects, fs::Permissions::from_mode(0o000)).expect("chmod");

    // A readable home with one session file that then becomes unreadable.
    let readable_home = dir.path().join("readable-claude-home");
    let proj = readable_home.join("projects").join("p");
    let good = proj.join("good.jsonl");
    let bad = proj.join("bad.jsonl");
    write_line(
        &good,
        &claude_line("good", "/w/p", "main", "2026-01-01T00:00:00Z", false),
        BASE_MS,
    );
    write_line(
        &bad,
        &claude_line("bad", "/w/p", "main", "2026-01-01T00:00:01Z", false),
        BASE_MS,
    );
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o000)).expect("chmod");

    let homes = vec![
        ScanHome {
            engine: Engine::Claude,
            home: locked_home.clone(),
        },
        ScanHome {
            engine: Engine::Claude,
            home: readable_home,
        },
    ];
    let report = scan::scan(&homes, &ScanOptions::default(), |_| {});

    // Restore permissions so the tempdir can be cleaned up.
    fs::set_permissions(&locked_projects, fs::Permissions::from_mode(0o755)).expect("chmod");
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o644)).expect("chmod");

    assert_eq!(report.counts.sessions, 1, "only `good` was readable");
    assert!(
        report.unreadable >= 2,
        "the locked home and the unreadable file: {}",
        report.unreadable
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_cycle_does_not_hang_the_scan() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("claude-home");
    let proj = home.join("projects").join("p");
    write_line(
        &proj.join("s1.jsonl"),
        &claude_line("s1", "/w/p", "main", "2026-01-01T00:00:00Z", false),
        BASE_MS,
    );
    // A folder that links to its own parent: a real walk that followed it would never stop.
    symlink(&proj, proj.join("self")).expect("symlink");

    let homes = vec![ScanHome {
        engine: Engine::Claude,
        home,
    }];
    let start = Instant::now();
    let report = scan::scan(&homes, &ScanOptions::default(), |_| {});
    assert!(start.elapsed().as_secs() < 5, "took {:?}", start.elapsed());
    assert_eq!(report.counts.sessions, 1);
}

/// `cargo test -p pitcrew-ingest --release -- --ignored --nocapture ten_thousand_sessions`
#[test]
#[ignore = "benchmark: writes 10,000 synthetic sessions"]
fn ten_thousand_sessions_scan_under_a_minute() {
    let dir = tempfile::tempdir().expect("tempdir");

    let claude_home = dir.path().join("claude-home");
    let proj = claude_home.join("projects").join("p");
    for n in 0..4000 {
        write_line(
            &proj.join(format!("s{n}.jsonl")),
            &claude_line(
                &format!("cs{n}"),
                &format!("/w/p{}", n % 20),
                "main",
                "2026-01-01T00:00:00Z",
                false,
            ),
            BASE_MS,
        );
    }

    let codex_home = dir.path().join("codex-home");
    for n in 0..3000 {
        let path = codex_home.join(format!(
            "sessions/2026/01/{:02}/rollout-{n}.jsonl",
            1 + n % 28
        ));
        write_line(
            &path,
            &codex_line(
                &format!("cx{n}"),
                &format!("/w/q{}", n % 20),
                "main",
                "2026-01-01T00:00:00Z",
                false,
            ),
            BASE_MS,
        );
    }

    let opencode_home = dir.path().join("opencode-home");
    fs::create_dir_all(&opencode_home).expect("mkdir");
    let db = opencode_home.join("opencode.db");
    let mut conn = Connection::open(&db).expect("open");
    conn.execute_batch(
        "CREATE TABLE session (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL, parent_id TEXT,
            directory TEXT NOT NULL, title TEXT NOT NULL, version TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL
         );",
    )
    .expect("schema");
    let tx = conn.transaction().expect("tx");
    for n in 0..3000 {
        tx.execute(
            "INSERT INTO session (id, project_id, parent_id, directory, title, version, time_created, time_updated)
             VALUES (?1, 'p', NULL, ?2, 'New session', 'v', 1767225600000, 1767225600000)",
            params![format!("oc{n}"), format!("/w/r{}", n % 20)],
        )
        .expect("insert");
    }
    tx.commit().expect("commit");
    drop(conn);

    let homes = vec![
        ScanHome {
            engine: Engine::Claude,
            home: claude_home,
        },
        ScanHome {
            engine: Engine::Codex,
            home: codex_home,
        },
        ScanHome {
            engine: Engine::OpenCode,
            home: opencode_home,
        },
    ];
    let mut last = 0usize;
    let start = Instant::now();
    let report = scan::scan(&homes, &ScanOptions::default(), |p| {
        assert!(p.scanned >= last, "progress went backwards");
        last = p.scanned;
    });
    let took = start.elapsed();
    println!(
        "scanned {} sessions in {took:?}",
        report.counts.sessions + report.counts.subagent_sessions
    );
    assert_eq!(report.counts.sessions, 10_000);
    assert!(took.as_secs() < 60, "took {took:?}");
}
