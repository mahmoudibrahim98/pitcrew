//! The single-writer rule: one `WorkService` per store allocates keys without errors, even under
//! concurrency; and when a second writer breaks the rule anyway, the projections resolve what it
//! appends deterministically (a clashing key is recorded and ignored, a stale move is ignored),
//! the log never stalls, and the losing command answers `409`, never `500`.

mod common;

use common::{PAPER, SAM, TOOLING, WRITER, agent, app, call, demo, expect, member, person, seeded};
use pitcrew_hub_work::projection::NAMES;
use pitcrew_hub_work::{NewTask, TaskRef, WorkService, projections};
use pitcrew_protocol::api::{Caller, ErrorCode, TokenScope};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, ProjectId, ProjectKey, TaskId, TaskKey};
use pitcrew_protocol::model::{Mover, Project, ProjectStatus, Task, TaskStatus};
use pitcrew_store::{Store, StoreOptions};
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn event(body: EventBody) -> Event {
    Event {
        id: EventId::new(),
        at: 1_790_800_000_000,
        workspace: demo().workspace.id,
        author: member(SAM),
        on_behalf_of: None,
        body,
    }
}

fn new_task(project: &str, title: &str) -> NewTask {
    NewTask {
        project: project.parse().expect("project"),
        workstream: None,
        title: title.into(),
        description: None,
        status: None,
        priority: None,
        assignee: None,
        labels: None,
        due: None,
    }
}

/// A task with `key`, as another writer would create it.
fn task_with_key(key: &str, title: &str) -> Task {
    let mut task = demo().tasks[0].clone();
    task.id = TaskId::new();
    task.key = key.parse::<TaskKey>().expect("key");
    task.title = title.into();
    task.subtasks.clear();
    task
}

fn task(work: &WorkService, key: &str) -> Task {
    work.task(&TaskRef::parse(key).expect("key")).expect("task")
}

fn count(work: &WorkService, sql: &str) -> i64 {
    work.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .expect("count")
}

#[test]
fn a_task_created_with_a_taken_key_is_recorded_not_applied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let before = task(&work, "PAP-2");
    let intruder = task_with_key("PAP-2", "Intruder");
    // Neither the append nor anything after it fails.
    work.store()
        .append(&[event(EventBody::TaskCreated {
            task: intruder.clone(),
        })])
        .expect("a clash does not fail the append");
    assert_eq!(task(&work, "PAP-2"), before, "the first task keeps its key");
    assert!(
        work.task(&TaskRef::Id(intruder.id)).is_err(),
        "the second task does not exist"
    );
    let clashes = work
        .read(|c| {
            Ok(c.query_row(
                "SELECT task, key_prefix, number, holder FROM work_task_clashes",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )?)
        })
        .expect("clash row");
    assert_eq!(
        clashes,
        (
            intruder.id.0.to_string(),
            "PAP".to_owned(),
            2,
            before.id.0.to_string()
        )
    );
    // Events about the refused task change nothing; the log goes on.
    work.store()
        .append(&[event(EventBody::TaskMoved {
            task: intruder.id,
            from: TaskStatus::InProgress,
            to: TaskStatus::Done,
            mover: Mover::Person,
        })])
        .expect("append");
    let created = work
        .create_task(&person(SAM), new_task(PAPER, "Still works"))
        .expect("create");
    assert_eq!(created.key.to_string(), "PAP-8");

    // A re-stated task that would take another's key is refused too, and keeps its own.
    let mut restated = task(&work, "PAP-3");
    restated.key = "PAP-2".parse().expect("key");
    restated.title = "Renamed and re-keyed".into();
    work.store()
        .append(&[event(EventBody::TaskCreated { task: restated })])
        .expect("append");
    assert_eq!(task(&work, "PAP-3").title, demo().tasks[2].title);
    assert_eq!(count(&work, "SELECT COUNT(*) FROM work_task_clashes"), 2);

    // Rebuilding gives the same answer.
    let tasks = work.tasks(&Default::default()).expect("tasks");
    for name in NAMES {
        work.store().rebuild(name).expect("rebuild");
    }
    assert_eq!(work.tasks(&Default::default()).expect("tasks"), tasks);
    assert_eq!(count(&work, "SELECT COUNT(*) FROM work_task_clashes"), 2);
}

