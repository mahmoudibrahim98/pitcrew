//! Every verb against an in-process fake daemon, and every error mapping.

#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use serde_json::{Value, json};

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

#[test]
fn whoami_checks_the_version_first_and_sends_the_token_only_with_api_calls() {
    let server = FakeServer::tcp(routes(&[
        ("GET /v1/me", 200, writer()),
        ("GET /v1/members", 200, members()),
    ]));
    let (code, out, err) = run_on(&server, &["whoami"], "");
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(out, "@writer (Writer), an agent of @sam\n");

    let requests = server.requests();
    assert_eq!(requests[0].target, "/v1/host/info");
    assert_eq!(requests[0].header("authorization"), None);
    assert_eq!(requests[0].header("host"), Some(&server.url[7..]));
    assert_eq!(requests[1].target, "/v1/me");
    assert_eq!(requests[1].header("authorization"), Some(bearer().as_str()));
    assert!(requests.iter().all(|r| !r.target.contains(TOKEN)));

    let (code, out, _) = run_on(&server, &["whoami", "--json"], "");
    assert_eq!(code, 0);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), writer());
}

#[test]
fn task_list_filters_by_me_and_status() {
    let server = FakeServer::tcp(routes(&[
        ("GET /v1/me", 200, writer()),
        ("GET /v1/members", 200, members()),
        (
            "GET /v1/tasks",
            200,
            json!([
                task(TASK1, "PAP-1", "in_progress"),
                task(TASK2, "PAP-10", "review")
            ]),
        ),
    ]));
    let (code, out, err) = run_on(
        &server,
        &["task", "list", "--mine", "--status", "In-Progress,review"],
        "",
    );
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        "PAP-1   in_progress  @writer  Synthetic task PAP-1\n\
         PAP-10  review       @writer  Synthetic task PAP-10\n"
    );
    let list = server
        .api_requests()
        .into_iter()
        .find(|r| r.route() == "GET /v1/tasks")
        .unwrap();
    assert_eq!(
        list.target,
        format!("/v1/tasks?assignee={WRITER}&status=in_progress&status=review")
    );

    let (code, _, err) = run_on(&server, &["task", "list", "--status", "doing"], "");
    assert_eq!(code, 2);
    assert!(err.contains("in_progress"), "{err}");
}

#[test]
fn task_show_prints_the_brief_and_subtasks() {
    let server = FakeServer::tcp(routes(&[
        (
            "GET /v1/tasks/PAP-1",
            200,
            task(TASK1, "PAP-1", "in_progress"),
        ),
        ("GET /v1/members", 200, members()),
    ]));
    let (code, out, _) = run_on(&server, &["task", "show", "pap-1"], "");
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "PAP-1  Synthetic task PAP-1\n\
         status in_progress · priority high · assignee @writer · labels writing\n\
         \n\
         Write the synthetic section.\nKeep it short.\n\
         \n\
         Subtasks:\n  \
         [x] Outline  (plan of @writer)\n  \
         [ ] Ask Sam about scope\n"
    );
    let (code, out, _) = run_on(&server, &["--json", "task", "show", "PAP-1"], "");
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        task(TASK1, "PAP-1", "in_progress")
    );
}

#[test]
fn task_move_posts_the_status() {
    let server = FakeServer::tcp(routes(&[(
        "POST /v1/tasks/PAP-1/move",
        200,
        task(TASK1, "PAP-1", "review"),
    )]));
    let (code, out, _) = run_on(&server, &["task", "move", "PAP-1", "review"], "");
    assert_eq!((code, out.as_str()), (0, "PAP-1 is now review.\n"));
    let request = &server.api_requests()[0];
    assert_eq!(request.json(), json!({"to": "review"}));
    assert_eq!(request.header("content-type"), Some("application/json"));
}

