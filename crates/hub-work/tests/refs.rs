//! The activity reference index (`work.refs`, [`EventRefs`]): which events are about a project,
//! workstream, task or session, how they page, and how the scan is bounded.

mod common;

use common::{SAM, WRITER, demo, member, seeded};
use pitcrew_hub_work::query::revs_matching;
use pitcrew_hub_work::{EventRefs, REF_SCAN_BUDGET, RefFilter, TaskRef, WorkService};
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::events::{BriefTarget, Event, EventBody};
use pitcrew_protocol::ids::{EventId, ProjectId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{Engine, LinkBasis, Receipt, Session, SessionState, TaskPatch};
use std::sync::Arc;

type About = (
    Option<ProjectId>,
    Option<WorkstreamId>,
    Option<TaskId>,
    Option<SessionId>,
);

/// What an event is about, worked out independently of the index: from what it names, and the
/// work model's **current** links (as the mock hub does). For the demo, where nothing is re-linked
/// after the fact, that is the same as the links in force when each event happened.
fn oracle(work: &WorkService, body: &EventBody) -> About {
    let (mut project, mut workstream, mut task, session) = match body {
        EventBody::ProjectCreated { project } => (Some(project.id), None, None, None),
        EventBody::WorkstreamCreated { workstream } => (None, Some(workstream.id), None, None),
        EventBody::WorkstreamChanged { workstream, .. } => (None, Some(*workstream), None, None),
        EventBody::TaskCreated { task } => (None, None, Some(task.id), None),
        EventBody::TaskMoved { task, .. }
        | EventBody::TaskAssigned { task, .. }
        | EventBody::TaskUpdated { task, .. }
        | EventBody::SubtasksReplaced { task, .. } => (None, None, Some(*task), None),
        EventBody::SessionDiscovered { session } => {
            (None, session.workstream, session.task, Some(session.id))
        }
        EventBody::SessionLinked {
            session,
            workstream,
            task,
            ..
        } => (None, *workstream, *task, Some(*session)),
        EventBody::SessionStateChanged { session, .. }
        | EventBody::TurnEnded { session, .. }
        | EventBody::ToolRan { session, .. }
        | EventBody::FileEdited { session, .. }
        | EventBody::SessionUpdated { session, .. }
        | EventBody::SessionEnded { session } => (None, None, None, Some(*session)),
        EventBody::DispatchStarted { dispatch } => {
            (None, None, Some(dispatch.task), dispatch.session)
        }
        EventBody::DispatchFinished { dispatch, .. } => {
            let d = work.dispatch(dispatch).expect("dispatch");
            (None, None, Some(d.task), d.session)
        }
        EventBody::AskRaised { ask } => (None, None, ask.task, ask.session),
        EventBody::AskAnswered { ask, .. } => {
            let a = work.ask(ask).expect("ask");
            (None, None, a.task, a.session)
        }
        EventBody::CommentPosted {
            task, workstream, ..
        } => (None, *workstream, *task, None),
        EventBody::BriefProposed { target, .. } | EventBody::BriefAccepted { target, .. } => {
            match target {
                BriefTarget::Project(p) => (Some(*p), None, None, None),
                BriefTarget::Workstream(w) => (None, Some(*w), None, None),
            }
        }
        EventBody::DecisionRecorded { workstream, .. } => (None, *workstream, None, None),
        _ => (None, None, None, None),
    };
    let linked = session.and_then(|s| work.session(&s).ok());
    if task.is_none() {
        task = linked.as_ref().and_then(|s| s.task);
    }
    let task_now = task.and_then(|t| work.task(&TaskRef::Id(t)).ok());
    if workstream.is_none() {
        workstream = task_now
            .as_ref()
            .and_then(|t| t.workstream)
            .or_else(|| linked.as_ref().and_then(|s| s.workstream));
    }
    if project.is_none() {
        project = workstream
            .and_then(|w| work.workstream(&w).ok())
            .map(|w| w.project)
            .or_else(|| task_now.map(|t| t.project));
    }
    (project, workstream, task, session)
}

fn matches(filter: &RefFilter, about: &About) -> bool {
    filter.project.is_none_or(|p| about.0 == Some(p))
        && filter.workstream.is_none_or(|w| about.1 == Some(w))
        && filter.task.is_none_or(|t| about.2 == Some(t))
        && filter.session.is_none_or(|s| about.3 == Some(s))
}

/// Every revision matching `filter`, oldest first, paging back `limit` at a time.
fn all_pages(refs: &dyn EventRefs, filter: &RefFilter, limit: usize) -> Vec<u64> {
    let mut out = Vec::new();
    let mut before = u64::MAX;
    loop {
        let (revs, scanned_to) = refs.revs_matching(filter, before, limit).expect("page");
        assert!(revs.len() <= limit);
        assert!(revs.windows(2).all(|w| w[0] < w[1]), "oldest first");
        assert!(revs.iter().all(|r| *r < before));
        out.splice(0..0, revs.iter().copied());
        if scanned_to == 0 {
            return out;
        }
        assert!(scanned_to < before, "paging moves back");
        before = scanned_to;
    }
}

fn log(work: &WorkService) -> Vec<(u64, Event)> {
    work.store()
        .since(0, usize::MAX)
        .expect("log")
        .into_iter()
        .map(|e| (e.rev, e.event))
        .collect()
}

#[test]
fn the_demo_is_indexed_through_its_links() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let refs: Arc<dyn EventRefs> = Arc::clone(&work) as Arc<dyn EventRefs>;
    let log = log(&work);
    let about: Vec<(u64, About)> = log
        .iter()
        .map(|(rev, e)| (*rev, oracle(&work, &e.body)))
        .collect();
    let demo = demo();

    let mut filters: Vec<RefFilter> = Vec::new();
    for p in &demo.projects {
        filters.push(RefFilter {
            project: Some(p.id),
            ..RefFilter::default()
        });
    }
    for w in &demo.workstreams {
        filters.push(RefFilter {
            workstream: Some(w.id),
            ..RefFilter::default()
        });
    }
    for t in &demo.tasks {
        filters.push(RefFilter {
            task: Some(t.id),
            ..RefFilter::default()
        });
    }
    for s in &demo.sessions {
        filters.push(RefFilter {
            session: Some(s.id),
            ..RefFilter::default()
        });
        // Two fields: the session within its project, and within a task it is not linked to.
        filters.push(RefFilter {
            project: Some(demo.projects[0].id),
            session: Some(s.id),
            ..RefFilter::default()
        });
        filters.push(RefFilter {
            task: Some(demo.tasks[4].id),
            session: Some(s.id),
            ..RefFilter::default()
        });
    }
    let mut two_fields_nonempty = 0;
    for filter in &filters {
        let expected: Vec<u64> = about
            .iter()
            .filter(|(_, a)| matches(filter, a))
            .map(|(rev, _)| *rev)
            .collect();
        let (got, scanned_to) = refs.revs_matching(filter, u64::MAX, 500).expect("revs");
        assert_eq!(got, expected, "{filter:?}");
        assert_eq!(scanned_to, 0, "{filter:?}: everything fits one page");
        // Paging one or two at a time gives the same.
        assert_eq!(all_pages(refs.as_ref(), filter, 1), expected, "{filter:?}");
        assert_eq!(all_pages(refs.as_ref(), filter, 2), expected, "{filter:?}");
        // Every project, workstream, task and session has at least the event that made it.
        let fields = [
            filter.project.is_some(),
            filter.workstream.is_some(),
            filter.task.is_some(),
            filter.session.is_some(),
        ];
        if fields.iter().filter(|f| **f).count() == 1 {
            assert!(!expected.is_empty(), "{filter:?}");
        } else {
            two_fields_nonempty += usize::from(!expected.is_empty());
        }
    }
    assert!(
        two_fields_nonempty > 0,
        "some two-field filters match something"
    );

    // The cases the contract says need an index: a turn in a session linked to a task, and
    // `dispatch_finished`, which names only the dispatch.
    let rev_of = |pred: &dyn Fn(&EventBody) -> bool| {
        log.iter()
            .find(|(_, e)| pred(&e.body))
            .map(|(rev, _)| *rev)
            .expect("event")
    };
    let pap1 = demo.tasks[0].id;
    let edit = rev_of(&|b| matches!(b, EventBody::FileEdited { .. }));
    let task = RefFilter {
        task: Some(pap1),
        ..RefFilter::default()
    };
    assert!(
        refs.revs_matching(&task, u64::MAX, 500)
            .expect("revs")
            .0
            .contains(&edit)
    );
    let finished = rev_of(&|b| matches!(b, EventBody::DispatchFinished { .. }));
    let pap3 = RefFilter {
        task: Some(demo.tasks[2].id),
        project: Some(demo.projects[0].id),
        ..RefFilter::default()
    };
    assert!(
        refs.revs_matching(&pap3, u64::MAX, 500)
            .expect("revs")
            .0
            .contains(&finished)
    );
}

