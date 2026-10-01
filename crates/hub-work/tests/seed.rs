//! Acceptance: after seeding the demo workspace, the routes return the demo's data, as the mock
//! hub does, compared structurally (every id, key and field).

mod common;

use common::{
    PAPER, PARSERS, SAM, SUBMISSION, TOOLING, WRITER, agent, app, demo, expect, get, person, seeded,
};
use serde_json::{Value, json};

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
async fn seeded_briefs_equal_the_demo_except_next() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let got = get(&app, person(SAM), "/v1/briefs").await;
    expect(&got, 200);

    // The briefs projection does not read `next` from `brief_accepted` yet (see "Contract gaps" in
    // the README). Every other field, receipts and source included, matches.
    let mut expected = value(&demo().briefs);
    let mut dropped = 0;
    for brief in expected.as_array_mut().expect("array") {
        if brief
            .as_object_mut()
            .expect("object")
            .remove("next")
            .is_some()
        {
            dropped += 1;
        }
    }
    assert_eq!(dropped, 4, "every demo brief has a next step");
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
/// Every answer must match, except briefs' `next` and `proposal` (not implemented yet; see
/// "Contract gaps" in the README) and the workspace's `rev`
/// (the two logs hold different events: the mock's is the demo's slice, ours the whole import).
#[tokio::test]
#[ignore = "needs a dump of the mock hub's answers in PITCREW_MOCK_DUMP"]
async fn seeded_routes_answer_like_the_running_mock_hub() {
    let dump =
        std::path::PathBuf::from(std::env::var("PITCREW_MOCK_DUMP").expect("PITCREW_MOCK_DUMP"));
    let read = |name: &str| -> Value {
        let text = std::fs::read_to_string(dump.join(name)).expect("dump file");
        serde_json::from_str(&text).expect("dump JSON")
    };
    let index: std::collections::BTreeMap<String, String> =
        serde_json::from_value(read("index.json")).expect("index");
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    for (name, path) in &index {
        let mut mock = read(&format!("{name}.json"));
        let mut got = get(&app, person(SAM), path).await;
        expect(&got, 200);
        if name == "briefs" {
            for brief in mock.as_array_mut().expect("array") {
                let brief = brief.as_object_mut().expect("object");
                brief.remove("next");
                brief.remove("proposal");
            }
        }
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