#[test]
fn task_plan_replaces_our_plan_and_keeps_ids_of_unchanged_steps() {
    let server = FakeServer::tcp(routes(&[
        ("GET /v1/me", 200, writer()),
        (
            "GET /v1/tasks/PAP-1",
            200,
            task(TASK1, "PAP-1", "in_progress"),
        ),
        (
            &format!("PUT /v1/tasks/{TASK1}/subtasks"),
            200,
            task(TASK1, "PAP-1", "in_progress"),
        ),
    ]));
    let plan = "- [x] Outline\n- [ ] Draft the section\n\n- [ ] Run the checks\n";
    let (code, out, err) = run_on(&server, &["task", "plan", "PAP-1"], plan);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(out, "PAP-1: plan updated, 1 of 3 steps done.\n");

    let put = server
        .api_requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    let body = put.json();
    let lines = body.as_array().unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["id"], SUB1, "the unchanged step keeps its id");
    assert_eq!(lines[0]["done"], true);
    assert_eq!(lines[1]["text"], "Draft the section");
    assert_ne!(lines[1]["id"], lines[2]["id"]);
    for line in lines {
        assert_eq!(
            line["source"],
            json!({"kind": "agent_plan", "agent": WRITER})
        );
    }
}

#[test]
fn claim_moves_to_in_progress_and_is_safe_to_repeat() {
    let server = FakeServer::tcp(routes(&[(
        "POST /v1/tasks/PAP-2/move",
        200,
        task(TASK2, "PAP-2", "in_progress"),
    )]));
    let (code, out, _) = run_on(&server, &["claim", "PAP-2"], "");
    assert_eq!(
        (code, out.as_str()),
        (0, "Claimed PAP-2: it is now in_progress.\n")
    );
    assert_eq!(
        server.api_requests()[0].json(),
        json!({"to": "in_progress"})
    );

    // Already in progress: the daemon says 409, and that is fine.
    let server = FakeServer::tcp(routes(&[
        (
            "POST /v1/tasks/PAP-1/move",
            409,
            api_error("conflict", "PAP-1 is already in_progress."),
        ),
        (
            "GET /v1/tasks/PAP-1",
            200,
            task(TASK1, "PAP-1", "in_progress"),
        ),
    ]));
    let (code, out, _) = run_on(&server, &["claim", "PAP-1"], "");
    assert_eq!((code, out.as_str()), (0, "PAP-1 is already in_progress.\n"));

    // In review: a real conflict.
    let server = FakeServer::tcp(routes(&[
        (
            "POST /v1/tasks/PAP-1/move",
            409,
            api_error("conflict", "An agent may only move a task forward."),
        ),
        ("GET /v1/tasks/PAP-1", 200, task(TASK1, "PAP-1", "review")),
    ]));
    let (code, _, err) = run_on(&server, &["claim", "PAP-1"], "");
    assert_eq!(code, 4);
    assert_eq!(err, "pitcrew: An agent may only move a task forward.\n");
}

