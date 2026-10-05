//! Data you can trust, end to end: the real `pitcrewd` over synthetic agent homes shaped like the
//! audit of 5 October 2026's, in the daemon's own temporary home folder (never the machine's).
//!
//! The homes hold three repositories. `atlas` has two linked worktrees, one where Claude Code
//! makes them (`atlas/.claude/worktrees/search`) and one beside it (`atlas-hotfix`); `beacon` and
//! `compass` have none. Ten sessions ran in them (seven Claude, three Codex, one of those a
//! `codex exec` run), and four sub-agents: two in a Claude session's `subagents/` folder, one an
//! older top-level Claude sidechain file, and one a Codex `thread_spawn` sub-agent.
//!
//! Before: the scan proposed five projects (each worktree its own), the import counted fourteen
//! sessions, and twelve stayed unsorted. Now: three projects, ten sessions with four sub-agents
//! nested under their parents, every session linked, and no sub-agent stated as an agent.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, home_of, request};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How long the test waits for the runner. Generous: CI machines are busy.
const WAIT: Duration = Duration::from_secs(60);

/// A path as the CLIs write it in their records.
fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Where the sessions ran.
struct Work {
    atlas: PathBuf,
    search: PathBuf,
    hotfix: PathBuf,
    beacon: PathBuf,
    compass: PathBuf,
}

/// A repository at `repo` whose main checkout is on `main`.
fn repository(repo: &Path) {
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
}

/// A linked worktree of `repo` at `worktree`, on `branch`, as `git worktree add` lays it out:
/// `<worktree>/.git` names `<repo>/.git/worktrees/<name>`, whose `commondir` is `../..`.
fn worktree(repo: &Path, worktree: &Path, name: &str, branch: &str, relative: bool) {
    let admin = repo.join(".git").join("worktrees").join(name);
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::create_dir_all(worktree).unwrap();
    std::fs::write(admin.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    std::fs::write(
        admin.join("gitdir"),
        format!("{}\n", text(&worktree.join(".git"))),
    )
    .unwrap();
    let gitdir = if relative {
        // `worktree.useRelativePaths`: from `atlas/.claude/worktrees/search` up to `atlas`.
        format!("../../../.git/worktrees/{name}")
    } else {
        // Git writes `/` on every platform, after a drive letter on Windows (`C:/…`).
        text(&admin).replace('\\', "/")
    };
    std::fs::write(worktree.join(".git"), format!("gitdir: {gitdir}\n")).unwrap();
}

impl Work {
    fn under(root: &Path) -> Self {
        let work = root.join("work");
        let atlas = work.join("atlas");
        let beacon = work.join("beacon");
        let compass = work.join("compass");
        for repo in [&atlas, &beacon, &compass] {
            repository(repo);
        }
        std::fs::create_dir_all(atlas.join("src")).unwrap();
        std::fs::create_dir_all(beacon.join("docs")).unwrap();
        let search = atlas.join(".claude").join("worktrees").join("search");
        worktree(&atlas, &search, "search", "feat/search", true);
        let hotfix = work.join("atlas-hotfix");
        worktree(&atlas, &hotfix, "atlas-hotfix", "fix/login", false);
        Self {
            atlas,
            search,
            hotfix,
            beacon,
            compass,
        }
    }
}

/// Sets `path`'s modification time to `ago` before now.
fn age(path: &Path, ago: Duration) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - ago)
        .unwrap();
}

fn lines(values: &[Value]) -> String {
    values.iter().map(|v| format!("{v}\n")).collect()
}

/// A Claude session's transcript: one prompt and one reply, every record carrying `extra`.
fn claude(cwd: &Path, branch: &str, minute: u32, extra: &Value) -> String {
    let at = |s: u32| format!("2026-10-01T09:{minute:02}:{s:02}Z");
    let mut prompt = json!({"type": "user", "cwd": text(cwd), "gitBranch": branch,
        "timestamp": at(0), "message": {"role": "user", "content": "A synthetic prompt"}});
    let mut reply = json!({"type": "assistant", "cwd": text(cwd), "gitBranch": branch,
        "timestamp": at(30), "message": {"role": "assistant", "model": "synthetic-claude",
        "content": [{"type": "text", "text": "Done."}], "stop_reason": "end_turn"}});
    for record in [&mut prompt, &mut reply] {
        for (k, v) in extra.as_object().unwrap() {
            record[k] = v.clone();
        }
    }
    lines(&[prompt, reply])
}

