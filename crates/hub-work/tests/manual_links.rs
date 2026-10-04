//! Manual link validation and the projection's protection against later inference.
mod common;

use common::{PARSERS, SAM, SUBMISSION, WRITER, agent, app, call, expect, get, person, seeded};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::model::LinkBasis;
use serde_json::json;

const SESSION: &str = "01JB000000000000000SES0001";
const TASK: &str = "01JB000000000000000TSK0001";

#[tokio::test]
async fn manual_links_validate_and_emit_and_survive_inference() {
    let dir = tempfile::tempdir().expect("synthetic test data");
    let work = seeded(dir.path());
    let app = app(&work);
    let path = format!("/v1/sessions/{SESSION}/link");
    let post = |caller, body| call(&app, Some(caller), "POST", &path, Some(body));
    for body in [
        json!({}),
        json!({"workstream": "nope"}),
        json!({"task": "nope"}),
        json!({"workstream": PARSERS, "task": TASK}),
    ] {
        expect(&post(person(SAM), body).await, 400);
    }
    expect(&post(agent(WRITER), json!({})).await, 403);
    expect(
        &call(
            &app,
            Some(person(SAM)),
            "POST",
            "/v1/sessions/nope/link",
            Some(json!({})),
        )
        .await,
        404,
    );
    let linked = post(person(SAM), json!({"task": TASK})).await;
    expect(&linked, 200);
    assert_eq!(linked.1["workstream"], SUBMISSION);
    assert_eq!(linked.1["task"], TASK);
    assert_eq!(linked.1["link_basis"], "manual");
    let linked = post(person(SAM), json!({"workstream": PARSERS})).await;
    expect(&linked, 200);
    assert_eq!(linked.1["workstream"], PARSERS);
    assert!(linked.1.get("task").is_none());
    for basis in [LinkBasis::Folder, LinkBasis::Branch] {
        work.store()
            .append(&[Event::now(
                work.workspace(),
                SAM.parse().expect("synthetic test data"),
                EventBody::SessionLinked {
                    session: SESSION.parse().expect("synthetic test data"),
                    workstream: Some(SUBMISSION.parse().expect("synthetic test data")),
                    task: None,
                    basis,
                },
            )])
            .expect("synthetic test data");
        assert_eq!(
            get(&app, person(SAM), &format!("/v1/sessions/{SESSION}"))
                .await
                .1["workstream"],
            PARSERS
        );
    }
}

#[test]
fn imported_links_are_firm_in_the_hub() {
    let dir = tempfile::tempdir().expect("synthetic test data");
    let work = seeded(dir.path());
    for basis in [LinkBasis::Imported, LinkBasis::Folder, LinkBasis::Branch] {
        work.store()
            .append(&[Event::now(
                work.workspace(),
                SAM.parse().expect("synthetic test data"),
                EventBody::SessionLinked {
                    session: SESSION.parse().expect("synthetic test data"),
                    workstream: Some(PARSERS.parse().expect("synthetic test data")),
                    task: None,
                    basis,
                },
            )])
            .expect("synthetic test data");
    }
    assert_eq!(
        work.session(&SESSION.parse().expect("synthetic test data"))
            .expect("synthetic test data")
            .link_basis,
        Some(LinkBasis::Imported)
    );
}
