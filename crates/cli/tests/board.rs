//! `pitcrew board submit`: a board draft's proposal, from a file or on stdin, against an
//! in-process fake daemon.

#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use serde_json::{Value, json};

const DRAFT: &str = "01J00000000000000000000001";
const WORKSTREAM: &str = "01JB000000000000000WST0001";
const SESSION: &str = "01JB000000000000000SES0001";

fn proposal() -> Value {
    json!({
        "tasks": [
            {"title": "Finish the method section", "status": "in_progress", "evidence": [SESSION]},
            {"title": "Submit", "status": "todo"}
        ],
        "note": "Synthetic note."
    })
}

/// The draft as the daemon answers it, with `proposal` in.
fn draft(proposal: Value) -> Value {
    json!({
        "id": DRAFT,
        "workstream": WORKSTREAM,
        "agent": WRITER,
        "engine": "claude",
        "session": SESSION,
        "by": SAM,
        "prompt": "draft-board/v1",
        "cost": {"sessions": 1, "sessions_left_out": 0, "tasks": 0, "summary_bytes": 10,
                 "prompt_bytes": 100, "redacted": 0,
                 "estimate": {"input_tokens": 15025, "output_tokens": 8192}},
        "started": 1,
        "state": "proposed",
        "proposal": proposal,
        "proposed": 2,
        "accepted": [],
        "rejected": []
    })
}

fn route() -> String {
    format!("POST /v1/board-drafts/{DRAFT}/proposal")
}

#[test]
fn submit_posts_the_proposal_as_given() {
    let server = FakeServer::tcp(routes(&[(route().as_str(), 201, draft(proposal()))]));
    let text = proposal().to_string();
    let (code, out, err) = run_on(
        &server,
        &["board", "submit", &format!("drf_{DRAFT}")],
        &text,
    );
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        format!(
            "Proposed 2 tasks for drf_{DRAFT}. Nothing is created until a person reviews it.\n"
        )
    );
    let posted = server
        .api_requests()
        .into_iter()
        .find(|r| r.route() == route())
        .unwrap();
    assert_eq!(posted.json(), proposal());
    assert_eq!(posted.header("content-type"), Some("application/json"));

    let (code, out, _) = run_on(&server, &["--json", "board", "submit", DRAFT], &text);
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        draft(proposal())
    );
}

#[test]
fn a_proposal_that_is_not_one_is_refused_before_it_is_sent() {
    let server = FakeServer::tcp(routes(&[(route().as_str(), 201, draft(proposal()))]));
    let big = json!({"tasks": [], "note": "n".repeat(33 * 1024)}).to_string();
    for (stdin, says) in [
        ("not json", "not JSON"),
        ("[1, 2]", "JSON object"),
        ("", "not JSON"),
        (big.as_str(), "at most"),
    ] {
        let (code, out, err) = run_on(&server, &["board", "submit", DRAFT], stdin);
        assert_eq!(code, 2, "{stdin:.20}: {err}");
        assert!(out.is_empty());
        assert!(err.contains(says), "{err}");
    }
    for bad in ["PAP-1", "..", "drf_", "tsk_01J00000000000000000000001/x"] {
        let (code, _, err) = run_on(&server, &["board", "submit", bad], "{}");
        assert_eq!(code, 2, "{bad}: {err}");
    }
    assert!(
        server.api_requests().iter().all(|r| r.route() != route()),
        "nothing refused here reaches the daemon"
    );
}

#[test]
fn the_daemons_refusals_keep_their_exit_codes() {
    for (status, code, exit) in [
        (400, "invalid", 2),
        (403, "forbidden", 3),
        (409, "conflict", 4),
        (404, "not_found", 6),
    ] {
        let server = FakeServer::tcp(routes(&[(
            route().as_str(),
            status,
            api_error(code, "Synthetic refusal."),
        )]));
        let (got, out, err) = run_on(
            &server,
            &["board", "submit", DRAFT],
            &proposal().to_string(),
        );
        assert_eq!(got, exit, "{status}: {err}");
        assert!(out.is_empty());
        assert!(err.contains("Synthetic refusal."), "{err}");
    }
}

#[test]
fn a_persons_token_is_refused_unsent() {
    let server = FakeServer::tcp(routes(&[
        ("GET /v1/me", 200, sam()),
        (route().as_str(), 201, draft(proposal())),
    ]));
    let (code, _, err) = run(
        &["board", "submit", DRAFT],
        &[
            ("PITCREW_URL", &server.url),
            ("PITCREW_TOKEN", "pcd_synthetic-device"),
        ],
        &proposal().to_string(),
    );
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("device token"), "{err}");
    assert!(server.api_requests().iter().all(|r| r.route() != route()));
}

/// A draft's run proposes with its session token, which may not even ask whose it is: the verb
/// sends the proposal alone, never `GET /v1/me`.
#[test]
fn a_session_token_proposes_without_asking_whose_it_is() {
    let server = FakeServer::tcp(routes(&[(route().as_str(), 201, draft(proposal()))]));
    let (code, _, err) = run(
        &["board", "submit", DRAFT],
        &[
            ("PITCREW_URL", &server.url),
            ("PITCREW_TOKEN", "pcs_synthetic-session"),
        ],
        &proposal().to_string(),
    );
    assert_eq!((code, err.as_str()), (0, ""));
    let routes: Vec<String> = server.api_requests().iter().map(|r| r.route()).collect();
    assert_eq!(routes, [route()]);
}

/// `--file`: the proposal from a file the run wrote in its folder (PowerShell and `cmd.exe` have
/// no heredoc); `--file -` is stdin.
#[test]
fn the_proposal_comes_from_a_file() {
    let server = FakeServer::tcp(routes(&[(route().as_str(), 201, draft(proposal()))]));
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("proposal.json");
    std::fs::write(&file, proposal().to_string()).unwrap();
    let path = file.to_str().unwrap();
    let (code, out, err) = run_on(&server, &["board", "submit", DRAFT, "--file", path], "");
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.starts_with("Proposed 2 tasks"), "{out}");
    let posted = server
        .api_requests()
        .into_iter()
        .find(|r| r.route() == route())
        .unwrap();
    assert_eq!(posted.json(), proposal());

    let (code, _, err) = run_on(
        &server,
        &["board", "submit", DRAFT, "--file", "-"],
        &proposal().to_string(),
    );
    assert_eq!((code, err.as_str()), (0, ""));

    // Refused before anything is sent: a missing file, a folder, one past the bound, not JSON.
    let posted = |server: &FakeServer| {
        server
            .api_requests()
            .iter()
            .filter(|r| r.route() == route())
            .count()
    };
    assert_eq!(posted(&server), 2);
    let big = dir.path().join("big.json");
    std::fs::write(
        &big,
        json!({"tasks": [], "note": "n".repeat(40 * 1024)}).to_string(),
    )
    .unwrap();
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "not json").unwrap();
    for (path, says) in [
        (dir.path().join("missing.json"), "cannot read"),
        (dir.path().to_path_buf(), "not a file"),
        (big, "at most"),
        (bad, "not JSON"),
    ] {
        let (code, out, err) = run_on(
            &server,
            &["board", "submit", DRAFT, "--file", path.to_str().unwrap()],
            "",
        );
        assert_eq!(code, 2, "{}: {err}", path.display());
        assert!(out.is_empty());
        assert!(err.contains(says), "{err}");
    }
    assert_eq!(
        posted(&server),
        2,
        "nothing refused here reaches the daemon"
    );
}