#[test]
fn report_comments_then_moves_to_review() {
    let server = FakeServer::tcp(routes(&[
        (
            "POST /v1/tasks/PAP-1/comments",
            201,
            comment_event("Tests pass"),
        ),
        (
            "POST /v1/tasks/PAP-1/move",
            200,
            task(TASK1, "PAP-1", "review"),
        ),
    ]));
    let (code, out, _) = run_on(
        &server,
        &["report", "PAP-1", "--note", "Tests pass", "--review"],
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(out, "Noted on PAP-1.\nPAP-1 is now in review.\n");
    let requests = server.api_requests();
    assert_eq!(requests[0].route(), "POST /v1/tasks/PAP-1/comments");
    assert_eq!(
        requests[0].json(),
        json!({"text": "Tests pass", "mentions": []})
    );
    assert_eq!(requests[1].json(), json!({"to": "review"}));

    // A note from stdin, in JSON.
    let (code, out, _) = run_on(
        &server,
        &["report", "PAP-1", "--note", "-", "--json"],
        "Long\nnote",
    );
    assert_eq!(code, 0);
    let out: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(out["task"], Value::Null);
    assert_eq!(out["comment"]["body"]["type"], "comment_posted");
    assert_eq!(server.api_requests()[2].json()["text"], "Long\nnote");

    // Neither flag: refused before any request.
    let before = server.requests().len();
    let (code, _, err) = run_on(&server, &["report", "PAP-1"], "");
    assert_eq!(code, 2, "{err}");
    assert_eq!(server.requests().len(), before);
}

#[test]
fn comment_resolves_mentions() {
    let server = FakeServer::tcp(routes(&[
        ("GET /v1/members", 200, members()),
        (
            "POST /v1/tasks/PAP-1/comments",
            201,
            comment_event("Looks good"),
        ),
    ]));
    let (code, out, _) = run_on(
        &server,
        &["comment", "pap-1", "Looks", "good", "--mention", "@SAM"],
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(out, "Commented on PAP-1. Mentioned @sam.\n");
    let post = server
        .api_requests()
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    assert_eq!(
        post.json(),
        json!({"text": "Looks good", "mentions": [SAM]})
    );

    let (code, _, err) = run_on(
        &server,
        &["comment", "PAP-1", "hi", "--mention", "@nobody"],
        "",
    );
    assert_eq!(code, 2);
    assert!(
        err.contains("no member @nobody; members: @sam, @writer"),
        "{err}"
    );
}

#[test]
fn ask_raises_a_question_with_options() {
    let ask = json!({
        "id": ASK1, "kind": "question", "from": WRITER, "to": SAM, "task": TASK1,
        "title": "Which seed?", "body": "", "options": ["3", "5"], "receipts": [],
        "state": "open", "created": 1_700_000_000_000_i64
    });
    let server = FakeServer::tcp(routes(&[
        ("GET /v1/members", 200, members()),
        (
            "GET /v1/tasks/PAP-1",
            200,
            task(TASK1, "PAP-1", "in_progress"),
        ),
        ("POST /v1/asks", 201, ask),
    ]));
    let (code, out, err) = run_on(
        &server,
        &[
            "ask",
            "@sam",
            "Which",
            "seed?",
            "--option",
            "3",
            "--option",
            "5",
            "--task",
            "PAP-1",
            "--body",
            "Both diverge.",
        ],
        "",
    );
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        format!("Asked @sam (ask_{ASK1}): Which seed?\n  options: 1) 3  2) 5\n")
    );
    let post = server
        .api_requests()
        .into_iter()
        .find(|r| r.route() == "POST /v1/asks")
        .unwrap();
    assert_eq!(
        post.json(),
        json!({
            "kind": "question", "to": SAM, "title": "Which seed?", "body": "Both diverge.",
            "options": ["3", "5"], "task": TASK1
        })
    );

    let (code, _, _) = run_on(&server, &["ask", "@sam", "x", "--kind", "gossip"], "");
    assert_eq!(code, 2);
}

#[test]
fn reply_answers_with_an_option_numbered_from_one() {
    let answered = json!({
        "id": ASK1, "kind": "question", "from": SAM, "to": WRITER, "title": "Ready?",
        "body": "", "options": ["Yes", "No"], "receipts": [], "state": "answered",
        "answer": {"by": WRITER, "option": 1, "at": 1_700_000_000_000_i64},
        "created": 1_700_000_000_000_i64
    });
    let server = FakeServer::tcp(routes(&[(
        &format!("POST /v1/asks/{ASK1}/answer"),
        200,
        answered,
    )]));
    let (code, out, _) = run_on(
        &server,
        &["reply", &format!("ask_{ASK1}"), "--option", "2"],
        "",
    );
    assert_eq!((code, out), (0, format!("Answered ask_{ASK1}.\n")));
    assert_eq!(server.api_requests()[0].json(), json!({"option": 1}));

    let (code, _, _) = run_on(&server, &["reply", ASK1, "Not", "yet"], "");
    assert_eq!(code, 0);
    assert_eq!(server.api_requests()[1].json(), json!({"text": "Not yet"}));

    let (code, _, err) = run_on(&server, &["reply", ASK1, "--option", "0"], "");
    assert_eq!(code, 2);
    assert!(err.contains("numbered from 1"), "{err}");
    let (code, _, _) = run_on(&server, &["reply", "not-an-ask", "x"], "");
    assert_eq!(code, 2);
    let (code, _, _) = run_on(&server, &["reply", ASK1], "");
    assert_eq!(code, 2);
}

#[test]
fn check_groups_what_needs_me() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let old = now - 30 * 24 * 60 * 60 * 1000;
    let ask = |id: &str, kind: &str, from: &str, to: &str, state: &str, created: i64| {
        json!({
            "id": id, "kind": kind, "from": from, "to": to, "title": format!("Title {kind}"),
            "body": "", "options": [], "receipts": [], "state": state, "created": created
        })
    };
    let mut answered = ask(ASK4, "question", WRITER, SAM, "answered", now);
    answered["options"] = json!(["A", "B"]);
    answered["answer"] = json!({"by": SAM, "option": 0, "text": "go", "at": now});
    let mut mention = ask(ASK2, "mention", SAM, WRITER, "open", now);
    mention["task"] = json!(TASK1);
    let asks = json!([
        ask(ASK1, "question", SAM, WRITER, "open", now),
        mention,
        ask(ASK3, "review", WRITER, SAM, "open", now),
        answered,
        // Not shown: an old mention, and asks between others.
        ask(
            "01JB0000000000000000000025",
            "mention",
            SAM,
            WRITER,
            "open",
            old
        ),
        ask(
            "01JB0000000000000000000026",
            "question",
            SAM,
            SAM,
            "open",
            now
        ),
    ]);
    let server = FakeServer::tcp(routes(&[
        ("GET /v1/me", 200, writer()),
        ("GET /v1/members", 200, members()),
        ("GET /v1/asks", 200, asks),
        (
            "GET /v1/tasks",
            200,
            json!([task(TASK1, "PAP-1", "in_progress")]),
        ),
    ]));
    let (code, out, err) = run_on(&server, &["check"], "");
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        format!(
            "For you:\n  ask_{ASK1}  question from @sam: Title question\n\
             Mentions (last 7 days):\n  ask_{ASK2}  mention from @sam on PAP-1: Title mention\n\
             Waiting for an answer:\n  ask_{ASK3}  review to @sam: Title review\n\
             Answered (last 7 days):\n  ask_{ASK4}  question to @sam: Title question  [1) A  2) B]\n      \u{2192} 1) A; \"go\"\n"
        )
    );

    let (code, out, _) = run_on(&server, &["check", "--json"], "");
    assert_eq!(code, 0);
    let out: Value = serde_json::from_str(&out).unwrap();
    for (group, id) in [
        ("for_me", ASK1),
        ("mentions", ASK2),
        ("waiting", ASK3),
        ("answered", ASK4),
    ] {
        let ids: Vec<&str> = out[group]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec![id], "{group}");
    }

    let quiet = FakeServer::tcp(routes(&[
        ("GET /v1/me", 200, writer()),
        ("GET /v1/asks", 200, json!([])),
    ]));
    let (code, out, _) = run_on(&quiet, &["check"], "");
    assert_eq!((code, out.as_str()), (0, "Nothing needs you.\n"));
}

