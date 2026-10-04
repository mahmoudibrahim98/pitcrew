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
fn imported_links_keep_hub_and_recap_activity_in_agreement() {
    use pitcrew_protocol::model::Receipt;
    use pitcrew_recap::{BlockBuilder, Config, Directory};

    for inferred in [Some(LinkBasis::Folder), Some(LinkBasis::Branch), None] {
        let dir = tempfile::tempdir().expect("synthetic test data");
        let work = seeded(dir.path());
        let id = SESSION.parse().expect("synthetic test data");
        let original = work.session(&id).expect("synthetic test data");
        let imported = PARSERS.parse().expect("synthetic test data");
        let mut directory = Directory::new();
        directory.add_session(&original);
        let mut builder = BlockBuilder::new(Config::default(), directory);
        let incoming = match inferred {
            Some(basis) => EventBody::SessionLinked {
                session: id,
                workstream: Some(SUBMISSION.parse().expect("synthetic test data")),
                task: Some(TASK.parse().expect("synthetic test data")),
                basis,
            },
            None => {
                let mut unlinked = original;
                unlinked.workstream = None;
                unlinked.task = None;
                unlinked.link_basis = None;
                EventBody::SessionDiscovered { session: unlinked }
            }
        };
        let bodies = [
            EventBody::SessionLinked {
                session: id,
                workstream: Some(imported),
                task: None,
                basis: LinkBasis::Imported,
            },
            incoming,
            EventBody::ToolRan {
                session: id,
                tool: "Bash".into(),
                target: "ls".into(),
                outcome: "ok".into(),
                failed: false,
                receipt: Receipt::Transcript {
                    session: id,
                    offset: 1,
                },
            },
        ];
        // Both sides replay these exact events, including activity after the attempted override.
        for body in bodies {
            let event = Event::now(
                work.workspace(),
                SAM.parse().expect("synthetic test data"),
                body,
            );
            work.store()
                .append(std::slice::from_ref(&event))
                .expect("synthetic test data");
            builder.push(&event);
            let session = work.session(&id).expect("synthetic test data");
            assert_eq!(
                session.link_basis,
                Some(LinkBasis::Imported),
                "{inferred:?}"
            );
            assert_eq!(session.workstream, Some(imported), "{inferred:?}");
            assert_eq!(session.task, None, "{inferred:?}");
        }
        let hub = work.session(&id).expect("synthetic test data");
        let blocks = builder.finish();
        assert!(!blocks.is_empty());
        for block in blocks {
            assert_eq!(block.workstream, hub.workstream, "{inferred:?}");
            assert!(block.tasks.is_empty(), "{inferred:?}");
        }
    }
}
