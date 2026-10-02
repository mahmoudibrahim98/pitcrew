//! The hub's recap index (`pitcrew_hub_work::Recaps`) over arbitrary event sequences, kept
//! current as events arrive in batches with queries in between, as `pitcrewd` keeps it. Event
//! text comes from agents and transcripts (B6, B7); recaps are what people, and agents, read.
//!
//! Input: a config byte (default caps, or small ones), a byte `k`, a day-cache byte, a byte of
//! query choices, `k % 8` batch sizes, then events as JSON lines (lines that are not an `Event`
//! are skipped). Event ids are renumbered in order, as the log would make them unique.
//!
//! Checks, besides "no panic":
//! - **incremental equals a rebuild**: the index fed in batches, with block and day queries
//!   between them (so the day cache holds paragraphs that later go stale), serves exactly what an
//!   index fed everything at once serves: every block page for every filter its blocks name, and
//!   every day page for every workstream and project at five time zones, paged to the start;
//! - the day cache holds at most its size; the same events fail the engine in both;
//! - paging ends: each block page moves back, and each days page moves to earlier dates;
//! - **hidden characters gone**: no line, paragraph or block text holds a character
//!   `pitcrew_fuzz::is_hidden_char` names (R33, fixed: the engine kept tag characters, the soft
//!   hyphen and U+180E, and U+2028/U+2029 in file paths). Receipts are not text: they are copied
//!   from the events they cite, as they are.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::is_hidden_char;
use pitcrew_hub_work::{BlockFilter, DaysScope, Recaps};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::{EventId, ProjectId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::Date;
use pitcrew_protocol::recap::{DayRecap, RecapBlock};
use pitcrew_recap::Config;
use std::collections::BTreeSet;

const ZONES: [i32; 5] = [0, 120, -300, 840, -840];
const CACHES: [usize; 4] = [0, 1, 5, 2048];

/// Everything an index serves for `filters` and `scopes`.
#[derive(Debug, PartialEq)]
struct Dump {
    blocks: Vec<Vec<RecapBlock>>,
    days: Vec<Vec<DayRecap>>,
}

fn all_blocks(index: &Recaps, filter: &BlockFilter, limit: usize, cap: usize) -> Vec<RecapBlock> {
    let mut out: Vec<RecapBlock> = Vec::new();
    let mut before: Option<EventId> = None;
    for _ in 0..=cap {
        let page = index
            .blocks(filter, before, Some(limit))
            .unwrap_or_else(|e| panic!("blocks: {e:?}"));
        if let (Some(b), Some(first)) = (before, page.blocks.first()) {
            assert!(first.block.id < b, "a block page that does not move back");
        }
        let last = page.blocks.last().map(|b| b.block.id);
        out.extend(page.blocks);
        if page.at_start {
            return out;
        }
        before = Some(last.expect("a page that is not at the start holds a block"));
    }
    panic!("block paging did not reach the start");
}

fn all_days(
    index: &mut Recaps,
    scope: DaysScope,
    tz: i32,
    limit: usize,
    cap: usize,
) -> Vec<DayRecap> {
    let mut out: Vec<DayRecap> = Vec::new();
    let mut before: Option<Date> = None;
    for _ in 0..=cap {
        let page = index
            .days(scope, tz, before.as_ref(), Some(limit))
            .unwrap_or_else(|e| panic!("days: {e:?}"));
        if let (Some(b), Some(first)) = (&before, page.days.first()) {
            assert!(first.date.0 < b.0, "a days page that does not move back");
        }
        let last = page.days.last().map(|d| d.date.clone());
        out.extend(page.days);
        if page.at_start {
            return out;
        }
        before = Some(last.expect("a page that is not at the start holds a day"));
    }
    panic!("day paging did not reach the start");
}

/// The filters and scopes the blocks name, and an unknown id of each kind.
fn names(blocks: &[RecapBlock]) -> (Vec<BlockFilter>, Vec<DaysScope>) {
    let unknown = format!("{:026}", 999_999);
    let mut sessions: BTreeSet<SessionId> = BTreeSet::new();
    let mut tasks: BTreeSet<TaskId> = BTreeSet::new();
    let mut workstreams: BTreeSet<WorkstreamId> = BTreeSet::new();
    let mut projects: BTreeSet<ProjectId> = BTreeSet::new();
    for b in blocks {
        sessions.extend(b.block.session);
        tasks.extend(b.block.tasks.iter().copied());
        workstreams.extend(b.block.workstream);
        projects.extend(b.block.project);
    }
    sessions.insert(unknown.parse().expect("an id"));
    tasks.insert(unknown.parse().expect("an id"));
    workstreams.insert(unknown.parse().expect("an id"));
    projects.insert(unknown.parse().expect("an id"));
    let mut filters = vec![BlockFilter::default()];
    filters.extend(sessions.iter().map(|&s| BlockFilter {
        session: Some(s),
        ..BlockFilter::default()
    }));
    filters.extend(tasks.iter().map(|&t| BlockFilter {
        task: Some(t),
        ..BlockFilter::default()
    }));
    filters.extend(workstreams.iter().map(|&w| BlockFilter {
        workstream: Some(w),
        ..BlockFilter::default()
    }));
    filters.extend(projects.iter().map(|&p| BlockFilter {
        project: Some(p),
        ..BlockFilter::default()
    }));
    if let (Some(&w), Some(&p)) = (workstreams.first(), projects.first()) {
        filters.push(BlockFilter {
            workstream: Some(w),
            project: Some(p),
            ..BlockFilter::default()
        });
    }
    let mut scopes: Vec<DaysScope> = workstreams
        .iter()
        .map(|&w| DaysScope::Workstream(w))
        .collect();
    scopes.extend(projects.iter().map(|&p| DaysScope::Project(p)));
    (filters, scopes)
}

fn dump(
    index: &mut Recaps,
    filters: &[BlockFilter],
    scopes: &[DaysScope],
    limit: usize,
    cap: usize,
) -> Dump {
    Dump {
        blocks: filters
            .iter()
            .map(|f| all_blocks(index, f, limit, cap))
            .collect(),
        days: scopes
            .iter()
            .flat_map(|&s| ZONES.map(|tz| (s, tz)))
            .map(|(s, tz)| all_days(index, s, tz, limit.min(30), cap))
            .collect(),
    }
}

fuzz_target!(|input: &[u8]| {
    let Some((&[c, k, cache, queries], rest)) = input.split_first_chunk::<4>() else {
        return;
    };
    let (sizes, text) = rest.split_at(usize::from(k % 8).min(rest.len()));
    let cfg = if c & 1 == 0 {
        Config::default()
    } else {
        Config {
            gap_ms: i64::from(c >> 1) * 60_000,
            max_open: 1 + usize::from(c >> 5),
            max_files: 2,
            max_facts: 3,
            max_tasks: 2,
            max_actors: 2,
            max_receipts: 2,
        }
    };
    let events: Vec<Event> = String::from_utf8_lossy(text)
        .lines()
        .filter_map(|l| serde_json::from_str::<Event>(l).ok())
        .zip(1u64..)
        .map(|(mut e, n)| {
            e.id = format!("{n:026}").parse::<EventId>().expect("an event id");
            e
        })
        .collect();
    let cache = CACHES[usize::from(cache) % CACHES.len()];
    let limit = 1 + usize::from(queries >> 3);

    let mut whole = Recaps::with_config(cfg.clone(), None);
    whole.push(&events);
    let everything = all_blocks(&whole, &BlockFilter::default(), 200, events.len() + 2);
    let (filters, scopes) = names(&everything);
    let cap = events.len() + 2;

    let mut batched = Recaps::with_config(cfg, None).with_day_cache(cache);
    let mut rest: &[Event] = &events;
    let mut size = sizes.iter().map(|&s| 1 + usize::from(s % 16)).cycle();
    let mut turn = 0usize;
    while !rest.is_empty() {
        let n = size.next().unwrap_or(1).min(rest.len());
        let (batch, tail) = rest.split_at(n);
        batched.push(batch);
        rest = tail;
        // Ask something between batches, so the cache holds days that may go stale.
        turn += 1;
        match (usize::from(queries) + turn) % 4 {
            0 => {
                let _ = all_blocks(&batched, &filters[turn % filters.len()], limit, cap);
            }
            1 if !scopes.is_empty() => {
                let scope = scopes[turn % scopes.len()];
                let _ = all_days(
                    &mut batched,
                    scope,
                    ZONES[turn % ZONES.len()],
                    limit.min(30),
                    cap,
                );
            }
            2 => {
                for &scope in &scopes {
                    let _ = all_days(&mut batched, scope, 0, 30, cap);
                }
            }
            _ => {}
        }
        assert!(
            batched.cached_days() <= cache,
            "the day cache is over its size"
        );
    }

    let got = dump(&mut batched, &filters, &scopes, limit, cap);
    let want = dump(&mut whole, &filters, &scopes, limit, cap);
    assert_eq!(batched.failed_events(), whole.failed_events());
    assert!(got == want, "batches give other recaps than a rebuild");
    assert!(
        batched.cached_days() <= cache,
        "the day cache is over its size"
    );

    let shown = serde_json::to_value((&got.blocks, &got.days)).expect("recaps serialize");
    check_text(&shown, "");
});

/// No hidden character in any text of the recaps. Receipts are left out: they cite the events'
/// own references (a commit, a job id, a link) and are copied as they are, to match them.
fn check_text(value: &serde_json::Value, key: &str) {
    match value {
        serde_json::Value::String(s) => {
            if let Some(c) = s.chars().find(|&c| is_hidden_char(c)) {
                panic!(
                    "a hidden character U+{:04X} in a recap's {key:?}: {s:?}",
                    u32::from(c)
                );
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| check_text(v, key)),
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if !k.ends_with("receipts") {
                    check_text(v, k);
                }
            }
        }
        _ => {}
    }
}
