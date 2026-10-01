//! Acceptance: after seeding the demo workspace, the routes return the demo's data, as the mock
//! hub does, compared structurally (every id, key and field).

mod common;

use common::{
    PAPER, PARSERS, SAM, SUBMISSION, TOOLING, WRITER, agent, app, call, demo, expect, get, member,
    person, seeded,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::EventId;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// @office, the back office's agent.
const OFFICE: &str = "01JB000000000000000MEM0006";

fn value<T: serde::Serialize>(t: &T) -> Value {
    serde_json::to_value(t).expect("serializes")
}

#[tokio::test]
async fn seeded_lists_equal_the_demo() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let demo = demo();
    let sam = person(SAM);

    let lists = [
        ("/v1/machines", value(&demo.machines)),
        ("/v1/members", value(&demo.members)),
        ("/v1/personas", value(&demo.personas)),
        ("/v1/teams", value(&demo.teams)),
        ("/v1/projects", value(&demo.projects)),
        ("/v1/workstreams", value(&demo.workstreams)),
        ("/v1/tasks", value(&demo.tasks)),
        ("/v1/asks", value(&demo.asks)),
        ("/v1/sessions", value(&demo.sessions)),
    ];
    for (path, expected) in lists {
        let got = get(&app, sam, path).await;
        expect(&got, 200);
        assert_eq!(got.1, expected, "{path}");
    }

    for project in &demo.projects {
        let got = get(&app, sam, &format!("/v1/projects/{}", project.id.0)).await;
        expect(&got, 200);
        assert_eq!(got.1, value(project));
    }
    for workstream in &demo.workstreams {
        let got = get(&app, sam, &format!("/v1/workstreams/{}", workstream.id.0)).await;
        expect(&got, 200);
        assert_eq!(got.1, value(workstream));
    }
    for task in &demo.tasks {
        for path in [
            format!("/v1/tasks/{}", task.key),
            format!("/v1/tasks/{}", task.id.0),
            format!("/v1/tasks/{}", task.id),
        ] {
            let got = get(&app, sam, &path).await;
            expect(&got, 200);
            assert_eq!(got.1, value(task), "{path}");
        }
    }
    // The internal lists without routes match too.
    assert_eq!(
        value(&work.dispatches().expect("dispatches")),
        value(&demo.dispatches)
    );
    assert_eq!(
        value(&work.sessions(&Default::default()).expect("sessions")),
        value(&demo.sessions)
    );
}

#[tokio::test]
async fn seeded_briefs_equal_the_demo_with_its_pending_proposal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let got = get(&app, person(SAM), "/v1/briefs").await;
    expect(&got, 200);

    // Every field of every brief, `next`, receipts and source included. The demo's last event
    // proposes a new brief for the paper, and no brief_accepted for it follows, so the paper's
    // brief has that proposal pending (as the mock hub answers).
    let demo = demo();
    let mut expected = value(&demo.briefs);
    let last = demo.events.last().expect("events");
    let pitcrew_protocol::events::EventBody::BriefProposed {
        target,
        text,
        next,
        receipts,
    } = &last.body
    else {
        panic!("the demo's last event is a proposal");
    };
    let mut pending = 0;
    for brief in expected.as_array_mut().expect("array") {
        assert!(
            brief["next"].is_string(),
            "every demo brief has a next step"
        );
        if brief["target"] == value(target) {
            brief["proposal"] = json!({ "text": text, "receipts": receipts, "at": last.at });
            assert!(next.is_none());
            pending += 1;
        }
    }
    assert_eq!(pending, 1);
    assert_eq!(got.1, expected);
}

