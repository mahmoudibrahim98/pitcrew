//! The directory follows the hub's rules (firm links stay, agents stay, stale moves are not moves),
//! keeps learning past its limit, and says when names may read differently.

mod common;

use common::{T0, World, event_id, session};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{AskId, MemberId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{
    Ask, AskKind, AskState, Dispatch, LinkBasis, Member, MemberKind, Mover, Receipt, Session,
    SessionState, TaskStatus, Workstream,
};
use pitcrew_recap::{Block, BlockBuilder, Config, Directory, block_line};
use ulid::Ulid;

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

    /// The blocks, and how many events were left out.
    fn build(&self, dir: &Directory) -> (Vec<Block>, u64) {
        let mut builder = BlockBuilder::new(Config::default(), dir.clone());
        for e in &self.events {
            builder.push(e);
        }
        let skipped = builder.skipped();
        (builder.finish(), skipped)
    }
}

fn tool(s: SessionId, offset: u64) -> EventBody {
    EventBody::ToolRan {
        session: s,
        tool: "Bash".into(),
        target: "ls".into(),
        outcome: "ok".into(),
        failed: false,
        receipt: Receipt::Transcript { session: s, offset },
    }
}

fn moved(task: TaskId, from: TaskStatus, to: TaskStatus) -> EventBody {
    EventBody::TaskMoved {
        task,
        from,
        to,
        mover: Mover::Person,
    }
}

fn linked(s: SessionId, workstream: WorkstreamId, task: TaskId, basis: LinkBasis) -> EventBody {
    EventBody::SessionLinked {
        session: s,
        workstream: Some(workstream),
        task: Some(task),
        basis,
    }
}

/// A session as the runner re-states it: no link, and no agent.
fn restated(s: SessionId) -> Session {
    let mut again = session(s, MemberId(Ulid::nil()), None, None);
    again.agent = None;
    again
}

fn new_session(n: u128) -> SessionId {
    SessionId(Ulid::from((70u128 << 96) | n))
}

fn lines(all: &[Block], dir: &Directory) -> Vec<String> {
    all.iter().map(|b| block_line(b, dir).text).collect()
}

#[test]
fn a_restated_session_keeps_its_dispatch_link() {
    let w = World::new(3, 5, 2);
    let (agent, task, ws) = (w.agents[0], w.tasks[1], w.workstreams[1]);
    // Linked by a dispatch, as the engine sees one, or as the hub's dispatch writes it.
    let dispatched = |s: SessionId| EventBody::DispatchStarted {
        dispatch: Dispatch {
            id: w.dispatches[0],
            task,
            agent,
            session: Some(s),
            brief: "Draft the method section.".into(),
            started: T0,
            ended: None,
            outcome: None,
            summary: None,
        },
    };
    let discovered = |s: SessionId| {
        let mut found = session(s, agent, Some(task), None);
        found.workstream = Some(ws);
        found.link_basis = Some(LinkBasis::Dispatch);
        EventBody::SessionDiscovered { session: found }
    };
    let firsts: [&dyn Fn(SessionId) -> EventBody; 2] = [&dispatched, &discovered];
    for (n, first) in firsts.into_iter().enumerate() {
        let s = new_session(n as u128);
        let mut log = Log::new();
        log.add(&w, 0, w.person, first(s))
            .add(&w, 60, agent, tool(s, 1))
            // An hour on, the runner discovers the session again, with no link.
            .add(
                &w,
                3600,
                agent,
                EventBody::SessionDiscovered {
                    session: restated(s),
                },
            )
            .add(&w, 60, agent, tool(s, 2));
        let (all, skipped) = log.build(&w.dir);
        assert_eq!(skipped, 0);
        assert_eq!(all.len(), 2);
        for b in &all {
            assert_eq!(b.tasks, vec![task], "{n}");
            assert_eq!(b.workstream, Some(ws), "{n}");
        }
    }
}