#[test]
fn api_errors_become_messages_and_exit_codes() {
    let cases = [
        (400, "invalid", 2),
        (401, "unauthorized", 3),
        (403, "forbidden", 3),
        (404, "not_found", 6),
        (409, "conflict", 4),
        (503, "unavailable", 5),
        (500, "internal", 1),
    ];
    for (status, code, exit) in cases {
        let server = FakeServer::tcp(routes(&[(
            "POST /v1/tasks/PAP-4/move",
            status,
            api_error(code, "Synthetic refusal."),
        )]));
        let (got, out, err) = run_on(&server, &["task", "move", "PAP-4", "review"], "");
        assert_eq!(got, exit, "{code}: {err}");
        assert_eq!(out, "");
        assert!(err.starts_with("pitcrew: "), "{err}");
        assert!(err.contains("Synthetic refusal."), "{err}");

        let (got, _, err) = run_on(&server, &["--json", "task", "move", "PAP-4", "review"], "");
        assert_eq!(got, exit);
        let err: Value = serde_json::from_str(&err).unwrap();
        assert_eq!(err["code"], code);
    }
}

#[test]
fn a_daemon_that_is_down_is_unavailable() {
    let url = dead_url();
    let (code, _, err) = run(
        &["whoami"],
        &[("PITCREW_URL", &url), ("PITCREW_TOKEN", TOKEN)],
        "",
    );
    assert_eq!(code, 5);
    assert!(err.contains("Is pitcrewd running?"), "{err}");
}

#[test]
fn an_incompatible_daemon_is_refused_before_the_token_is_sent() {
    let server = FakeServer::tcp(routes(&[
        ("GET /v1/host/info", 200, host_info(2, 3)),
        ("GET /v1/me", 200, writer()),
    ]));
    let (code, _, err) = run_on(&server, &["whoami"], "");
    assert_eq!(code, 1);
    assert!(err.contains("protocol 2–3"), "{err}");
    assert!(server.api_requests().is_empty());
}

