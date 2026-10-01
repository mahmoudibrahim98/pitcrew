//! The office over the demo workspace: a snapshot of its run log, and its actions applied through
//! a recording `Commands`.

mod common;

use common::{DAY, demo_config, demo_log, demo_names, run, show, tick};
use pitcrew_fixtures::demo_workspace;
use pitcrew_office::{Action, AskDraft, Commands, Office, Outcome, apply};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::model::Receipt;
use pitcrew_recap::BriefProposal;
use std::collections::HashSet;

fn demo() -> (pitcrew_fixtures::DemoWorkspace, Vec<Event>) {
    let ws = demo_workspace().expect("the demo workspace parses");
    let mut log = demo_log(&ws);
    // Four quiet days later: reminders and "paused?" questions come due.
    let later = tick(&log, 4 * DAY, 1);
    log.push(later);
    (ws, log)
}

#[test]
fn demo_run_log_snapshot() {
    let (ws, log) = demo();
    let mut office = Office::new(demo_config(&ws));
    let entries = run(&mut office, &log);
    let names = demo_names(&ws);
    let out: Vec<String> = entries.iter().map(|e| show(e, &names)).collect();
    insta::assert_snapshot!("demo_run_log", out.join("\n"));
}

/// Every receipt an action cites is an event of the log or a receipt one of them carries.
#[test]
fn every_action_cites_the_log() {
    let (ws, log) = demo();
    let mut allowed: HashSet<Receipt> = HashSet::new();
    for e in &log {
        allowed.insert(Receipt::Event { id: e.id });
        match &e.body {
            EventBody::ToolRan { receipt, .. } | EventBody::TurnEnded { receipt, .. } => {
                allowed.insert(receipt.clone());
            }
            EventBody::AskRaised { ask } => allowed.extend(ask.receipts.iter().cloned()),
            _ => {}
        }
    }
    let mut office = Office::new(demo_config(&ws));
    let entries = run(&mut office, &log);
    assert!(!entries.is_empty());
    for e in &entries {
        assert!(!e.action.receipts().is_empty(), "no evidence: {e:?}");
        for r in e.action.receipts() {
            assert!(allowed.contains(r), "receipt not in the log: {r:?}");
        }
    }
}

#[derive(Default)]
struct Recorder {
    appended: Vec<EventBody>,
    asks: Vec<AskDraft>,
    briefs: Vec<BriefProposal>,
}

impl Commands for Recorder {
    type Error = String;

    fn append(&mut self, body: &EventBody, because: &[Receipt]) -> Result<(), String> {
        assert!(!because.is_empty());
        self.appended.push(body.clone());
        Ok(())
    }

    fn raise_ask(&mut self, ask: &AskDraft) -> Result<(), String> {
        self.asks.push(ask.clone());
        Ok(())
    }

    fn propose_brief(&mut self, proposal: &BriefProposal) -> Result<(), String> {
        if self.briefs.len() >= 2 {
            return Err("full".into());
        }
        self.briefs.push(proposal.clone());
        Ok(())
    }
}

#[test]
fn emitted_actions_reach_commands_in_order() {
    let (ws, log) = demo();
    let mut office = Office::new(demo_config(&ws));
    let entries = run(&mut office, &log);
    let mut rec = Recorder::default();
    let results = apply(&entries, &mut rec);
    let emitted = entries
        .iter()
        .filter(|e| e.outcome == Outcome::Emitted)
        .count();
    assert_eq!(results.len(), emitted);
    let count = |f: fn(&Action) -> bool| entries.iter().filter(|e| f(&e.action)).count();
    assert_eq!(
        rec.appended.len(),
        count(|a| matches!(a, Action::Append { .. }))
    );
    assert_eq!(
        rec.asks.len(),
        count(|a| matches!(a, Action::RaiseAsk { .. }))
    );
    // The recorder refuses the third brief: its failure comes back, the others still apply.
    let briefs = count(|a| matches!(a, Action::ProposeBrief { .. }));
    assert!(briefs > 2);
    let failed = results.iter().filter(|r| r.is_err()).count();
    assert_eq!(failed, briefs - 2);
    assert!(
        rec.asks
            .iter()
            .all(|a| a.kind != pitcrew_protocol::model::AskKind::Approval)
    );
}
