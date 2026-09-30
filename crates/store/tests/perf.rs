//! Timings for the acceptance targets. Run with:
//! `cargo test -p pitcrew-store --release --test perf -- --ignored --nocapture`

use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::EventId;
use pitcrew_store::{Store, StoreOptions};
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

    println!("append 10,000 events in batches of 100: {append:?}");
    println!(
        "since(rev, 100) over {pages} pages: mean {:?}, worst {worst:?}",
        total / pages
    );
    assert!(append < Duration::from_secs(1), "append took {append:?}");
    assert!(worst < Duration::from_millis(5), "since took {worst:?}");
}
