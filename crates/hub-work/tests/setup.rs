//! Acceptance: `POST /v1/setup` (api-v1.md, "The first run"), end to end through the routes,
//! against a fresh, unseeded hub.

mod common;

use common::{app, call, expect};
use pitcrew_hub_work::{SetupListener, WorkService};
use pitcrew_protocol::api::{Caller, SetupDone, TokenScope};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, MemberId, WorkspaceId};
use pitcrew_protocol::model::{Member, MemberKind, Workspace};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// A fresh, empty store (no seed): a device token but no person, machine or name, as a real hub
/// starts (`crates/daemon`'s README, "Start").
fn fresh(dir: &std::path::Path) -> Arc<WorkService> {
    let store = common::open(&dir.join("hub.db"));
    let workspace = Workspace {
        id: WorkspaceId::new(),
        name: String::new(),
    };
    Arc::new(WorkService::new(store, workspace))
}

/// A device token acting as a member id nothing knows yet, as api-v1.md's "The first run" says.
fn device(member: MemberId) -> Caller {
    Caller {
        member,
        scope: TokenScope::Device,
        on_behalf_of: None,
    }
}

fn agent(member: MemberId) -> Caller {
    Caller {
        member,
        scope: TokenScope::Agent,
        on_behalf_of: Some(MemberId::new()),
    }
}

fn body(workspace_name: &str, name: &str, handle: &str, machine_name: &str) -> Value {
    json!({
        "workspace_name": workspace_name,
        "person": { "name": name, "handle": handle },
        "machine_name": machine_name,
    })
}

const GOOD: (&str, &str, &str, &str) = ("Demo Lab", "Sam Rivera", "@sam", "This laptop");

#[tokio::test]
async fn each_validation_case_answers_400() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = fresh(dir.path());
    let app = app(&work);
    let caller = device(MemberId::new());
    let (ws, name, handle, machine) = GOOD;
    let cases: Vec<(String, String, String, String)> = vec![
        (String::new(), name.into(), handle.into(), machine.into()), // workspace_name empty
        ("x".repeat(81), name.into(), handle.into(), machine.into()), // workspace_name too long
        (ws.into(), String::new(), handle.into(), machine.into()),   // person.name empty
        (ws.into(), "x".repeat(81), handle.into(), machine.into()),  // person.name too long
        (ws.into(), name.into(), "sam".into(), machine.into()),      // handle: no "@"
        (ws.into(), name.into(), "@".into(), machine.into()),        // handle: nothing after "@"
        (ws.into(), name.into(), "@Sam".into(), machine.into()),     // handle: uppercase
        (ws.into(), name.into(), "@sam rivera".into(), machine.into()), // handle: a space
        (
            ws.into(),
            name.into(),
            format!("@{}", "a".repeat(33)),
            machine.into(),
        ), // handle too long
        (ws.into(), name.into(), handle.into(), String::new()),      // machine_name empty
        (ws.into(), name.into(), handle.into(), "x".repeat(61)),     // machine_name too long
    ];
    for (ws, name, handle, machine) in cases {
        let result = call(
            &app,
            Some(caller),
            "POST",
            "/v1/setup",
            Some(body(&ws, &name, &handle, &machine)),
        )
        .await;
        expect(&result, 400);
    }
    // A control character anywhere is also 400.
    let result = call(
        &app,
        Some(caller),
        "POST",
        "/v1/setup",
        Some(body("Demo\u{0007}Lab", name, handle, machine)),
    )
    .await;
    expect(&result, 400);
    // None of the bad attempts left a person behind.
    assert!(work.members().expect("members").is_empty());
}

#[tokio::test]
async fn an_agent_token_gets_403_and_leaves_no_person() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = fresh(dir.path());
    let app = app(&work);
    let (ws, name, handle, machine) = GOOD;
    let result = call(
        &app,
        Some(agent(MemberId::new())),
        "POST",
        "/v1/setup",
        Some(body(ws, name, handle, machine)),
    )
    .await;
    expect(&result, 403);
    assert!(work.members().expect("members").is_empty());
    assert!(work.machines().expect("machines").is_empty());
}

#[tokio::test]
async fn setup_succeeds_once_then_conflicts_and_is_idempotent_under_a_retry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = fresh(dir.path());
    let app = app(&work);
    let caller = device(MemberId::new());
    let (ws, name, handle, machine) = GOOD;
    let setup = body(ws, name, handle, machine);

    let first = call(&app, Some(caller), "POST", "/v1/setup", Some(setup.clone())).await;
    expect(&first, 200);
    assert_eq!(first.1["workspace"]["name"], json!(ws));
    assert_eq!(first.1["me"]["handle"], json!(handle));
    assert_eq!(first.1["me"]["name"], json!(name));
    assert_eq!(first.1["me"]["kind"], json!("human"));
    assert!(first.1["me"]["owner"].is_null());
    assert_eq!(first.1["machine"]["name"], json!(machine));
    assert_eq!(first.1["machine"]["kind"], json!("local"));
    assert_eq!(first.1["machine"]["liveness"], json!("live"));

    // A retried, identical request: never a second person.
    let retried = call(&app, Some(caller), "POST", "/v1/setup", Some(setup)).await;
    expect(&retried, 409);

    // A different request once set up also conflicts; still nothing changed.
    let different = call(
        &app,
        Some(device(MemberId::new())),
        "POST",
        "/v1/setup",
        Some(body("Other Lab", "Robin", "@robin", "Robin's PC")),
    )
    .await;
    expect(&different, 409);

    assert_eq!(work.members().expect("members").len(), 1);
    assert_eq!(work.machines().expect("machines").len(), 1);
}