fn dispatch_of(w: &World, s: SessionId, task: TaskId, agent: MemberId) -> EventBody {
    EventBody::DispatchStarted {
        dispatch: Dispatch {
            id: w.dispatches[1],
            task,
            agent,
            session: Some(s),
            brief: "Rerun seed 3.".into(),
            started: T0,
            ended: None,
            outcome: None,
            summary: None,
        },
    }
}

/// The hub never links a session from `dispatch_started`: a person's link stays, and a later one
/// is taken.
#[test]
fn a_dispatch_never_replaces_a_firm_link() {
    let w = World::new(3, 5, 2);
    let (s, agent) = (w.sessions[2], w.agents[2]);
    let mut log = Log::new();
    log.add(
        &w,
        0,
        w.person,
        linked(s, w.workstreams[0], w.tasks[0], LinkBasis::Manual),
    )
    .add(&w, 3600, w.person, dispatch_of(&w, s, w.tasks[1], agent))
    .add(&w, 3600, agent, tool(s, 1));
    let (all, skipped) = log.build(&w.dir);
    assert_eq!(skipped, 0);
    let last = all.last().expect("blocks");
    assert_eq!(last.tasks, vec![w.tasks[0]]);
    assert_eq!(last.workstream, Some(w.workstreams[0]));

    // A person's later link is taken.
    log.add(
        &w,
        60,
        w.person,
        linked(s, w.workstreams[1], w.tasks[2], LinkBasis::Manual),
    )
    .add(&w, 3600, agent, tool(s, 2));
    let (all, skipped) = log.build(&w.dir);
    assert_eq!(skipped, 0);
    let last = all.last().expect("blocks");
    assert_eq!(last.tasks, vec![w.tasks[2]]);
    assert_eq!(last.workstream, Some(w.workstreams[1]));
}

/// A session with only an inferred link, or none, takes the dispatch's link (the hub takes it from
/// the `session_discovered` that follows the dispatch), and the dispatch's agent if it had none.
#[test]
fn a_dispatch_links_a_session_without_a_firm_link() {
    let w = World::new(3, 5, 2);
    let (s, agent) = (new_session(9), w.agents[1]);
    let waiting = EventBody::SessionStateChanged {
        session: s,
        from: SessionState::Working,
        to: SessionState::Waiting,
        status_line: None,
    };
    let mut log = Log::new();
    log.add(
        &w,
        0,
        agent,
        linked(s, w.workstreams[0], w.tasks[0], LinkBasis::Folder),
    )
    .add(&w, 3600, w.person, dispatch_of(&w, s, w.tasks[1], agent))
    // Now the folder link changes nothing.
    .add(
        &w,
        60,
        agent,
        linked(s, w.workstreams[0], w.tasks[0], LinkBasis::Folder),
    )
    .add(&w, 3600, w.person, waiting);
    let (all, skipped) = log.build(&w.dir);
    assert_eq!(skipped, 1);
    let last = all.last().expect("blocks");
    assert_eq!(last.tasks, vec![w.tasks[1]]);
    assert_eq!(last.workstream, Some(w.workstreams[1]));
    assert_eq!(last.agent, Some(agent));
}

