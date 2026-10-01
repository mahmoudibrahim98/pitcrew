//! Acceptance: briefs' next steps and pending proposals, through the routes (the same cases as
//! apps/mock-hub/test/edits.test.ts, "PUT /v1/briefs: next steps and accepted proposals").

mod common;

use common::{
    PAPER, SAM, SEED_RUNS, SUBMISSION, agent, app, call, expect, get, member, person, seeded,
};
use pitcrew_hub_work::WorkService;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::EventId;
use pitcrew_protocol::model::{BriefTarget, Receipt, Scheduler};
use serde_json::{Value, json};
use std::sync::Arc;

/// @office, the back office's agent.
const OFFICE: &str = "01JB000000000000000MEM0006";
const UNKNOWN_PROJECT: &str = "01JB000000000000000PRJ0099";
const PAP1: &str = "01JB000000000000000TSK0001";

fn job() -> Receipt {
    Receipt::Job {
        scheduler: Scheduler::Slurm,
        id: "4815170".into(),
    }
}

struct Hub {
    _dir: tempfile::TempDir,
    work: Arc<WorkService>,
    app: axum::Router,
}

fn hub() -> Hub {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let app = app(&work);
    Hub {
        _dir: dir,
        work,
        app,
    }
}

impl Hub {
    async fn put(&self, path: &str, body: Value) -> (u16, Value) {
        call(
            &self.app,
            Some(person(SAM)),
            "PUT",
            &format!("/v1/briefs/{path}"),
            Some(body),
        )
        .await
    }

    async fn brief_of(&self, id: &str) -> Option<Value> {
        let briefs = get(&self.app, person(SAM), "/v1/briefs").await;
        expect(&briefs, 200);
        briefs
            .1
            .as_array()
            .expect("list")
            .iter()
            .find(|b| b["target"]["id"] == json!(id))
            .cloned()
    }

    fn last_event(&self) -> Event {
        let rev = self.work.store().latest_rev().expect("rev");
        self.work
            .store()
            .since(rev - 1, 1)
            .expect("since")
            .pop()
            .expect("event")
            .event
    }

    /// Appends a `brief_proposed` from @office, as the back office would.
    fn propose(&self, target: BriefTarget, text: &str, next: Option<&str>) -> Event {
        let event = Event {
            id: EventId::new(),
            at: 1_790_900_000_000,
            workspace: common::demo().workspace.id,
            author: member(OFFICE),
            on_behalf_of: Some(member(SAM)),
            body: EventBody::BriefProposed {
                target,
                text: text.into(),
                next: next.map(str::to_owned),
                receipts: vec![job()],
            },
        };
        self.work
            .store()
            .append(std::slice::from_ref(&event))
            .expect("append");
        event
    }

    /// The demo's own pending proposal for the paper: its last event.
    fn papers_proposal(&self) -> (String, Vec<Receipt>, i64) {
        let event = common::demo().events.pop().expect("events");
        match event.body {
            EventBody::BriefProposed {
                target: BriefTarget::Project(p),
                text,
                next: None,
                receipts,
            } if p == PAPER.parse().expect("id") => (text, receipts, event.at),
            other => panic!("the demo's last event is the paper's proposal, not {other:?}"),
        }
    }
}

fn seeds() -> BriefTarget {
    BriefTarget::Workstream(SEED_RUNS.parse().expect("id"))
}

#[tokio::test]
async fn put_stores_next_and_brief_accepted_carries_it() {
    let hub = hub();
    let res = hub
        .put(
            &format!("workstream/{SUBMISSION}"),
            json!({ "text": "§3.2 is drafted.", "next": "Send it to the co-authors.", "pinned": false }),
        )
        .await;
    expect(&res, 200);
    assert_eq!(res.1["next"], "Send it to the co-authors.");
    assert_eq!(res.1["source"], "person");
    assert_eq!(res.1["receipts"], json!([]));
    let event = hub.last_event();
    assert_eq!(event.author, member(SAM));
    assert_eq!(res.1["updated"], json!(event.at));
    assert_eq!(
        serde_json::to_value(&event.body).expect("json"),
        json!({
            "type": "brief_accepted",
            "data": {
                "target": { "kind": "workstream", "id": SUBMISSION },
                "text": "§3.2 is drafted.",
                "next": "Send it to the co-authors.",
                "pinned": false,
            },
        })
    );
    assert_eq!(hub.brief_of(SUBMISSION).await, Some(res.1));
    // Without a next step, brief_accepted leaves it out and the brief has none.
    let res = hub
        .put(
            &format!("workstream/{SUBMISSION}"),
            json!({ "text": "§3.2 is drafted.", "pinned": false }),
        )
        .await;
    expect(&res, 200);
    assert!(res.1.get("next").is_none());
    let body = serde_json::to_value(&hub.last_event().body).expect("json");
    assert!(body["data"].get("next").is_none(), "{body}");
}