#[tokio::test]
async fn a_handle_clash_gives_409_even_without_a_person_yet() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = fresh(dir.path());
    let app = app(&work);
    // An agent member already holds "@office", as the back office's bootstrap might append
    // before any person exists (crates/daemon's README, "The back office").
    let office = Member {
        id: MemberId::new(),
        kind: MemberKind::Agent,
        handle: "@office".to_owned(),
        name: "Back office".to_owned(),
        owner: Some(MemberId::new()),
        persona: None,
    };
    let event = Event {
        id: EventId::new(),
        at: 1_790_800_000_000,
        workspace: work.workspace(),
        author: office.id,
        on_behalf_of: None,
        body: EventBody::MemberAdded {
            member: office.clone(),
        },
    };
    work.store().append(&[event]).expect("append");
    assert!(!work.members().expect("members").is_empty());

    let (ws, name, _, machine) = GOOD;
    let result = call(
        &app,
        Some(device(MemberId::new())),
        "POST",
        "/v1/setup",
        Some(body(ws, name, "@office", machine)),
    )
    .await;
    expect(&result, 409);
    // Only the pre-existing agent is there; still no person.
    assert_eq!(work.members().expect("members").len(), 1);
}

#[tokio::test]
async fn after_setup_me_answers_the_person_and_workspace_setup_needed_is_false() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = fresh(dir.path());
    let app = app(&work);
    let me = MemberId::new();
    let caller = device(me);

    let before = call(&app, Some(caller), "GET", "/v1/workspace", None).await;
    expect(&before, 200);
    assert_eq!(before.1["setup_needed"], json!(true));
    // `GET /v1/me` is 404 before setup: the token acts as a member id nothing knows yet.
    let me_before = call(&app, Some(caller), "GET", "/v1/me", None).await;
    expect(&me_before, 404);

    let (ws, name, handle, machine) = GOOD;
    let setup = call(
        &app,
        Some(caller),
        "POST",
        "/v1/setup",
        Some(body(ws, name, handle, machine)),
    )
    .await;
    expect(&setup, 200);

    let after = call(&app, Some(caller), "GET", "/v1/workspace", None).await;
    expect(&after, 200);
    assert!(
        after.1.get("setup_needed").is_none(),
        "setup_needed must be omitted, not false: {}",
        after.1
    );
    assert_eq!(after.1["workspace"]["name"], json!(ws));

    let me_after = call(&app, Some(caller), "GET", "/v1/me", None).await;
    expect(&me_after, 200);
    assert_eq!(me_after.1["id"], json!(me.0.to_string()));
    assert_eq!(me_after.1["handle"], json!(handle));
}

/// Collects every `SetupDone` it is called with.
#[derive(Default)]
struct Collector(Mutex<Vec<SetupDone>>);

impl SetupListener for Collector {
    fn set_up(&self, done: &SetupDone) {
        self.0.lock().expect("lock").push(done.clone());
    }
}

#[tokio::test]
async fn the_listener_is_called_exactly_once_after_the_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = common::open(&dir.path().join("hub.db"));
    let workspace = Workspace {
        id: WorkspaceId::new(),
        name: String::new(),
    };
    let collector = Arc::new(Collector::default());
    let work = Arc::new(
        WorkService::new(store, workspace)
            .with_setup_listener(Arc::clone(&collector) as Arc<dyn SetupListener>),
    );
    let app = app(&work);
    let caller = device(MemberId::new());
    let (ws, name, handle, machine) = GOOD;
    let setup = body(ws, name, handle, machine);

    let ok = call(&app, Some(caller), "POST", "/v1/setup", Some(setup.clone())).await;
    expect(&ok, 200);
    {
        let calls = collector.0.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].me.handle, handle);
        assert_eq!(calls[0].workspace.name, ws);
    }

    // The commit has already happened by the time the listener runs: the member it was given is
    // readable right away, through the service the listener itself could use.
    assert_eq!(work.members().expect("members").len(), 1);

    // A conflicting retry never calls it again.
    let retried = call(&app, Some(caller), "POST", "/v1/setup", Some(setup)).await;
    expect(&retried, 409);
    assert_eq!(collector.0.lock().expect("lock").len(), 1);
}
