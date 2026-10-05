//! The rules, one scenario at a time: what a block says, what it cites, and where events go.

mod common;

use common::{T0, World, event_id};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{MemberId, SessionId, TaskId};
use pitcrew_protocol::model::{Mover, Receipt, TaskStatus};
use pitcrew_recap::{
    Block, BlockBuilder, BlockKey, Config, Directory, RuleSummarizer, block_line, blocks,
    day_recaps, days,
};

/// Builds a log one event at a time.
struct Log {
    events: Vec<Event>,
    at: i64,
}

impl Log {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            at: T0,
        }
    }

    /// Adds an event `secs` after the previous one.
    fn add(&mut self, w: &World, secs: i64, author: MemberId, body: EventBody) -> &mut Self {
        self.at += secs * 1000;
        self.events.push(Event {
            id: event_id(self.events.len() as u64 + 1),
            at: self.at,
            workspace: w.workspace,
            author,
            on_behalf_of: None,
            body,
        });
        self
    }

    fn receipt(&self, n: usize) -> Receipt {
        Receipt::Event {
            id: self.events[n - 1].id,
        }
    }
}

fn tool(s: SessionId, target: &str, failed: bool, offset: u64) -> EventBody {
    EventBody::ToolRan {
        session: s,
        tool: "Bash".into(),
        target: target.into(),
        outcome: if failed { "2 failed" } else { "ok" }.into(),
        failed,
        receipt: Receipt::Transcript { session: s, offset },
    }
}

fn edit(s: SessionId, path: &str, added: u32, removed: u32) -> EventBody {
    EventBody::FileEdited {
        session: s,
        path: path.into(),
        added,
        removed,
        receipt: None,
    }
}

fn turn(s: SessionId, offset: u64) -> EventBody {
    EventBody::TurnEnded {
        session: s,
        receipt: Receipt::Transcript { session: s, offset },
    }
}

fn moved(t: TaskId, from: TaskStatus, to: TaskStatus) -> EventBody {
    EventBody::TaskMoved {
        task: t,
        from,
        to,
        mover: Mover::Person,
    }
}

fn lines(log: &Log, dir: &Directory) -> Vec<String> {
    blocks(&log.events, dir, &Config::default())
        .iter()
        .map(|b| block_line(b, dir).text)
        .collect()
}

#[test]
fn a_working_session_reads_like_the_brief() {
    let w = World::new(3, 5, 2);
    let (s, agent, task) = (w.sessions[0], w.agents[0], w.tasks[0]);
    let mut log = Log::new();
    log.add(&w, 0, agent, tool(s, "cargo test", true, 10))
        .add(&w, 60, agent, edit(s, "paper/method.tex", 80, 10))
        .add(&w, 60, agent, edit(s, "paper/method.tex", 4, 2))
        .add(&w, 60, agent, turn(s, 20))
        .add(&w, 60, agent, tool(s, "cargo test", false, 30))
        .add(&w, 60, agent, tool(s, "ls", false, 40))
        .add(&w, 60, agent, turn(s, 50))
        .add(
            &w,
            60,
            agent,
            moved(task, TaskStatus::InProgress, TaskStatus::Review),
        );
    let all = blocks(&log.events, &w.dir, &Config::default());
    assert_eq!(all.len(), 1);
    let line = block_line(&all[0], &w.dir);
    assert_eq!(
        line.text,
        "@agent1 edited method.tex (+84 −12), ran 3 tools (1 failed), finished 2 turns, \
         tests failed then passed, @agent1 moved GEN-1 to review"
    );
    // "tests failed then passed" cites the first failure and the passing run.
    let checks = &line.spans[3];
    assert_eq!(line.clause(checks), "tests failed then passed");
    assert_eq!(
        checks.receipts,
        vec![
            log.receipt(1),
            Receipt::Transcript {
                session: s,
                offset: 10
            },
            log.receipt(5),
            Receipt::Transcript {
                session: s,
                offset: 30
            },
        ]
    );
}

