//! `pitcrew_recap` over arbitrary event sequences: the block builder, the summaries and the
//! "where it stands" proposals. Event text is untrusted (titles, comments, tool targets, file
//! paths come from agents and transcripts), and recaps are what people read to decide.
//!
//! Input: a config byte (default caps, or small ones), a byte `k` and `k % 8` batch sizes, then
//! events as JSON lines (lines that are not an `Event` are skipped). Event ids are renumbered in
//! order, as the log would make them unique. The directory is the demo workspace's.
//!
//! Checks, besides "no panic":
//! - feeding the events in batches, and replaying the changes each batch reports, gives the same
//!   blocks as building them at once; no block closes twice or changes after closing;
//! - two runs give byte-identical blocks (internal maps use random seeds);
//! - every block is within its caps, and no event lands in two blocks;
//! - **every fact keeps its receipt**, and every receipt points into the input;
//! - every line, day paragraph and proposal **verifies**: each span cites receipts from its draft
//!   and from the input, and only joining punctuation lies outside spans.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::EventId;
use pitcrew_protocol::model::Receipt;
use pitcrew_recap::{
    Block, BlockBuilder, BriefPolicy, Config, Directory, RuleSummarizer, Summary, blocks, days,
    draft_line, draft_paragraph, propose_workstream, standing, verify,
};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

fn directory() -> &'static (Directory, Vec<pitcrew_protocol::ids::WorkstreamId>) {
    static DIR: OnceLock<(Directory, Vec<pitcrew_protocol::ids::WorkstreamId>)> = OnceLock::new();
    DIR.get_or_init(|| {
        let ws = pitcrew_fixtures::demo_workspace().expect("the demo workspace");
        let mut dir = Directory::new();
        ws.members.iter().for_each(|m| dir.add_member(m));
        ws.workstreams.iter().for_each(|w| dir.add_workstream(w));
        ws.tasks.iter().for_each(|t| dir.add_task(t));
        ws.sessions.iter().for_each(|s| dir.add_session(s));
        ws.dispatches.iter().for_each(|d| dir.add_dispatch(d));
        ws.asks.iter().for_each(|a| dir.add_ask(a));
        (dir, ws.workstreams.iter().map(|w| w.id).collect())
    })
}