/// A Codex rollout: its `session_meta` (with `source`), a turn context, a prompt and a reply.
fn codex(id: &str, cwd: &Path, minute: u32, source: &Value) -> String {
    let at = format!("2026-10-01T10:{minute:02}:00Z");
    lines(&[
        json!({"timestamp": at, "type": "session_meta", "payload": {"id": id, "timestamp": at,
               "cwd": text(cwd), "originator": "codex_cli_rs", "cli_version": "0.0.0",
               "source": source, "git": {"branch": "main"}}}),
        json!({"timestamp": at, "type": "turn_context",
               "payload": {"cwd": text(cwd), "model": "synthetic-codex"}}),
        json!({"timestamp": at, "type": "response_item", "payload": {"type": "message",
               "role": "user", "content": [{"type": "input_text", "text": "A synthetic task"}]}}),
        json!({"timestamp": at, "type": "response_item", "payload": {"type": "message",
               "role": "assistant", "content": [{"type": "output_text", "text": "Done."}]}}),
        json!({"timestamp": at, "type": "event_msg", "payload": {"type": "task_complete"}}),
    ])
}

const CODEX_ATLAS: &str = "0199a000-0000-7000-8000-0000000000a1";
const CODEX_SUB: &str = "0199a000-0000-7000-8000-0000000000a2";
const CODEX_EXEC: &str = "0199a000-0000-7000-8000-0000000000a3";
const CODEX_BEACON: &str = "0199a000-0000-7000-8000-0000000000a4";

/// The agent homes under `home`: what the CLIs wrote for the sessions in `work`. Returns each
/// session's CLI id and the CLI id of the parent a sub-agent should be nested under.
fn agent_homes(home: &Path, work: &Work) -> HashMap<&'static str, Option<&'static str>> {
    let projects = home.join(".claude").join("projects");
    let write = |folder: &str, name: &str, body: String| {
        let path = projects.join(folder).join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    };
    let session = |id: &str| json!({"sessionId": id});
    let sub = |parent: &str, agent: &str| json!({"sessionId": parent, "agentId": agent, "isSidechain": true});
    write(
        "-work-atlas",
        "c-atlas.jsonl",
        claude(&work.atlas, "main", 0, &session("c-atlas")),
    );
    // Two sub-agents in the session's own folder, as Claude Code writes them now.
    write(
        "-work-atlas",
        "c-atlas/subagents/agent-a1.jsonl",
        claude(&work.atlas, "main", 1, &sub("c-atlas", "a1")),
    );
    write(
        "-work-atlas",
        "c-atlas/subagents/agent-a2.jsonl",
        claude(&work.atlas, "main", 2, &sub("c-atlas", "a2")),
    );
    write(
        "-work-atlas-src",
        "c-atlas-src.jsonl",
        claude(&work.atlas.join("src"), "main", 3, &session("c-atlas-src")),
    );
    write(
        "-work-atlas--claude-worktrees-search",
        "c-search.jsonl",
        claude(&work.search, "feat/search", 4, &session("c-search")),
    );
    // An older sub-agent: a top-level sidechain file beside its parent.
    write(
        "-work-atlas--claude-worktrees-search",
        "agent-s1.jsonl",
        claude(&work.search, "feat/search", 5, &sub("c-search", "s1")),
    );
    write(
        "-work-atlas-hotfix",
        "c-hotfix.jsonl",
        claude(&work.hotfix, "fix/login", 6, &session("c-hotfix")),
    );
    write(
        "-work-beacon",
        "c-beacon.jsonl",
        claude(&work.beacon, "main", 7, &session("c-beacon")),
    );
    write(
        "-work-beacon-docs",
        "c-beacon-docs.jsonl",
        claude(
            &work.beacon.join("docs"),
            "main",
            8,
            &session("c-beacon-docs"),
        ),
    );
    write(
        "-work-compass",
        "c-compass.jsonl",
        claude(&work.compass, "main", 9, &session("c-compass")),
    );

    let day = home.join(".codex").join("sessions/2026/10/01");
    std::fs::create_dir_all(&day).unwrap();
    let rollout = |minute: u32, id: &str, cwd: &Path, source: Value| {
        let path = day.join(format!("rollout-2026-10-01T10-{minute:02}-00-{id}.jsonl"));
        std::fs::write(&path, codex(id, cwd, minute, &source)).unwrap();
        path
    };
    let parent = rollout(0, CODEX_ATLAS, &work.atlas, json!("cli"));
    let spawn =
        json!({"subagent": {"thread_spawn": {"parent_thread_id": CODEX_ATLAS, "depth": 1}}});
    rollout(1, CODEX_SUB, &work.atlas, spawn);
    // The parent's last write is older than its sub-agent's: the sub-agent is read first.
    age(&parent, Duration::from_secs(600));
    rollout(2, CODEX_EXEC, &work.compass, json!("exec"));
    rollout(3, CODEX_BEACON, &work.beacon, json!("cli"));

    HashMap::from([
        ("c-atlas", None),
        ("a1", Some("c-atlas")),
        ("a2", Some("c-atlas")),
        ("c-atlas-src", None),
        ("c-search", None),
        ("s1", Some("c-search")),
        ("c-hotfix", None),
        ("c-beacon", None),
        ("c-beacon-docs", None),
        ("c-compass", None),
        (CODEX_ATLAS, None),
        (CODEX_SUB, Some(CODEX_ATLAS)),
        (CODEX_EXEC, None),
        (CODEX_BEACON, None),
    ])
}

