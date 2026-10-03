//! The back office and the activity index in the real daemon: `@office` acting on a finished
//! dispatch, an action the hub refuses, a restart that runs a range again, and the `project=` and
//! `workstream=` activity filters.
//!
//! A dispatch's end is appended to the store from the test, as another writer would (the hub
//! itself finishes a dispatch as succeeded together with the agent's move to review, so the
//! office's rule only meets a task still in progress after such an end); the daemon looks at it
//! with its next append (a comment through the API here) or at its next start.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, append_to_store, id};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::model::{DispatchOutcome, Mover, TaskStatus};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

fn state_dir() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    (tmp, state)
}

/// Waits until `holds`, at most [`WAIT`].
fn eventually(what: &str, mut holds: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !holds() {
        assert!(Instant::now() < deadline, "never: {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn status(daemon: &Daemon, token: &str, task: &str) -> String {
    let reply = daemon.get(&format!("/v1/tasks/{task}"), Some(token));
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()["status"].as_str().unwrap().to_owned()
}

/// A dispatch reporting success.
fn finished(dispatch: &str) -> EventBody {
    EventBody::DispatchFinished {
        dispatch: dispatch.parse().unwrap(),
        outcome: DispatchOutcome::Succeeded,
        summary: Some("Done; ready for review.".into()),
    }
}

/// An append through the API: the daemon looks at everything up to it.
fn nudge(daemon: &Daemon, token: &str) {
    let reply = daemon.post(
        "/v1/tasks/PAP-2/comments",
        Some(token),
        &json!({ "text": "Starting on the figure.", "mentions": [] }),
    );
    assert_eq!(reply.status, 201, "{}", reply.body);
}

fn by_office(events: &[Value]) -> Vec<&Value> {
    events
        .iter()
        .filter(|e| e["author"] == id::OFFICE)
        .collect()
}

#[test]
fn with_demo_a_finished_dispatch_moves_its_task_to_review_as_office() {
    let (_tmp, state) = state_dir();
    let daemon = Daemon::start(&state, &["--demo"]);
    let device = daemon.device_token();
    let agent = daemon.agent_token();

    // The back office is the demo's @office, an agent owned by @sam.
    let members = daemon.get("/v1/members", Some(&device)).json();
    let office = members
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["handle"] == "@office")
        .unwrap();
    assert_eq!(office["id"], id::OFFICE);
    assert_eq!(office["kind"], "agent");
    assert_eq!(office["owner"], id::SAM);
    assert!(daemon.stderr().contains("the back office acts as @office"));

    // PAP-1's dispatch finishes.
    assert_eq!(status(&daemon, &device, "PAP-1"), "in_progress");
    append_to_store(&state, id::WRITER, Some(id::SAM), vec![finished(id::DSP1)]);
    nudge(&daemon, &device);
    eventually("PAP-1 moves to review", || {
        status(&daemon, &device, "PAP-1") == "review"
    });

    // The move is the back office's: @office, on behalf of @sam, as the back office.
    let pap1 = daemon.events_matching(&format!("&task={}", id::PAP1), &device);
    let to_review: Vec<&Value> = pap1
        .iter()
        .filter(|e| e["body"]["type"] == "task_moved" && e["body"]["data"]["to"] == "review")
        .collect();
    assert_eq!(to_review.len(), 1, "{to_review:?}");
    let moved = to_review[0];
    assert_eq!(moved["author"], id::OFFICE);
    assert_eq!(moved["on_behalf_of"], id::SAM);
    assert_eq!(
        moved["body"]["data"],
        json!({
            "task": id::PAP1,
            "from": "in_progress",
            "to": "review",
            "mover": { "kind": "back_office", "accept_auto": false },
        })
    );
    // The task's activity includes its dispatch's end (through the index), before the move.
    let ended = pap1
        .iter()
        .position(|e| e["body"]["type"] == "dispatch_finished")
        .unwrap();
    let moved_at = pap1
        .iter()
        .position(|e| e["author"] == id::OFFICE && e["body"]["type"] == "task_moved")
        .unwrap();
    assert!(ended < moved_at);
    // Logged once the run that applied it returns.
    daemon.wait_for_log("rule=dispatch_to_review", WAIT);
    let logs = daemon.stderr();

    // Everything the back office appended is @office's, for @sam.
    let all = daemon.all_events(&device);
    let office_events = by_office(&all);
    assert!(!office_events.is_empty());
    for event in office_events {
        assert_eq!(event["on_behalf_of"], id::SAM, "{event}");
    }

    // The office has no token: no token file for it, and none of the tokens is in the logs.
    let mut files: Vec<String> = std::fs::read_dir(&state)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".token"))
        .collect();
    files.sort();
    assert_eq!(files, ["demo-agent.token", "device.token"]);
    for token in [&device, &agent] {
        assert!(!logs.contains(&token[4..]), "a token leaked into the logs");
    }
}

