//! `pitcrew_store::Store::import` on arbitrary export files (JSON lines, one event each). An
//! export may come from another machine, a support bundle, or a hand edit.
//!
//! Input: a byte `n`, then the file. `8 * n` valid event lines (demo events with fresh ids) come
//! first, so files longer than the import's 1,000-line batches are reached too (the import
//! appends in batches of 1,000 inside one transaction).
//!
//! Each input imports into a fresh, empty store (a copy of one made once, in local WAL mode).
//!
//! Checks, besides "no panic":
//! - **whole or nothing**: an import that fails leaves the store empty; one that succeeds holds
//!   exactly the file's events, in order (what `export` writes back decodes to the same events).
//!   R7 (a bad line after a full batch kept the batches before it) is fixed, and its input in
//!   `fuzz/regressions/store_import/` must pass;
//! - the store stays usable: its integrity check passes either way.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::scratch_path;
use pitcrew_protocol::events::Event;
use pitcrew_store::{FsMode, IntegrityReport, Store, StoreOptions};
use std::sync::OnceLock;

fn options() -> StoreOptions {
    let mut options = StoreOptions::default();
    options.fs = FsMode::Local;
    options
}

/// The bytes of a fresh, migrated, empty store.
fn template() -> &'static [u8] {
    static TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        let path = scratch_path("template.db");
        let _ = std::fs::remove_file(&path);
        drop(Store::open(&path, options()).expect("create the template store"));
        std::fs::read(&path).expect("read the template store")
    })
}

/// The lines the repetition byte adds: copies of a demo event, each with its own id (the log's
/// ids are unique, so copies with one id would fail the first batch, not a later one).
fn filler() -> &'static [String] {
    static LINES: OnceLock<Vec<String>> = OnceLock::new();
    LINES.get_or_init(|| {
        let demo = pitcrew_fixtures::demo_workspace().expect("the demo workspace");
        (0..255 * 8)
            .map(|i| {
                let mut e = demo.events[i % demo.events.len()].clone();
                e.id = format!("{:026}", 1_000_000 + i)
                    .parse()
                    .expect("an event id");
                serde_json::to_string(&e).expect("an event line")
            })
            .collect()
    })
}

fuzz_target!(|input: &[u8]| {
    let Some((&n, file)) = input.split_first() else {
        return;
    };
    let mut text = Vec::new();
    for line in &filler()[..usize::from(n) * 8] {
        text.extend_from_slice(line.as_bytes());
        text.push(b'\n');
    }
    text.extend_from_slice(file);

    let path = scratch_path("import.db");
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.clone().into_os_string();
        name.push(suffix);
        let _ = std::fs::remove_file(name);
    }
    std::fs::write(&path, template()).expect("copy the template");
    let store = Store::open(&path, options()).expect("open the copy");

    let result = store.import(text.as_slice());
    let held = store.latest_rev().expect("the store answers");
    match result {
        Ok(()) => {
            let expected: Vec<Event> = text
                .split(|&b| b == b'\n')
                .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
                .filter(|l| !l.is_empty())
                .map(|l| serde_json::from_slice(l).expect("an imported line decodes"))
                .collect();
            let mut exported = Vec::new();
            store.export(&mut exported).expect("export");
            let got: Vec<Event> = exported
                .split(|&b| b == b'\n')
                .filter(|l| !l.is_empty())
                .map(|l| serde_json::from_slice(l).expect("an exported line decodes"))
                .collect();
            assert_eq!(got, expected, "the import is not the file's events");
            assert_eq!(held, expected.len() as u64);
        }
        Err(_) => assert_eq!(held, 0, "a failed import left {held} events in the store"),
    }
    assert_eq!(
        store.integrity_check(false).expect("a check"),
        IntegrityReport::Ok
    );
});