#[test]
fn setup_errors_are_clear() {
    let (code, _, err) = run(
        &["whoami"],
        &[
            ("PITCREW_URL", "http://192.0.2.10:47317"),
            ("PITCREW_TOKEN", TOKEN),
        ],
        "",
    );
    assert_eq!(code, 2);
    assert!(err.contains("must be this machine"), "{err}");

    let server = FakeServer::tcp(routes(&[]));
    let (code, _, err) = run(&["whoami"], &[("PITCREW_URL", &server.url)], "");
    assert_eq!(code, 2);
    assert!(err.contains("PITCREW_TOKEN"), "{err}");
    assert!(server.requests().is_empty());
}

#[test]
fn help_and_usage_errors() {
    let (code, out, _) = run(&["--help"], &[], "");
    assert_eq!(code, 0);
    assert!(out.contains("Usage: pitcrew"), "{out}");
    assert!(out.contains("PITCREW_TOKEN_FILE"), "{out}");
    let (code, _, err) = run(&["task", "fly"], &[], "");
    assert_eq!(code, 2);
    assert!(!err.is_empty());
}

#[test]
fn the_hook_subcommand_through_the_parser_is_silent() {
    let (code, out, err) = run(&["--json", "hook", "claude", "Stop"], &[], "{}");
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
}

#[test]
fn a_persons_token_is_refused_and_nothing_changes() {
    let server = FakeServer::tcp(routes(&[
        ("GET /v1/me", 200, sam()),
        (
            "GET /v1/tasks/PAP-1",
            200,
            task(TASK1, "PAP-1", "in_progress"),
        ),
        (
            &format!("PUT /v1/tasks/{TASK1}/subtasks"),
            200,
            task(TASK1, "PAP-1", "in_progress"),
        ),
    ]));
    for args in [
        &["task", "plan", "PAP-1"][..],
        &["whoami"],
        &["claim", "PAP-1"],
        &["check"],
    ] {
        let (code, out, err) = run_on(&server, args, "- [ ] Replace everything\n");
        assert_eq!(code, 2, "{args:?}: {err}");
        assert_eq!(out, "");
        assert!(err.contains("only agent tokens are accepted"), "{err}");
        assert!(err.contains("@sam, a person"), "{err}");
    }
    let routes: Vec<String> = server.requests().iter().map(Recorded::route).collect();
    assert!(
        routes
            .iter()
            .all(|r| r == "GET /v1/host/info" || r == "GET /v1/me"),
        "{routes:?}"
    );
}

#[test]
fn task_references_other_than_keys_and_ids_are_refused_before_sending() {
    let server = FakeServer::tcp(routes(&[]));
    for args in [
        &["task", "show", ".."][..],
        &["task", "show", "."],
        &["task", "move", "../me", "review"],
        &["task", "plan", "PAP-1/.."],
        &["claim", "%2e%2e"],
        &["report", "..", "--review"],
        &["comment", ".", "hi"],
        &["ask", "@sam", "x", "--task", ".."],
    ] {
        let (code, _, err) = run_on(&server, args, "");
        assert_eq!(code, 2, "{args:?}");
        assert!(err.contains("not a task key"), "{err}");
    }
    assert!(server.requests().is_empty());
}

