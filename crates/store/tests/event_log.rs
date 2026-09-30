use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::EventId;
use pitcrew_store::{EventFilter, RevRange, Store, StoreOptions, event_type};
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
    let empty = EventFilter::default().types(Vec::<String>::new());
    assert!(
        store
            .before(u64::MAX, 100, &empty)
            .expect("before")
            .is_empty()
    );
}

#[test]
fn a_failed_append_appends_nothing() {
    let (_dir, store) = open();
    let events = fixture_events();
    store.append(&events[..5]).expect("append");
    // The last event repeats an id that is already stored.
    let mut batch = events[5..].to_vec();
    batch.push(events[0].clone());
    store.append(&batch).expect_err("duplicate id");
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
