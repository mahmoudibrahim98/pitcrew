mod common;

use common::*;
use pitcrew_hub_work::projection::Cursors;
use pitcrew_protocol::events::EventBody;
use serde_json::json;

#[tokio::test]
async fn routes_are_forward_only_per_person_and_scope() {
    let dir = tempfile::tempdir().expect("temporary home");
    let work = seeded(dir.path());
    let router = app(&work);
    let sam = person(SAM);
    let other = person("01JB000000000000000MEM0007");
    let project = format!("project:{PAPER}");
    for scope in ["workspace", &project] {
        let path = format!("/v1/me/cursors/{scope}");
        assert_eq!(
            call(&router, Some(sam), "PUT", &path, Some(json!({"rev": 10}))).await,
            (200, json!({"scope": scope, "rev": 10}))
        );
        let latest = work.store().latest_rev().expect("revision");
        for rev in [10, 2, 0] {
            assert_eq!(
                call(&router, Some(sam), "PUT", &path, Some(json!({"rev": rev}))).await,
                (200, json!({"scope": scope, "rev": 10}))
            );
        }
        assert_eq!(work.store().latest_rev().expect("revision"), latest);
        assert_eq!(
            call(&router, Some(other), "PUT", &path, Some(json!({"rev": 3})))
                .await
                .0,
            200
        );
    }
    assert_eq!(work.cursors(&sam).expect("cursors").len(), 2);
    assert!(
        work.cursors(&other)
            .expect("other")
            .iter()
            .all(|c| c.rev == 3)
    );
    for method in ["GET", "PUT"] {
        let path = if method == "GET" {
            "/v1/me/cursors"
        } else {
            "/v1/me/cursors/workspace"
        };
        assert_eq!(
            call(
                &router,
                Some(agent(WRITER)),
                method,
                path,
                Some(json!({"rev": "bad"}))
            )
            .await
            .0,
            403
        );
    }
    for (scope, rev, status) in [
        ("task:bad", json!(1), 400),
        ("workspace", json!(-1), 400),
        ("workspace", json!(u64::MAX), 400),
        ("project:01J00000000000000000000000", json!(1), 404),
    ] {
        assert_eq!(
            call(
                &router,
                Some(sam),
                "PUT",
                &format!("/v1/me/cursors/{scope}"),
                Some(json!({"rev": rev}))
            )
            .await
            .0,
            status
        );
    }
    let before = work.cursors(&sam).expect("before");
    work.store().rebuild(Cursors::NAME).expect("replay");
    assert_eq!(before, work.cursors(&sam).expect("after"));
    let events = work.store().since(0, 1000).expect("log");
    assert!(
        events
            .iter()
            .any(|e| matches!(e.event.body, EventBody::CursorMoved { .. }))
    );
}