#[tokio::test]
async fn accepting_a_pending_proposal_unchanged_copies_its_receipts_as_the_back_office() {
    let hub = hub();
    let (text, receipts, _) = hub.papers_proposal();
    let accepted = hub
        .put(
            &format!("project/{PAPER}"),
            json!({ "text": text, "pinned": false }),
        )
        .await;
    expect(&accepted, 200);
    assert_eq!(accepted.1["source"], "back_office");
    assert_eq!(accepted.1["receipts"], json!(receipts));
    assert!(accepted.1.get("next").is_none());
    assert!(accepted.1.get("proposal").is_none());
    assert_eq!(
        serde_json::to_value(&hub.last_event().body).expect("json"),
        json!({
            "type": "brief_accepted",
            "data": {
                "target": { "kind": "project", "id": PAPER },
                "text": text, "pinned": false, "receipts": receipts,
            },
        })
    );
    // The accepted brief is now newer than the proposal: the same PUT again ("keep current") is
    // the person's own.
    let kept = hub
        .put(
            &format!("project/{PAPER}"),
            json!({ "text": text, "pinned": false }),
        )
        .await;
    expect(&kept, 200);
    assert_eq!(kept.1["source"], "person");
    assert_eq!(kept.1["receipts"], json!([]));
    let body = serde_json::to_value(&hub.last_event().body).expect("json");
    assert!(body["data"].get("receipts").is_none(), "{body}");
}

#[tokio::test]
async fn a_changed_text_or_next_step_is_the_persons_own() {
    let hub = hub();
    let (text, _, _) = hub.papers_proposal();
    let with_next = hub
        .put(
            &format!("project/{PAPER}"),
            json!({ "text": text, "next": "Decide on seed 3.", "pinned": false }),
        )
        .await;
    expect(&with_next, 200);
    assert_eq!(with_next.1["source"], "person");
    assert_eq!(with_next.1["receipts"], json!([]));

    let hub = self::hub();
    let edited = hub
        .put(
            &format!("project/{PAPER}"),
            json!({ "text": "Half of the method is drafted.", "pinned": true }),
        )
        .await;
    expect(&edited, 200);
    assert_eq!(edited.1["source"], "person");
    assert_eq!(edited.1["receipts"], json!([]));
    // A proposal with a next step needs that same next step, not a missing one.
    hub.propose(seeds(), "Seed 3 converged.", Some("Make figure 3."));
    let without_next = hub
        .put(
            &format!("workstream/{SEED_RUNS}"),
            json!({ "text": "Seed 3 converged.", "pinned": true }),
        )
        .await;
    assert_eq!(without_next.1["source"], "person");
}

#[tokio::test]
async fn each_brief_is_listed_with_its_pending_proposal() {
    let hub = hub();
    let (text, receipts, at) = hub.papers_proposal();
    let paper = hub.brief_of(PAPER).await.expect("paper");
    assert_eq!(
        paper["proposal"],
        json!({ "text": text, "receipts": receipts, "at": at })
    );
    // The other briefs have nothing pending: the key is left out.
    let briefs = get(&hub.app, person(SAM), "/v1/briefs").await;
    for brief in briefs.1.as_array().expect("list") {
        if brief["target"]["id"] != json!(PAPER) {
            assert!(brief.get("proposal").is_none(), "{brief}");
        }
    }
    // A new proposal shows with its next step and time.
    let event = hub.propose(
        seeds(),
        "Seed 3 reran and converged.",
        Some("Make figure 3."),
    );
    let brief = hub.brief_of(SEED_RUNS).await.expect("seeds");
    assert_eq!(
        brief["proposal"],
        json!({
            "text": "Seed 3 reran and converged.", "next": "Make figure 3.",
            "receipts": [job()], "at": event.at,
        })
    );
    // The brief in force is unchanged by a proposal.
    assert_eq!(brief["source"], "person");
    assert_eq!(brief["pinned"], true);
    // A target with a proposal but no brief in force is not listed.
    let empty = BriefTarget::Workstream("01JB000000000000000WST0004".parse().expect("id"));
    hub.propose(empty, "Nothing yet.", None);
    assert!(hub.brief_of("01JB000000000000000WST0004").await.is_none());
}

#[tokio::test]
async fn keep_current_clears_the_proposal() {
    let hub = hub();
    hub.propose(
        seeds(),
        "Seed 3 reran and converged.",
        Some("Make figure 3."),
    );
    let current = hub.brief_of(SEED_RUNS).await.expect("seeds");
    assert!(current.get("proposal").is_some());
    let kept = hub
        .put(
            &format!("workstream/{SEED_RUNS}"),
            json!({ "text": current["text"], "next": current["next"], "pinned": current["pinned"] }),
        )
        .await;
    expect(&kept, 200);
    assert_eq!(kept.1["source"], "person");
    assert!(kept.1.get("proposal").is_none());
    let after = hub.brief_of(SEED_RUNS).await.expect("seeds");
    assert!(after.get("proposal").is_none());
    assert_eq!(after["text"], current["text"]);
}