#[test]
fn a_clash_appended_without_the_projections_is_caught_up_on_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hub.db");
    let work = seeded(dir.path());
    let pap2 = task(&work, "PAP-2");
    drop(work);
    // Another process, without the work projections, appends a clash and a move after it.
    let plain = Store::open(&path, StoreOptions::default()).expect("open");
    let intruder = task_with_key("PAP-2", "Intruder");
    plain
        .append(&[
            event(EventBody::TaskCreated {
                task: intruder.clone(),
            }),
            event(EventBody::TaskMoved {
                task: pap2.id,
                from: TaskStatus::Todo,
                to: TaskStatus::InProgress,
                mover: Mover::Person,
            }),
        ])
        .expect("append without projections");
    drop(plain);
    // Opening with the projections catches up past the clash instead of failing.
    let store = Arc::new(
        Store::open_with(&path, StoreOptions::default(), projections()).expect("catch-up open"),
    );
    let work = WorkService::new(store, demo().workspace);
    let now = task(&work, "PAP-2");
    assert_eq!(now.id, pap2.id);
    assert_eq!(
        now.status,
        TaskStatus::InProgress,
        "the move after it applied"
    );
    assert!(work.task(&TaskRef::Id(intruder.id)).is_err());
    assert_eq!(count(&work, "SELECT COUNT(*) FROM work_task_clashes"), 1);
}

#[test]
fn a_move_from_a_status_the_task_has_left_is_ignored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let pap2 = task(&work, "PAP-2");
    assert_eq!(pap2.status, TaskStatus::Todo);
    // Two writers both saw `todo`; the first moves it to in progress...
    let first = EventBody::TaskMoved {
        task: pap2.id,
        from: TaskStatus::Todo,
        to: TaskStatus::InProgress,
        mover: Mover::Person,
    };
    // ...and the second's move from `todo` to `canceled` arrives after it.
    let second = EventBody::TaskMoved {
        task: pap2.id,
        from: TaskStatus::Todo,
        to: TaskStatus::Canceled,
        mover: Mover::Person,
    };
    work.store()
        .append(&[event(first), event(second)])
        .expect("append");
    assert_eq!(task(&work, "PAP-2").status, TaskStatus::InProgress);
    // A move that does start where the task is applies.
    work.store()
        .append(&[event(EventBody::TaskMoved {
            task: pap2.id,
            from: TaskStatus::InProgress,
            to: TaskStatus::Review,
            mover: Mover::Person,
        })])
        .expect("append");
    assert_eq!(task(&work, "PAP-2").status, TaskStatus::Review);
    work.store().rebuild("work.tasks").expect("rebuild");
    assert_eq!(task(&work, "PAP-2").status, TaskStatus::Review);
}

#[test]
fn many_tasks_created_at_once_get_unique_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let threads = 8;
    let each = 25;
    let keys: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let work = Arc::clone(&work);
                s.spawn(move || {
                    (0..each)
                        .map(|i| {
                            let project = if (t + i) % 2 == 0 { PAPER } else { TOOLING };
                            work.create_task(&person(SAM), new_task(project, &format!("{t}/{i}")))
                                .expect("no command fails")
                                .key
                                .to_string()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("thread"))
            .collect()
    });
    assert_eq!(keys.len(), threads * each);
    let unique: BTreeSet<&String> = keys.iter().collect();
    assert_eq!(unique.len(), keys.len(), "every key is unique");
    // And the numbers are contiguous after the demo's: PAP-8.., TL-4..
    for (prefix, first) in [("PAP", 8), ("TL", 4)] {
        let mut numbers: Vec<u32> = keys
            .iter()
            .filter_map(|k| k.strip_prefix(&format!("{prefix}-")))
            .map(|n| n.parse().expect("number"))
            .collect();
        numbers.sort_unstable();
        let expected: Vec<u32> =
            (first..first + u32::try_from(numbers.len()).expect("n")).collect();
        assert_eq!(numbers, expected, "{prefix}");
    }
    assert_eq!(count(&work, "SELECT COUNT(*) FROM work_task_clashes"), 0);
}