#[test]
fn an_inferred_link_never_replaces_a_firm_one() {
    let w = World::new(3, 5, 2);
    let (s, agent) = (w.sessions[2], w.agents[2]);
    let (t1, t2) = (w.tasks[0], w.tasks[1]);
    let (ws1, ws2) = (w.workstreams[0], w.workstreams[1]);
    let mut log = Log::new();
    log.add(&w, 0, w.person, linked(s, ws1, t1, LinkBasis::Manual))
        .add(&w, 60, agent, tool(s, 1))
        // The runner infers another link from the folder: the hub keeps the person's.
        .add(&w, 60, agent, linked(s, ws2, t2, LinkBasis::Folder))
        .add(&w, 60, agent, tool(s, 2))
        .add(&w, 3600, agent, tool(s, 3));
    let (all, skipped) = log.build(&w.dir);
    assert_eq!(skipped, 1, "the refused link is not activity");
    assert_eq!(all.len(), 2);
    for b in &all {
        assert_eq!(b.tasks, vec![t1]);
        assert_eq!(b.workstream, Some(ws1));
    }
    assert_eq!(all[0].counts.events, 3);
    assert_eq!(
        lines(&all, &w.dir),
        [
            "@lead linked the session to GEN-1, @agent3 ran 2 tools",
            "@agent3 ran a tool"
        ]
    );

    // Another firm link replaces it, whole.
    log.add(&w, 60, w.person, linked(s, ws2, t2, LinkBasis::Claimed))
        .add(&w, 3600, agent, tool(s, 4));
    let (all, skipped) = log.build(&w.dir);
    assert_eq!(skipped, 1);
    let last = all.last().expect("blocks");
    assert_eq!(last.tasks, vec![t2]);
    assert_eq!(last.workstream, Some(ws2));
}

#[test]
fn anything_replaces_an_inferred_link_as_the_hub_does() {
    let w = World::new(3, 5, 2);
    let (s, agent) = (w.sessions[2], w.agents[2]);
    let mut log = Log::new();
    log.add(
        &w,
        0,
        agent,
        linked(s, w.workstreams[0], w.tasks[0], LinkBasis::Folder),
    )
    .add(
        &w,
        60,
        agent,
        linked(s, w.workstreams[1], w.tasks[1], LinkBasis::Branch),
    )
    .add(&w, 60, agent, tool(s, 1))
    // A re-statement with no link unlinks an inferred one, in the hub too.
    .add(
        &w,
        3600,
        agent,
        EventBody::SessionDiscovered {
            session: restated(s),
        },
    )
    .add(&w, 60, agent, tool(s, 2));
    let (all, skipped) = log.build(&w.dir);
    assert_eq!(skipped, 0);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].tasks, vec![w.tasks[0], w.tasks[1]]);
    assert_eq!(all[0].workstream, Some(w.workstreams[1]));
    assert!(all[1].tasks.is_empty());
    assert_eq!(all[1].workstream, None);
}

#[test]
fn a_restated_session_keeps_the_agent_it_does_not_name() {
    let w = World::new(1, 1, 1);
    let s = new_session(1);
    let (a1, a2) = (w.agents[0], w.agents[1]);
    let waiting = EventBody::SessionStateChanged {
        session: s,
        from: SessionState::Working,
        to: SessionState::Waiting,
        status_line: None,
    };
    let mut log = Log::new();
    log.add(
        &w,
        0,
        w.person,
        EventBody::SessionDiscovered {
            session: session(s, a1, None, None),
        },
    )
    // Re-stated without an agent: the session keeps @agent1.
    .add(
        &w,
        3600,
        w.person,
        EventBody::SessionDiscovered {
            session: restated(s),
        },
    )
    .add(&w, 60, w.person, waiting.clone())
    // Re-stated with another agent: it takes that one.
    .add(
        &w,
        3600,
        w.person,
        EventBody::SessionDiscovered {
            session: session(s, a2, None, None),
        },
    )
    .add(&w, 60, w.person, waiting);
    let (all, _) = log.build(&w.dir);
    let agents: Vec<Option<MemberId>> = all.iter().map(|b| b.agent).collect();
    assert_eq!(agents, [Some(a1), Some(a1), Some(a2)]);
}

