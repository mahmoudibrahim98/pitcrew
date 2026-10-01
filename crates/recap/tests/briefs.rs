//! "Where it stands" proposals: snapshots over the demo workspace, one scenario per rule, and a
//! property test that every claim over any input cites receipts from that input.

mod common;

use common::{
    T0, World, allowed_receipts, assert_summary, demo_directory, event_id, gen_events,
    show_receipt, show_summary,
};
use pitcrew_fixtures::{DemoWorkspace, demo_workspace};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{AskId, MemberId, SessionId, SubtaskId, TaskId};
use pitcrew_protocol::model::{
    Answer, Ask, AskKind, AskState, Brief, BriefSource, BriefTarget, Mover, Receipt, Subtask,
    SubtaskSource, TaskPatch, TaskStatus,
};
use pitcrew_recap::{
    Block, BriefPolicy, BriefProposal, Config, Directory, Disposition, FakeSummarizer,
    RuleSummarizer, Standing, Urgency, blocks, draft_brief, propose_paused, propose_project,
    propose_workstream, standing, verify,
};
use proptest::collection::vec;
use proptest::prelude::*;
use std::collections::HashSet;

const AUTO: BriefPolicy = BriefPolicy {
    auto_accept_unpinned: true,
};

fn demo() -> (DemoWorkspace, Directory, Vec<Block>) {
    let ws = demo_workspace().expect("the demo workspace parses");
    let dir = demo_directory(&ws);
    let all = blocks(&ws.events, &dir, &Config::default());
    (ws, dir, all)
}

fn current(ws: &DemoWorkspace, target: BriefTarget) -> Option<&Brief> {
    ws.briefs.iter().find(|b| b.target == target)
}

/// Checks a proposal: the summary and the next step verify, every character outside a span is
/// punctuation, and every receipt points into the input.
fn check(p: &BriefProposal, allowed: &HashSet<Receipt>) {
    assert_summary(&p.summary, allowed);
    if let Some(n) = &p.next {
        assert_summary(n, allowed);
    }
    assert!(!p.receipts.is_empty());
    for r in &p.receipts {
        assert!(allowed.contains(r), "receipt not in the input: {r:?}");
    }
    let EventBody::BriefProposed {
        target,
        text,
        next,
        receipts,
    } = p.body()
    else {
        panic!("body is a brief proposal");
    };
    assert_eq!(target, p.target);
    assert_eq!(text, p.summary.text);
    assert_eq!(next.as_deref(), p.next.as_ref().map(|n| n.text.as_str()));
    assert!(!text.contains("Next:"), "the next step has its own field");
    assert_eq!(receipts, p.receipts);
    // Applying it automatically accepts it unchanged: same text and next step, its receipts.
    match p.accepted_body() {
        Some(EventBody::BriefAccepted {
            target: t,
            text: accepted,
            next: accepted_next,
            pinned,
            receipts: r,
        }) => {
            assert_eq!(p.disposition, Disposition::AutoAccept);
            assert_eq!((t, accepted, accepted_next), (target, text, next));
            assert!(!pinned);
            assert_eq!(r, receipts);
        }
        Some(other) => panic!("not an acceptance: {other:?}"),
        None => assert_eq!(p.disposition, Disposition::Propose),
    }
}

fn show(label: &str, p: &BriefProposal) -> String {
    let mut out = format!("== {label} · {:?}\n", p.disposition);
    out.push_str(&show_summary(&p.summary));
    if let Some(n) = &p.next {
        out.push_str("next: ");
        out.push_str(&show_summary(n));
    }
    out
}

fn demo_standings(ws: &DemoWorkspace, dir: &Directory, all: &[Block]) -> Vec<Standing> {
    let refs: Vec<&Block> = all.iter().collect();
    ws.workstreams
        .iter()
        .map(|w| standing(w.id, &refs, dir))
        .collect()
}

#[test]
fn demo_proposals_snapshot() {
    let (ws, dir, all) = demo();
    let allowed = allowed_receipts(&ws.events);
    let standings = demo_standings(&ws, &dir, &all);
    let mut out = String::new();
    for s in &standings {
        let target = BriefTarget::Workstream(s.workstream);
        let p = propose_workstream(s, current(&ws, target), AUTO, &RuleSummarizer)
            .expect("rules never fail");
        match p {
            Some(p) => {
                check(&p, &allowed);
                out.push_str(&show(&s.name, &p));
            }
            None => out.push_str(&format!("== {} · nothing to propose\n", s.name)),
        }
        out.push('\n');
    }
    for project in &ws.projects {
        let target = BriefTarget::Project(project.id);
        let refs: Vec<&Standing> = standings.iter().collect();
        let p = propose_project(
            project.id,
            &refs,
            current(&ws, target),
            AUTO,
            &RuleSummarizer,
        )
        .expect("rules never fail")
        .expect("each demo project has activity");
        check(&p, &allowed);
        out.push_str(&show(&project.name, &p));
        out.push('\n');
    }
    insta::assert_snapshot!("demo_proposals", out);
}

