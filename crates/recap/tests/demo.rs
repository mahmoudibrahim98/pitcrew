//! Blocks and summaries over the demo workspace's events, as snapshots.

mod common;

use common::{allowed_receipts, assert_block_receipts, assert_summary, show_receipt, show_summary};
use pitcrew_fixtures::{DemoWorkspace, demo_workspace};
use pitcrew_recap::{
    Block, BlockBuilder, BlockKey, Config, Directory, FakeSummarizer, RuleSummarizer, Summarizer,
    block_line, blocks, date_of, day_recaps, days, draft_line, draft_paragraph, verify,
};

fn demo() -> DemoWorkspace {
    demo_workspace().expect("the demo workspace parses")
}

/// What the hub's projections would know before the event slice.
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

fn demo_blocks() -> (DemoWorkspace, Directory, Vec<Block>) {
    let ws = demo();
    let dir = directory(&ws);
    let blocks = blocks(&ws.events, &dir, &Config::default());
    (ws, dir, blocks)
}

fn key(b: &Block, ws: &DemoWorkspace, dir: &Directory) -> String {
    match b.key {
        BlockKey::Session(s) => ws
            .sessions
            .iter()
            .find(|x| x.id == s)
            .and_then(|x| x.title.clone())
            .unwrap_or_else(|| s.to_string()),
        BlockKey::Workstream(w) => dir.workstream_name(w).unwrap_or("?").to_owned(),
        BlockKey::Project(p) => p.to_string(),
    }
}

#[test]
fn blocks_snapshot() {
    let (_, _, blocks) = demo_blocks();
    insta::assert_json_snapshot!("demo_blocks", blocks);
}

#[test]
fn lines_snapshot() {
    let (ws, dir, blocks) = demo_blocks();
    let mut out = String::new();
    for b in &blocks {
        out.push_str(&format!(
            "{} · {} · {}\n{}\n",
            date_of(b.start, 0).0,
            key(b, &ws, &dir),
            b.counts.events,
            show_summary(&block_line(b, &dir))
        ));
    }
    insta::assert_snapshot!("demo_lines", out);
}

#[test]
fn days_snapshot() {
    let (_, dir, blocks) = demo_blocks();
    let recaps = day_recaps(&blocks, &dir, 0, &RuleSummarizer).expect("rules never fail");
    let mut out = String::new();
    for r in &recaps {
        let ws = r
            .workstream
            .and_then(|w| dir.workstream_name(w))
            .unwrap_or("(no workstream)");
        let ids: Vec<String> = r
            .blocks
            .iter()
            .map(|id| show_receipt(&pitcrew_protocol::model::Receipt::Event { id: *id }))
            .collect();
        out.push_str(&format!(
            "{} · {ws} · blocks {}\n{}\n",
            r.date.0,
            ids.join(" "),
            show_summary(&r.summary)
        ));
    }
    insta::assert_snapshot!("demo_days", out);
}

#[test]
fn every_receipt_points_into_the_input() {
    let (ws, dir, blocks) = demo_blocks();
    let allowed = allowed_receipts(&ws.events);
    assert_block_receipts(&blocks, &allowed);
    for b in &blocks {
        let draft = draft_line(b, &dir);
        let line = RuleSummarizer.render(&draft);
        assert_eq!(verify(&line, &draft), Ok(()));
        assert_summary(&line, &allowed);
    }
    for day in days(&blocks, 0) {
        let draft = draft_paragraph(&day.blocks, &dir);
        let paragraph = RuleSummarizer.render(&draft);
        assert_eq!(verify(&paragraph, &draft), Ok(()));
        assert_summary(&paragraph, &allowed);
    }
}

#[test]
fn every_event_is_placed_or_skipped_for_a_reason() {
    let (ws, dir, blocks) = demo_blocks();
    let placed: u32 = blocks.iter().map(|b| b.counts.events).sum();
    let mut builder = BlockBuilder::new(Config::default(), dir);
    for e in &ws.events {
        builder.push(e);
    }
    // Only the brief proposal is not activity.
    assert_eq!(builder.skipped(), 1);
    assert_eq!(placed as usize + 1, ws.events.len());
}

#[test]
fn rules_are_deterministic() {
    let (_, dir, blocks) = demo_blocks();
    let (_, _, again) = demo_blocks();
    assert_eq!(blocks, again);
    let a = day_recaps(&blocks, &dir, 0, &RuleSummarizer).unwrap();
    let b = day_recaps(&again, &dir, 0, &RuleSummarizer).unwrap();
    assert_eq!(a, b);
}

/// Each map inside the engine hashes with its own random seed, so two independent runs iterate
/// their maps in different orders. Nothing in the output may follow that order: it is always
/// sorted first. Two runs must therefore give byte-identical JSON.
#[test]
fn output_never_depends_on_the_hash_seed() {
    let run = || {
        let (_, dir, blocks) = demo_blocks();
        let recaps = day_recaps(&blocks, &dir, 0, &RuleSummarizer).expect("rules never fail");
        (
            serde_json::to_string(&blocks).expect("blocks serialize"),
            serde_json::to_string(&recaps).expect("recaps serialize"),
        )
    };
    assert_eq!(run(), run());
}

#[test]
fn the_gap_decides_what_is_one_burst() {
    let ws = demo();
    let dir = directory(&ws);
    let short = blocks(&ws.events, &dir, &Config::default());
    let long = blocks(
        &ws.events,
        &dir,
        &Config {
            gap_ms: 30 * 60 * 1000,
            ..Config::default()
        },
    );
    // A 30-minute gap joins the method-section edit to its dispatch, and the queue check to the
    // divergence ask.
    assert_eq!(short.len(), 10);
    assert_eq!(long.len(), 8);
}

#[test]
fn a_fake_summarizer_is_verified_and_counted() {
    let (_, dir, blocks) = demo_blocks();
    let fake = FakeSummarizer::new();
    let recaps = day_recaps(&blocks, &dir, 0, &fake).unwrap();
    assert_eq!(fake.calls(), recaps.len());
    assert!(recaps.iter().all(|r| r.summary.text.starts_with("fake: ")));
    let failing = FakeSummarizer::failing();
    assert!(day_recaps(&blocks, &dir, 0, &failing).is_err());
    assert_eq!(failing.calls(), 1);
    // A summarizer that cites something the draft does not have is rejected.
    struct Liar;
    impl Summarizer for Liar {
        fn summarize(
            &self,
            draft: &pitcrew_recap::Draft,
        ) -> Result<pitcrew_recap::Summary, pitcrew_recap::SummaryError> {
            let mut s = RuleSummarizer.render(draft);
            if let Some(span) = s.spans.first_mut() {
                span.receipts = vec![pitcrew_protocol::model::Receipt::PullRequest {
                    url: "https://example.invalid/1".into(),
                }];
            }
            Ok(s)
        }
    }
    assert!(matches!(
        day_recaps(&blocks, &dir, 0, &Liar),
        Err(pitcrew_recap::SummaryError::UnknownReceipt(0))
    ));
}
