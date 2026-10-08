//! The read verbs (`session`, `recap`, `activity`, `search`) against an in-process fake daemon:
//! what each asks, what it prints, and that each only reads. Synthetic data only.

#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use serde_json::{Value, json};

const SES1: &str = "01JB0000000000000000000071";
const SES2: &str = "01JB0000000000000000000072";
const WST1: &str = "01JB0000000000000000000081";
const MCH1: &str = "01JB0000000000000000000091";
const BLK1: &str = "01JB0000000000000000000101";
/// 2026-09-30 10:00 UTC.
const LATER: i64 = 1_790_762_400_000;
/// 2026-09-29 10:00 UTC.
const EARLIER: i64 = 1_790_676_000_000;

fn session(id: &str, title: &str, last: i64, linked: bool) -> Value {
    let mut s = json!({
        "id": id, "engine": "claude", "native_id": "n", "machine": MCH1, "cwd": "/w",
        "title": title, "state": "working", "started": last - 60_000, "last_activity": last,
    });
    if linked {
        s["workstream"] = json!(WST1);
        s["task"] = json!(TASK1);
    }
    s
}

fn sessions() -> Value {
    json!([
        session(SES1, "Draft\u{1b}[2J the paper", EARLIER, true),
        session(SES2, "Orchestrator", LATER, false),
    ])
}

fn workstreams() -> Value {
    json!([{"id": WST1, "project": PROJECT, "name": "Paper", "status": "active",
            "health": "on_track", "locations": []}])
}

fn tasks() -> Value {
    json!([task(TASK1, "PAP-1", "in_progress")])
}

fn blocks() -> Value {
    json!({
        "blocks": [{
            "block": {
                "id": BLK1, "last": BLK1, "key": {"kind": "session", "id": SES1},
                "start": EARLIER, "end": EARLIER + 600_000, "session": SES1, "workstream": WST1,
                "project": PROJECT, "tasks": [TASK1], "actors": [WRITER],
                "counts": {"events": 4, "tools_run": 2, "tools_failed": 0, "file_edits": 1,
                           "lines_added": 3, "lines_removed": 1, "turns": 1, "asks_raised": 0,
                           "asks_answered": 0, "task_moves": 0, "comments": 0},
                "files": [{"path": "paper/method.tex", "edits": 1, "added": 3, "removed": 1,
                           "receipts": []}],
                "files_omitted": 0, "facts": [], "facts_omitted": 0, "tool_receipts": [],
                "turn_receipts": []
            },
            "line": {"text": "@writer edited method.tex (+3 −1)", "spans": []}
        }],
        "at_start": true
    })
}

/// The reads every verb may make.
fn reads(extra: &[(&str, u16, Value)]) -> Handler {
    let mut all: Vec<(&str, u16, Value)> = vec![
        ("GET /v1/sessions", 200, sessions()),
        ("GET /v1/workstreams", 200, workstreams()),
        ("GET /v1/tasks", 200, tasks()),
        ("GET /v1/tasks/PAP-1", 200, tasks()[0].clone()),
        ("GET /v1/members", 200, members()),
        ("GET /v1/recaps/blocks", 200, blocks()),
    ];
    all.extend(extra.iter().cloned());
    routes(&all)
}

/// Every request a run made was a `GET`.
fn only_reads(server: &FakeServer) {
    for request in server.api_requests() {
        assert_eq!(request.method, "GET", "{}", request.route());
    }
}

