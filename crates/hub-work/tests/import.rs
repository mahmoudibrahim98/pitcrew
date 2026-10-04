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
    let count = work.import_dry_run(all.clone()).unwrap();
    assert!(count > 0);
    assert_eq!(work.commit_import(all.clone()).unwrap(), count);
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
    assert_eq!(
        work.import_dry_run(filter.clone()).unwrap(),
        work.commit_import(filter.clone()).unwrap()
    );
    assert!(work.session_included(&selected.id).unwrap());
    let none = ImportFilter {
        mode: ImportMode::None,
        ..Default::default()
    };
    assert_eq!(work.import_dry_run(none.clone()).unwrap(), 0);
    assert_eq!(work.commit_import(none).unwrap(), 0);
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
    assert_eq!(work.sessions(&Default::default()).unwrap().len(), count + 1);
}