#[tokio::test]
async fn seeded_filters_answer_like_the_mock_hub() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let keys = |v: &Value| -> Vec<String> {
        v.as_array()
            .expect("array")
            .iter()
            .map(|t| t["key"].as_str().expect("key").to_owned())
            .collect()
    };
    let sam = person(SAM);
    // The same filters and answers as apps/mock-hub/test/http.test.ts.
    for (query, expected) in [
        (format!("?project={TOOLING}"), vec!["TL-1", "TL-2", "TL-3"]),
        (
            format!("?workstream={SUBMISSION}"),
            vec!["PAP-1", "PAP-2", "PAP-3", "PAP-7"],
        ),
        (
            format!("?assignee={WRITER}"),
            vec!["PAP-1", "PAP-2", "PAP-3"],
        ),
        (
            "?status=todo&status=backlog".to_owned(),
            vec!["PAP-2", "PAP-5", "PAP-6", "TL-2"],
        ),
        (
            format!("?project={PAPER}&status=in_progress"),
            vec!["PAP-1", "PAP-4"],
        ),
        (
            format!("?project=prj_{TOOLING}&workstream={PARSERS}&status="),
            vec!["TL-1", "TL-2", "TL-3"],
        ),
    ] {
        let got = get(&app, sam, &format!("/v1/tasks{query}")).await;
        expect(&got, 200);
        assert_eq!(keys(&got.1), expected, "{query}");
    }
    // Agents read the whole workspace.
    let all = get(&app, agent(WRITER), "/v1/tasks").await;
    expect(&all, 200);
    assert_eq!(all.1.as_array().map(Vec::len), Some(10));

    let inbox = get(&app, sam, &format!("/v1/asks?to={SAM}&state=open")).await;
    expect(&inbox, 200);
    assert_eq!(inbox.1.as_array().map(Vec::len), Some(3));
    let workstreams = get(&app, sam, &format!("/v1/workstreams?project={TOOLING}")).await;
    assert_eq!(workstreams.1[0]["id"], json!(PARSERS));
    assert_eq!(workstreams.1.as_array().map(Vec::len), Some(1));

    let me = get(&app, agent(WRITER), "/v1/me").await;
    expect(&me, 200);
    assert_eq!(me.1["handle"], "@writer");

    for bad in [
        "/v1/tasks?status=finished",
        "/v1/tasks?project=nope",
        "/v1/asks?state=closed",
    ] {
        expect(&get(&app, sam, bad).await, 400);
    }
}

#[tokio::test]
async fn seeding_twice_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let rev = work.store().latest_rev().expect("rev");
    let err = work.seed(&demo()).expect_err("second seed");
    assert_eq!(err.code(), pitcrew_protocol::api::ErrorCode::Conflict);
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
}

/// Parity with the real mock hub, not just the fixture it serves. Dump the mock's answers with
/// `tests/dump-mock-hub.mjs` (it starts the mock in-process and writes each GET route's answer to
/// `<name>.json`, plus an `index.json` of `{ "name": "/v1/..." }`):
///
/// ```text
/// node crates/hub-work/tests/dump-mock-hub.mjs <folder>
/// PITCREW_MOCK_DUMP=<folder> cargo test -p pitcrew-hub-work --test seed -- --ignored
/// ```
///
/// Every answer must match, except the workspace's `rev` (the two logs hold different events: the
/// mock's is the demo's slice, ours the whole import).
#[tokio::test]
#[ignore = "needs a dump of the mock hub's answers in PITCREW_MOCK_DUMP"]
async fn seeded_routes_answer_like_the_running_mock_hub() {
    let dump = mock_dump();
    let index: BTreeMap<String, String> =
        serde_json::from_value(read_dump(&dump, "index.json")).expect("index");
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    for (name, path) in &index {
        let mut mock = read_dump(&dump, &format!("{name}.json"));
        let mut got = get(&app, person(SAM), path).await;
        expect(&got, 200);
        if name == "workspace" {
            assert!(got.1["rev"].as_u64().is_some_and(|rev| rev > 0));
            for answer in [&mut got.1, &mut mock] {
                answer.as_object_mut().expect("object").remove("rev");
            }
        }
        assert_eq!(got.1, mock, "{path}");
        println!("same as the mock hub: GET {path}");
    }
}

fn mock_dump() -> PathBuf {
    PathBuf::from(std::env::var("PITCREW_MOCK_DUMP").expect("PITCREW_MOCK_DUMP"))
}

fn read_dump(dump: &Path, name: &str) -> Value {
    let text = std::fs::read_to_string(dump.join(name)).expect("dump file");
    serde_json::from_str(&text).expect("dump JSON")
}