/// With the demo's rules nothing the office's own "never" list forbids is ever emitted (each rule
/// checks first; those refusals are tested in `pitcrew-office` and `pitcrew-hub-work`). What the
/// daemon must handle is the hub's refusal of an action the office emitted on an out-of-date view:
/// it is refused, logged, and appends nothing.
#[test]
fn an_action_the_hub_refuses_is_logged_and_appends_nothing() {
    let (_tmp, state) = state_dir();
    let daemon = Daemon::start(&state, &["--demo"]);
    let device = daemon.device_token();
    assert_eq!(status(&daemon, &device, "PAP-3"), "review");

    // A racing writer's stale move: the hub ignores it (PAP-3 is in review, not todo), but the
    // office's view of PAP-3 follows it to in progress...
    let stale = EventBody::TaskMoved {
        task: id::PAP3.parse().unwrap(),
        from: TaskStatus::Todo,
        to: TaskStatus::InProgress,
        mover: Mover::Person,
    };
    append_to_store(&state, id::SAM, None, vec![stale]);
    // ...so when PAP-3's dispatch reports success, the office asks for in progress → review.
    let done = append_to_store(&state, id::WRITER, Some(id::SAM), vec![finished(id::DSP4)]);
    nudge(&daemon, &device);

    // The refusal of the action the office took on that revision, by the hub's move rules.
    daemon.wait_for_log("the hub refused a back-office action", WAIT);
    daemon.settle(&device);
    let logs = daemon.stderr();
    let at = format!(" rev={} ", done.to_rev);
    let refusals: Vec<&str> = logs
        .lines()
        .filter(|l| l.contains("the hub refused a back-office action") && l.contains(&at))
        .collect();
    assert_eq!(refusals.len(), 1, "{logs}");
    let refusal = refusals[0];
    assert!(refusal.contains("WARN"), "{refusal}");
    assert!(refusal.contains("rule=dispatch_to_review"), "{refusal}");
    assert!(refusal.contains("Conflict"), "{refusal}");
    // A refusal by the rules is final: the range is not run again.
    assert!(!logs.contains("runs these revisions again later"), "{logs}");

    // PAP-3 stays in review, and the office appended nothing about it but what it may.
    assert_eq!(status(&daemon, &device, "PAP-3"), "review");
    let pap3 = daemon.events_matching(&format!("&task={}", id::PAP3), &device);
    assert!(
        by_office(&pap3)
            .iter()
            .all(|e| e["body"]["type"] != "task_moved"),
        "{pap3:?}"
    );
}

#[test]
fn a_restart_does_not_duplicate_office_actions() {
    let (_tmp, state) = state_dir();
    let mut daemon = Daemon::start(&state, &["--demo"]);
    let device = daemon.device_token();
    let done = append_to_store(&state, id::WRITER, Some(id::SAM), vec![finished(id::DSP1)]);
    nudge(&daemon, &device);
    eventually("PAP-1 moves to review", || {
        status(&daemon, &device, "PAP-1") == "review"
    });
    let latest = daemon.settle(&device);
    let office_events = by_office(&daemon.all_events(&device)).len();
    // Progress is saved within a second of a run, without waiting for a stop.
    let progress = state.join("office.json");
    let read =
        || -> Value { serde_json::from_str(&std::fs::read_to_string(&progress).unwrap()).unwrap() };
    eventually("office.json catches up", || read()["done"] == latest);
    daemon.stop();
    drop(daemon);
    let saved = read();
    assert_eq!(saved["done"], latest);

    // A plain restart has nothing to run again.
    let mut daemon = Daemon::start(&state, &[]);
    assert_eq!(daemon.latest_rev(&device), latest);
    daemon.stop();
    drop(daemon);

    // As if it had crashed before saving: the next start runs the range with the finished
    // dispatch again, and the office's actions are already in the log.
    std::fs::write(
        &progress,
        json!({ "log": saved["log"], "done": done.from_rev - 1 }).to_string(),
    )
    .unwrap();
    let daemon = Daemon::start(&state, &[]);
    daemon.wait_for_log(
        "the back office had applied these actions before; nothing was appended again",
        WAIT,
    );
    assert_eq!(daemon.settle(&device), latest, "appended again");
    let all = daemon.all_events(&device);
    assert_eq!(by_office(&all).len(), office_events);
    let moves = all
        .iter()
        .filter(|e| e["author"] == id::OFFICE && e["body"]["type"] == "task_moved")
        .count();
    assert_eq!(moves, 1);
    assert_eq!(status(&daemon, &device, "PAP-1"), "review");
}