fn append(work: &WorkService, body: EventBody) -> u64 {
    work.store()
        .append(&[Event {
            id: EventId::new(),
            at: 1_790_900_000_000,
            workspace: demo().workspace.id,
            author: member(WRITER),
            on_behalf_of: Some(member(SAM)),
            body,
        }])
        .expect("append")
        .to_rev
}

fn turn(session: SessionId) -> EventBody {
    EventBody::TurnEnded {
        session,
        receipt: Receipt::Transcript { session, offset: 0 },
    }
}

fn revs(work: &WorkService, filter: &RefFilter) -> Vec<u64> {
    work.revs_matching(filter, u64::MAX, 500).expect("revs").0
}

#[test]
fn links_count_from_when_they_are_made_and_firm_links_stay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let demo = demo();
    let pap2 = demo.tasks[1].id;
    let by_task = RefFilter {
        task: Some(pap2),
        ..RefFilter::default()
    };
    let before = revs(&work, &by_task);

    // A session nobody linked works a turn, then a person links it to PAP-2, then another turn.
    let session = Session {
        id: SessionId::new(),
        engine: Engine::Claude,
        native_id: "n-1".into(),
        machine: demo.machines[0].id,
        cwd: "/tmp/elsewhere".into(),
        branch: None,
        title: None,
        agent: Some(member(WRITER)),
        workstream: None,
        task: None,
        link_basis: None,
        state: SessionState::Working,
        status_line: None,
        started: 1_790_900_000_000,
        last_activity: 1_790_900_000_000,
        terminal: None,
        parent: None,
        model: None,
        account: None,
    };
    let discovered = append(
        &work,
        EventBody::SessionDiscovered {
            session: session.clone(),
        },
    );
    let early = append(&work, turn(session.id));
    let linked = append(
        &work,
        EventBody::SessionLinked {
            session: session.id,
            workstream: None,
            task: Some(pap2),
            basis: LinkBasis::Manual,
        },
    );
    let late = append(&work, turn(session.id));
    let mut expected = before;
    expected.extend([linked, late]);
    assert_eq!(
        revs(&work, &by_task),
        expected,
        "the link does not reach back"
    );
    // ...and the task's workstream and project come with it.
    let by_project = RefFilter {
        project: Some(demo.projects[0].id),
        session: Some(session.id),
        ..RefFilter::default()
    };
    assert_eq!(revs(&work, &by_project), [linked, late]);
    let by_session = RefFilter {
        session: Some(session.id),
        ..RefFilter::default()
    };
    assert_eq!(revs(&work, &by_session), [discovered, early, linked, late]);

    // The runner re-states the session without the link, and infers a folder link: the person's
    // link stays, for the index as for the session itself.
    let id = session.id;
    let mut restated = session;
    restated.title = Some("Re-stated".into());
    append(&work, EventBody::SessionDiscovered { session: restated });
    append(
        &work,
        EventBody::SessionLinked {
            session: id,
            workstream: Some(demo.workstreams[2].id),
            task: None,
            basis: LinkBasis::Folder,
        },
    );
    let after = append(&work, turn(id));
    assert_eq!(revs(&work, &by_task).last(), Some(&after));
    assert_eq!(
        work.session(&id).expect("session").task,
        Some(pap2),
        "the sessions table agrees"
    );
}

