//! Timing: listing 10,000 tasks with a filter. Target: under 20 ms.
//!
//! `cargo test -p pitcrew-hub-work --release --test perf -- --ignored --nocapture`

mod common;

use common::{PAPER, SAM, TOOLING, WRITER, app, call, member, person, seeded};
use pitcrew_hub_work::{EventRefs, RefFilter, TaskFilter, WorkService};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, ProjectKey, SubtaskId, TaskId, TaskKey};
use pitcrew_protocol::model::{Priority, Subtask, SubtaskSource, Task, TaskStatus};
use std::time::{Duration, Instant};
use tower::ServiceExt as _;

const TASKS: u32 = 10_000;
const RUNS: usize = 30;

const STATUSES: [TaskStatus; 6] = [
    TaskStatus::Backlog,
    TaskStatus::Todo,
    TaskStatus::InProgress,
    TaskStatus::Review,
    TaskStatus::Done,
    TaskStatus::Canceled,
];

/// 10,000 tasks over the demo's two projects, each with two subtasks, a label and a dependency.
fn fill(work: &WorkService) {
    let agents = [
        "01JB000000000000000MEM0002",
        "01JB000000000000000MEM0003",
        "01JB000000000000000MEM0004",
        "01JB000000000000000MEM0005",
    ];
    let mut previous: Option<TaskId> = None;
    let mut batch = Vec::new();
    for i in 0..TASKS {
        let (project, key) = if i % 2 == 0 {
            (PAPER, "PAP")
        } else {
            (TOOLING, "TL")
        };
        let id = TaskId::new();
        let agent = member(agents[(i % 4) as usize]);
        let task = Task {
            id,
            key: TaskKey::new(ProjectKey::new(key).expect("key"), 100 + i).expect("task key"),
            project: project.parse().expect("project"),
            workstream: None,
            title: format!("Generated task {i}"),
            description: "Generated for the timing test.".into(),
            status: STATUSES[(i % 6) as usize],
            priority: Priority::Medium,
            assignee: Some(agent),
            labels: vec!["generated".into()],
            start: None,
            due: None,
            blocked_by: previous.into_iter().collect(),
            source: None,
            archived: false,
            accept_auto: false,
            subtasks: vec![
                Subtask {
                    id: SubtaskId::new(),
                    text: "First step".into(),
                    done: true,
                    source: SubtaskSource::AgentPlan { agent },
                },
                Subtask {
                    id: SubtaskId::new(),
                    text: "Second step".into(),
                    done: false,
                    source: SubtaskSource::Human,
                },
            ],
        };
        previous = Some(id);
        batch.push(Event {
            id: EventId::new(),
            at: 1_790_800_000_000,
            workspace: work.workspace(),
            author: member(SAM),
            on_behalf_of: None,
            body: EventBody::TaskCreated { task },
        });
        if batch.len() == 1_000 {
            work.store().append(&batch).expect("append");
            batch.clear();
        }
    }
    work.store().append(&batch).expect("append");
}

fn stats(mut times: Vec<Duration>) -> (Duration, Duration, Duration) {
    times.sort();
    (times[0], times[times.len() / 2], times[times.len() - 1])
}

fn time<T>(mut f: impl FnMut() -> T) -> ((Duration, Duration, Duration), T) {
    let mut out = f();
    let mut times = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let start = Instant::now();
        out = f();
        times.push(start.elapsed());
    }
    (stats(times), out)
}

