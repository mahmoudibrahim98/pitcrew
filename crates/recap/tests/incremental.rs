//! Properties of block building over generated events: batching never changes the blocks, every
//! receipt points into the input, and no input makes anything panic.

mod common;

use common::{
    T0, World, allowed_receipts, assert_block_receipts, assert_summary, event_id, gen_events,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{DispatchId, EventId, SessionId};
use pitcrew_protocol::model::{
    Ask, AskKind, AskState, DispatchOutcome, Receipt, Scheduler, SessionState,
};
use pitcrew_recap::{
    Block, BlockBuilder, Config, RuleSummarizer, blocks, day_recaps, days, draft_line,
    draft_paragraph, verify,
};
use proptest::collection::vec;
use proptest::prelude::*;
use std::collections::{HashMap, HashSet};

fn spec() -> impl Strategy<Value = common::Spec> {
    let dt = prop_oneof![
        6 => 0i64..300_000,
        3 => 300_000i64..2_400_000,
        1 => 3_600_000i64..86_400_000,
        1 => -600_000i64..0,
    ];
    (any::<u8>(), any::<u8>(), any::<u8>(), any::<bool>(), dt)
}

fn config() -> impl Strategy<Value = Config> {
    (1i64..40, any::<bool>()).prop_map(|(minutes, small)| {
        let gap_ms = minutes * 60_000;
        if small {
            Config {
                gap_ms,
                max_open: 3,
                max_files: 2,
                max_facts: 3,
                max_tasks: 2,
                max_actors: 2,
                max_receipts: 2,
            }
        } else {
            Config {
                gap_ms,
                ..Config::default()
            }
        }
    })
}

fn is_hidden(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn sorted(mut blocks: Vec<Block>) -> Vec<Block> {
    blocks.sort_by(|a, b| (a.start, a.id).cmp(&(b.start, b.id)));
    blocks
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Feeding events in random batches, and replaying the changes each batch reports, gives the
    /// same blocks as building from all the events at once.
    #[test]
    fn batches_give_the_same_blocks(
        specs in vec(spec(), 0..250),
        sizes in vec(1usize..25, 1..40),
        cfg in config(),
    ) {
        let world = World::new(5, 7, 3);
        let events = gen_events(&specs, &world, T0, 1);
        let all = blocks(&events, &world.dir, &cfg);

        let mut builder = BlockBuilder::new(cfg.clone(), world.dir.clone());
        let mut plain = BlockBuilder::new(cfg.clone(), world.dir.clone());
        let mut replayed: HashMap<EventId, Block> = HashMap::new();
        let mut closed: HashSet<EventId> = HashSet::new();
        let mut rest: &[Event] = &events;
        let mut size = sizes.iter().cycle();
        while !rest.is_empty() {
            let n = size.next().copied().unwrap_or(1).min(rest.len());
            let (batch, tail) = rest.split_at(n);
            let changes = builder.push_batch(batch);
            for b in changes.closed {
                prop_assert!(closed.insert(b.id), "block closed twice");
                replayed.insert(b.id, b);
            }
            for b in changes.open {
                prop_assert!(!closed.contains(&b.id), "a closed block changed");
                replayed.insert(b.id, b);
            }
            for e in batch {
                plain.push(e);
            }
            rest = tail;
        }
        let open = builder.open_blocks();
        prop_assert!(open.len() <= cfg.max_open);
        for b in &open {
            prop_assert_eq!(replayed.get(&b.id), Some(b));
        }
        prop_assert_eq!(sorted(replayed.into_values().collect()), all.clone());
        prop_assert_eq!(plain.finish(), all.clone());

        // Every event lands in exactly one block, or is skipped.
        let placed: u64 = all.iter().map(|b| u64::from(b.counts.events)).sum();
        prop_assert!(placed <= events.len() as u64);

        // Caps hold.
        for b in &all {
            prop_assert!(b.files.len() <= cfg.max_files);
            prop_assert!(b.facts.len() <= cfg.max_facts);
            prop_assert!(b.tasks.len() <= cfg.max_tasks);
            prop_assert!(b.actors.len() <= cfg.max_actors);
            prop_assert!(b.facts.iter().all(|f| f.receipts.len() <= cfg.max_receipts.max(2)));
            prop_assert!(b.start <= b.end);
        }

        // Receipts point into the input, and summaries cite only receipts.
        let allowed = allowed_receipts(&events);
        assert_block_receipts(&all, &allowed);
        for b in &all {
            let draft = draft_line(b, &world.dir);
            let line = RuleSummarizer.render(&draft);
            prop_assert_eq!(verify(&line, &draft), Ok(()));
            assert_summary(&line, &allowed);
        }
        for day in days(&all, 0) {
            let draft = draft_paragraph(&day.blocks, &world.dir);
            let paragraph = RuleSummarizer.render(&draft);
            prop_assert_eq!(verify(&paragraph, &draft), Ok(()));
            assert_summary(&paragraph, &allowed);
        }
    }

    /// Hostile text and timestamps never panic, stay within the caps, and still give valid
    /// receipts and summaries.
    #[test]
    fn untrusted_input_is_safe(
        texts in vec(any::<String>(), 1..12),
        times in vec(any::<i64>(), 1..40),
        kinds in vec(0u8..8, 1..40),
        big in any::<u32>(),
    ) {
        let world = World::new(3, 3, 2);
        let s = world.sessions[0];
        let text = |i: usize| texts[i % texts.len()].clone();
        let events: Vec<Event> = kinds
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let at = times[i % times.len()];
                let body = match k {
                    0 => EventBody::ToolRan {
                        session: s,
                        tool: text(i),
                        target: text(i + 1),
                        outcome: text(i + 2),
                        failed: i % 2 == 0,
                        receipt: Receipt::Transcript { session: s, offset: u64::from(big) },
                    },
                    1 => EventBody::FileEdited {
                        session: s,
                        path: text(i),
                        added: big,
                        removed: u32::MAX,
                        receipt: None,
                    },
                    2 => EventBody::AskRaised {
                        ask: Ask {
                            id: world.asks[0],
                            kind: AskKind::Decision,
                            from: world.agents[0],
                            to: world.person,
                            task: None,
                            session: Some(SessionId(ulid::Ulid::from(u128::from(big)))),
                            title: text(i),
                            body: text(i + 1),
                            options: vec![],
                            receipts: vec![Receipt::Job { scheduler: Scheduler::Slurm, id: text(i + 2) }],
                            state: AskState::Open,
                            answer: None,
                            created: at,
                        },
                    },
                    3 => EventBody::SessionDiscovered {
                        session: common::session(s, world.agents[0], None, Some(text(i))),
                    },
                    4 => EventBody::SessionStateChanged {
                        session: s,
                        from: SessionState::Working,
                        to: SessionState::Waiting,
                        status_line: Some(text(i)),
                    },
                    5 => EventBody::DispatchFinished {
                        dispatch: DispatchId(ulid::Ulid::from(u128::from(big))),
                        outcome: DispatchOutcome::Failed,
                        summary: Some(text(i)),
                    },
                    6 => EventBody::DecisionRecorded {
                        workstream: Some(world.workstreams[0]),
                        text: text(i),
                        why: None,
                        receipts: vec![Receipt::Commit { repo: text(i + 1), sha: text(i + 2) }],
                    },
                    _ => EventBody::CommentPosted {
                        task: None,
                        workstream: Some(world.workstreams[1]),
                        text: text(i),
                        mentions: vec![world.person; 100],
                    },
                };
                Event {
                    id: event_id(i as u64 + 1),
                    at,
                    workspace: world.workspace,
                    author: world.agents[0],
                    on_behalf_of: None,
                    body,
                }
            })
            .collect();
        let cfg = Config::default();
        let all = blocks(&events, &world.dir, &cfg);
        let allowed = allowed_receipts(&events);
        assert_block_receipts(&all, &allowed);
        for b in &all {
            for f in &b.files {
                prop_assert!(f.path.chars().count() <= 200);
            }
            let line = RuleSummarizer.render(&draft_line(b, &world.dir));
            assert_summary(&line, &allowed);
            prop_assert!(line.text.len() < 8_000, "line too long: {}", line.text.len());
            let hidden = line.text.chars().any(is_hidden);
            prop_assert!(!hidden, "hidden characters in {:?}", line.text);
        }
        for offset in [i32::MIN, 0, i32::MAX] {
            let recaps = day_recaps(&all, &world.dir, offset, &RuleSummarizer);
            prop_assert!(recaps.is_ok());
            for r in recaps.unwrap_or_default() {
                prop_assert!(r.date.is_well_formed());
                assert_summary(&r.summary, &allowed);
            }
        }
        let mut builder = BlockBuilder::new(cfg, world.dir.clone());
        builder.push_batch(&events);
        builder.close_idle(i64::MAX);
        builder.close_idle(i64::MIN);
        prop_assert!(builder.open_blocks().is_empty());
    }
}