/// What was appended while the daemon was stopped (here: in place of the runner link, a dispatch
/// that finished) is looked at on the next start, and acted on then.
#[test]
fn an_action_missed_while_stopped_is_applied_at_the_next_start() {
    let (_tmp, state) = state_dir();
    let mut daemon = Daemon::start(&state, &["--demo"]);
    let device = daemon.device_token();
    daemon.settle(&device);
    daemon.stop();
    drop(daemon);

    let done = append_to_store(&state, id::WRITER, Some(id::SAM), vec![finished(id::DSP1)]);
    let daemon = Daemon::start(&state, &[]);
    eventually("PAP-1 moves to review", || {
        status(&daemon, &device, "PAP-1") == "review"
    });
    daemon.wait_for_log("rule=dispatch_to_review", WAIT);
    let after = daemon.events_matching(&format!("&task={}", id::PAP1), &device);
    let moved: Vec<&Value> = after
        .iter()
        .filter(|e| e["author"] == id::OFFICE && e["body"]["type"] == "task_moved")
        .collect();
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert_eq!(moved[0]["body"]["data"]["to"], "review");
    // The office looked at that revision as part of this start's first run.
    let started = daemon
        .stderr()
        .lines()
        .find(|l| l.contains("the back office acts as @office"))
        .unwrap()
        .to_owned();
    let from: u64 = started
        .split_whitespace()
        .find_map(|w| w.strip_prefix("from="))
        .unwrap()
        .parse()
        .unwrap();
    assert!(from <= done.from_rev, "{started}");
}

/// Where the office starts is saved before the listener binds: a `--demo` start that seeds and
/// then cannot listen still gets its office pass over the seed on the next start.
#[test]
fn a_demo_that_cannot_listen_gets_its_office_pass_on_the_next_start() {
    let (_tmp, state) = state_dir();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let refused =
        Daemon::try_start_on(&state, &format!("tcp:127.0.0.1:{port}"), &["--demo"]).unwrap_err();
    assert!(!refused.status.success());
    assert!(
        refused.stderr.contains("cannot listen"),
        "{}",
        refused.stderr
    );
    assert!(
        refused.stderr.contains("from=1 "),
        "this start began at the seed:\n{}",
        refused.stderr
    );
    drop(taken);

    let daemon = Daemon::start(&state, &[]);
    let device = daemon.device_token();
    let started = daemon.stderr();
    assert!(
        started.contains("the back office acts as @office") && started.contains("from=1 "),
        "{started}"
    );
    daemon.settle(&device);
}