#[test]
fn a_move_that_lost_a_race_is_not_a_move() {
    let w = World::new(1, 3, 1);
    let t = w.tasks[0];
    let mut log = Log::new();
    log.add(
        &w,
        0,
        w.person,
        moved(t, TaskStatus::Todo, TaskStatus::InProgress),
    )
    // Two more writers that read "todo" before the first move: one moves elsewhere, one to
    // the same place. The hub ignores both.
    .add(
        &w,
        60,
        w.agents[0],
        moved(t, TaskStatus::Todo, TaskStatus::Review),
    )
    .add(
        &w,
        60,
        w.agents[1],
        moved(t, TaskStatus::Todo, TaskStatus::InProgress),
    );
    let (all, skipped) = log.build(&w.dir);
    assert_eq!(skipped, 2);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].counts.task_moves, 1);
    assert_eq!(all[0].counts.events, 1);
    assert_eq!(lines(&all, &w.dir), ["@lead moved GEN-1 to in progress"]);

    // A move from where the task is now counts (and merges with the first).
    log.add(
        &w,
        60,
        w.agents[0],
        moved(t, TaskStatus::InProgress, TaskStatus::Review),
    );
    let (all, skipped) = log.build(&w.dir);
    assert_eq!(skipped, 2);
    assert_eq!(all[0].counts.task_moves, 2);
    assert_eq!(lines(&all, &w.dir), ["@lead moved GEN-1 to review"]);
}

#[test]
fn a_task_is_where_its_creation_put_it() {
    let w = World::new(1, 3, 1);
    let mut task = w.task_docs[1].clone();
    task.status = TaskStatus::Review;
    let t = task.id;
    let mut log = Log::new();
    log.add(&w, 0, w.person, EventBody::TaskCreated { task })
        // Starts and ends elsewhere than review: stale.
        .add(
            &w,
            60,
            w.person,
            moved(t, TaskStatus::Todo, TaskStatus::InProgress),
        )
        // Ends where the creation put it: the move that took it there (a log that states tasks
        // as they are now, then replays older moves, as the hub's seed writes).
        .add(
            &w,
            60,
            w.person,
            moved(t, TaskStatus::InProgress, TaskStatus::Review),
        )
        // The same again is a duplicate: the task is in review by a move now.
        .add(
            &w,
            60,
            w.person,
            moved(t, TaskStatus::InProgress, TaskStatus::Review),
        )
        .add(
            &w,
            60,
            w.person,
            moved(t, TaskStatus::Review, TaskStatus::Done),
        );
    let (all, skipped) = log.build(&w.dir);
    assert_eq!(skipped, 2);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].counts.task_moves, 2);
    assert_eq!(
        lines(&all, &w.dir),
        ["@lead created GEN-2, moved GEN-2 to done"]
    );
}

#[test]
fn a_seeded_status_is_not_checked() {
    // The seed is the projections as they are now: the task is done, and the log then replays
    // the moves that took it there.
    let w = World::new(1, 3, 1);
    let mut dir = w.dir.clone();
    let mut done = w.task_docs[0].clone();
    done.status = TaskStatus::Done;
    dir.add_task(&done);
    let t = done.id;
    let mut log = Log::new();
    log.add(
        &w,
        0,
        w.person,
        moved(t, TaskStatus::Todo, TaskStatus::InProgress),
    )
    .add(
        &w,
        60,
        w.person,
        moved(t, TaskStatus::InProgress, TaskStatus::Review),
    )
    .add(
        &w,
        60,
        w.person,
        moved(t, TaskStatus::Review, TaskStatus::Done),
    );
    let (all, skipped) = log.build(&dir);
    assert_eq!(skipped, 0);
    assert_eq!(all[0].counts.task_moves, 3);
}

/// A first-come cap stopped learning sessions once full, for good: replaying a long log, every
/// session after the limit was unlinked. Now the one used longest ago goes instead.
#[test]
fn the_directory_keeps_learning_past_its_limit() {
    let w = World::new(1, 6, 2);
    let mut dir = Directory::with_limit(3);
    for t in &w.task_docs {
        dir.add_task(t);
    }
    let mut log = Log::new();
    for n in 0..5usize {
        let s = new_session(n as u128);
        let found = session(s, w.agents[0], Some(w.tasks[n]), None);
        log.add(
            &w,
            1,
            w.agents[0],
            EventBody::SessionDiscovered { session: found },
        );
    }
    // An hour on, the first and the last session work again.
    log.add(&w, 3600, w.agents[0], tool(new_session(0), 1)).add(
        &w,
        1,
        w.agents[0],
        tool(new_session(4), 2),
    );
    let (all, _) = log.build(&dir);
    let last = &all[all.len() - 2..];
    assert_eq!(last[0].session, Some(new_session(0)));
    assert!(last[0].tasks.is_empty(), "the first session was forgotten");
    assert_eq!(last[1].session, Some(new_session(4)));
    assert_eq!(last[1].tasks, vec![w.tasks[4]], "the last one is known");
}