/// `{{NAME}}` in a request replaced by the id bound to `NAME`.
fn bind_ids(value: &Value, ids: &BTreeMap<String, String>) -> Value {
    let mut text = value.to_string();
    for (name, id) in ids {
        text = text.replace(&format!("{{{{{name}}}}}"), id);
    }
    serde_json::from_str(&text).expect("JSON")
}

/// An answer with each bound id written `{{NAME}}`, and without the times a brief route reports
/// for events each hub stamps with its own clock (a brief's `updated`, a proposal's `at`).
fn normalized(answer: &Value, ids: &BTreeMap<String, String>, briefs: bool) -> Value {
    let mut text = answer.to_string();
    for (name, id) in ids {
        text = text.replace(id, &format!("{{{{{name}}}}}"));
    }
    let mut value: Value = serde_json::from_str(&text).expect("JSON");
    if briefs {
        let list = match &mut value {
            Value::Array(list) => list.iter_mut().collect::<Vec<_>>(),
            one => vec![one],
        };
        for brief in list {
            let brief = brief.as_object_mut().expect("brief");
            brief.remove("updated");
            if let Some(Value::Object(proposal)) = brief.get_mut("proposal") {
                proposal.remove("at");
            }
        }
    }
    value
}

/// Parity for the write routes: `PATCH /v1/tasks`, `POST /v1/projects`, `POST /v1/workstreams`
/// and the briefs' next steps and proposals. `tests/dump-mock-hub.mjs` makes the same requests on
/// the mock hub, in order, and writes them with its answers to `writes.json`; this replays them on
/// a seeded hub. Every status must match; for errors the `code` (messages are each hub's own), and
/// for successes the whole body, with created ids matched up and brief times left out.
#[tokio::test]
#[ignore = "needs a dump of the mock hub's answers in PITCREW_MOCK_DUMP"]
async fn writes_answer_like_the_running_mock_hub() {
    let dump = mock_dump();
    let steps = read_dump(&dump, "writes.json");
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let mut ours = BTreeMap::new();
    let mut theirs = BTreeMap::new();
    let mut compared = 0;
    for step in steps.as_array().expect("steps") {
        let name = step["name"].as_str().expect("name");
        if let Some(proposal) = step.get("propose") {
            let data = bind_ids(proposal, &ours);
            let body: EventBody =
                serde_json::from_value(json!({ "type": "brief_proposed", "data": data }))
                    .expect("proposal");
            let event = Event {
                id: EventId::new(),
                at: 1_790_900_000_000,
                workspace: work.workspace(),
                author: member(OFFICE),
                on_behalf_of: Some(member(SAM)),
                body,
            };
            work.store().append(&[event]).expect("append");
            continue;
        }
        let method = step["method"].as_str().expect("method");
        let path = bind_ids(&step["path"], &ours);
        let path = path.as_str().expect("path");
        let body = step.get("body").map(|b| bind_ids(b, &ours));
        let caller = match step["token"].as_str() {
            Some("agent") => agent(WRITER),
            _ => person(SAM),
        };
        let got = call(&app, Some(caller), method, path, body).await;
        let mock = &step["response"];
        assert_eq!(
            Some(u64::from(got.0)),
            step["status"].as_u64(),
            "{name}: {method} {path}: ours {} / the mock's {mock}",
            got.1
        );
        if let Some(bind) = step["bind"].as_str() {
            let id = |v: &Value| v["id"].as_str().expect("created id").to_owned();
            ours.insert(bind.to_owned(), id(&got.1));
            theirs.insert(bind.to_owned(), id(mock));
        }
        if got.0 >= 400 {
            assert_eq!(got.1["code"], mock["code"], "{name}: {}", got.1);
        } else {
            let briefs = path.starts_with("/v1/briefs");
            assert_eq!(
                normalized(&got.1, &ours, briefs),
                normalized(mock, &theirs, briefs),
                "{name}: {method} {path}"
            );
        }
        compared += 1;
        println!("same as the mock hub: {name} ({method} {path} → {})", got.0);
    }
    assert!(compared > 100, "only {compared} requests compared");
}