/// Every id an event about `filter=scope` may name: the project or workstream, its workstreams,
/// tasks, the sessions linked to them, and their dispatches and asks. In the demo nothing is
/// relinked, so "about it then" is "about it now".
fn related_ids(
    daemon: &Daemon,
    token: &str,
    all: &[Value],
    filter: &str,
    scope: &str,
) -> BTreeSet<String> {
    let get = |path: &str| -> Vec<Value> {
        let reply = daemon.get(path, Some(token));
        assert_eq!(reply.status, 200, "{path}: {}", reply.body);
        reply.json().as_array().unwrap().clone()
    };
    let ids = |values: &[Value]| -> BTreeSet<String> {
        values
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let mut out = BTreeSet::from([scope.to_owned()]);
    let workstreams = if filter == "project" {
        ids(&get(&format!("/v1/workstreams?project={scope}")))
    } else {
        BTreeSet::from([scope.to_owned()])
    };
    let tasks = ids(&get(&format!("/v1/tasks?{filter}={scope}")));
    let has = |set: &BTreeSet<String>, v: &Value| v.as_str().is_some_and(|s| set.contains(s));
    let sessions: BTreeSet<String> = get("/v1/sessions")
        .iter()
        .filter(|s| has(&tasks, &s["task"]) || has(&workstreams, &s["workstream"]))
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .collect();
    let dispatches: BTreeSet<String> = all
        .iter()
        .filter(|e| e["body"]["type"] == "dispatch_started")
        .map(|e| &e["body"]["data"]["dispatch"])
        .filter(|d| has(&tasks, &d["task"]))
        .map(|d| d["id"].as_str().unwrap().to_owned())
        .collect();
    let asks: BTreeSet<String> = get("/v1/asks")
        .iter()
        .filter(|a| has(&tasks, &a["task"]) || has(&sessions, &a["session"]))
        .map(|a| a["id"].as_str().unwrap().to_owned())
        .collect();
    for set in [workstreams, tasks, sessions, dispatches, asks] {
        out.extend(set);
    }
    out
}

/// Whether any string in `value`, at any depth, is one of `ids`.
fn names_any(value: &Value, ids: &BTreeSet<String>) -> bool {
    match value {
        Value::String(s) => ids.contains(s),
        Value::Array(items) => items.iter().any(|v| names_any(v, ids)),
        Value::Object(fields) => fields.values().any(|v| names_any(v, ids)),
        _ => false,
    }
}

fn event_ids(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn activity_filters_by_project_and_workstream_through_the_index() {
    let (_tmp, state) = state_dir();
    let daemon = Daemon::start(&state, &["--demo"]);
    let device = daemon.device_token();
    let all = daemon.all_events(&device);

    for (filter, scope) in [
        ("project", id::TOOLING),
        ("project", id::PAPER),
        ("workstream", id::PARSERS),
        ("workstream", id::SUBMISSION),
    ] {
        let reply = daemon.get(&format!("/v1/events?{filter}={scope}"), Some(&device));
        assert_eq!(reply.status, 200, "{filter}: {}", reply.body);
        let related = related_ids(&daemon, &device, &all, filter, scope);
        let expected: Vec<&Value> = all
            .iter()
            .filter(|e| names_any(&e["body"], &related))
            .collect();
        let expected: Vec<String> = expected
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_owned())
            .collect();
        // Paged one event at a time, too: every match, in order, once.
        for limit in [500, 1] {
            let query = format!("&{filter}={scope}");
            let got = event_ids(&daemon.events_paged(&query, limit, &device));
            assert!(!got.is_empty(), "{query}");
            assert_eq!(got, expected, "{query} by {limit}");
        }
    }

    // The demo slice's events about Tooling, and none about the paper.
    let tooling = event_ids(&daemon.events_matching(&format!("&project={}", id::TOOLING), &device));
    for suffix in ["EVT0008", "EVT0013", "EVT0014"] {
        assert!(tooling.iter().any(|i| i.ends_with(suffix)), "{suffix}");
    }
    assert!(!tooling.iter().any(|i| i.ends_with("EVT0010")));

    // Filters combine: Tooling's events about TL-1.
    let both = event_ids(&daemon.events_matching(
        &format!("&project={}&task={}", id::TOOLING, id::TL1),
        &device,
    ));
    let tl1 = event_ids(&daemon.events_matching(&format!("&task={}", id::TL1), &device));
    assert!(!both.is_empty());
    assert_eq!(both, tl1, "TL-1 is in Tooling");
    let none = daemon.get(
        &format!("/v1/events?project={}&task={}", id::PAPER, id::TL1),
        Some(&device),
    );
    assert_eq!(none.status, 200);
    assert_eq!(none.json()["events"], json!([]));

    // An unknown project has no events; a malformed one is a 400; agents may not page activity.
    let unknown = daemon.get(
        "/v1/events?project=01JB000000000000000PRJ0099",
        Some(&device),
    );
    assert_eq!(unknown.status, 200);
    assert_eq!(
        unknown.json(),
        json!({ "events": [], "from_rev": 0, "to_rev": 0, "at_start": true })
    );
    let malformed = daemon.get("/v1/events?workstream=nope", Some(&device));
    assert_eq!(malformed.status, 400);
    assert_eq!(malformed.code(), "invalid");
    let agent = daemon.get(
        &format!("/v1/events?project={}", id::TOOLING),
        Some(&daemon.agent_token()),
    );
    assert_eq!(agent.status, 403);
}