#[test]
fn a_task_moved_to_another_workstream_counts_there_from_then_on() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let demo = demo();
    let sibling = |t: &pitcrew_protocol::model::Task| {
        demo.workstreams
            .iter()
            .find(|w| w.project == t.project && Some(w.id) != t.workstream)
            .map(|w| w.id)
    };
    let task = demo
        .tasks
        .iter()
        .find(|t| t.workstream.is_some() && sibling(t).is_some())
        .expect("a task whose project has another workstream");
    let (from, to) = (
        task.workstream.expect("workstream"),
        sibling(task).expect("sibling"),
    );
    let in_workstream = |w: WorkstreamId| RefFilter {
        workstream: Some(w),
        ..RefFilter::default()
    };
    let before_from = revs(&work, &in_workstream(from));
    let mut expected = revs(&work, &in_workstream(to));
    let update = |patch: TaskPatch| EventBody::TaskUpdated {
        task: task.id,
        patch,
    };

    let moved = append(
        &work,
        update(TaskPatch {
            workstream: Some(Some(to)),
            ..TaskPatch::default()
        }),
    );
    let later = append(
        &work,
        EventBody::TaskAssigned {
            task: task.id,
            assignee: Some(member(SAM)),
        },
    );
    expected.extend([moved, later]);
    assert_eq!(revs(&work, &in_workstream(to)), expected);
    assert_eq!(
        revs(&work, &in_workstream(from)),
        before_from,
        "earlier events stay where they were"
    );

    // Out of any workstream: still about the task and its project; a patch that does not move it
    // changes nothing about where it is.
    let by_task_in_project = RefFilter {
        project: Some(task.project),
        task: Some(task.id),
        ..RefFilter::default()
    };
    let out = append(
        &work,
        update(TaskPatch {
            workstream: Some(None),
            ..TaskPatch::default()
        }),
    );
    let renamed = append(
        &work,
        update(TaskPatch {
            title: Some("Renamed".into()),
            ..TaskPatch::default()
        }),
    );
    assert_eq!(revs(&work, &in_workstream(to)), expected);
    assert!(revs(&work, &by_task_in_project).ends_with(&[out, renamed]));

    // A task nobody knows gets no parents from a patch.
    let stranger = TaskId::new();
    let unknown = append(
        &work,
        EventBody::TaskUpdated {
            task: stranger,
            patch: TaskPatch {
                workstream: Some(Some(to)),
                ..TaskPatch::default()
            },
        },
    );
    let by_stranger = RefFilter {
        task: Some(stranger),
        ..RefFilter::default()
    };
    assert_eq!(revs(&work, &by_stranger), [unknown]);
    assert_eq!(revs(&work, &in_workstream(to)), expected);
}

