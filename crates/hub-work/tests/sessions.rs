//! `GET /v1/sessions`, `GET /v1/sessions/{id}` and `GET /v1/workspace`, through the routes.

mod common;

use common::{
    PARSERS, SAM, SEED_RUNS, SUBMISSION, WRITER, agent, app, demo, expect, get, member, person,
    seeded,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::EventId;
use pitcrew_protocol::model::{LinkBasis, SessionState};
use serde_json::{Value, json};

const LAPTOP: &str = "01JB000000000000000MCH0001";
const CLUSTER: &str = "01JB000000000000000MCH0002";
const PAP1: &str = "01JB000000000000000TSK0001";
const SES1: &str = "01JB000000000000000SES0001";
const SES2: &str = "01JB000000000000000SES0002";

fn ids(v: &Value) -> Vec<String> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|s| s["id"].as_str().expect("id").to_owned())
        .collect()
}

fn ses(n: u8) -> String {
    format!("01JB000000000000000SES000{n}")
}

#[tokio::test]
async fn sessions_list_with_every_filter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let sam = person(SAM);

    let all = get(&app, sam, "/v1/sessions").await;
    expect(&all, 200);
    assert_eq!(
        all.1,
        serde_json::to_value(&demo().sessions).expect("json"),
        "the demo's sessions, in order"
    );

    for (query, expected) in [
        (
            format!("?machine={LAPTOP}"),
            vec![ses(1), ses(3), ses(4), ses(6)],
        ),
        (format!("?machine={CLUSTER}"), vec![ses(2)]),
        (format!("?workstream={SUBMISSION}"), vec![ses(1), ses(6)]),
        (format!("?workstream={PARSERS}"), vec![ses(3), ses(4)]),
        (format!("?task={PAP1}"), vec![ses(1)]),
        (format!("?task=tsk_{PAP1}"), vec![ses(1)]),
        ("?state=working".to_owned(), vec![ses(1), ses(2)]),
        (
            "?state=working&state=idle".to_owned(),
            vec![ses(1), ses(2), ses(4)],
        ),
        (format!("?machine={LAPTOP}&state=ended"), vec![ses(6)]),
        (format!("?workstream={SEED_RUNS}&state=idle"), vec![]),
        ("?state=".to_owned(), (1..=6).map(ses).collect()),
    ] {
        let got = get(&app, sam, &format!("/v1/sessions{query}")).await;
        expect(&got, 200);
        assert_eq!(ids(&got.1), expected, "{query}");
    }

    for bad in [
        "/v1/sessions?state=sleeping",
        "/v1/sessions?machine=nope",
        "/v1/sessions?task=PAP-1",
        "/v1/sessions?workstream=01JB",
    ] {
        expect(&get(&app, sam, bad).await, 400);
    }
}

#[tokio::test]
async fn one_session_by_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let sam = person(SAM);
    for session in &demo().sessions {
        for path in [
            format!("/v1/sessions/{}", session.id.0),
            format!("/v1/sessions/{}", session.id),
        ] {
            let got = get(&app, sam, &path).await;
            expect(&got, 200);
            assert_eq!(
                got.1,
                serde_json::to_value(session).expect("json"),
                "{path}"
            );
        }
    }
    for missing in [
        "/v1/sessions/01JB000000000000000SES0099",
        "/v1/sessions/garbage",
        "/v1/sessions/tsk_01JB000000000000000TSK0001",
    ] {
        expect(&get(&app, sam, missing).await, 404);
    }

    // The projection follows the runner's events.
    let event = Event {
        id: EventId::new(),
        at: 1_790_900_000_000,
        workspace: demo().workspace.id,
        author: member(WRITER),
        on_behalf_of: Some(member(SAM)),
        body: EventBody::SessionStateChanged {
            session: SES2.parse().expect("session"),
            from: SessionState::Working,
            to: SessionState::Waiting,
            status_line: Some("Needs a decision".into()),
        },
    };
    work.store().append(&[event]).expect("append");
    let got = get(&app, sam, &format!("/v1/sessions/{SES2}")).await;
    assert_eq!(got.1["state"], "waiting");
    assert_eq!(got.1["status_line"], "Needs a decision");
    assert_eq!(got.1["last_activity"], 1_790_900_000_000_i64);
}

