//! Properties: the office is deterministic and idempotent under replay, its saved state restores
//! exactly, and its caps hold under a flood.

mod common;

use common::{HOUR, Log, crafted};
use pitcrew_office::{
    Action, AskDraft, Config, Context, Entry, Office, Outcome, Rule, StateRow, default_rules,
};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::model::{AskKind, Receipt};
use proptest::collection::vec;
use proptest::prelude::*;
use std::collections::BTreeMap;

type Spec = (u8, u8, u8, bool, i64);

fn spec() -> impl Strategy<Value = Spec> {
    let dt = prop_oneof![
        6 => 0i64..HOUR,
        3 => HOUR..(30 * HOUR),
        1 => (2 * 24 * HOUR)..(5 * 24 * HOUR),
        1 => -HOUR..0,
    ];
    (any::<u8>(), any::<u8>(), any::<u8>(), any::<bool>(), dt)
}

fn crafted_log(specs: &[Spec]) -> Log {
    let mut log = Log::new();
    for &(kind, a, b, flag, dt) in specs {
        let (author, body) = crafted(&log.world, kind, a, b, flag);
        log.push(dt, author, body);
    }
    log
}

fn single_pass(config: &Config, events: &[Event]) -> Vec<Entry> {
    let mut office = Office::new(config.clone());
    events
        .iter()
        .zip(1u64..)
        .flat_map(|(e, rev)| office.on_event(rev, e))
        .collect()
}

/// Applies saved rows to a table, as the run log's projection does.
fn save(table: &mut BTreeMap<String, String>, rows: Vec<StateRow>) {
    for row in rows {
        match row.value {
            Some(v) => table.insert(row.key, v),
            None => table.remove(&row.key),
        };
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// The same events give the same entries, byte for byte; events already seen are ignored,
    /// so feeding batches that overlap (a retry after an unknown outcome) gives the same run log
    /// as one pass, and replaying the whole log again does nothing.
    #[test]
    fn replay_is_deterministic_and_idempotent(
        specs in vec(spec(), 0..150),
        sizes in vec(1usize..20, 1..20),
        overlaps in vec(0usize..5, 1..20),
    ) {
        let log = crafted_log(&specs);
        let config = log.world.config();
        let once = single_pass(&config, &log.events);
        let again = single_pass(&config, &log.events);
        prop_assert_eq!(
            serde_json::to_string(&once).expect("entries serialize"),
            serde_json::to_string(&again).expect("entries serialize")
        );

        let mut office = Office::new(config.clone());
        let mut fed = Vec::new();
        let mut next = 0usize;
        let mut size = sizes.iter().cycle();
        let mut overlap = overlaps.iter().cycle();
        let numbered: Vec<(u64, &Event)> = (1u64..).zip(&log.events).collect();
        while next < numbered.len() {
            let back = overlap.next().copied().unwrap_or(0).min(next);
            let end = (next + size.next().copied().unwrap_or(1)).min(numbered.len());
            for (rev, e) in &numbered[next - back..end] {
                fed.extend(office.on_event(*rev, e));
            }
            next = end;
        }
        prop_assert_eq!(&fed, &once);
        for (rev, e) in &numbered {
            prop_assert!(office.on_event(*rev, e).is_empty());
        }
    }

    /// Saving the state's changed rows and restoring from them, at any points, gives the same run
    /// log as an office that never stopped.
    #[test]
    fn saved_state_restores_exactly(
        specs in vec(spec(), 0..150),
        cuts in vec(any::<bool>(), 0..150),
    ) {
        let log = crafted_log(&specs);
        let config = log.world.config();
        let once = single_pass(&config, &log.events);

        let mut table = BTreeMap::new();
        let mut office = Office::new(config.clone());
        let mut entries = Vec::new();
        for (i, (e, rev)) in log.events.iter().zip(1u64..).enumerate() {
            entries.extend(office.on_event(rev, e));
            save(&mut table, office.take_changes().expect("state saves"));
            if cuts.get(i).copied().unwrap_or(false) {
                office = Office::restore(
                    config.clone(),
                    default_rules(),
                    table.iter().map(|(k, v)| (k.as_str(), v.as_str())),
                )
                .expect("state restores");
                prop_assert_eq!(office.rev(), rev);
            }
        }
        prop_assert_eq!(&entries, &once);
        // What it knows ends up the same too.
        let mut whole = Office::new(config);
        for (e, rev) in log.events.iter().zip(1u64..) {
            whole.on_event(rev, e);
        }
        prop_assert_eq!(office.world().sizes(), whole.world().sizes());
        prop_assert_eq!(office.now(), whole.now());
    }
}

/// A rule that wants `n` asks on every event.
struct Flood {
    name: &'static str,
    n: usize,
    to: pitcrew_protocol::ids::MemberId,
}

impl Rule for Flood {
    fn name(&self) -> &'static str {
        self.name
    }

    fn on_event(&mut self, _ctx: &mut Context<'_>, event: &Event) -> Vec<Action> {
        (0..self.n)
            .map(|i| Action::RaiseAsk {
                ask: AskDraft {
                    kind: AskKind::Question,
                    to: self.to,
                    task: None,
                    session: None,
                    title: format!("Question {i}"),
                    body: String::new(),
                    options: vec![],
                    receipts: vec![Receipt::Event { id: event.id }],
                },
            })
            .collect()
    }
}