#[test]
fn an_active_session_outlives_idle_ones() {
    let w = World::new(1, 6, 2);
    let (s0, s1, s2) = (new_session(0), new_session(1), new_session(2));
    let mut log = Log::new();
    for (n, s) in [s0, s1].into_iter().enumerate() {
        let found = session(s, w.agents[0], Some(w.tasks[n]), None);
        log.add(
            &w,
            1,
            w.agents[0],
            EventBody::SessionDiscovered { session: found },
        );
    }
    // Session 0 works; then a third session arrives and the idle one goes.
    log.add(&w, 1, w.agents[0], tool(s0, 1)).add(
        &w,
        1,
        w.agents[0],
        EventBody::SessionDiscovered {
            session: session(s2, w.agents[0], Some(w.tasks[2]), None),
        },
    );
    log.add(&w, 3600, w.agents[0], tool(s0, 2))
        .add(&w, 1, w.agents[0], tool(s1, 3));
    let (all, _) = log.build(&Directory::with_limit(2));
    let last = &all[all.len() - 2..];
    assert_eq!(last[0].tasks, vec![w.tasks[0]]);
    assert!(last[1].tasks.is_empty());
}

/// The link from a task to its session is used by the task's events, so it outlives one nothing
/// mentions.
#[test]
fn a_tasks_events_keep_its_link_to_a_session() {
    let w = World::new(1, 4, 1);
    let ws = w.workstreams[0];
    let (s0, s1, s2) = (new_session(0), new_session(1), new_session(2));
    let (t0, t1, t2) = (w.tasks[0], w.tasks[1], w.tasks[2]);
    let comment = |task: TaskId| EventBody::CommentPosted {
        task: Some(task),
        workstream: None,
        text: "Looks right.".into(),
        mentions: vec![],
    };
    let mut log = Log::new();
    log.add(&w, 0, w.person, linked(s0, ws, t0, LinkBasis::Manual))
        .add(&w, 1, w.person, linked(s1, ws, t1, LinkBasis::Manual))
        .add(&w, 1, w.person, comment(t0))
        // A third link drops the task link used longest ago: task 1's, not task 0's.
        .add(&w, 1, w.person, linked(s2, ws, t2, LinkBasis::Manual))
        .add(&w, 1, w.person, comment(t1))
        .add(&w, 1, w.person, comment(t0));
    // No tasks are known, so a task's event can only be placed through its session.
    let (all, skipped) = log.build(&Directory::with_limit(2));
    assert_eq!(skipped, 1, "task 1's comment has nowhere to go");
    let s0_block = all
        .iter()
        .find(|b| b.session == Some(s0))
        .expect("session 0's block");
    assert_eq!(s0_block.counts.comments, 2);
}

fn member(n: u128, handle: &str) -> Member {
    Member {
        id: MemberId(Ulid::from((40u128 << 96) | n)),
        kind: MemberKind::Agent,
        handle: handle.into(),
        name: handle.into(),
        owner: None,
        persona: None,
    }
}

fn ask(id: AskId, kind: AskKind, from: MemberId) -> Ask {
    Ask {
        id,
        kind,
        from,
        to: from,
        task: None,
        session: None,
        title: "Which seed?".into(),
        body: String::new(),
        options: vec![],
        receipts: vec![],
        state: AskState::Open,
        answer: None,
        created: T0,
    }
}