fn report(what: &str, n: usize, (best, median, worst): (Duration, Duration, Duration)) {
    println!(
        "{what:<52.52} {n:>6} tasks   best {:>7.2} ms   median {:>7.2} ms   worst {:>7.2} ms",
        best.as_secs_f64() * 1e3,
        median.as_secs_f64() * 1e3,
        worst.as_secs_f64() * 1e3
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "timing; run in release with --ignored --nocapture"]
async fn listing_10k_tasks_with_a_filter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = seeded(dir.path());
    let start = Instant::now();
    fill(&work);
    println!("appended {TASKS} tasks in {:?}", start.elapsed());
    let total = work.tasks(&TaskFilter::default()).expect("all").len();
    assert_eq!(total, 10_010);

    let in_progress = TaskFilter {
        statuses: vec![TaskStatus::InProgress],
        ..TaskFilter::default()
    };
    let (filtered, tasks) = time(|| work.tasks(&in_progress).expect("list"));
    report("service: status=in_progress", tasks.len(), filtered);

    let paper = TaskFilter {
        project: Some(PAPER.parse().expect("project")),
        ..TaskFilter::default()
    };
    let (by_project, tasks) = time(|| work.tasks(&paper).expect("list"));
    report("service: project=PAP", tasks.len(), by_project);

    let narrow = TaskFilter {
        assignee: Some(member(WRITER)),
        statuses: vec![TaskStatus::Todo, TaskStatus::InProgress],
        ..TaskFilter::default()
    };
    let (narrowed, tasks) = time(|| work.tasks(&narrow).expect("list"));
    report(
        "service: assignee=@writer, status=todo|in_progress",
        tasks.len(),
        narrowed,
    );

    let (all, tasks) = time(|| work.tasks(&TaskFilter::default()).expect("list"));
    report("service: no filter", tasks.len(), all);

    // Through the route, up to the whole response body (the client's parsing is not counted).
    let app = app(&work);
    let mut routes = Vec::new();
    for query in [
        "status=in_progress".to_owned(),
        format!("project={PAPER}"),
        format!("assignee={WRITER}&status=todo&status=in_progress"),
        String::new(),
    ] {
        let path = format!("/v1/tasks?{query}");
        let mut times = Vec::with_capacity(RUNS);
        let mut bytes = 0;
        for _ in 0..=RUNS {
            let mut request = axum::http::Request::builder()
                .uri(&path)
                .body(axum::body::Body::empty())
                .expect("request");
            request.extensions_mut().insert(person(SAM));
            let start = Instant::now();
            let response = app.clone().oneshot(request).await.expect("infallible");
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            times.push(start.elapsed());
            assert_eq!(status, 200);
            bytes = body.len();
        }
        times.remove(0);
        let timing = stats(times);
        let n = call(&app, Some(person(SAM)), "GET", &path, None)
            .await
            .1
            .as_array()
            .map_or(0, Vec::len);
        report(&format!("route: GET /v1/tasks?{query}"), n, timing);
        println!("{:>52} {bytes} bytes of JSON", "");
        routes.push((path, n, timing));
    }

    // The target: `GET /v1/tasks` with a filter over the 10,000, median under 20 ms. The
    // service's decoded lists and the half-the-workspace list are reported, not asserted: they
    // are for internal callers, and the latter is a filter in name only.
    for (path, _, timing) in [&routes[0], &routes[2]] {
        assert!(timing.1 < Duration::from_millis(20), "{path}: {timing:?}");
    }

    // The activity reference index over the same log: the newest page of 100 by project, and
    // every page of 500 back to the start (each one indexed walk). Reported; the store's own pages
    // aim at under 5 ms each.
    let by_project = RefFilter {
        project: Some(PAPER.parse().expect("project")),
        ..RefFilter::default()
    };
    let (page, revs) = time(|| {
        work.revs_matching(&by_project, u64::MAX, 100)
            .expect("revs")
    });
    report("refs: project=PAP, newest 100", revs.0.len(), page);
    let (pages, n) = time(|| {
        let mut before = u64::MAX;
        let mut n = 0;
        loop {
            let (revs, scanned_to) = work.revs_matching(&by_project, before, 500).expect("revs");
            n += revs.len();
            if scanned_to == 0 {
                return n;
            }
            before = scanned_to;
        }
    });
    report("refs: project=PAP, every page of 500 (events)", n, pages);
    assert!(
        n > 5_000,
        "every generated PAP task's event, and the demo's"
    );
}