#[test]
fn a_pinned_brief_only_gets_a_proposal() {
    let (ws, dir, all) = demo();
    let standings = demo_standings(&ws, &dir, &all);
    for s in &standings {
        let target = BriefTarget::Workstream(s.workstream);
        let Some(brief) = current(&ws, target) else {
            continue;
        };
        let Some(p) = propose_workstream(s, Some(brief), AUTO, &RuleSummarizer).unwrap() else {
            continue;
        };
        if brief.pinned {
            assert_eq!(p.disposition, Disposition::Propose);
            assert_eq!(p.accepted_body(), None);
        } else {
            assert_eq!(p.disposition, Disposition::AutoAccept);
            assert!(matches!(
                p.accepted_body(),
                Some(EventBody::BriefAccepted { pinned: false, .. })
            ));
        }
        // Without the policy, nothing is applied automatically.
        let p = propose_workstream(s, Some(brief), BriefPolicy::default(), &RuleSummarizer)
            .unwrap()
            .expect("same standing");
        assert_eq!(p.disposition, Disposition::Propose);
    }
    // The fixture's "Seed runs" brief is pinned, and it has activity.
    assert!(ws.briefs.iter().any(|b| b.pinned));
}

#[test]
fn a_proposal_that_says_what_is_in_force_is_not_made() {
    let (ws, dir, all) = demo();
    let standings = demo_standings(&ws, &dir, &all);
    let s = standings
        .iter()
        .find(|s| !s.is_empty())
        .expect("some standing");
    let target = BriefTarget::Workstream(s.workstream);
    let p = propose_workstream(s, None, AUTO, &RuleSummarizer)
        .unwrap()
        .expect("a proposal");
    let same = Brief {
        target,
        text: p.text().to_owned(),
        next: p.next_text().map(str::to_owned),
        pinned: false,
        source: BriefSource::BackOffice,
        updated: 0,
        receipts: p.receipts.clone(),
        proposal: None,
    };
    let again = |brief: &Brief| propose_workstream(s, Some(brief), AUTO, &RuleSummarizer).unwrap();
    assert_eq!(again(&same), None);

    // The same text with another next step is news.
    let other_next = Brief {
        next: Some("Something else.".into()),
        ..same.clone()
    };
    assert_eq!(again(&other_next), Some(p.clone()));

    // While it waits as the pending proposal of an older brief, it is not proposed again.
    let older = Brief {
        text: "An older brief.".into(),
        next: None,
        proposal: Some(pitcrew_protocol::model::BriefProposal {
            text: p.text().to_owned(),
            next: p.next_text().map(str::to_owned),
            receipts: p.receipts.clone(),
            at: 0,
        }),
        ..same.clone()
    };
    assert_eq!(again(&older), None);
    let without = Brief {
        proposal: None,
        ..older
    };
    assert_eq!(again(&without), Some(p));
}

#[test]
fn a_model_summarizer_is_verified_and_its_failure_surfaces() {
    let (ws, dir, all) = demo();
    let standings = demo_standings(&ws, &dir, &all);
    let s = standings.iter().find(|s| s.next.is_some()).expect("a next");
    let fake = FakeSummarizer::new();
    let p = propose_workstream(s, None, AUTO, &fake)
        .unwrap()
        .expect("a proposal");
    assert!(p.summary.text.starts_with("fake: "));
    assert_eq!(
        fake.calls(),
        2,
        "one call for the summary, one for the next step"
    );
    let failing = FakeSummarizer::failing();
    assert!(propose_workstream(s, None, AUTO, &failing).is_err());
    let draft = draft_brief(s);
    assert_eq!(verify(&RuleSummarizer.render(&draft), &draft), Ok(()));
}

// ─── Scenarios ───────────────────────────────────────────────────────────────────────────────

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

    fn add(&mut self, w: &World, author: MemberId, body: EventBody) -> &mut Self {
        self.at += 60_000;
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

    fn standing(&self, w: &World) -> Standing {
        let all = blocks(&self.events, &w.dir, &Config::default());
        let refs: Vec<&Block> = all.iter().collect();
        standing(w.workstreams[0], &refs, &w.dir)
    }
}