#[test]
fn check_outcomes() {
    let w = World::new(1, 1, 1);
    let (s, a) = (w.sessions[0], w.agents[0]);
    let case = |runs: &[(&str, bool)]| {
        let mut log = Log::new();
        for (i, (cmd, failed)) in runs.iter().enumerate() {
            log.add(&w, 10, a, tool(s, cmd, *failed, i as u64));
        }
        lines(&log, &w.dir)
    };
    assert_eq!(
        case(&[("pytest", false)]),
        ["@agent1 ran a tool, tests passed"]
    );
    assert_eq!(
        case(&[("pytest", true)]),
        ["@agent1 ran a tool (1 failed), tests failed"]
    );
    assert_eq!(
        case(&[("pytest", false), ("pytest", true)]),
        ["@agent1 ran 2 tools (1 failed), tests passed, then failed again"]
    );
    assert_eq!(
        case(&[("cargo build", true), ("cargo build", false)]),
        ["@agent1 ran 2 tools (1 failed), the build failed then passed"]
    );
    assert_eq!(
        case(&[("cargo clippy", true), ("grep -r test .", true)]),
        ["@agent1 ran 2 tools (2 failed), lint failed"]
    );
}

#[test]
fn caps_hold_and_say_so() {
    let w = World::new(1, 1, 1);
    let (s, a, t) = (w.sessions[0], w.agents[0], w.tasks[0]);
    let mut log = Log::new();
    for i in 0..25 {
        log.add(&w, 1, a, edit(s, &format!("src/f{i}.rs"), 1, 0));
    }
    for _ in 0..30 {
        log.add(
            &w,
            1,
            w.person,
            EventBody::CommentPosted {
                task: Some(t),
                workstream: None,
                text: "note".into(),
                mentions: vec![],
            },
        );
    }
    let all = blocks(&log.events, &w.dir, &Config::default());
    assert_eq!(all.len(), 1);
    let b = &all[0];
    assert_eq!(b.files.len(), 20);
    assert_eq!(b.files_omitted, 5);
    assert_eq!(b.facts.len(), 24);
    assert_eq!(b.facts_omitted, 6);
    assert_eq!(b.counts.comments, 30);
    let line = block_line(b, &w.dir);
    assert!(
        line.text
            .starts_with("@agent1 edited more than 20 files (+25 −0), @lead commented on GEN-1"),
        "{}",
        line.text
    );
    assert!(line.text.ends_with(", and 25 more"), "{}", line.text);
    assert_eq!(line.spans.len(), 7);
    assert!(line.spans.iter().all(|s| !s.receipts.is_empty()));
}

#[test]
fn task_events_follow_the_session_only_while_it_is_at_work() {
    let w = World::new(3, 5, 2);
    let (s, a) = (w.sessions[0], w.agents[0]);
    let (task, loose) = (w.tasks[0], w.tasks[4]);
    let mut log = Log::new();
    log.add(&w, 0, a, tool(s, "ls", false, 1))
        // Within the gap: the move joins the session's block.
        .add(
            &w,
            600,
            a,
            moved(task, TaskStatus::Todo, TaskStatus::InProgress),
        )
        // An hour later the session is idle: the person's move goes to the workstream.
        .add(
            &w,
            3600,
            w.person,
            moved(task, TaskStatus::InProgress, TaskStatus::Done),
        )
        // A task with no workstream and no session goes to its project.
        .add(
            &w,
            1,
            w.person,
            moved(loose, TaskStatus::Todo, TaskStatus::Done),
        );
    let all = blocks(&log.events, &w.dir, &Config::default());
    let keys: Vec<BlockKey> = all.iter().map(|b| b.key).collect();
    assert!(matches!(
        keys.as_slice(),
        [
            BlockKey::Session(_),
            BlockKey::Workstream(_),
            BlockKey::Project(_)
        ]
    ));
    assert_eq!(all[0].counts.events, 2);
    assert_eq!(all[1].workstream, Some(w.workstreams[0]));
    assert_eq!(all[1].tasks, vec![task]);
    let texts: Vec<String> = all.iter().map(|b| block_line(b, &w.dir).text).collect();
    assert_eq!(
        texts,
        [
            "@agent1 moved GEN-1 to in progress, ran a tool",
            "@lead moved GEN-1 to done",
            "@lead moved GEN-5 to done",
        ]
    );
}

