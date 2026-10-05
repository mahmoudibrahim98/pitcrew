use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::EventId;
use pitcrew_store::{Error, EventFilter, RevRange, Store, StoreOptions, event_type};
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use tempfile::TempDir;

fn fixture_events() -> Vec<Event> {
    pitcrew_fixtures::demo_workspace()
        .expect("fixture parses")
        .events
}

fn open() -> (TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("store.db"), StoreOptions::default()).expect("open");
    (dir, store)
}

#[test]
fn fixture_events_round_trip_with_revs_1_to_15() {
    let (_dir, store) = open();
    let events = fixture_events();
    assert_eq!(events.len(), 15);
    assert_eq!(store.latest_rev().expect("rev"), 0);

    let range = store.append(&events).expect("append");
    assert_eq!(
        range,
        RevRange {
            from_rev: 1,
            to_rev: 15
        }
    );
    assert_eq!(range.len(), 15);
    assert_eq!(store.latest_rev().expect("rev"), 15);

    let back = store.since(0, 100).expect("since");
    assert_eq!(
        back.iter().map(|e| e.rev).collect::<Vec<_>>(),
        (1..=15).collect::<Vec<_>>()
    );
    let back: Vec<Event> = back.into_iter().map(|e| e.event).collect();
    assert_eq!(back, events);
}

#[test]
fn columns_match_0001_init() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let store = Store::open(&path, StoreOptions::default()).expect("open");
    let events = fixture_events();
    store.append(&events).expect("append");

    let conn = rusqlite::Connection::open(&path).expect("raw");
    let (id, kind, data): (String, String, String) = conn
        .query_row("SELECT id, type, data FROM events WHERE rev = 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .expect("row");
    assert_eq!(id, events[0].id.0.to_string());
    assert_eq!(id.len(), 26);
    assert_eq!(kind, event_type(&events[0].body).expect("type"));
    let wire = serde_json::to_value(&events[0].body).expect("json");
    let data: serde_json::Value = serde_json::from_str(&data).expect("data is JSON");
    assert_eq!(data, wire["data"]);
}

#[test]
fn events_cannot_be_updated_or_deleted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let store = Store::open(&path, StoreOptions::default()).expect("open");
    store.append(&fixture_events()).expect("append");

    let conn = rusqlite::Connection::open(&path).expect("raw");
    let err = conn
        .execute("UPDATE events SET author = 'x' WHERE rev = 1", [])
        .expect_err("update must fail");
    assert!(err.to_string().contains("append-only"), "{err}");
    let err = conn
        .execute("DELETE FROM events WHERE rev = 1", [])
        .expect_err("delete must fail");
    assert!(err.to_string().contains("append-only"), "{err}");
    assert_eq!(store.since(0, 100).expect("since").len(), 15);
}

#[test]
fn paging_forward_and_back() {
    let (_dir, store) = open();
    let events = fixture_events();
    store.append(&events).expect("append");

    let page = store.since(10, 3).expect("since");
    assert_eq!(
        page.iter().map(|e| e.rev).collect::<Vec<_>>(),
        vec![11, 12, 13]
    );
    assert!(store.since(15, 10).expect("since").is_empty());

    let page = store
        .before(16, 5, &EventFilter::default())
        .expect("before");
    assert_eq!(
        page.iter().map(|e| e.rev).collect::<Vec<_>>(),
        vec![11, 12, 13, 14, 15]
    );
    let page = store
        .before(11, 100, &EventFilter::default())
        .expect("before");
    assert_eq!(page.len(), 10);
    assert_eq!(page[0].rev, 1);
    assert!(
        store
            .before(1, 10, &EventFilter::default())
            .expect("before")
            .is_empty()
    );
}

#[test]
fn before_filters_by_type() {
    let (_dir, store) = open();
    let events = fixture_events();
    store.append(&events).expect("append");

    let kind = event_type(&events[0].body).expect("type");
    let expected: Vec<u64> = events
        .iter()
        .zip(1u64..)
        .filter(|(e, _)| event_type(&e.body).expect("type") == kind)
        .map(|(_, rev)| rev)
        .collect();
    let filter = EventFilter::default().types([kind.clone()]);
    let page = store.before(u64::MAX, 100, &filter).expect("before");
    assert_eq!(page.iter().map(|e| e.rev).collect::<Vec<_>>(), expected);
    assert!(
        page.iter()
            .all(|e| event_type(&e.event.body).expect("type") == kind)
    );

    let none = EventFilter::default().types(["no_such_type"]);
    assert!(
        store
            .before(u64::MAX, 100, &none)
            .expect("before")
            .is_empty()
    );
    // An empty type list matches everything, as the doc says.
    let empty = EventFilter::default().types(Vec::<String>::new());
    assert_eq!(empty, EventFilter::default());
    assert_eq!(
        store.before(u64::MAX, 100, &empty).expect("before").len(),
        15
    );
    let mut raw_empty = EventFilter::default();
    raw_empty.types = Some(Vec::new());
    assert_eq!(
        store
            .before(u64::MAX, 100, &raw_empty)
            .expect("before")
            .len(),
        15
    );
}