#[tokio::test]
async fn a_key_taken_by_a_racing_writer_is_a_409_not_a_500() {
    let dir = tempfile::tempdir().expect("tempdir");
    let seeded = seeded(dir.path());
    // A second writer appends PAP-8 between the service's check and its append. The service's
    // clock runs exactly there, so it stands in for the race.
    let store = Arc::clone(seeded.store());
    let raced = Arc::new(AtomicBool::new(false));
    let racer = Arc::clone(&raced);
    let clock_store = Arc::clone(&store);
    let work = Arc::new(
        WorkService::new(Arc::clone(&store), demo().workspace).with_clock(Arc::new(move || {
            if !racer.swap(true, Ordering::SeqCst) {
                clock_store
                    .append(&[event(EventBody::TaskCreated {
                        task: task_with_key("PAP-8", "The racing writer's"),
                    })])
                    .expect("racing append");
            }
            1_790_800_000_000
        })),
    );
    let app = app(&work);
    let res = call(
        &app,
        Some(person(SAM)),
        "POST",
        "/v1/tasks",
        Some(json!({ "project": PAPER, "title": "Mine" })),
    )
    .await;
    expect(&res, 409);
    assert!(raced.load(Ordering::SeqCst));
    let message = res.1["message"].as_str().expect("message");
    assert!(message.contains("PAP-8"), "{message}");
    assert!(!message.to_lowercase().contains("constraint"), "{message}");
    // The racing writer's task holds PAP-8; trying again gets PAP-9.
    assert_eq!(task(&work, "PAP-8").title, "The racing writer's");
    let again = call(
        &app,
        Some(person(SAM)),
        "POST",
        "/v1/tasks",
        Some(json!({ "project": PAPER, "title": "Mine" })),
    )
    .await;
    expect(&again, 201);
    assert_eq!(again.1["key"], "PAP-9");
}

#[test]
fn keys_are_allocated_by_key_prefix_not_by_project() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    // A second project that shares the key PAP with the paper (nothing stops that yet).
    let twin = Project {
        id: ProjectId::new(),
        key: ProjectKey::new("PAP").expect("key"),
        name: "Paper, again".into(),
        status: ProjectStatus::InProgress,
        lead: member(SAM),
        members: Vec::new(),
        start: None,
        due: None,
        root: None,
        external: Vec::new(),
    };
    work.store()
        .append(&[event(EventBody::ProjectCreated {
            project: twin.clone(),
        })])
        .expect("append");
    let in_twin = work
        .create_task(
            &person(SAM),
            new_task(&twin.id.0.to_string(), "In the twin"),
        )
        .expect("create");
    assert_eq!(
        in_twin.key.to_string(),
        "PAP-8",
        "not PAP-1, which the paper holds"
    );
    let in_paper = work
        .create_task(&person(SAM), new_task(PAPER, "In the paper"))
        .expect("create");
    assert_eq!(in_paper.key.to_string(), "PAP-9");
}

#[test]
fn only_agents_act_on_behalf_of_someone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    // A device caller that claims an owner (it should not have one) acts for nobody.
    let odd = Caller {
        member: member(SAM),
        scope: TokenScope::Device,
        on_behalf_of: Some(member(WRITER)),
    };
    work.create_task(&odd, new_task(PAPER, "Mine"))
        .expect("create");
    let last = |work: &WorkService| {
        let rev = work.store().latest_rev().expect("rev");
        work.store().since(rev - 1, 1).expect("log").remove(0).event
    };
    let event = last(&work);
    assert_eq!(event.author, member(SAM));
    assert_eq!(event.on_behalf_of, None);
    // An agent's events name its owner.
    work.move_task(
        &agent(WRITER),
        &TaskRef::parse("PAP-2").expect("key"),
        TaskStatus::InProgress,
    )
    .expect("move");
    let event = last(&work);
    assert_eq!(event.author, member(WRITER));
    assert_eq!(event.on_behalf_of, Some(member(SAM)));
}

#[test]
fn errors_from_the_store_never_leak_to_clients() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    // Break a table under the service, as a corrupted or foreign database would.
    let path = dir.path().join("hub.db");
    let raw = pitcrew_store::sql::Connection::open(&path).expect("raw connection");
    raw.execute_batch("DROP TABLE work_personas")
        .expect("drop a table");
    drop(raw);
    let err = work.personas().expect_err("the table is gone");
    assert_eq!(err.code(), ErrorCode::Internal);
    assert!(err.message().contains("work_personas"), "kept for the log");
    let body = err.to_api();
    assert_eq!(body.message, pitcrew_hub_work::INTERNAL_MESSAGE);
}