#[tokio::test]
async fn a_firm_link_survives_a_restatement_and_an_inferred_link() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let sam = person(SAM);
    let mut ses1 = work.session(&SES1.parse().expect("id")).expect("session");
    assert_eq!(ses1.link_basis, Some(LinkBasis::Dispatch));
    let append = |body: EventBody| {
        work.store()
            .append(&[Event {
                id: EventId::new(),
                at: 1_790_900_000_000,
                workspace: demo().workspace.id,
                author: member(WRITER),
                on_behalf_of: Some(member(SAM)),
                body,
            }])
            .expect("append");
    };
    // The runner re-states the session without its link, then infers one from the folder.
    ses1.workstream = None;
    ses1.task = None;
    ses1.link_basis = None;
    ses1.title = Some("Re-stated".into());
    append(EventBody::SessionDiscovered { session: ses1 });
    append(EventBody::SessionLinked {
        session: SES1.parse().expect("id"),
        workstream: Some(PARSERS.parse().expect("ws")),
        task: None,
        basis: LinkBasis::Folder,
    });
    let got = get(&app, sam, &format!("/v1/sessions/{SES1}")).await;
    assert_eq!(got.1["title"], "Re-stated", "the rest is re-stated");
    assert_eq!(got.1["task"], PAP1);
    assert_eq!(got.1["workstream"], SUBMISSION);
    assert_eq!(got.1["link_basis"], "dispatch");
    // A person's link replaces it.
    append(EventBody::SessionLinked {
        session: SES1.parse().expect("id"),
        workstream: Some(PARSERS.parse().expect("ws")),
        task: None,
        basis: LinkBasis::Manual,
    });
    let got = get(&app, sam, &format!("/v1/sessions/{SES1}")).await;
    assert_eq!(got.1["workstream"], PARSERS);
    assert!(got.1.get("task").is_none());
    assert_eq!(got.1["link_basis"], "manual");
    // An unlinked session takes an inferred link.
    let ses5 = "01JB000000000000000SES0005";
    append(EventBody::SessionLinked {
        session: ses5.parse().expect("id"),
        workstream: Some(SEED_RUNS.parse().expect("ws")),
        task: None,
        basis: LinkBasis::Branch,
    });
    let got = get(&app, sam, &format!("/v1/sessions/{ses5}")).await;
    assert_eq!(got.1["link_basis"], "branch");
}

#[tokio::test]
async fn sessions_and_the_workspace_are_for_devices() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let one = format!("/v1/sessions/{SES1}");
    for path in ["/v1/sessions", one.as_str(), "/v1/workspace"] {
        expect(&get(&app, agent(WRITER), path).await, 403);
    }
}

#[tokio::test]
async fn the_workspace_and_the_revision_it_reflects() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    let got = get(&app, person(SAM), "/v1/workspace").await;
    expect(&got, 200);
    let rev = work.store().latest_rev().expect("rev");
    assert_eq!(
        got.1,
        json!({ "workspace": { "id": "01JB000000000000000WSP0001", "name": "Demo Lab" }, "rev": rev })
    );
    // A change moves it on.
    work.move_task(
        &person(SAM),
        &pitcrew_hub_work::TaskRef::parse("PAP-2").expect("key"),
        pitcrew_protocol::model::TaskStatus::InProgress,
    )
    .expect("move");
    let got = get(&app, person(SAM), "/v1/workspace").await;
    assert_eq!(got.1["rev"], rev + 1);
    assert_eq!(work.workspace_at().expect("at").rev, rev + 1);
}