#[test]
fn before_with_several_types_pages_in_order() {
    let (_dir, store) = open();
    let events = fixture_events();
    store.append(&events).expect("append");

    let a = event_type(&events[0].body).expect("type");
    let b = event_type(&events[2].body).expect("type");
    assert_ne!(a, b);
    let expected: Vec<u64> = events
        .iter()
        .zip(1u64..)
        .filter(|(e, _)| {
            let t = event_type(&e.body).expect("type");
            t == a || t == b
        })
        .map(|(_, rev)| rev)
        .collect();
    // A repeated type must not repeat rows.
    let filter = EventFilter::default().types([a.clone(), b.clone(), a.clone()]);

    // Page back two at a time and stitch the pages together.
    let mut got = Vec::new();
    let mut rev = u64::MAX;
    loop {
        let page = store.before(rev, 2, &filter).expect("before");
        let Some(first) = page.first() else { break };
        rev = first.rev;
        let mut revs: Vec<u64> = page.iter().map(|e| e.rev).collect();
        revs.append(&mut got);
        got = revs;
    }
    assert_eq!(got, expected);
}

#[test]
fn append_new_stores_only_unknown_ids() {
    let (_dir, store) = open();
    let events = fixture_events();
    let (e1, e2, e3) = (&events[0], &events[1], &events[2]);
    store.append(&[e1.clone(), e2.clone()]).expect("append");

    // A retry that differs from the original: e2 is stored, e3 is not.
    let err = store
        .append(&[e2.clone(), e3.clone()])
        .expect_err("duplicate");
    assert!(matches!(err, Error::DuplicateEvent { id } if id == e2.id));
    assert_eq!(store.latest_rev().expect("rev"), 2);

    let (range, skipped) = store
        .append_new(&[e2.clone(), e3.clone(), e3.clone()])
        .expect("append_new");
    assert_eq!(
        range,
        RevRange {
            from_rev: 3,
            to_rev: 3
        }
    );
    assert_eq!(skipped, vec![e2.id, e3.id]);
    let stored: Vec<EventId> = store
        .since(0, 10)
        .expect("since")
        .into_iter()
        .map(|e| e.event.id)
        .collect();
    assert_eq!(stored, vec![e1.id, e2.id, e3.id]);

    // All known: nothing appended, nobody told.
    let mut rx = store.subscribe();
    let (range, skipped) = store
        .append_new(std::slice::from_ref(e1))
        .expect("append_new");
    assert!(range.is_empty());
    assert_eq!(skipped, vec![e1.id]);
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_failed_append_appends_nothing() {
    let (_dir, store) = open();
    let events = fixture_events();
    store.append(&events[..5]).expect("append");
    // The last event repeats an id that is already stored.
    let mut batch = events[5..].to_vec();
    batch.push(events[0].clone());
    let err = store.append(&batch).expect_err("duplicate id");
    assert!(
        matches!(err, Error::DuplicateEvent { id } if id == events[0].id),
        "{err:?}"
    );
    assert_eq!(store.latest_rev().expect("rev"), 5);

    // A repeat inside one batch is the same error.
    let mut fresh = events[5].clone();
    fresh.id = EventId::new();
    let err = store
        .append(&[fresh.clone(), fresh.clone()])
        .expect_err("repeat in batch");
    assert!(
        matches!(err, Error::DuplicateEvent { id } if id == fresh.id),
        "{err:?}"
    );
    assert_eq!(store.latest_rev().expect("rev"), 5);

    let range = store.append(&events[5..]).expect("append rest");
    assert_eq!(range.from_rev, 6);
    assert_eq!(range.to_rev, 15);
}

#[test]
fn empty_append_is_empty() {
    let (_dir, store) = open();
    let mut rx = store.subscribe();
    let range = store.append(&[]).expect("append");
    assert!(range.is_empty());
    assert_eq!(range.len(), 0);
    assert!(rx.try_recv().is_err());
}

#[test]
fn subscribers_see_new_ranges() {
    let (_dir, store) = open();
    let events = fixture_events();
    let mut a = store.subscribe();
    let mut b = store.subscribe();
    store.append(&events[..10]).expect("append");
    store.append(&events[10..]).expect("append");
    for rx in [&mut a, &mut b] {
        assert_eq!(
            rx.try_recv().expect("first"),
            RevRange {
                from_rev: 1,
                to_rev: 10
            }
        );
        assert_eq!(
            rx.try_recv().expect("second"),
            RevRange {
                from_rev: 11,
                to_rev: 15
            }
        );
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn reopening_keeps_the_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let events = fixture_events();
    {
        let store = Store::open(&path, StoreOptions::default()).expect("open");
        store.append(&events).expect("append");
    }
    let store = Store::open(&path, StoreOptions::default()).expect("reopen");
    assert_eq!(store.latest_rev().expect("rev"), 15);
    let mut more = events[0].clone();
    more.id = EventId::new();
    assert_eq!(store.append(&[more]).expect("append").from_rev, 16);
}

/// Every `EventBody` tag, read from serde's "unknown variant" message so a new variant in the
/// protocol shows up here without editing this test.
fn all_event_types() -> BTreeSet<String> {
    let err = serde_json::from_value::<EventBody>(json!({"type": "__none__", "data": {}}))
        .expect_err("unknown tag");
    let msg = err.to_string();
    let list = msg
        .split_once("expected one of ")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("unexpected serde message: {msg}"));
    list.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// One event for each body the fixture's log does not already have, built from fixture records.
fn other_bodies() -> Vec<EventBody> {
    let demo = pitcrew_fixtures::demo_workspace().expect("fixture");
    let session = &demo.sessions[0];
    let bodies = [
        json!({"type": "cursor_moved", "data": {"scope": "workspace", "rev": 15}}),
        json!({"type": "machine_liveness", "data": {
            "machine": demo.machines[2].id, "liveness": "stopped"}}),
        json!({"type": "session_discovered", "data": {"session": session}}),
        json!({"type": "turn_ended", "data": {"session": session.id,
            "receipt": {"kind": "transcript", "session": session.id, "offset": 4096}}}),
        json!({"type": "session_linked", "data": {"session": session.id,
            "workstream": demo.workstreams[0].id, "task": demo.tasks[0].id, "basis": "folder"}}),
        json!({"type": "session_ended", "data": {"session": session.id}}),
        json!({"type": "project_created", "data": {"project": demo.projects[0]}}),
        json!({"type": "workstream_created", "data": {"workstream": demo.workstreams[1]}}),
        json!({"type": "workstream_linked", "data": {"workstream": demo.workstreams[1].id,
            "external": [{"system": "github", "key": "example-org/demo-repo#milestone:1",
                          "url": "https://github.com/example-org/demo-repo/milestone/1"}]}}),
        json!({"type": "task_created", "data": {"task": demo.tasks[3]}}),
        json!({"type": "task_assigned", "data": {"task": demo.tasks[4].id,
            "assignee": demo.members[2].id}}),
        json!({"type": "task_updated", "data": {"task": demo.tasks[4].id,
            "patch": {"title": "Rerun seed 3", "due": null, "labels": ["gpu"]}}}),
        json!({"type": "ask_answered", "data": {"ask": demo.asks[1].id,
            "answer": {"by": demo.members[0].id, "option": 1, "text": "Drop it.",
                       "at": 1_790_762_500_000_i64}}}),
        json!({"type": "decision_recorded", "data": {"workstream": demo.workstreams[1].id,
            "text": "Report four seeds.", "why": "Seed 3 diverged.",
            "receipts": [{"kind": "job", "scheduler": "slurm", "id": "4815164"}]}}),
        json!({"type": "machine_added", "data": {"machine": demo.machines[1]}}),
        json!({"type": "member_added", "data": {"member": demo.members[1]}}),
        json!({"type": "persona_saved", "data": {"persona": demo.personas[0]}}),
        json!({"type": "team_saved", "data": {"team": demo.teams[0]}}),
        json!({"type": "session_updated", "data": {"session": session.id,
            "title": "Method section, second pass"}}),
        json!({"type": "write_proposed", "data": {"write": {
            "ask": demo.asks[1].id, "integration": "01J00000000000000000000001",
            "system": "github", "scope": "example-org/demo-repo",
            "target": {"system": "github", "key": "example-org/demo-repo#1"},
            "task": demo.tasks[4].id, "operation": "close",
            "before": {"state": "open"}, "after": {"state": "closed", "close_reason": "completed"},
            "requested_by": demo.members[0].id}}}),
        json!({"type": "write_started", "data": {"ask": demo.asks[1].id,
            "task": demo.tasks[4].id, "attempt": 1}}),
        json!({"type": "write_finished", "data": {"ask": demo.asks[1].id,
            "task": demo.tasks[4].id, "result": {"outcome": "failed", "message": "Not Found",
            "status": 404}}}),
        json!({"type": "safety_changed", "data": {"settings": {"permission_mode": "plan",
            "back_office_enabled": true, "back_office_caps": {"max_auto_accept_per_hour": 10}}}}),
    ];
    bodies
        .into_iter()
        .map(|b| serde_json::from_value(b).expect("body"))
        .collect()
}

#[test]
fn every_event_body_variant_round_trips() {
    let (_dir, store) = open();
    let mut events = fixture_events();
    let template = events[0].clone();
    for body in other_bodies() {
        let mut e = template.clone();
        e.id = EventId::new();
        e.body = body;
        events.push(e);
    }
    let covered: BTreeSet<String> = events
        .iter()
        .map(|e| event_type(&e.body).expect("type"))
        .collect();
    assert_eq!(covered, all_event_types(), "add a body for the new variant");

    store.append(&events).expect("append");
    let back: Vec<Event> = store
        .since(0, 1000)
        .expect("since")
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(back, events);
}

fn numbered_events(n: usize) -> Vec<Event> {
    fixture_events()
        .into_iter()
        .cycle()
        .take(n)
        .map(|mut e| {
            e.id = EventId::new();
            e
        })
        .collect()
}

#[test]
fn concurrent_appenders_notify_in_order() {
    const THREADS: usize = 8;
    const BATCHES: usize = 25;
    let (_dir, store) = open();
    let store = Arc::new(store);
    let mut rx = store.subscribe();
    let barrier = Arc::new(Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|i| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                // Batches of different sizes, so ranges from different threads differ.
                let events = numbered_events(BATCHES * (i + 1));
                barrier.wait();
                for batch in events.chunks(i + 1) {
                    store.append(batch).expect("append");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread");
    }

    let mut next = 1;
    let mut count = 0;
    while let Ok(range) = rx.try_recv() {
        assert_eq!(
            range.from_rev, next,
            "ranges must be contiguous and increasing"
        );
        assert!(range.to_rev >= range.from_rev);
        next = range.to_rev + 1;
        count += 1;
    }
    assert_eq!(count, THREADS * BATCHES);
    assert_eq!(next - 1, store.latest_rev().expect("rev"));
}

#[test]
fn contains_reports_stored_and_unknown_ids() {
    let (_dir, store) = open();
    let events = fixture_events();
    assert!(!store.contains(events[0].id).expect("contains"));
    store.append(&events[..3]).expect("append");
    for e in &events[..3] {
        assert!(store.contains(e.id).expect("contains"), "{:?}", e.id);
    }
    for e in &events[3..] {
        assert!(!store.contains(e.id).expect("contains"), "{:?}", e.id);
    }
    // An id nothing ever stored.
    assert!(!store.contains(EventId::new()).expect("contains"));
}

#[test]
fn two_stores_on_one_file_append_concurrently() {
    const BATCHES: usize = 50;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let stores: Vec<Arc<Store>> = (0..2)
        .map(|_| Arc::new(Store::open(&path, StoreOptions::default()).expect("open")))
        .collect();
    let barrier = Arc::new(Barrier::new(stores.len()));
    let handles: Vec<_> = stores
        .iter()
        .map(|store| {
            let store = Arc::clone(store);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let events = numbered_events(BATCHES * 4);
                barrier.wait();
                for batch in events.chunks(4) {
                    store.append(batch).expect("append");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread");
    }
    let total = u64::try_from(BATCHES * 4 * stores.len()).expect("fits");
    for store in &stores {
        assert_eq!(store.latest_rev().expect("rev"), total);
    }
    let revs: Vec<u64> = stores[0]
        .since(0, 10_000)
        .expect("since")
        .iter()
        .map(|e| e.rev)
        .collect();
    assert_eq!(revs, (1..=total).collect::<Vec<_>>());
}
