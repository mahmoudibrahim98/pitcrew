//! `data/demo-recaps.json` holds the demo workspace's recaps as the recap engine writes them, and
//! the mock hub serves its recap routes from it. These tests fail when the file is stale; to
//! regenerate it after a change to the engine or the demo workspace, run
//!
//! ```text
//! PITCREW_UPDATE_FIXTURES=1 cargo test -p pitcrew-fixtures --test recaps
//! ```
//!
//! and review the diff like any snapshot.

use pitcrew_fixtures::{
    DemoRecaps, DemoWorkspace, ProjectDays, data_dir, demo_recaps, demo_workspace,
};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::EventId;
use pitcrew_protocol::model::Receipt;
use pitcrew_protocol::recap::{Block, RecapBlock};
use pitcrew_recap::{Config, Directory, RuleSummarizer, block_line, blocks, day_recaps};
use std::collections::{BTreeSet, HashSet};

const UPDATE: &str = "PITCREW_UPDATE_FIXTURES";

fn demo() -> DemoWorkspace {
    demo_workspace().expect("the demo workspace parses")
}

/// What the hub's projections know before the demo's slice of events, as in the recap engine's
/// own demo tests.
fn directory(ws: &DemoWorkspace) -> Directory {
    let mut dir = Directory::new();
    ws.members.iter().for_each(|m| dir.add_member(m));
    ws.workstreams.iter().for_each(|w| dir.add_workstream(w));
    ws.tasks.iter().for_each(|t| dir.add_task(t));
    ws.sessions.iter().for_each(|s| dir.add_session(s));
    ws.dispatches.iter().for_each(|d| dir.add_dispatch(d));
    ws.asks.iter().for_each(|a| dir.add_ask(a));
    dir
}

/// The recaps, freshly computed: every block with its line, and each project's days at UTC.
fn compute(ws: &DemoWorkspace) -> DemoRecaps {
    let dir = directory(ws);
    let all = blocks(&ws.events, &dir, &Config::default());
    let lines = all
        .iter()
        .map(|b| RecapBlock {
            block: b.clone(),
            line: block_line(b, &dir),
        })
        .collect();
    let projects: BTreeSet<_> = ws.projects.iter().map(|p| p.id).collect();
    let projects = projects
        .into_iter()
        .map(|project| {
            let mine: Vec<Block> = all
                .iter()
                .filter(|b| b.project == Some(project))
                .cloned()
                .collect();
            let days = day_recaps(&mine, &dir, 0, &RuleSummarizer).expect("rules never fail");
            ProjectDays { project, days }
        })
        .collect();
    DemoRecaps {
        tz: 0,
        blocks: lines,
        projects,
    }
}

fn pretty(recaps: &DemoRecaps) -> String {
    serde_json::to_string_pretty(recaps).expect("recaps serialize") + "\n"
}

#[test]
fn the_file_is_current() {
    let expected = pretty(&compute(&demo()));
    let path = data_dir().join("demo-recaps.json");
    if std::env::var_os(UPDATE).is_some() {
        std::fs::write(&path, &expected).expect("write demo-recaps.json");
        return;
    }
    let actual = std::fs::read_to_string(&path).expect("read demo-recaps.json");
    assert!(
        actual.replace("\r\n", "\n") == expected,
        "data/demo-recaps.json is stale: run `{UPDATE}=1 cargo test -p pitcrew-fixtures --test \
         recaps` and review the diff"
    );
}

#[test]
fn the_embedded_copy_parses_and_writes_back_unchanged() {
    let recaps = demo_recaps().expect("demo-recaps.json matches the protocol types");
    assert_eq!(
        pretty(&recaps),
        pitcrew_fixtures::DEMO_RECAPS_JSON.replace("\r\n", "\n")
    );
    assert_eq!(recaps.tz, 0);
    assert!(!recaps.blocks.is_empty());
}

/// The mock serves `?workstream=W` as W's entries among its project's days. That holds when they
/// are what the engine writes for W's blocks alone.
#[test]
fn a_workstreams_days_are_its_entries_among_its_projects() {
    let ws = demo();
    let dir = directory(&ws);
    let recaps = demo_recaps().expect("parses");
    let all: Vec<Block> = recaps.blocks.iter().map(|b| b.block.clone()).collect();
    for w in &ws.workstreams {
        let mine: Vec<Block> = all
            .iter()
            .filter(|b| b.workstream == Some(w.id))
            .cloned()
            .collect();
        let direct = day_recaps(&mine, &dir, 0, &RuleSummarizer).expect("rules never fail");
        let project = recaps
            .projects
            .iter()
            .find(|p| p.project == w.project)
            .expect("every project is listed");
        let derived: Vec<_> = project
            .days
            .iter()
            .filter(|d| d.workstream == Some(w.id))
            .cloned()
            .collect();
        assert_eq!(derived, direct, "{}", w.name);
    }
}

#[test]
fn every_block_with_a_project_is_in_exactly_one_of_its_days() {
    let recaps = demo_recaps().expect("parses");
    let mut placed: Vec<EventId> = recaps
        .projects
        .iter()
        .flat_map(|p| p.days.iter().flat_map(|d| d.blocks.iter().copied()))
        .collect();
    placed.sort();
    let mut expected: Vec<EventId> = recaps
        .blocks
        .iter()
        .filter(|b| b.block.project.is_some())
        .map(|b| b.block.id)
        .collect();
    expected.sort();
    assert_eq!(placed, expected);
    for p in &recaps.projects {
        for d in &p.days {
            for id in &d.blocks {
                let block = recaps.blocks.iter().find(|b| b.block.id == *id);
                assert_eq!(block.and_then(|b| b.block.project), Some(p.project));
            }
        }
    }
}

/// Every receipt points into the demo: at one of its events, or at something an event carries.
#[test]
fn every_receipt_points_into_the_demo() {
    let ws = demo();
    let mut known: HashSet<Receipt> = HashSet::new();
    for e in &ws.events {
        known.insert(Receipt::Event { id: e.id });
        match &e.body {
            EventBody::ToolRan { receipt, .. } | EventBody::TurnEnded { receipt, .. } => {
                known.insert(receipt.clone());
            }
            EventBody::AskRaised { ask } => known.extend(ask.receipts.iter().cloned()),
            EventBody::DecisionRecorded { receipts, .. }
            | EventBody::BriefAccepted { receipts, .. } => known.extend(receipts.iter().cloned()),
            _ => {}
        }
    }
    let recaps = demo_recaps().expect("parses");
    let lines = recaps.blocks.iter().map(|b| &b.line);
    let days = recaps
        .projects
        .iter()
        .flat_map(|p| p.days.iter().map(|d| &d.summary));
    for summary in lines.chain(days) {
        assert!(!summary.spans.is_empty(), "{:?} has no spans", summary.text);
        for span in &summary.spans {
            assert!(!summary.clause(span).is_empty(), "a span off a boundary");
            for r in &span.receipts {
                assert!(known.contains(r), "{r:?} is not in the demo");
            }
        }
    }
    for b in &recaps.blocks {
        for r in b.block.receipts() {
            assert!(known.contains(r), "{r:?} is not in the demo");
        }
    }
}