#[test]
fn text_output_strips_control_characters_and_json_keeps_them() {
    let evil_title = "Fix \u{1b}]0;owned\u{7}the \u{1b}[31mbug\u{1b}[0m\u{202e}txt.exe";
    let mut hostile = task(TASK1, "PAP-1", "in_progress");
    hostile["title"] = json!(evil_title);
    hostile["description"] = json!("Line one\u{1b}[2J\nLine two\u{2066}");
    hostile["labels"] = json!(["a\u{1b}[1mb"]);
    hostile["subtasks"][0]["text"] = json!("Out\u{9b}31mline");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let answered = json!({
        "id": ASK1, "kind": "question", "from": WRITER, "to": SAM, "task": TASK1,
        "title": evil_title, "body": "", "options": ["Y\u{1b}[5mes"], "receipts": [],
        "state": "answered",
        // A server-sent index that `+ 1` would overflow.
        "answer": {"by": SAM, "option": u64::MAX, "text": "ok\u{1b}[8m", "at": now},
        "created": now
    });
    let server = FakeServer::tcp(routes(&[
        ("GET /v1/members", 200, members()),
        ("GET /v1/tasks/PAP-1", 200, hostile.clone()),
        ("GET /v1/tasks", 200, json!([hostile.clone()])),
        ("GET /v1/asks", 200, json!([answered])),
        (
            "POST /v1/tasks/PAP-1/move",
            409,
            api_error("conflict", "No \u{1b}[31mway\u{202e}"),
        ),
    ]));
    for args in [
        &["task", "show", "PAP-1"][..],
        &["task", "list"],
        &["check"],
    ] {
        let (code, out, err) = run_on(&server, args, "");
        assert_eq!((code, err.as_str()), (0, ""), "{args:?}");
        assert!(
            !out.contains(['\u{1b}', '\u{7}', '\u{9b}', '\u{202e}', '\u{2066}']),
            "{args:?}: {out:?}"
        );
        assert!(out.contains("Fix ]0;ownedthe [31mbug[0mtxt.exe"), "{out:?}");
    }
    let (_, out, _) = run_on(&server, &["check"], "");
    assert!(out.contains("[1) Y[5mes]"), "{out}");
    assert!(
        out.contains(&format!("\u{2192} {}) ; \"ok[8m\"", u64::MAX)),
        "{out}"
    );

    let (code, _, err) = run_on(&server, &["task", "move", "PAP-1", "review"], "");
    assert_eq!(code, 4);
    assert_eq!(err, "pitcrew: No [31mway\n");

    let (code, out, _) = run_on(&server, &["--json", "task", "show", "PAP-1"], "");
    assert_eq!(code, 0);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), hostile);
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn verbs_work_over_a_private_socket() {
        let (server, socket) = FakeServer::unix(
            routes(&[
                ("GET /v1/me", 200, writer()),
                ("GET /v1/members", 200, members()),
            ]),
            0o700,
        );
        for path in [socket.clone(), socket.parent().unwrap().to_path_buf()] {
            let (code, out, err) = run(
                &["whoami"],
                &[
                    ("PITCREW_SOCKET", path.to_str().unwrap()),
                    ("PITCREW_TOKEN", TOKEN),
                ],
                "",
            );
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(out, "@writer (Writer), an agent of @sam\n");
        }
        let me = server.api_requests().into_iter().next().unwrap();
        assert_eq!(me.header("host"), Some("localhost"));
        assert_eq!(me.header("authorization"), Some(bearer().as_str()));
    }

    #[test]
    fn a_socket_in_an_open_directory_never_gets_the_token() {
        let (server, socket) = FakeServer::unix(routes(&[("GET /v1/me", 200, writer())]), 0o755);
        let (code, _, err) = run(
            &["whoami"],
            &[
                ("PITCREW_SOCKET", socket.to_str().unwrap()),
                ("PITCREW_TOKEN", TOKEN),
            ],
            "",
        );
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("not sending the token"), "{err}");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(server.requests().is_empty());
    }

    #[test]
    fn a_token_file_open_to_others_is_refused() {
        let (server, socket) = FakeServer::unix(
            routes(&[
                ("GET /v1/me", 200, writer()),
                ("GET /v1/members", 200, members()),
            ]),
            0o700,
        );
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("token");
        std::fs::write(&file, format!("{TOKEN}\n")).unwrap();
        let env = [
            ("PITCREW_SOCKET", socket.to_str().unwrap()),
            ("PITCREW_TOKEN_FILE", file.to_str().unwrap()),
        ];

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let (code, _, err) = run(&["whoami"], &env, "");
        assert_eq!(code, 1);
        assert!(err.contains("chmod 600"), "{err}");
        assert!(server.requests().is_empty());

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (code, _, err) = run(&["whoami"], &env, "");
        assert_eq!((code, err.as_str()), (0, ""));
        let me = server.api_requests().into_iter().next().unwrap();
        assert_eq!(me.header("authorization"), Some(bearer().as_str()));
    }
}