fuzz_target!(|input: &[u8]| {
    let Some((&[c, k], rest)) = input.split_first_chunk::<2>() else {
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
    let (dir, workstreams) = directory();

    let all = blocks(&events, dir, &cfg);
    let again = blocks(&events, dir, &cfg);
    assert_eq!(
        serde_json::to_string(&all).expect("blocks serialize"),
        serde_json::to_string(&again).expect("blocks serialize"),
        "two runs differ"
    );

    // Batches.
    let mut builder = BlockBuilder::new(cfg.clone(), dir.clone());
    let mut plain = BlockBuilder::new(cfg.clone(), dir.clone());
    let mut replayed: HashMap<EventId, Block> = HashMap::new();
    let mut closed: HashSet<EventId> = HashSet::new();
    let mut rest: &[Event] = &events;
    let mut size = sizes.iter().map(|&s| 1 + usize::from(s % 16)).cycle();
    while !rest.is_empty() {
        let n = size.next().unwrap_or(1).min(rest.len());
        let (batch, tail) = rest.split_at(n);
        let changes = builder.push_batch(batch);
        for b in changes.closed {
            assert!(closed.insert(b.id), "a block closed twice");
            replayed.insert(b.id, b);
        }
        for b in changes.open {
            assert!(!closed.contains(&b.id), "a closed block changed");
            replayed.insert(b.id, b);
        }
        for e in batch {
            plain.push(e);
        }
        rest = tail;
    }
    let open = builder.open_blocks();
    assert!(open.len() <= cfg.max_open.max(1), "too many open blocks");
    for b in &open {
        assert_eq!(
            replayed.get(&b.id),
            Some(b),
            "an open block was not reported"
        );
    }
    let mut replayed: Vec<Block> = replayed.into_values().collect();
    replayed.sort_by_key(|b| b.id);
    let mut sorted = all.clone();
    sorted.sort_by_key(|b| b.id);
    assert_eq!(replayed, sorted, "batches give other blocks");
    assert_eq!(plain.finish(), all, "pushing one by one gives other blocks");

    // Caps.
    let placed: u64 = all.iter().map(|b| u64::from(b.counts.events)).sum();
    assert!(placed <= events.len() as u64, "an event in two blocks");
    let cfg = cfg.normalized();
    for b in &all {
        assert!(b.files.len() <= cfg.max_files);
        assert!(b.facts.len() <= cfg.max_facts);
        assert!(b.tasks.len() <= cfg.max_tasks);
        assert!(b.actors.len() <= cfg.max_actors);
        assert!(b.facts.iter().all(|f| f.receipts.len() <= cfg.max_receipts));
        assert!(b.start <= b.end);
    }

    // Receipts.
    let allowed = allowed_receipts(&events);
    for b in &all {
        for f in &b.facts {
            assert!(!f.receipts.is_empty(), "a fact without receipts: {f:?}");
        }
        for r in b.receipts() {
            assert!(allowed.contains(r), "a receipt not in the input: {r:?}");
        }
        let draft = draft_line(b, dir);
        let line = RuleSummarizer.render(&draft);
        assert_eq!(
            verify(&line, &draft),
            Ok(()),
            "a block line does not verify"
        );
        check_summary(&line, &allowed);
    }
    for day in days(&all, 0) {
        let draft = draft_paragraph(&day.blocks, dir);
        let paragraph = RuleSummarizer.render(&draft);
        assert_eq!(verify(&paragraph, &draft), Ok(()), "a day does not verify");
        check_summary(&paragraph, &allowed);
    }

    // Where it stands.
    let refs: Vec<&Block> = all.iter().collect();
    let mut seen = HashSet::new();
    let targets = workstreams
        .iter()
        .copied()
        .chain(all.iter().filter_map(|b| b.workstream));
    for w in targets.filter(|w| seen.insert(*w)).take(16) {
        let s = standing(w, &refs, dir);
        let policy = BriefPolicy {
            auto_accept_unpinned: true,
        };
        let proposal = propose_workstream(&s, None, policy, &RuleSummarizer)
            .unwrap_or_else(|e| panic!("a rule-made proposal fails: {e:?}"));
        if let Some(p) = proposal {
            check_summary(&p.summary, &allowed);
            if let Some(next) = &p.next {
                check_summary(next, &allowed);
            }
            assert!(!p.receipts.is_empty(), "a proposal without receipts");
            for r in &p.receipts {
                assert!(
                    allowed.contains(r),
                    "a proposal cites a receipt not in the input"
                );
            }
        }
    }
});

/// Every receipt the input can justify: its events, and the receipts the events carry.
fn allowed_receipts(events: &[Event]) -> HashSet<Receipt> {
    let mut out = HashSet::new();
    for e in events {
        out.insert(Receipt::Event { id: e.id });
        match &e.body {
            EventBody::ToolRan { receipt, .. } | EventBody::TurnEnded { receipt, .. } => {
                out.insert(receipt.clone());
            }
            EventBody::AskRaised { ask } => out.extend(ask.receipts.iter().cloned()),
            EventBody::DecisionRecorded { receipts, .. }
            | EventBody::BriefProposed { receipts, .. }
            | EventBody::BriefAccepted { receipts, .. } => out.extend(receipts.iter().cloned()),
            _ => {}
        }
    }
    out
}

/// Spans have receipts from the input and valid ranges; between spans there is only the
/// punctuation that joins clauses.
fn check_summary(s: &Summary, allowed: &HashSet<Receipt>) {
    let mut covered = vec![false; s.text.len()];
    for span in &s.spans {
        assert!(
            !span.receipts.is_empty(),
            "a span without receipts in {:?}",
            s.text
        );
        assert!(
            s.text.get(span.range.clone()).is_some(),
            "a bad range in {:?}",
            s.text
        );
        for r in &span.receipts {
            assert!(
                allowed.contains(r),
                "a span cites a receipt not in the input: {r:?}"
            );
        }
        for c in &mut covered[span.range.clone()] {
            *c = true;
        }
    }
    for (i, ch) in s.text.char_indices() {
        if !covered[i] {
            assert!(
                matches!(ch, ',' | '.' | ' '),
                "uncovered {ch:?} at {i} in {:?}",
                s.text
            );
        }
    }
}