/// The most entries in any hour-long window `(t - 1h, t]`, over the given times.
fn busiest_hour(times: &[i64]) -> usize {
    times
        .iter()
        .map(|t| times.iter().filter(|u| **u <= *t && **u > t - HOUR).count())
        .max()
        .unwrap_or(0)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Under a flood, no rule emits more than its cap in any hour and all rules together no more
    /// than the global cap; everything over is logged as capped, nothing is lost or applied.
    #[test]
    fn caps_hold_under_a_flood(
        per_rule in 1usize..8,
        global in 1usize..12,
        wants in (1usize..6, 1usize..6),
        gaps in vec(0i64..(10 * 60_000), 1..200),
    ) {
        let mut log = Log::new();
        let person = log.world.person;
        let config = Config { per_rule_per_hour: per_rule, global_per_hour: global, ..log.world.config() };
        for dt in &gaps {
            log.push(*dt, person, pitcrew_protocol::events::EventBody::MachineLiveness {
                machine: pitcrew_protocol::ids::MachineId(ulid::Ulid::from(1u128)),
                liveness: pitcrew_protocol::model::Liveness::Live,
            });
        }
        let rules: Vec<Box<dyn Rule>> = vec![
            Box::new(Flood { name: "flood_a", n: wants.0, to: person }),
            Box::new(Flood { name: "flood_b", n: wants.1, to: person }),
        ];
        let mut office = Office::with_rules(config, rules);
        let entries: Vec<Entry> = log
            .events
            .iter()
            .zip(1u64..)
            .flat_map(|(e, rev)| office.on_event(rev, e))
            .collect();
        prop_assert_eq!(entries.len(), log.events.len() * (wants.0 + wants.1));
        let emitted = |rule: Option<&str>| -> Vec<i64> {
            entries
                .iter()
                .filter(|e| e.outcome == Outcome::Emitted && rule.is_none_or(|r| e.rule == r))
                .map(|e| e.at)
                .collect()
        };
        prop_assert!(busiest_hour(&emitted(Some("flood_a"))) <= per_rule);
        prop_assert!(busiest_hour(&emitted(Some("flood_b"))) <= per_rule);
        prop_assert!(busiest_hour(&emitted(None)) <= global);
        for e in &entries {
            let logged = matches!(e.outcome, Outcome::Emitted | Outcome::Capped { .. });
            prop_assert!(logged, "{:?}", e.outcome);
        }
        // The first event's demand is met up to the caps.
        let first: Vec<&Entry> = entries.iter().filter(|e| e.rev == entries[0].rev).collect();
        let first_emitted = first.iter().filter(|e| e.outcome == Outcome::Emitted).count();
        prop_assert_eq!(
            first_emitted,
            (wants.0.min(per_rule) + wants.1.min(per_rule)).min(global)
        );
    }
}

/// The default rules under a flood of failing tests and divergence in many sessions within an
/// hour: their caps hold too.
#[test]
fn default_rules_hold_their_caps() {
    let mut log = Log::new();
    let (person, agent) = (log.world.person, log.world.agents[0]);
    let mut sessions = Vec::new();
    for n in 0..200u128 {
        let mut s = log.world.session(0);
        s.id = pitcrew_protocol::ids::SessionId(ulid::Ulid::from((7u128 << 96) | (100 + n)));
        sessions.push(s.id);
        log.push(
            0,
            person,
            pitcrew_protocol::events::EventBody::SessionDiscovered { session: s },
        );
    }
    for (i, s) in sessions.iter().enumerate() {
        log.push(
            1_000,
            agent,
            common::tool(*s, "train", "loss=nan", false, i as u64),
        );
    }
    let config = Config {
        per_rule_per_hour: 5,
        global_per_hour: 8,
        ..log.world.config()
    };
    let entries = single_pass(&config, &log.events);
    let emitted: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.outcome == Outcome::Emitted)
        .collect();
    let capped = entries
        .iter()
        .filter(|e| matches!(e.outcome, Outcome::Capped { .. }))
        .count();
    assert_eq!(emitted.len(), 5);
    assert_eq!(capped, 195);
    assert!(emitted.iter().all(|e| e.rule == "job_diverged"));
}