#[test]
fn session_list_shows_ids_states_names_and_filters() {
    let server = FakeServer::tcp(reads(&[]));
    let (code, out, err) = run_on(&server, &["session", "list"], "");
    assert_eq!((code, err.as_str()), (0, ""));
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("SESSION"), "{out}");
    assert!(lines[1].starts_with(&format!("ses_{SES2}  working  claude  2026-09-30 10:00")));
    assert!(lines[1].ends_with("Orchestrator"));
    assert!(lines[2].contains("Paper"), "{out}");
    assert!(lines[2].contains("PAP-1"), "{out}");
    assert!(
        lines[2].ends_with("Draft[2J the paper"),
        "escapes are dropped: {out}"
    );

    // --since filters here; --state and --task (by its id) go to the daemon.
    let (code, out, _) = run_on(
        &server,
        &[
            "--json",
            "session",
            "list",
            "--since",
            "2026-09-30",
            "--state",
            "working",
            "--task",
            "pap-1",
        ],
        "",
    );
    assert_eq!(code, 0);
    let listed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(listed, json!([sessions()[1].clone()]));
    let asked = server
        .api_requests()
        .into_iter()
        .rfind(|r| r.route() == "GET /v1/sessions")
        .unwrap();
    assert_eq!(
        asked.target,
        format!("/v1/sessions?task={TASK1}&state=working")
    );
    let (code, _, err) = run_on(&server, &["session", "list", "--since", "yesterday"], "");
    assert_eq!(code, 2);
    assert!(err.contains("not a day"), "{err}");
    only_reads(&server);
}

#[test]
fn session_show_prints_its_facts_and_recent_work() {
    let server = FakeServer::tcp(reads(&[
        (
            &format!("GET /v1/sessions/{SES1}"),
            200,
            sessions()[0].clone(),
        ),
        (
            "GET /v1/machines",
            200,
            json!([{"id": MCH1, "name": "This laptop", "kind": "local", "liveness": "live"}]),
        ),
    ]));
    let (code, out, err) = run_on(&server, &["session", "show", &format!("ses_{SES1}")], "");
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(
        out.starts_with(&format!("ses_{SES1}  Draft[2J the paper\n")),
        "{out}"
    );
    assert!(out.contains("machine: This laptop"));
    assert!(out.contains(&format!("workstream: wst_{WST1} Paper")));
    assert!(out.contains("task: PAP-1"));
    assert!(out.contains("2026-09-29 10:00  @writer edited method.tex (+3 −1)"));
    assert!(out.contains("files: paper/method.tex"));
    let asked = server
        .api_requests()
        .into_iter()
        .find(|r| r.route() == "GET /v1/recaps/blocks")
        .unwrap();
    assert_eq!(
        asked.target,
        format!("/v1/recaps/blocks?session={SES1}&limit=10")
    );
    let (code, out, _) = run_on(&server, &["--json", "session", "show", SES1], "");
    assert_eq!(code, 0);
    let both: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(both["session"], sessions()[0]);
    assert_eq!(both["blocks"], blocks());
    let (code, _, err) = run_on(&server, &["session", "show", "PAP-1"], "");
    assert_eq!(code, 2);
    assert!(err.contains("not a session id"), "{err}");
    only_reads(&server);
}

#[test]
fn recaps_print_their_lines_and_days_cite_themselves() {
    let days = json!({
        "days": [{"workstream": WST1, "date": "2026-09-29", "blocks": [BLK1],
                  "summary": {"text": "@writer drafted the method.", "spans": []}}],
        "at_start": true
    });
    let server = FakeServer::tcp(reads(&[("GET /v1/recaps/days", 200, days.clone())]));
    let (code, out, err) = run_on(
        &server,
        &[
            "recap",
            "blocks",
            "--task",
            "PAP-1",
            "--workstream",
            &format!("wst_{WST1}"),
            "--limit",
            "5",
        ],
        "",
    );
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        format!(
            "2026-09-29 10:00 to 2026-09-29 10:10  ses_{SES1}  @writer edited method.tex (+3 −1)\n    workstream wst_{WST1}\n"
        )
    );
    let asked = server
        .api_requests()
        .into_iter()
        .find(|r| r.route() == "GET /v1/recaps/blocks")
        .unwrap();
    assert_eq!(
        asked.target,
        format!("/v1/recaps/blocks?task={TASK1}&workstream={WST1}&limit=5")
    );

    let (code, out, err) = run_on(&server, &["recap", "days", "--workstream", WST1], "");
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        format!("recap:wst_{WST1}@2026-09-29  Paper\n  @writer drafted the method.\n")
    );
    let (code, out, _) = run_on(
        &server,
        &["--json", "recap", "days", "--workstream", WST1],
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), days);
    for bad in [
        &["recap", "days"][..],
        &["recap", "days", "--workstream", WST1, "--project", PROJECT],
        &["recap", "days", "--session", SES1],
        &["recap", "blocks", "--project", "PAP-1"],
    ] {
        assert_eq!(run_on(&server, bad, "").0, 2, "{bad:?}");
    }
    only_reads(&server);
}

