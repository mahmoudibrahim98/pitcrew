//! Timings for the acceptance targets. Run with:
//! `cargo test -p pitcrew-store --release --test perf -- --ignored --nocapture`

use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::EventId;
use pitcrew_store::{EventFilter, Store, StoreOptions, event_type};
use std::time::{Duration, Instant};

#[test]
#[ignore = "timing; run in release"]
fn append_10k_and_page() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("store.db"), StoreOptions::default()).expect("open");
    let fixture = pitcrew_fixtures::demo_workspace().expect("fixture").events;
    let events: Vec<Event> = fixture
        .iter()
        .cycle()
        .take(10_000)
        .map(|e| {
            let mut e = e.clone();
            e.id = EventId::new();
            e
        })
        .collect();

    let start = Instant::now();
    for batch in events.chunks(100) {
        store.append(batch).expect("append");
    }
    let append = start.elapsed();
    assert_eq!(store.latest_rev().expect("rev"), 10_000);

    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    let mut pages = 0u32;
    for rev in (0..10_000).step_by(100) {
        let start = Instant::now();
        let page = store.since(rev, 100).expect("since");
        let took = start.elapsed();
        assert_eq!(page.len(), 100);
        worst = worst.max(took);
        total += took;
        pages += 1;
    }

    // Page back through the whole log by one type (dispatch_started, 1 in 5 events) and by two.
    let one = EventFilter::default().types([event_type(&fixture[0].body).expect("type")]);
    let two = EventFilter::default().types([
        event_type(&fixture[0].body).expect("type"),
        event_type(&fixture[6].body).expect("type"),
    ]);
    let mut filtered = Vec::new();
    for (label, filter) in [("one type", &one), ("two types", &two)] {
        let mut worst_f = Duration::ZERO;
        let mut total_f = Duration::ZERO;
        let mut pages_f = 0u32;
        let mut rev = u64::MAX;
        loop {
            let start = Instant::now();
            let page = store.before(rev, 100, filter).expect("before");
            let took = start.elapsed();
            worst_f = worst_f.max(took);
            total_f += took;
            pages_f += 1;
            match page.first() {
                Some(first) if page.len() == 100 => rev = first.rev,
                _ => break,
            }
        }
        filtered.push((label, pages_f, total_f / pages_f, worst_f));
    }

    println!("append 10,000 events in batches of 100: {append:?}");
    println!(
        "since(rev, 100) over {pages} pages: mean {:?}, worst {worst:?}",
        total / pages
    );
    for (label, pages, mean, worst) in &filtered {
        println!(
            "filtered before(rev, 100), {label}, {pages} pages: mean {mean:?}, worst {worst:?}"
        );
    }
    assert!(append < Duration::from_secs(1), "append took {append:?}");
    assert!(worst < Duration::from_millis(5), "since took {worst:?}");
    for (label, _, _, worst) in &filtered {
        assert!(
            *worst < Duration::from_millis(5),
            "filtered before ({label}) took {worst:?}"
        );
    }
}