#[test]
fn close_idle_closes_quiet_blocks_and_late_events_start_new_ones() {
    let world = World::new(2, 2, 1);
    // Two tool runs in one session, a minute apart.
    let specs = [(0u8, 0u8, 1u8, false, 0i64), (0, 0, 1, false, 60_000)];
    let events = gen_events(&specs, &world, T0, 1);
    let mut b = BlockBuilder::new(Config::default(), world.dir.clone());
    let changes = b.push_batch(&events[..1]);
    assert_eq!(changes.open.len(), 1);
    assert!(changes.closed.is_empty());
    // Nothing is idle yet at the time of the first event plus the gap.
    b.close_idle(T0 + Config::default().gap_ms);
    assert!(b.take_changes().closed.is_empty());
    b.close_idle(T0 + Config::default().gap_ms + 1);
    let changes = b.take_changes();
    assert_eq!(changes.closed.len(), 1);
    assert!(changes.open.is_empty());
    // The second event is within the gap of the first, but its block was closed on a timer.
    let changes = b.push_batch(&events[1..]);
    assert_eq!(changes.open.len(), 1);
    assert_ne!(changes.open[0].id, events[0].id);
    assert_eq!(blocks(&events, &world.dir, &Config::default()).len(), 1);
}

#[test]
fn max_open_closes_the_longest_idle() {
    let world = World::new(3, 3, 1);
    // One tool run in each of three sessions.
    let specs = [
        (0u8, 0u8, 4u8, false, 0i64),
        (0, 1, 4, false, 1_000),
        (0, 2, 4, false, 1_000),
    ];
    let events = gen_events(&specs, &world, T0, 1);
    let cfg = Config {
        max_open: 2,
        ..Config::default()
    };
    let mut b = BlockBuilder::new(cfg, world.dir.clone());
    let changes = b.push_batch(&events);
    assert_eq!(changes.closed.len(), 1);
    assert_eq!(changes.closed[0].id, events[0].id);
    assert_eq!(b.open_blocks().len(), 2);
}
