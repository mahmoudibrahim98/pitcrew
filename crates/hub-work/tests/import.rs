//! Synthetic history remains reversible across choices and hub restarts.
use pitcrew_hub_work::{BlockFilter, RecapIndex, WorkService};
use pitcrew_protocol::import::{ImportFilter, ImportMode};
use pitcrew_store::{Store, StoreOptions};
use std::sync::Arc;

#[test]
fn choices_are_durable_reversible_and_apply_to_future_sessions_and_recaps() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        Store::open_with(
            dir.path().join("hub.db"),
            StoreOptions::default(),
            pitcrew_hub_work::projections(),
        )
        .unwrap(),
    );
    let demo = pitcrew_fixtures::demo_workspace().unwrap();
    store
        .append(&pitcrew_hub_work::demo_events(&demo, demo.members[0].id))
        .unwrap();
    let work = WorkService::new(store.clone(), demo.workspace.clone())
        .with_clock(Arc::new(|| 2_000_000_000_000))
        .with_import_file(dir.path().join("import.json"))
        .unwrap();
    let all = ImportFilter::default();
    let dry = work.import_dry_run(all.clone()).unwrap();
    assert!(dry.count > 0);
    let total = dry.count + dry.subagents;
    let committed = work.commit_import(all.clone()).unwrap();
    assert_eq!(
        (committed.imported, committed.subagents),
        (dry.count, dry.subagents)
    );
    let before = work
        .recap_blocks(&BlockFilter::default(), None, None)
        .unwrap();
    let selected = work.sessions(&Default::default()).unwrap()[0].clone();
    let filter = ImportFilter {
        mode: ImportMode::Filtered,
        engines: vec![selected.engine],
        folders: vec![selected.cwd.clone()],
        since: None,
    };
    let dry = work.import_dry_run(filter.clone()).unwrap();
    let committed = work.commit_import(filter.clone()).unwrap();
    assert_eq!(
        (dry.count, dry.subagents),
        (committed.imported, committed.subagents)
    );
    assert!(work.session_included(&selected.id).unwrap());
    let none = ImportFilter {
        mode: ImportMode::None,
        ..Default::default()
    };
    assert_eq!(work.import_dry_run(none.clone()).unwrap().count, 0);
    assert_eq!(work.commit_import(none).unwrap().imported, 0);
    assert!(!work.session_included(&selected.id).unwrap());
    assert!(
        work.recap_blocks(&BlockFilter::default(), None, None)
            .unwrap()
            .blocks
            .iter()
            .all(|b| b.block.session.is_none())
    );
    let restarted = WorkService::new(store.clone(), demo.workspace.clone())
        .with_import_file(dir.path().join("import.json"))
        .unwrap();
    assert_eq!(restarted.import_choice(), work.import_choice());
    let mut future = selected.clone();
    future.id = pitcrew_protocol::ids::SessionId::new();
    future.started = 2_000_000_000_001;
    let mut event = demo.events[0].clone();
    event.id = pitcrew_protocol::ids::EventId::new();
    event.body = pitcrew_protocol::events::EventBody::SessionDiscovered {
        session: future.clone(),
    };
    store.append(&[event]).unwrap();
    assert!(work.session_included(&future.id).unwrap());
    work.commit_import(all).unwrap();
    assert!(work.session_included(&selected.id).unwrap());
    assert!(
        work.recap_blocks(&BlockFilter::default(), None, None)
            .unwrap()
            .blocks
            .len()
            >= before.blocks.len()
    );
    // No history was deleted while excluded.
    assert_eq!(work.sessions(&Default::default()).unwrap().len(), total + 1);
}