#[tokio::test]
async fn accepting_the_proposal_unchanged_clears_it_as_the_back_office() {
    let hub = hub();
    hub.propose(
        seeds(),
        "Seed 3 reran and converged.",
        Some("Make figure 3."),
    );
    let accepted = hub
        .put(
            &format!("workstream/{SEED_RUNS}"),
            json!({ "text": "Seed 3 reran and converged.", "next": "Make figure 3.", "pinned": true }),
        )
        .await;
    expect(&accepted, 200);
    assert_eq!(accepted.1["source"], "back_office");
    assert_eq!(accepted.1["receipts"], json!([job()]));
    let after = hub.brief_of(SEED_RUNS).await.expect("seeds");
    assert!(after.get("proposal").is_none());
    assert_eq!(after["source"], "back_office");
    assert_eq!(after["receipts"], json!([job()]));
    assert_eq!(after["next"], "Make figure 3.");
    // The demo's own pending proposal for the paper clears the same way.
    let paper = hub.brief_of(PAPER).await.expect("paper");
    let text = paper["proposal"]["text"].clone();
    expect(
        &hub.put(
            &format!("project/{PAPER}"),
            json!({ "text": text, "pinned": false }),
        )
        .await,
        200,
    );
    assert!(
        hub.brief_of(PAPER)
            .await
            .expect("paper")
            .get("proposal")
            .is_none()
    );
}

#[tokio::test]
async fn a_newer_proposal_is_pending_again_after_an_accepted_brief() {
    let hub = hub();
    expect(
        &hub.put(
            &format!("workstream/{SEED_RUNS}"),
            json!({ "text": "Rerunning seed 3.", "pinned": true }),
        )
        .await,
        200,
    );
    assert!(
        hub.brief_of(SEED_RUNS)
            .await
            .expect("seeds")
            .get("proposal")
            .is_none()
    );
    hub.propose(seeds(), "Seed 3 converged.", None);
    assert_eq!(
        hub.brief_of(SEED_RUNS).await.expect("seeds")["proposal"]["text"],
        "Seed 3 converged."
    );
    // Proposing for one brief leaves the others alone.
    assert!(
        hub.brief_of(SUBMISSION)
            .await
            .expect("submission")
            .get("proposal")
            .is_none()
    );
    // The newest proposal is the pending one.
    hub.propose(seeds(), "Seed 3 converged twice.", None);
    assert_eq!(
        hub.brief_of(SEED_RUNS).await.expect("seeds")["proposal"]["text"],
        "Seed 3 converged twice."
    );
}

#[tokio::test]
async fn a_brief_the_back_office_applies_itself_is_the_back_offices() {
    let hub = hub();
    let event = Event {
        id: EventId::new(),
        at: 1_790_900_000_000,
        workspace: common::demo().workspace.id,
        author: member(OFFICE),
        on_behalf_of: Some(member(SAM)),
        body: EventBody::BriefAccepted {
            target: seeds(),
            text: "Applied by the office.".into(),
            next: Some("Check it.".into()),
            pinned: false,
            receipts: vec![job()],
        },
    };
    hub.work.store().append(&[event]).expect("append");
    let brief = hub.brief_of(SEED_RUNS).await.expect("seeds");
    assert_eq!(brief["source"], "back_office");
    assert_eq!(brief["receipts"], json!([job()]));
    assert_eq!(brief["next"], "Check it.");
}

#[tokio::test]
async fn briefs_keep_their_old_checks() {
    let hub = hub();
    expect(
        &hub.put(
            &format!("project/{UNKNOWN_PROJECT}"),
            json!({ "text": "x", "pinned": false }),
        )
        .await,
        404,
    );
    expect(
        &hub.put(
            &format!("task/{PAP1}"),
            json!({ "text": "x", "pinned": false }),
        )
        .await,
        404,
    );
    expect(
        &hub.put(&format!("project/{PAPER}"), json!({ "text": "x" }))
            .await,
        400,
    );
    expect(
        &hub.put(
            &format!("project/{PAPER}"),
            json!({ "text": "x", "next": 3, "pinned": false }),
        )
        .await,
        400,
    );
    let agent_tries = call(
        &hub.app,
        Some(agent("01JB000000000000000MEM0002")),
        "PUT",
        &format!("/v1/briefs/project/{PAPER}"),
        Some(json!({ "text": "x", "pinned": false })),
    )
    .await;
    expect(&agent_tries, 403);
}