#[test]
fn the_names_version_moves_when_prose_may_read_differently() {
    let mut dir = Directory::with_limit(2);
    let v = |dir: &Directory| dir.names_version();
    let (a, b, c) = (member(1, "@a"), member(2, "@b"), member(3, "@c"));
    // New names, and the same name again, change nothing written.
    dir.add_member(&a);
    dir.add_member(&b);
    dir.add_member(&a);
    assert_eq!(v(&dir), 0);
    // A rename does.
    dir.add_member(&member(1, "@a2"));
    assert_eq!(v(&dir), 1);
    assert_eq!(dir.handle(a.id), Some("@a2"));
    // Sessions are never named in prose: dropping one changes nothing written.
    for n in 0..3 {
        dir.add_session(&session(new_session(n), a.id, None, None));
    }
    assert_eq!(v(&dir), 1);
    // A third member drops the one used longest ago (@b): prose that named it now says
    // "someone".
    dir.add_member(&c);
    assert_eq!(v(&dir), 2);
    assert_eq!(dir.handle(b.id), None);
    // Once a member was dropped, any member learned may be one dropped before.
    dir.add_member(&b);
    assert_eq!(v(&dir), 3);

    // Asks: what an answer says.
    let id = AskId(Ulid::from(9u128));
    dir.add_ask(&ask(id, AskKind::Question, a.id));
    assert_eq!(dir.ask(id), Some((AskKind::Question, a.id)));
    assert_eq!(v(&dir), 3);
    dir.add_ask(&ask(id, AskKind::Question, a.id));
    assert_eq!(v(&dir), 3);
    dir.add_ask(&ask(id, AskKind::Decision, a.id));
    assert_eq!(v(&dir), 4);
    assert_eq!(dir.ask(AskId(Ulid::from(10u128))), None);

    // Tasks and workstreams, renamed by events.
    let w = World::new(1, 2, 1);
    let mut dir = w.dir.clone();
    let mut task = w.task_docs[0].clone();
    let at = |body| Event {
        id: event_id(1),
        at: T0,
        workspace: w.workspace,
        author: w.person,
        on_behalf_of: None,
        body,
    };
    dir.observe(&at(EventBody::TaskCreated { task: task.clone() }));
    assert_eq!(v(&dir), 0);
    task.key = pitcrew_protocol::ids::TaskKey::new(
        pitcrew_protocol::ids::ProjectKey::new("NEW").expect("key"),
        7,
    )
    .expect("key");
    dir.observe(&at(EventBody::TaskCreated { task }));
    assert_eq!(v(&dir), 1);
    assert_eq!(dir.task_key(w.tasks[0]), Some("NEW-7"));
    let renamed = Workstream {
        id: w.workstreams[0],
        project: w.task_docs[0].project,
        name: "Renamed".into(),
        status: pitcrew_protocol::model::WorkstreamStatus::Active,
        health: pitcrew_protocol::model::Health::OnTrack,
        locations: vec![],
        external: vec![],
    };
    dir.observe(&at(EventBody::WorkstreamCreated {
        workstream: renamed,
    }));
    assert_eq!(v(&dir), 2);
}

#[test]
fn imported_assignment_survives_inference_and_unlinked_discovery() {
    let w = World::new(3, 5, 2);
    let (s, agent, task, ws) = (
        w.sessions[2],
        w.agents[2],
        w.tasks[1],
        w.workstreams[1],
    );
    for incoming in [
        linked(s, w.workstreams[0], w.tasks[0], LinkBasis::Folder),
        linked(s, w.workstreams[0], w.tasks[0], LinkBasis::Branch),
        EventBody::SessionDiscovered {
            session: restated(s),
        },
    ] {
        let mut log = Log::new();
        log.add(&w, 0, w.person, linked(s, ws, task, LinkBasis::Imported))
            .add(&w, 60, agent, incoming)
            .add(&w, 60, agent, tool(s, 1));
        let (all, _) = log.build(&w.dir);
        assert!(!all.is_empty());
        for block in all {
            assert_eq!(block.workstream, Some(ws));
            assert_eq!(block.tasks, vec![task]);
        }
    }
}