fn tool(s: SessionId, target: &str, failed: bool) -> EventBody {
    EventBody::ToolRan {
        session: s,
        tool: "Bash".into(),
        target: target.into(),
        outcome: if failed { "2 failed" } else { "ok" }.into(),
        failed,
        receipt: Receipt::Transcript {
            session: s,
            offset: 1,
        },
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

fn plan(t: TaskId, agent: MemberId, done: usize, total: usize) -> EventBody {
    EventBody::SubtasksReplaced {
        task: t,
        subtasks: (0..total)
            .map(|k| Subtask {
                id: SubtaskId(ulid::Ulid::from(k as u128 + 1)),
                text: format!("step {k}"),
                done: k < done,
                source: SubtaskSource::AgentPlan { agent },
            })
            .collect(),
    }
}

fn ask(w: &World, id: AskId, kind: AskKind, s: SessionId, title: &str) -> EventBody {
    EventBody::AskRaised {
        ask: Ask {
            id,
            kind,
            from: w.agents[0],
            to: w.person,
            task: None,
            session: Some(s),
            title: title.into(),
            body: String::new(),
            options: vec![],
            receipts: vec![],
            state: AskState::Open,
            answer: None,
            created: T0,
        },
    }
}

fn texts(s: &Standing) -> Vec<String> {
    s.state
        .iter()
        .chain(&s.tasks)
        .chain(&s.signals)
        .chain(&s.waiting)
        .map(|c| c.text.clone())
        .collect()
}

#[test]
fn tasks_checks_and_asks_read_as_where_it_stands() {
    // Session 0 works on task 0, which is in workstream 0.
    let w = World::new(3, 5, 2);
    let (s, agent, t) = (w.sessions[0], w.agents[0], w.tasks[0]);
    let mut log = Log::new();
    log.add(
        &w,
        agent,
        moved(t, TaskStatus::Todo, TaskStatus::InProgress),
    )
    .add(&w, agent, plan(t, agent, 2, 5))
    .add(&w, agent, tool(s, "cargo test", true))
    .add(&w, agent, tool(s, "cargo test", true))
    .add(
        &w,
        agent,
        ask(&w, w.asks[0], AskKind::Question, s, "Which seed?"),
    );
    let st = log.standing(&w);
    assert_eq!(
        texts(&st),
        [
            "GEN-1 is in progress (2 of 5 steps done)",
            "tests are failing",
            "waiting on @lead to answer \"Which seed?\"",
        ]
    );
    let next = st.next.as_ref().expect("a next step");
    assert_eq!(next.urgency, Urgency::Answer);
    assert_eq!(next.clause.text, "@lead to answer \"Which seed?\"");

    // Answering the ask leaves the failing tests as the next step.
    log.add(
        &w,
        w.person,
        EventBody::AskAnswered {
            ask: w.asks[0],
            answer: Answer {
                by: w.person,
                option: None,
                text: Some("3".into()),
                at: T0,
            },
        },
    );
    let st = log.standing(&w);
    assert!(st.waiting.is_empty());
    let next = st.next.as_ref().expect("a next step");
    assert_eq!(
        (next.urgency, next.clause.text.as_str()),
        (Urgency::Fix, "fix the failing tests")
    );

    // Passing tests and a move to review: the review is next.
    log.add(&w, agent, tool(s, "cargo test", false)).add(
        &w,
        agent,
        moved(t, TaskStatus::InProgress, TaskStatus::Review),
    );
    let st = log.standing(&w);
    assert_eq!(texts(&st), ["GEN-1 is in review", "tests pass again"]);
    let next = st.next.as_ref().expect("a next step");
    assert_eq!(
        (next.urgency, next.clause.text.as_str()),
        (Urgency::Review, "review GEN-1")
    );
    let p = propose_workstream(&st, None, BriefPolicy::default(), &RuleSummarizer)
        .unwrap()
        .expect("a proposal");
    assert_eq!(p.text(), "GEN-1 is in review. Tests pass again.");
    assert_eq!(p.next_text(), Some("Review GEN-1."));
    check(&p, &allowed_receipts(&log.events));
}

#[test]
fn a_task_moved_to_another_workstream_counts_there() {
    let w = World::new(3, 5, 2);
    let t = w.tasks[0];
    let mut log = Log::new();
    log.add(
        &w,
        w.person,
        EventBody::TaskUpdated {
            task: t,
            patch: TaskPatch {
                workstream: Some(Some(w.workstreams[1])),
                ..TaskPatch::default()
            },
        },
    )
    .add(
        &w,
        w.person,
        moved(t, TaskStatus::InProgress, TaskStatus::Review),
    );
    let all = blocks(&log.events, &w.dir, &Config::default());
    let refs: Vec<&Block> = all.iter().collect();
    let in_review = |ws| texts(&standing(ws, &refs, &w.dir)).contains(&"GEN-1 is in review".into());
    assert!(in_review(w.workstreams[1]));
    assert!(!in_review(w.workstreams[0]));
}

#[test]
fn a_decision_comes_before_everything_else() {
    let w = World::new(3, 5, 2);
    let (s, agent, t) = (w.sessions[0], w.agents[0], w.tasks[0]);
    let mut log = Log::new();
    log.add(&w, agent, tool(s, "cargo test", true))
        .add(
            &w,
            agent,
            moved(t, TaskStatus::InProgress, TaskStatus::Review),
        )
        .add(
            &w,
            agent,
            ask(&w, w.asks[1], AskKind::Decision, s, "Loss is NaN. Rerun?"),
        );
    let st = log.standing(&w);
    let next = st.next.as_ref().expect("a next step");
    assert_eq!(next.urgency, Urgency::Decide);
    assert_eq!(next.clause.text, "@lead to decide \"Loss is NaN. Rerun?\"");
    assert!(texts(&st).contains(&"a job diverged".to_owned()));
}

#[test]
fn a_quiet_workstream_gets_a_paused_question() {
    let w = World::new(1, 1, 1);
    let p = propose_paused(
        w.workstreams[0],
        event_id(7),
        T0,
        T0 + 4 * 86_400_000 + 5,
        0,
    );
    assert_eq!(
        p.text(),
        "No activity for 4 days (since 2026-09-30), paused?"
    );
    assert_eq!(
        p.next_text(),
        Some("Mark it paused, or give it a next step.")
    );
    assert_eq!(p.disposition, Disposition::Propose);
    assert_eq!(p.receipts, [Receipt::Event { id: event_id(7) }]);
    let early = propose_paused(w.workstreams[0], event_id(7), T0, T0 + 1_000, 0);
    assert!(early.text().starts_with("No activity for less than a day"));
    // Every clause, including the question and the next step, cites the last activity.
    for span in p
        .summary
        .spans
        .iter()
        .chain(p.next.iter().flat_map(|n| &n.spans))
    {
        assert_eq!(span.receipts.iter().map(show_receipt).count(), 1);
    }
}

#[test]
fn a_project_rolls_up_its_workstreams_and_their_most_pressing_step() {
    let w = World::new(6, 10, 2);
    let mut log = Log::new();
    // Session 0 is on task 0 (workstream 0); session 1 on task 1 (workstream 1).
    let (s0, s1) = (w.sessions[0], w.sessions[1]);
    log.add(&w, w.agents[0], tool(s0, "cargo test", true)).add(
        &w,
        w.agents[1],
        ask(&w, w.asks[2], AskKind::Decision, s1, "Drop seed 3?"),
    );
    let all = blocks(&log.events, &w.dir, &Config::default());
    let refs: Vec<&Block> = all.iter().collect();
    let a = standing(w.workstreams[0], &refs, &w.dir);
    let b = standing(w.workstreams[1], &refs, &w.dir);
    let project = a.project.expect("known project");
    let p = propose_project(
        project,
        &[&a, &b],
        None,
        BriefPolicy::default(),
        &RuleSummarizer,
    )
    .unwrap()
    .expect("a roll-up");
    assert_eq!(
        p.text(),
        "Stream 1: tests are failing. Stream 2: waiting on @lead to decide \"Drop seed 3?\"."
    );
    assert_eq!(p.next_text(), Some("@lead to decide \"Drop seed 3?\"."));
    check(&p, &allowed_receipts(&log.events));
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// Over any generated log, every proposal verifies and cites only receipts from the input,
    /// and building them twice gives the same result.
    #[test]
    fn every_claim_cites_the_input(
        specs in vec((any::<u8>(), any::<u8>(), any::<u8>(), any::<bool>(), 0i64..900_000), 0..200),
        auto in any::<bool>(),
    ) {
        let world = World::new(5, 7, 3);
        let events = gen_events(&specs, &world, T0, 1);
        let all = blocks(&events, &world.dir, &Config::default());
        let refs: Vec<&Block> = all.iter().collect();
        let allowed = allowed_receipts(&events);
        let policy = BriefPolicy { auto_accept_unpinned: auto };
        let standings: Vec<Standing> =
            world.workstreams.iter().map(|w| standing(*w, &refs, &world.dir)).collect();
        for s in &standings {
            if let Some(p) = propose_workstream(s, None, policy, &RuleSummarizer).unwrap() {
                check(&p, &allowed);
            }
            let again = standing(s.workstream, &refs, &world.dir);
            prop_assert_eq!(&again, s);
        }
        let refs: Vec<&Standing> = standings.iter().collect();
        if let Some(project) = standings.iter().find_map(|s| s.project)
            && let Some(p) = propose_project(project, &refs, None, policy, &RuleSummarizer).unwrap()
        {
            check(&p, &allowed);
        }
    }
}