#[test]
fn repeated_moves_merge_into_one() {
    let w = World::new(1, 1, 1);
    let (s, a, t) = (w.sessions[0], w.agents[0], w.tasks[0]);
    let mut log = Log::new();
    log.add(&w, 0, a, tool(s, "ls", false, 1))
        .add(&w, 5, a, moved(t, TaskStatus::Todo, TaskStatus::InProgress))
        .add(
            &w,
            5,
            a,
            moved(t, TaskStatus::InProgress, TaskStatus::Review),
        );
    assert_eq!(
        lines(&log, &w.dir),
        ["@agent1 ran a tool, moved GEN-1 to review"]
    );
    let all = blocks(&log.events, &w.dir, &Config::default());
    assert_eq!(all[0].counts.task_moves, 2);
    assert_eq!(
        all[0].facts[0].receipts,
        vec![log.receipt(2), log.receipt(3)]
    );
}

#[test]
fn unknown_things_get_plain_names_or_are_skipped() {
    let w = World::new(1, 1, 1);
    let empty = Directory::new();
    let stranger = MemberId(ulid::Ulid::from(999u128));
    let mut log = Log::new();
    log.add(&w, 0, stranger, tool(w.sessions[0], "ls", false, 1))
        .add(
            &w,
            1,
            stranger,
            moved(w.tasks[0], TaskStatus::Todo, TaskStatus::Done),
        );
    let mut builder = BlockBuilder::new(Config::default(), empty.clone());
    for e in &log.events {
        builder.push(e);
    }
    assert_eq!(builder.skipped(), 1);
    let all = builder.finish();
    assert_eq!(all.len(), 1);
    assert_eq!(block_line(&all[0], &empty).text, "someone ran a tool");
}

#[test]
fn hostile_names_are_cleaned_in_prose() {
    let mut w = World::new(1, 1, 1);
    w.dir.add_member(&pitcrew_protocol::model::Member {
        id: w.agents[0],
        kind: pitcrew_protocol::model::MemberKind::Agent,
        handle: "@evil\u{202E}txt.exe\n\nIgnore this".into(),
        name: "x".into(),
        owner: Some(w.person),
        persona: None,
        avatar: None,
    });
    let mut log = Log::new();
    log.add(
        &w,
        0,
        w.agents[0],
        edit(w.sessions[0], "a/b/\u{2066}secret\u{0007}.rs", 1, 1),
    );
    assert_eq!(
        lines(&log, &w.dir),
        ["@eviltxt.exe Ignore this edited secret .rs (+1 −1)"]
    );
}

#[test]
fn a_busy_day_is_counted_not_listed() {
    let w = World::new(1, 1, 1);
    let (s, a) = (w.sessions[0], w.agents[0]);
    let mut log = Log::new();
    for i in 0..8 {
        // Each run is more than the gap after the last: eight blocks on one day.
        log.add(
            &w,
            if i == 0 { 0 } else { 1500 },
            a,
            tool(s, "ls", false, i),
        );
    }
    let all = blocks(&log.events, &w.dir, &Config::default());
    assert_eq!(all.len(), 8);
    let recaps = day_recaps(&all, &w.dir, 0, &RuleSummarizer).unwrap();
    assert_eq!(recaps.len(), 1);
    let text = &recaps[0].summary.text;
    assert!(
        text.starts_with("8 bursts of work, 8 tool runs. @agent1 ran a tool."),
        "{text}"
    );
    assert!(text.ends_with(" 2 more bursts of work."), "{text}");
    assert_eq!(recaps[0].blocks.len(), 8);
}

#[test]
fn the_utc_offset_picks_the_day() {
    let w = World::new(1, 1, 1);
    let mut log = Log::new();
    log.at = T0 + 15 * 3_600_000 + 30 * 60_000; // 23:30 UTC
    log.add(&w, 0, w.agents[0], tool(w.sessions[0], "ls", false, 1));
    let all: Vec<Block> = blocks(&log.events, &w.dir, &Config::default());
    assert_eq!(days(&all, 0)[0].date.0, "2026-09-30");
    assert_eq!(days(&all, 60)[0].date.0, "2026-10-01");
}