/// Sub-agents come with their parents: counted apart, and included exactly when the parent is,
/// whatever their own start, engine or folder.
#[test]
fn sub_agents_follow_their_parents_and_are_counted_apart() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        Store::open_with(
            dir.path().join("hub.db"),
            StoreOptions::default(),
            pitcrew_hub_work::projections(),
        )
        .unwrap(),
    );
    let demo = pitcrew_fixtures::demo_workspace().unwrap();
    store
        .append(&pitcrew_hub_work::demo_events(&demo, demo.members[0].id))
        .unwrap();
    let work = WorkService::new(store.clone(), demo.workspace.clone())
        .with_clock(Arc::new(|| 2_000_000_000_000))
        .with_import_file(dir.path().join("import.json"))
        .unwrap();
    let all = ImportFilter::default();
    let before = work.import_dry_run(all.clone()).unwrap();

    // Two sub-agents of one demo session: one in another folder, one started years later
    // (2030-01-01T00:00:00Z).
    const LATER: i64 = 1_893_456_000_000;
    let parent = work.sessions(&Default::default()).unwrap()[0].clone();
    let mut events = Vec::new();
    for (n, (cwd, started)) in [
        (format!("{}/elsewhere", parent.cwd), parent.started),
        (parent.cwd.clone(), LATER),
    ]
    .into_iter()
    .enumerate()
    {
        let mut sub = parent.clone();
        sub.id = pitcrew_protocol::ids::SessionId::new();
        sub.native_id = format!("agent-{n}");
        sub.parent = Some(parent.id);
        sub.cwd = cwd;
        sub.started = started;
        let mut event = demo.events[0].clone();
        event.id = pitcrew_protocol::ids::EventId::new();
        event.body = pitcrew_protocol::events::EventBody::SessionDiscovered { session: sub };
        events.push(event);
    }
    store.append(&events).unwrap();

    let after = work.import_dry_run(all.clone()).unwrap();
    assert_eq!(after.count, before.count, "sub-agents are not sessions");
    assert_eq!(after.subagents, before.subagents + 2);

    // Children whose chain ends nowhere the hub knows are sessions of their own, as clients show
    // them: one naming a parent the hub never saw, and two naming each other.
    let (a, b) = (
        pitcrew_protocol::ids::SessionId::new(),
        pitcrew_protocol::ids::SessionId::new(),
    );
    let strays: Vec<_> = [
        (
            pitcrew_protocol::ids::SessionId::new(),
            pitcrew_protocol::ids::SessionId::new(),
        ),
        (a, b),
        (b, a),
    ]
    .into_iter()
    .map(|(id, named)| {
        let mut stray = parent.clone();
        stray.id = id;
        stray.native_id = format!("stray-{id}");
        stray.parent = Some(named);
        let mut event = demo.events[0].clone();
        event.id = pitcrew_protocol::ids::EventId::new();
        event.body = pitcrew_protocol::events::EventBody::SessionDiscovered { session: stray };
        event
    })
    .collect();
    store.append(&strays).unwrap();
    let with_strays = work.import_dry_run(all).unwrap();
    assert_eq!(with_strays.count, after.count + 3);
    assert_eq!(with_strays.subagents, after.subagents);

    // Only the parent's folder: both sub-agents come with it, the one elsewhere too.
    let only_parent = ImportFilter {
        mode: ImportMode::Filtered,
        folders: vec![parent.cwd.clone()],
        engines: Vec::new(),
        since: None,
    };
    let dry = work.import_dry_run(only_parent.clone()).unwrap();
    assert!(dry.subagents >= 2, "{dry:?}");
    work.commit_import(only_parent).unwrap();
    for e in &events {
        let pitcrew_protocol::events::EventBody::SessionDiscovered { session } = &e.body else {
            unreachable!()
        };
        assert!(work.session_included(&session.id).unwrap());
    }

    // A filter that leaves the parent out leaves its sub-agents out, even one that matches.
    use pitcrew_protocol::model::Engine;
    let other_engine = [Engine::Claude, Engine::Codex, Engine::OpenCode]
        .into_iter()
        .find(|e| *e != parent.engine)
        .unwrap();
    let without_parent = ImportFilter {
        mode: ImportMode::Filtered,
        engines: vec![other_engine],
        folders: Vec::new(),
        since: None,
    };
    work.commit_import(without_parent).unwrap();
    for e in &events {
        let pitcrew_protocol::events::EventBody::SessionDiscovered { session } = &e.body else {
            unreachable!()
        };
        assert!(!work.session_included(&session.id).unwrap());
    }

    // Since a day the parent started before: the later sub-agent matches it, and is still left
    // out, of the list too.
    let since_later = ImportFilter {
        mode: ImportMode::Filtered,
        engines: Vec::new(),
        folders: Vec::new(),
        since: Some("2030-01-01".into()),
    };
    work.commit_import(since_later).unwrap();
    let listed = work.included_sessions(&Default::default()).unwrap();
    assert!(
        listed
            .iter()
            .all(|s| s.parent.is_none() || listed.iter().any(|p| Some(p.id) == s.parent)),
        "{listed:?}"
    );
    assert!(!listed.iter().any(|s| s.native_id == "agent-1"));
}