#[test]
fn a_filtered_scan_is_bounded_and_says_where_it_stopped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let demo = demo();
    // SES0002 runs PAP-4, but the demo's decision about PAP-5 names it too. Driving by the
    // session, most rows examined are not about PAP-5.
    let filter = RefFilter {
        task: Some(demo.tasks[4].id),
        session: Some(demo.sessions[1].id),
        ..RefFilter::default()
    };
    let everything = work.revs_matching(&filter, u64::MAX, 500).expect("revs").0;
    assert_eq!(
        everything.len(),
        2,
        "the ask, raised in the snapshot and again in the slice"
    );
    // One row per call: each call returns at most one revision, and paging from `scanned_to`
    // still finds them all.
    let mut found = Vec::new();
    let mut before = u64::MAX;
    let mut calls = 0;
    let mut empty_pages = 0;
    loop {
        let (revs, scanned_to) = work
            .read(|c| revs_matching(c, &filter, before, 10, 1))
            .expect("page");
        calls += 1;
        empty_pages += usize::from(revs.is_empty());
        found.splice(0..0, revs);
        if scanned_to == 0 {
            break;
        }
        before = scanned_to;
    }
    assert_eq!(found, everything);
    assert!(empty_pages > 0, "some pages ran out of budget with nothing");
    let session_rows = work
        .revs_matching(
            &RefFilter {
                session: Some(demo.sessions[1].id),
                ..RefFilter::default()
            },
            u64::MAX,
            500,
        )
        .expect("revs")
        .0
        .len();
    assert_eq!(
        calls,
        session_rows + 1,
        "one row per call, then one to find the start"
    );

    // More matches than the limit: `scanned_to` is the oldest returned.
    let by_session = RefFilter {
        session: Some(demo.sessions[1].id),
        ..RefFilter::default()
    };
    let (page, scanned_to) = work.revs_matching(&by_session, u64::MAX, 2).expect("page");
    assert_eq!(page.len(), 2);
    assert_eq!(scanned_to, page[0]);
    assert_eq!(REF_SCAN_BUDGET, 10_000);
}

#[test]
fn bad_filters_and_the_start_of_the_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let err = work
        .revs_matching(&RefFilter::default(), u64::MAX, 10)
        .expect_err("empty filter");
    assert_eq!(err.code(), ErrorCode::Invalid);
    let task = RefFilter {
        task: Some(demo().tasks[0].id),
        ..RefFilter::default()
    };
    let err = work.revs_matching(&task, u64::MAX, 0).expect_err("limit 0");
    assert_eq!(err.code(), ErrorCode::Invalid);
    for before in [0, 1] {
        assert_eq!(
            work.revs_matching(&task, before, 10).expect("start"),
            (Vec::new(), 0)
        );
    }
    // An unknown id matches nothing, at the start.
    let nothing = RefFilter {
        session: Some(SessionId::new()),
        ..RefFilter::default()
    };
    assert_eq!(
        work.revs_matching(&nothing, u64::MAX, 10).expect("none"),
        (Vec::new(), 0)
    );
}

#[test]
fn each_filter_walks_its_own_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    for column in ["project", "workstream", "task", "session"] {
        let plan: Vec<String> = work
            .read(|c| {
                let mut stmt = c.prepare(&format!(
                    "EXPLAIN QUERY PLAN SELECT rev, project, workstream, task, session
                     FROM work_event_refs WHERE {column} = ?1 AND rev < ?2
                     ORDER BY rev DESC LIMIT ?3"
                ))?;
                let rows = stmt
                    .query_map(pitcrew_store::sql::params!["x", 10, 10], |r| {
                        r.get::<_, String>(3)
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .expect("plan");
        let plan = plan.join(" | ");
        assert!(
            plan.contains(&format!("work_event_refs_by_{column}")),
            "{column}: {plan}"
        );
        assert!(!plan.contains("TEMP B-TREE"), "{column}: no sort: {plan}");
    }
}