#[test]
fn activity_describes_events_by_what_they_name() {
    let page = json!({
        "revisions": [7],
        "events": [{
            "id": EVENT1, "at": LATER, "workspace": WORKSPACE, "author": WRITER,
            "on_behalf_of": SAM,
            "body": {"type": "task_moved",
                     "data": {"task": TASK1, "from": "todo", "to": "in_progress",
                              "mover": {"kind": "person"}}}
        }],
        "from_rev": 7, "to_rev": 7, "at_start": true
    });
    let server = FakeServer::tcp(reads(&[("GET /v1/events", 200, page)]));
    let (code, out, err) = run_on(
        &server,
        &["activity", "--session", SES1, "--limit", "3"],
        "",
    );
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        "2026-09-30 10:00  task_moved  @writer: PAP-1 todo → in_progress\n"
    );
    let asked = server
        .api_requests()
        .into_iter()
        .find(|r| r.route() == "GET /v1/events")
        .unwrap();
    assert_eq!(asked.target, format!("/v1/events?session={SES1}&limit=3"));
    only_reads(&server);
}

#[test]
fn search_finds_every_word_across_kinds() {
    let server = FakeServer::tcp(reads(&[(
        "GET /v1/projects",
        200,
        json!([{"id": PROJECT, "key": "PAP", "name": "Diffusion paper", "status": "in_progress",
                "lead": SAM, "members": []}]),
    )]));
    let (code, out, err) = run_on(&server, &["search", "METHOD"], "");
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        format!(
            "Recent work:\n  2026-09-29 10:00  ses_{SES1}  @writer edited method.tex (+3 −1)\n"
        )
    );
    let (code, out, _) = run_on(&server, &["search", "synthetic", "pap-1"], "");
    assert_eq!(code, 0);
    assert_eq!(out, "Tasks:\n  PAP-1  in_progress  Synthetic task PAP-1\n");
    let (code, out, _) = run_on(&server, &["search", "paper"], "");
    assert_eq!(code, 0);
    assert!(out.contains(&format!("Projects:\n  prj_{PROJECT}  PAP  Diffusion paper")));
    assert!(out.contains(&format!("Workstreams:\n  wst_{WST1}  Paper")));
    assert!(out.contains(&format!("Sessions:\n  ses_{SES1}")));
    let (code, out, _) = run_on(&server, &["search", "nothing-like-it"], "");
    assert_eq!((code, out.as_str()), (0, "Nothing matches.\n"));
    let (code, out, _) = run_on(&server, &["--json", "search", "method"], "");
    assert_eq!(code, 0);
    let found: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(found["blocks"][0], blocks()["blocks"][0]);
    assert_eq!(found["tasks"], json!([]));
    only_reads(&server);
}

/// What a reader token's refusal looks like to the agent: exit 3, the daemon's message.
#[test]
fn a_write_a_reader_may_not_make_exits_3() {
    let server = FakeServer::tcp(routes(&[(
        "POST /v1/tasks/PAP-1/move",
        403,
        api_error(
            "forbidden",
            "POST /v1/tasks/PAP-1/move is refused: this token may only read.",
        ),
    )]));
    let (code, _, err) = run_on(&server, &["task", "move", "PAP-1", "done"], "");
    assert_eq!(code, 3);
    assert!(err.contains("may only read"), "{err}");
}