/// The scan's report, read whole from its newline-delimited answer.
fn scan(daemon: &Daemon, machine: &str, token: &str) -> Value {
    let reply = daemon.post(
        &format!("/v1/machines/{machine}/scan"),
        Some(token),
        &Value::Null,
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let last: Value = serde_json::from_str(reply.body.lines().last().unwrap()).unwrap();
    assert_eq!(last["type"], "done", "{last}");
    last["report"].clone()
}

/// Waits until `ok` holds for the hub's sessions; returns them.
fn sessions_until(
    daemon: &Daemon,
    token: &str,
    what: &str,
    ok: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let started = Instant::now();
    loop {
        let sessions = daemon.get("/v1/sessions", Some(token)).json();
        let list = sessions.as_array().unwrap().clone();
        if ok(&list) {
            return list;
        }
        assert!(
            started.elapsed() < WAIT,
            "{what}: {sessions:#}\nlog: {}",
            daemon.stderr()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn the_audit_homes_give_three_projects_ten_sessions_and_nothing_unsorted() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let work = Work::under(tmp.path());
    let expected = agent_homes(&home_of(&state), &work);

    let daemon = Daemon::start(&state, &["--no-office"]);
    let token = daemon.device_token();
    let setup = daemon.post(
        "/v1/setup",
        Some(&token),
        &json!({
            "workspace_name": "Audit Lab",
            "person": { "name": "Alex Doe", "handle": "@alex" },
            "machine_name": "This laptop",
        }),
    );
    assert_eq!(setup.status, 200, "{}", setup.body);
    let machine = setup.json()["machine"]["id"].as_str().unwrap().to_owned();

    // ── The scan: three projects, ten sessions, four sub-agents. ──────────────────────────────
    let report = scan(&daemon, &machine, &token);
    assert_eq!(report["counts"]["sessions"], 10, "{report:#}");
    assert_eq!(report["counts"]["subagent_sessions"], 4, "{report:#}");
    let suggestions = report["suggestions"].as_array().unwrap();
    let by_path: HashMap<&str, &Value> = suggestions
        .iter()
        .map(|s| (s["path"].as_str().unwrap(), s))
        .collect();
    let mut paths: Vec<&str> = by_path.keys().copied().collect();
    paths.sort_unstable();
    let mut want = vec![text(&work.atlas), text(&work.beacon), text(&work.compass)];
    want.sort_unstable();
    assert_eq!(paths, want, "one project per repository: {report:#}");
    let workstreams = |repo: &Path| -> Vec<(String, String, String)> {
        by_path[text(repo)]["workstreams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| {
                (
                    w["kind"].as_str().unwrap().to_owned(),
                    w["id"].as_str().unwrap().to_owned(),
                    w["name"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let atlas = workstreams(&work.atlas);
    assert_eq!(
        atlas[0],
        ("main".into(), text(&work.atlas).into(), "main".into()),
        "the default workstream comes first: {atlas:?}"
    );
    for wanted in [
        ("worktree", text(&work.search), "feat/search"),
        ("worktree", text(&work.hotfix), "fix/login"),
        ("folder", text(&work.atlas.join("src")), "src"),
    ] {
        assert!(
            atlas
                .iter()
                .any(|(k, i, n)| (k.as_str(), i.as_str(), n.as_str()) == wanted),
            "{wanted:?} in {atlas:?}"
        );
    }
    assert_eq!(atlas.len(), 4, "no `.claude` folder, no branch: {atlas:?}");
    assert_eq!(workstreams(&work.beacon).len(), 2);
    assert_eq!(workstreams(&work.compass).len(), 1);

    // ── Creating from the scan, as onboarding does: every suggested workstream, the default one
    // of each project included. ─────────────────────────────────────────────────────────────────
    let mut workstream_at: HashMap<String, String> = HashMap::new();
    for (n, suggestion) in suggestions.iter().enumerate() {
        let project = daemon.post(
            "/v1/projects",
            Some(&token),
            &json!({
                "key": format!("PR{n}"),
                "name": suggestion["name"],
                "root": { "machine": machine, "path": suggestion["path"] },
            }),
        );
        assert_eq!(project.status, 201, "{}", project.body);
        let project = project.json()["id"].as_str().unwrap().to_owned();
        for w in suggestion["workstreams"].as_array().unwrap() {
            let location = match w["branch"].as_str() {
                Some(branch) => {
                    json!({"machine": machine, "path": suggestion["path"], "branch": branch})
                }
                None => json!({"machine": machine, "path": w["id"]}),
            };
            let created = daemon.post(
                "/v1/workstreams",
                Some(&token),
                &json!({"project": project, "name": w["name"], "locations": [location]}),
            );
            assert_eq!(created.status, 201, "{}", created.body);
            workstream_at.insert(
                w["id"].as_str().unwrap().to_owned(),
                created.json()["id"].as_str().unwrap().to_owned(),
            );
        }
    }

    // ── The import: every transcript indexed, ten sessions with four sub-agents. ─────────────────
    let sessions = sessions_until(&daemon, &token, "all fourteen transcripts", |s| {
        s.len() == 14
    });
    let dry = daemon.post("/v1/import/dry-run", Some(&token), &json!({"mode": "all"}));
    assert_eq!(dry.status, 200, "{}", dry.body);
    assert_eq!(dry.json(), json!({"count": 10, "subagents": 4}));
    let committed = request(
        daemon.port,
        "PUT",
        "/v1/import",
        Some(&token),
        Some(&json!({"mode": "all"})),
        &[],
    );
    assert_eq!(committed.status, 200, "{}", committed.body);
    assert_eq!(committed.json(), json!({"imported": 10, "subagents": 4}));

    // ── Nesting: each sub-agent names its parent's session. ──────────────────────────────────────
    let id_of: HashMap<&str, &str> = sessions
        .iter()
        .map(|s| (s["native_id"].as_str().unwrap(), s["id"].as_str().unwrap()))
        .collect();
    for (native, parent) in &expected {
        let session = sessions
            .iter()
            .find(|s| s["native_id"] == *native)
            .unwrap_or_else(|| panic!("no session {native}: {sessions:#?}"));
        assert_eq!(
            session["parent"].as_str(),
            parent.map(|p| id_of[p]),
            "{native}: {session:#}"
        );
        // Found on disk: no terminal of PitCrew's and no agent, so it is the session's doing,
        // never a person's or an agent's.
        assert!(session.get("terminal").is_none(), "{session:#}");
        assert!(session.get("agent").is_none(), "{session:#}");
        // What the transcript records about it.
        let model = if session["engine"] == "codex" {
            "synthetic-codex"
        } else {
            "synthetic-claude"
        };
        assert_eq!(session["recorded"]["model"], model, "{session:#}");
        let account = if session["engine"] == "codex" {
            Path::new("~").join(".codex")
        } else {
            Path::new("~").join(".claude")
        };
        assert_eq!(
            session["recorded"]["account"],
            text(&account),
            "{session:#}"
        );
    }
    // "Agents now" lists sessions without a parent: no sub-agent is an agent.
    let agents_now: Vec<&Value> = sessions
        .iter()
        .filter(|s| s.get("parent").is_none() && s["state"] != "ended")
        .collect();
    assert!(
        agents_now
            .iter()
            .all(|s| expected[s["native_id"].as_str().unwrap()].is_none()),
        "{agents_now:#?}"
    );

    // ── Links: every session in its workstream, the worktrees' in theirs. Nothing unsorted. ──────
    let linked = sessions_until(&daemon, &token, "every session linked", |s| {
        s.len() == 14 && s.iter().all(|s| s.get("workstream").is_some())
    });
    let workstream_of = |native: &str| {
        linked
            .iter()
            .find(|s| s["native_id"] == native)
            .and_then(|s| s["workstream"].as_str())
            .unwrap()
            .to_owned()
    };
    let at = |path: &Path| workstream_at[text(path)].clone();
    for (native, folder) in [
        ("c-atlas", work.atlas.clone()),
        ("a1", work.atlas.clone()),
        ("c-atlas-src", work.atlas.join("src")),
        ("c-search", work.search.clone()),
        ("s1", work.search.clone()),
        ("c-hotfix", work.hotfix.clone()),
        ("c-beacon", work.beacon.clone()),
        ("c-beacon-docs", work.beacon.join("docs")),
        ("c-compass", work.compass.clone()),
        (CODEX_ATLAS, work.atlas.clone()),
        (CODEX_SUB, work.atlas.clone()),
        (CODEX_EXEC, work.compass.clone()),
        (CODEX_BEACON, work.beacon.clone()),
    ] {
        assert_eq!(workstream_of(native), at(&folder), "{native}");
    }

    // ── Attribution: the discoveries are stated as found, not started by @alex. ──────────────────
    let events = daemon.all_events(&token);
    let discovered: Vec<&Value> = events
        .iter()
        .filter(|e| e["body"]["type"] == "session_discovered")
        .collect();
    assert_eq!(discovered.len(), 14);
    assert!(
        discovered
            .iter()
            .all(|e| e["body"]["data"]["session"].get("terminal").is_none()
                && e["body"]["data"]["session"].get("agent").is_none()),
        "a discovered session is nobody's start"
    );
}
