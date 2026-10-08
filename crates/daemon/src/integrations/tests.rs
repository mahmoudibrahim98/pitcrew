//! The sync loop's work, over the demo workspace and a copy of the recorded fixtures in
//! `apps/mock-hub/fixtures` (no network), with a stand-in `gh` (never the machine's own). A test
//! changes "upstream" between two syncs by adding a fixture file that sorts first
//! ([`upstream_says`]).

#![allow(clippy::unwrap_used)]

use super::*;
use pitcrew_hub_work::{TaskFilter, WorkstreamPatch};
use pitcrew_protocol::api::TokenScope;
use pitcrew_protocol::ids::{TaskId, WorkstreamId};
use pitcrew_protocol::model::{AskState, ExternalRef, Mover, Task, TaskStatus, WorkstreamStatus};
use std::collections::HashMap;
use std::path::PathBuf;

const SAM: &str = "01JB000000000000000MEM0001";
/// A second person, added to the demo workspace for these tests.
const LEE: &str = "01JB000000000000000MEM0007";
const SEED_RUNS: &str = "01JB000000000000000WST0002";
const SUBMISSION: &str = "01JB000000000000000WST0001";
const IDEA: &str = "01JB000000000000000WST0004";
const GH_TOKEN: &str = "synthetic-gh-credential-0001";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/mock-hub/fixtures")
}

fn person(member: &str) -> Caller {
    Caller {
        member: member.parse().unwrap(),
        scope: TokenScope::Device,
        on_behalf_of: None,
    }
}

fn sam() -> Caller {
    person(SAM)
}

struct Hub {
    dir: tempfile::TempDir,
    root: PathBuf,
    /// The copy of the recorded fixtures this hub reads.
    fixtures: PathBuf,
    work: Arc<WorkService>,
    integrations: Arc<Integrations>,
}

fn stand_in_gh(dir: &Path, script: &str) -> GhCli {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    crate::test_scripts::write_script(&bin.join("gh"), script, 0o700);
    GhCli::with_path(bin.into_os_string())
}

fn hub() -> Hub {
    hub_with_gh(&format!("#!/bin/sh\necho {GH_TOKEN}\n"))
}

/// A hub whose `gh` is `script`.
fn hub_with_gh(script: &str) -> Hub {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    std::fs::create_dir(&root).unwrap();
    let fixtures = dir.path().join("fixtures");
    std::fs::create_dir(&fixtures).unwrap();
    for entry in std::fs::read_dir(self::fixtures()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|x| x == "fixture") {
            std::fs::copy(&path, fixtures.join(path.file_name().unwrap())).unwrap();
        }
    }
    let mut demo = pitcrew_fixtures::demo_workspace().unwrap();
    demo.members.push(pitcrew_protocol::model::Member {
        id: LEE.parse().unwrap(),
        kind: pitcrew_protocol::model::MemberKind::Human,
        handle: "@lee".into(),
        name: "Lee".into(),
        owner: None,
        persona: None,
    });
    let store = Arc::new(
        pitcrew_store::Store::open_with(
            root.join("hub.db"),
            pitcrew_store::StoreOptions::default(),
            pitcrew_hub_work::projections(),
        )
        .unwrap(),
    );
    let work = Arc::new(WorkService::new(store, demo.workspace.clone()));
    work.seed(&demo).unwrap();
    let upstream = Ok(Upstream::Fixtures(
        http::FixtureTransport::load(&fixtures).unwrap(),
    ));
    let gh = stand_in_gh(dir.path(), script);
    let integrations = Arc::new(Integrations::open(&root, &work, upstream, gh).unwrap());
    Hub {
        dir,
        root,
        fixtures,
        work,
        integrations,
    }
}

const ISSUES: &str = "https://api.github.com/repos/example-org/demo-repo/issues?state=all&sort=updated&direction=asc&per_page=100";
const MILESTONES: &str = "https://api.github.com/repos/example-org/demo-repo/milestones?state=all&sort=due_on&direction=asc&per_page=100";

/// The recorded answer's body for `url`, as JSON.
fn recorded(url: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(fixtures().join("github.fixture")).unwrap();
    let exchange = pitcrew_sync_github::fixture::parse_fixture(&text)
        .unwrap()
        .into_iter()
        .find(|e| e.url == url)
        .unwrap();
    serde_json::from_slice(&exchange.body).unwrap()
}

/// From now on, "upstream" answers `body` for `url`: a fixture file that sorts before the
/// recorded ones.
fn upstream_says(hub: &Hub, url: &str, body: &serde_json::Value) {
    let name = if url.contains("/milestones") {
        "0-milestones"
    } else {
        "0-issues"
    };
    std::fs::write(
        hub.fixtures.join(format!("{name}.fixture")),
        format!(
            "GET {url} HTTP/1.1\nAccept: application/vnd.github+json\n\nHTTP/1.1 200\n\n{body}\n"
        ),
    )
    .unwrap();
}

/// The recorded issues, with `change` made to issue `number` (and its `updated_at` moved on).
fn issues_with(
    number: u64,
    change: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> serde_json::Value {
    let mut issues = recorded(ISSUES);
    let issue = issues
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|i| i["number"] == number)
        .unwrap()
        .as_object_mut()
        .unwrap();
    change(issue);
    issue.insert("updated_at".into(), "2026-01-03T09:00:00Z".into());
    issues
}

fn milestone_one() -> serde_json::Value {
    serde_json::json!({
        "number": 1,
        "title": "v1 launch",
        "state": "open",
        "html_url": "https://github.com/example-org/demo-repo/milestone/1"
    })
}

/// The member integration `id` syncs as.
fn member_of(hub: &Hub, id: &IntegrationId) -> MemberId {
    hub.integrations.record(id).unwrap().sync_member.unwrap()
}

fn github() -> NewIntegration {
    NewIntegration {
        name: "Demo repository".into(),
        settings: IntegrationSettings::Github {
            repos: vec!["example-org/demo-repo".into()],
            api_base: None,
        },
        credential: CredentialSource::GhCli,
        interval_minutes: None,
    }
}

fn jira() -> NewIntegration {
    NewIntegration {
        name: "Demo Jira".into(),
        settings: IntegrationSettings::Jira {
            deployment: JiraDeployment::Cloud,
            site: "https://jira.example.com".into(),
            projects: vec!["DEMO".into()],
            email: Some("sam@example.com".into()),
            epic_link_field: None,
        },
        credential: CredentialSource::Stored,
        interval_minutes: Some(60),
    }
}

fn link(work: &WorkService, workstream: &str, links: Vec<ExternalRef>) {
    let id: WorkstreamId = workstream.parse().unwrap();
    work.patch_workstream(
        &sam(),
        &id,
        WorkstreamPatch {
            external: Some(links),
            ..WorkstreamPatch::default()
        },
    )
    .unwrap();
}

fn github_ref(key: &str) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: key.into(),
        url: None,
    }
}

fn tasks_of(work: &WorkService, workstream: &str) -> Vec<Task> {
    work.tasks(&TaskFilter {
        workstream: Some(workstream.parse().unwrap()),
        ..TaskFilter::default()
    })
    .unwrap()
}

fn mirrored(work: &WorkService, key: &str) -> Option<Task> {
    work.tasks(&TaskFilter::default())
        .unwrap()
        .into_iter()
        .find(|t| t.source.as_ref().is_some_and(|s| s.key == key))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issues_in_a_linked_milestone_become_tasks_once_linked() {
    let hub = hub();
    let added = hub.integrations.add(&sam(), github()).await.unwrap();
    assert_eq!(added.credential.source, CredentialSource::GhCli);
    assert!(!added.credential.stored);

    // Before any link: everything is out of scope.
    hub.integrations.sync_one(added.id).await;
    let first = hub.integrations.get(&added.id).await.unwrap();
    assert!(
        first.status.problems.is_empty(),
        "{:?}",
        first.status.problems
    );
    assert!(first.status.last_success_at.is_some());
    assert!(mirrored(&hub.work, "example-org/demo-repo#1").is_none());
    assert_eq!(first.status.last_run.unwrap().applied, 0);

    // Linking a milestone makes the next sync read the issues again: #1 becomes a task, the
    // merged pull request that closes it is noted, and the issues closed before they were first
    // seen stay out.
    link(
        &hub.work,
        SEED_RUNS,
        vec![github_ref("example-org/demo-repo#milestone:1")],
    );
    hub.integrations.sync_one(added.id).await;
    let after = hub.integrations.get(&added.id).await.unwrap();
    assert!(
        after.status.problems.is_empty(),
        "{:?}",
        after.status.problems
    );
    let task = mirrored(&hub.work, "example-org/demo-repo#1").expect("PAP task for #1");
    assert_eq!(task.title, "Fix flaky login test");
    assert_eq!(task.workstream, Some(SEED_RUNS.parse().unwrap()));
    assert_eq!(task.labels, vec!["bug".to_string(), "tests".to_string()]);
    assert_eq!(task.status, TaskStatus::Todo);
    assert_eq!(task.assignee, None, "the hub owns the assignee");
    for closed in ["example-org/demo-repo#2", "example-org/demo-repo#3"] {
        assert!(mirrored(&hub.work, closed).is_none(), "{closed}");
    }
    // #4 has no milestone, and the repository itself is not linked.
    assert!(mirrored(&hub.work, "example-org/demo-repo#4").is_none());
    let events = hub.work.store().since(0, 100_000).unwrap();
    let sync_member = member_of(&hub, &added.id);
    let noted = events.iter().any(|e| {
        e.event.author == sync_member
            && matches!(&e.event.body, pitcrew_protocol::events::EventBody::CommentPosted { task: Some(t), text, .. }
                if *t == task.id && text.contains("https://github.com/example-org/demo-repo/pull/7"))
    });
    assert!(noted, "the merged pull request is noted on its task");
    assert_eq!(
        after.links,
        vec![IntegrationLink {
            workstream: SEED_RUNS.parse().unwrap(),
            scope: github_ref("example-org/demo-repo#milestone:1"),
            title: Some("v1 launch".into()),
        }]
    );

    // A third sync with nothing new changes nothing.
    let rev = hub.work.store().latest_rev().unwrap();
    hub.integrations.sync_one(added.id).await;
    assert_eq!(hub.work.store().latest_rev().unwrap(), rev);

    // Linking the whole repository brings in #4 too, in that workstream.
    link(
        &hub.work,
        SUBMISSION,
        vec![github_ref("example-org/demo-repo")],
    );
    hub.integrations.sync_one(added.id).await;
    let notes = mirrored(&hub.work, "example-org/demo-repo#4").expect("#4");
    assert_eq!(notes.workstream, Some(SUBMISSION.parse().unwrap()));
    // #1 stays where its milestone's link put it.
    assert_eq!(
        mirrored(&hub.work, "example-org/demo-repo#1")
            .unwrap()
            .workstream,
        Some(SEED_RUNS.parse().unwrap())
    );
    assert_eq!(
        tasks_of(&hub.work, SUBMISSION)
            .iter()
            .filter(|t| t.source.is_some())
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upstream_close_never_moves_work_in_progress_it_asks() {
    let hub = hub();
    let added = hub.integrations.add(&sam(), github()).await.unwrap();
    link(
        &hub.work,
        SEED_RUNS,
        vec![github_ref("example-org/demo-repo#milestone:1")],
    );
    hub.integrations.sync_one(added.id).await;
    let task = mirrored(&hub.work, "example-org/demo-repo#1").unwrap();
    // A person starts on it.
    hub.work
        .move_task(
            &sam(),
            &pitcrew_hub_work::TaskRef::Id(task.id),
            TaskStatus::InProgress,
        )
        .unwrap();

    // Upstream closes #1: the next read says so.
    let sync_member = member_of(&hub, &added.id);
    let commands = hub.work.sync_commands(sync_member).unwrap();
    let mut applier = Applier::new(commands, ExternalSystem::Github).unwrap();
    let closed = pitcrew_sync_github::UpstreamChange::IssueClosed {
        source: task.source.clone().unwrap(),
        at: pitcrew_sync_github::GithubTimestamp::new("2026-01-03T00:00:00Z"),
        reason: pitcrew_sync_github::CloseReason::Completed,
    };
    apply::apply_github(&mut applier, std::slice::from_ref(&closed), &HashMap::new());
    let applied = applier.finish();
    assert_eq!(applied.counts.conflicts, 1);
    let now = hub
        .work
        .task(&pitcrew_hub_work::TaskRef::Id(task.id))
        .unwrap();
    assert_eq!(
        now.status,
        TaskStatus::InProgress,
        "in-progress work is never touched"
    );
    let asks = hub
        .work
        .asks(&pitcrew_hub_work::AskFilter::default())
        .unwrap();
    let ask = asks
        .iter()
        .find(|a| a.from == sync_member && a.task == Some(task.id))
        .expect("a conflict ask");
    assert_eq!(ask.state, AskState::Open);
    assert_eq!(ask.to, SAM.parse().unwrap());

    // Once the person has finished, a close moves it from review to done, as the sync.
    hub.work
        .move_task(
            &sam(),
            &pitcrew_hub_work::TaskRef::Id(task.id),
            TaskStatus::Review,
        )
        .unwrap();
    let commands = hub.work.sync_commands(sync_member).unwrap();
    let mut applier = Applier::new(commands, ExternalSystem::Github).unwrap();
    apply::apply_github(&mut applier, std::slice::from_ref(&closed), &HashMap::new());
    let done = hub
        .work
        .task(&pitcrew_hub_work::TaskRef::Id(task.id))
        .unwrap();
    assert_eq!(done.status, TaskStatus::Done);
    let last = hub
        .work
        .store()
        .since(hub.work.store().latest_rev().unwrap() - 1, 1)
        .unwrap();
    assert!(matches!(
        last[0].event.body,
        pitcrew_protocol::events::EventBody::TaskMoved {
            mover: Mover::Sync,
            ..
        }
    ));
}

/// What a sync writes reaches activity and `/v1/stream` only through the hub's shared visibility
/// check, as `serve.rs` mounts it (`pitcrew_api::visibility` over `crate::visibility`). None of it
/// names a session, so excluding every session (`/v1/import`, mode `none`) hides none of it, while
/// the demo sessions' own events are hidden by the same check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_a_sync_writes_passes_the_shared_visibility_check() {
    use pitcrew_protocol::events::EventBody;
    use pitcrew_protocol::import::{ImportFilter, ImportMode};

    let hub = hub();
    let added = hub.integrations.add(&sam(), github()).await.unwrap();
    link(
        &hub.work,
        SEED_RUNS,
        vec![github_ref("example-org/demo-repo#milestone:1")],
    );
    hub.integrations.sync_one(added.id).await;
    let task = mirrored(&hub.work, "example-org/demo-repo#1").unwrap();
    // A conflict ask as well: the person starts on the task, then upstream closes it.
    hub.work
        .move_task(
            &sam(),
            &pitcrew_hub_work::TaskRef::Id(task.id),
            TaskStatus::InProgress,
        )
        .unwrap();
    let sync_member = member_of(&hub, &added.id);
    let commands = hub.work.sync_commands(sync_member).unwrap();
    let mut applier = Applier::new(commands, ExternalSystem::Github).unwrap();
    let closed = pitcrew_sync_github::UpstreamChange::IssueClosed {
        source: task.source.clone().unwrap(),
        at: pitcrew_sync_github::GithubTimestamp::new("2026-01-03T00:00:00Z"),
        reason: pitcrew_sync_github::CloseReason::Completed,
    };
    apply::apply_github(&mut applier, std::slice::from_ref(&closed), &HashMap::new());
    assert_eq!(applier.finish().counts.conflicts, 1);

    let visibility = pitcrew_api::visibility::Visibility(Some(Arc::new(
        crate::visibility::WorkVisibility(Arc::clone(&hub.work)),
    )));
    hub.work
        .commit_import(ImportFilter {
            mode: ImportMode::None,
            ..ImportFilter::default()
        })
        .unwrap();
    let person = Some(SAM.parse().unwrap());
    let events = hub.work.store().since(0, 100_000).unwrap();
    let mut kinds = HashSet::new();
    for stored in &events {
        let event = &stored.event;
        let from_sync = event.author == sync_member
            || matches!(event.body, EventBody::WorkstreamLinked { .. })
            || matches!(&event.body, EventBody::MemberAdded { member } if member.id == sync_member);
        if !from_sync {
            continue;
        }
        let kind = serde_json::to_value(&event.body).unwrap()["type"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            visibility.visible(event, None).unwrap(),
            "activity hides the sync's {kind}"
        );
        assert!(
            visibility.visible(event, person).unwrap(),
            "the stream hides the sync's {kind}"
        );
        kinds.insert(kind);
    }
    for kind in [
        "member_added",
        "workstream_linked",
        "task_created",
        "comment_posted",
        "ask_raised",
    ] {
        assert!(kinds.contains(kind), "no {kind} in {kinds:?}");
    }
    // The same check hides the excluded sessions' events: it is live, not passing everything.
    let hidden = events
        .iter()
        .filter(|e| !matches!(e.event.body, EventBody::CursorMoved { .. }))
        .filter(|e| !visibility.visible(&e.event, person).unwrap())
        .count();
    assert!(hidden > 0, "no session event was hidden");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_milestone_ships_its_workstream_only_when_seen_closing() {
    let hub = hub();
    let sync_member = hub
        .work
        .ensure_sync_member(SAM.parse().unwrap())
        .unwrap()
        .id;
    link(
        &hub.work,
        IDEA,
        vec![github_ref("example-org/demo-repo#milestone:2")],
    );
    let milestone = github_ref("example-org/demo-repo#milestone:2");
    let at = pitcrew_sync_github::GithubTimestamp::new("2026-01-03T00:00:00Z");
    let created = pitcrew_sync_github::UpstreamChange::MilestoneCreated {
        source: milestone.clone(),
        at: at.clone(),
        title: "v0 cleanup".into(),
    };
    let closed = pitcrew_sync_github::UpstreamChange::MilestoneClosed {
        source: milestone,
        at,
    };
    // First seen already closed: nothing ships.
    let mut applier = Applier::new(
        hub.work.sync_commands(sync_member).unwrap(),
        ExternalSystem::Github,
    )
    .unwrap();
    apply::apply_github(&mut applier, &[created, closed.clone()], &HashMap::new());
    let idea: WorkstreamId = IDEA.parse().unwrap();
    assert_ne!(
        hub.work.workstream(&idea).unwrap().status,
        WorkstreamStatus::Shipped
    );
    // Seen closing: it ships.
    let mut applier = Applier::new(
        hub.work.sync_commands(sync_member).unwrap(),
        ExternalSystem::Github,
    )
    .unwrap();
    apply::apply_github(&mut applier, &[closed], &HashMap::new());
    assert_eq!(
        hub.work.workstream(&idea).unwrap().status,
        WorkstreamStatus::Shipped
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_jira_epic_routes_its_issues_and_the_secret_is_never_shown() {
    const SECRET: &str = "synthetic-jira-secret-0001";
    let hub = hub();
    let added = hub.integrations.add(&sam(), jira()).await.unwrap();
    // No credential yet: the sync says so.
    hub.integrations.sync_one(added.id).await;
    let waiting = hub.integrations.get(&added.id).await.unwrap();
    assert_eq!(waiting.status.problems.len(), 1);
    assert!(waiting.status.last_success_at.is_none());

    hub.integrations
        .set_credential(&sam(), &added.id, SECRET)
        .unwrap();
    assert_eq!(
        hub.integrations
            .set_credential(&sam(), &added.id, "two words")
            .unwrap_err()
            .code,
        ErrorCode::Invalid
    );
    link(
        &hub.work,
        SEED_RUNS,
        vec![ExternalRef {
            system: ExternalSystem::Jira,
            key: "DEMO-5".into(),
            url: Some("https://jira.example.com/browse/DEMO-5".into()),
        }],
    );
    hub.integrations.sync_one(added.id).await;
    let synced = hub.integrations.get(&added.id).await.unwrap();
    assert!(
        synced.status.problems.is_empty(),
        "{:?}",
        synced.status.problems
    );
    assert!(synced.credential.stored);
    let story = mirrored(&hub.work, "DEMO-6").expect("DEMO-6");
    assert_eq!(story.workstream, Some(SEED_RUNS.parse().unwrap()));
    assert_eq!(story.description.trim_end(), "Monthly, as a PDF.");
    assert!(
        mirrored(&hub.work, "DEMO-7").is_none(),
        "done before first seen"
    );
    assert!(
        mirrored(&hub.work, "DEMO-5").is_none(),
        "an epic is not a task"
    );
    assert_eq!(synced.links[0].title.as_deref(), Some("Billing"));

    let check = hub.integrations.check(&added.id).await.unwrap();
    assert!(check.ok, "{check:?}");

    // The secret is in no answer and no saved file but its own.
    let everything = serde_json::to_string(&(
        hub.integrations.list().await.unwrap(),
        check,
        hub.integrations.sync_now(&added.id).await.unwrap(),
    ))
    .unwrap();
    assert!(!everything.contains(SECRET));
    let saved = std::fs::read_to_string(hub.root.join("integrations.json")).unwrap();
    assert!(!saved.contains(SECRET));
    let state = std::fs::read_to_string(
        hub.root
            .join("integrations")
            .join(format!("{}.state.json", added.id.0)),
    )
    .unwrap();
    assert!(!state.contains(SECRET));

    hub.integrations.remove(&sam(), &added.id).unwrap();
    assert!(
        !hub.root
            .join("integrations")
            .join(format!("{}.secret", added.id.0))
            .exists()
    );
    assert_eq!(
        hub.integrations.get(&added.id).await.unwrap_err().code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn connections_are_checked_and_never_overlap() {
    let hub = hub();
    let mut bad = github();
    bad.settings = IntegrationSettings::Github {
        repos: vec!["not a repo".into()],
        api_base: None,
    };
    assert_eq!(
        hub.integrations.add(&sam(), bad).await.unwrap_err().code,
        ErrorCode::Invalid
    );
    let added = hub.integrations.add(&sam(), github()).await.unwrap();
    assert_eq!(
        hub.integrations
            .add(&sam(), github())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    // The same repository on an Enterprise server, or a Jira project on two sites: links and
    // task sources name no host, so one would move the other's tasks.
    let mut enterprise = github();
    enterprise.settings = IntegrationSettings::Github {
        repos: vec!["Example-Org/Demo-Repo".into()],
        api_base: Some("https://ghe.example.com/api/v3".into()),
    };
    assert_eq!(
        hub.integrations
            .add(&sam(), enterprise)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    hub.integrations.add(&sam(), jira()).await.unwrap();
    let mut other_site = jira();
    if let IntegrationSettings::Jira { site, .. } = &mut other_site.settings {
        *site = "https://jira-b.example.com".into();
    }
    assert_eq!(
        hub.integrations
            .add(&sam(), other_site)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    // A gh_cli connection keeps no secret.
    assert_eq!(
        hub.integrations
            .set_credential(&sam(), &added.id, "synthetic-value")
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    // Only a person of the workspace adds one.
    let stranger = Caller {
        member: pitcrew_protocol::ids::MemberId::new(),
        ..sam()
    };
    let mut other = github();
    other.settings = IntegrationSettings::Github {
        repos: vec!["example-org/other".into()],
        api_base: None,
    };
    assert_eq!(
        hub.integrations
            .add(&stranger, other)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Invalid
    );
    // Kept across a restart, without `running`.
    let reopened = Integrations::open(
        &hub.root,
        &hub.work,
        Err("no transport in this test".into()),
        GhCli::with_path(std::ffi::OsString::new()),
    )
    .unwrap();
    assert_eq!(reopened.list().await.unwrap().len(), 2);
    let _ = TaskId::new();
}

/// An open issue that a later read moves into a linked milestone becomes a task, from the
/// snapshot that read took; a closed one moved there does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_open_issue_moved_into_a_linked_milestone_becomes_a_task() {
    let hub = hub();
    let added = hub.integrations.add(&sam(), github()).await.unwrap();
    link(
        &hub.work,
        SEED_RUNS,
        vec![github_ref("example-org/demo-repo#milestone:1")],
    );
    hub.integrations.sync_one(added.id).await;
    assert!(mirrored(&hub.work, "example-org/demo-repo#1").is_some());
    assert!(mirrored(&hub.work, "example-org/demo-repo#4").is_none());

    // Upstream, #4 (open) and #2 (closed) join milestone 1, and #4 is retitled in the same read.
    let issues = issues_with(4, |issue| {
        issue.insert("milestone".into(), milestone_one());
        issue.insert("title".into(), "Write the v1 release notes".into());
    });
    let mut issues = issues.as_array().unwrap().clone();
    let two = issues.iter_mut().find(|i| i["number"] == 2).unwrap();
    two["milestone"] = milestone_one();
    two["updated_at"] = "2026-01-03T09:00:00Z".into();
    upstream_says(&hub, ISSUES, &serde_json::Value::Array(issues));
    hub.integrations.sync_one(added.id).await;
    let after = hub.integrations.get(&added.id).await.unwrap();
    assert!(
        after.status.problems.is_empty(),
        "{:?}",
        after.status.problems
    );
    let notes = mirrored(&hub.work, "example-org/demo-repo#4").expect("#4 becomes a task");
    assert_eq!(notes.workstream, Some(SEED_RUNS.parse().unwrap()));
    assert_eq!(notes.title, "Write the v1 release notes");
    assert_eq!(notes.labels, vec!["docs".to_string()]);
    assert_eq!(notes.status, TaskStatus::Todo);
    assert_eq!(
        notes.source.unwrap().url.as_deref(),
        Some("https://github.com/example-org/demo-repo/issues/4")
    );
    assert!(
        mirrored(&hub.work, "example-org/demo-repo#2").is_none(),
        "a closed issue moved into the milestone stays out"
    );

    // Read again with nothing new: nothing changes.
    let rev = hub.work.store().latest_rev().unwrap();
    hub.integrations.sync_one(added.id).await;
    assert_eq!(hub.work.store().latest_rev().unwrap(), rev);
}

/// Moves and shipping follow upstream changes only: an issue closing upstream moves its task to
/// done and its milestone closing ships the workstream; a task a person reopens afterwards stays
/// reopened on the next sync, since nothing changed upstream. The mock does the same
/// (`apps/mock-hub/test/integrations.test.ts`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moves_and_shipping_follow_upstream_changes_only() {
    let hub = hub();
    let added = hub.integrations.add(&sam(), github()).await.unwrap();
    link(
        &hub.work,
        IDEA,
        vec![github_ref("example-org/demo-repo#milestone:1")],
    );
    hub.integrations.sync_one(added.id).await;
    let task = mirrored(&hub.work, "example-org/demo-repo#1").unwrap();
    assert_eq!(task.status, TaskStatus::Todo);

    // Upstream closes #1 and milestone 1.
    upstream_says(
        &hub,
        ISSUES,
        &issues_with(1, |issue| {
            issue.insert("state".into(), "closed".into());
            issue.insert("state_reason".into(), "completed".into());
        }),
    );
    let mut milestones = recorded(MILESTONES);
    milestones[0]["state"] = "closed".into();
    upstream_says(&hub, MILESTONES, &milestones);
    hub.integrations.sync_one(added.id).await;
    let idea: WorkstreamId = IDEA.parse().unwrap();
    let task_now = |hub: &Hub| {
        hub.work
            .task(&pitcrew_hub_work::TaskRef::Id(task.id))
            .unwrap()
            .status
    };
    assert_eq!(task_now(&hub), TaskStatus::Done);
    assert_eq!(
        hub.work.workstream(&idea).unwrap().status,
        WorkstreamStatus::Shipped
    );

    // A person reopens the task: the next sync, with nothing new upstream, leaves it.
    hub.work
        .move_task(
            &sam(),
            &pitcrew_hub_work::TaskRef::Id(task.id),
            TaskStatus::Todo,
        )
        .unwrap();
    let rev = hub.work.store().latest_rev().unwrap();
    hub.integrations.sync_one(added.id).await;
    assert_eq!(task_now(&hub), TaskStatus::Todo);
    assert_eq!(hub.work.store().latest_rev().unwrap(), rev);
}

/// The Jira side: an issue a later read moves under a linked epic becomes a task, from the
/// snapshot the sync kept; a done one does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_jira_issue_moved_under_a_linked_epic_becomes_a_task() {
    let hub = hub();
    let added = hub.integrations.add(&sam(), jira()).await.unwrap();
    hub.integrations
        .set_credential(&sam(), &added.id, "synthetic-jira-secret-0002")
        .unwrap();
    let epic = ExternalRef {
        system: ExternalSystem::Jira,
        key: "DEMO-9".into(),
        url: Some("https://jira.example.com/browse/DEMO-9".into()),
    };
    link(&hub.work, SEED_RUNS, vec![epic.clone()]);
    hub.integrations.sync_one(added.id).await;
    let first = hub.integrations.get(&added.id).await.unwrap();
    assert!(
        first.status.problems.is_empty(),
        "{:?}",
        first.status.problems
    );
    // DEMO-6 is under DEMO-5, which no workstream links.
    assert!(mirrored(&hub.work, "DEMO-6").is_none());

    // The next read moves DEMO-6 (open) and DEMO-7 (done) under DEMO-9.
    let state: pitcrew_sync_jira::SyncState = hub.integrations.files.load_state(&added.id).unwrap();
    let at = pitcrew_sync_jira::JiraTimestamp::new("2026-01-03T09:00:00.000+0000");
    let moved = |key: &str| pitcrew_sync_jira::UpstreamChange::IssueReparented {
        source: ExternalRef {
            system: ExternalSystem::Jira,
            key: key.into(),
            url: Some(format!("https://jira.example.com/browse/{key}")),
        },
        at: at.clone(),
        epic: Some(epic.clone()),
    };
    let changes = vec![moved("DEMO-6"), moved("DEMO-7")];
    let openings = apply::jira_openings(&changes, &state);
    assert_eq!(openings.keys().collect::<Vec<_>>(), vec!["DEMO-6"]);
    let mut applier = Applier::new(
        hub.work.sync_commands(member_of(&hub, &added.id)).unwrap(),
        ExternalSystem::Jira,
    )
    .unwrap();
    apply::apply_jira(&mut applier, &changes, &openings);
    let applied = applier.finish();
    assert_eq!((applied.counts.applied, applied.counts.skipped), (1, 1));
    let story = mirrored(&hub.work, "DEMO-6").expect("DEMO-6 becomes a task");
    assert_eq!(story.workstream, Some(SEED_RUNS.parse().unwrap()));
    assert_eq!(story.title, "Send invoices by e-mail");
    assert_eq!(story.description.trim_end(), "Monthly, as a PDF.");
    assert!(
        mirrored(&hub.work, "DEMO-7").is_none(),
        "a done issue stays out"
    );
}

/// `DELETE` during a sync: what that sync read is neither applied nor kept, and its state file
/// is not written back after the removal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_integration_is_neither_applied_nor_kept() {
    // `gh` says it started, then waits to be let go: the sync is under way meanwhile.
    let hub = hub_with_gh(&format!(
        "#!/bin/sh\nhere=\"$(dirname \"$0\")\"\n: > \"$here/started\"\n\
         while [ ! -f \"$here/go\" ]; do sleep 0.05; done\necho {GH_TOKEN}\n"
    ));
    let bin = hub.dir.path().join("bin");
    let added = hub.integrations.add(&sam(), github()).await.unwrap();
    link(
        &hub.work,
        SEED_RUNS,
        vec![github_ref("example-org/demo-repo#milestone:1")],
    );
    let integrations = Arc::clone(&hub.integrations);
    let id = added.id;
    let sync = tokio::spawn(async move { integrations.sync_one(id).await });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !bin.join("started").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the sync never asked gh"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    hub.integrations.remove(&sam(), &added.id).unwrap();
    std::fs::write(bin.join("go"), "").unwrap();
    sync.await.unwrap();

    assert!(
        mirrored(&hub.work, "example-org/demo-repo#1").is_none(),
        "a removed integration's read was applied"
    );
    let state = hub
        .root
        .join("integrations")
        .join(format!("{}.state.json", added.id.0));
    assert!(!state.exists(), "a removed integration's state was kept");
    let saved = std::fs::read_to_string(hub.root.join("integrations.json")).unwrap();
    assert!(!saved.contains(&added.id.0.to_string()));
    assert_eq!(
        hub.integrations.get(&added.id).await.unwrap_err().code,
        ErrorCode::NotFound
    );
}

/// Each integration acts through its own owner's member: a second person's integration does not
/// take over the first one's, and the first one's conflicts are asked of the person who added it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_integration_acts_through_its_owners_member() {
    let hub = hub();
    let sams = hub.integrations.add(&sam(), github()).await.unwrap();
    let lees = hub.integrations.add(&person(LEE), jira()).await.unwrap();
    let sams_member = member_of(&hub, &sams.id);
    let lees_member = member_of(&hub, &lees.id);
    assert_ne!(sams_member, lees_member);
    let members = hub.work.members().unwrap();
    let owner_of = |id: MemberId| members.iter().find(|m| m.id == id).unwrap().owner;
    assert_eq!(owner_of(sams_member), Some(SAM.parse().unwrap()));
    assert_eq!(owner_of(lees_member), Some(LEE.parse().unwrap()));

    // Sam's integration syncs a task, Sam starts on it, and upstream closes it: the conflict is
    // asked of Sam, by Sam's member.
    link(
        &hub.work,
        SEED_RUNS,
        vec![github_ref("example-org/demo-repo#milestone:1")],
    );
    hub.integrations.sync_one(sams.id).await;
    let task = mirrored(&hub.work, "example-org/demo-repo#1").unwrap();
    assert!(
        hub.work
            .store()
            .since(0, 100_000)
            .unwrap()
            .iter()
            .any(|e| e.event.author == sams_member
                && matches!(&e.event.body, pitcrew_protocol::events::EventBody::TaskCreated { task: t } if t.id == task.id))
    );
    hub.work
        .move_task(
            &sam(),
            &pitcrew_hub_work::TaskRef::Id(task.id),
            TaskStatus::InProgress,
        )
        .unwrap();
    upstream_says(
        &hub,
        ISSUES,
        &issues_with(1, |issue| {
            issue.insert("state".into(), "closed".into());
            issue.insert("state_reason".into(), "completed".into());
        }),
    );
    hub.integrations.sync_one(sams.id).await;
    let asks = hub
        .work
        .asks(&pitcrew_hub_work::AskFilter::default())
        .unwrap();
    let ask = asks
        .iter()
        .find(|a| a.task == Some(task.id))
        .expect("a conflict ask");
    assert_eq!((ask.from, ask.to), (sams_member, SAM.parse().unwrap()));

    // A connection saved before the member was kept with it finds its owner's again.
    lock(&hub.integrations.saved)
        .integrations
        .iter_mut()
        .find(|r| r.id == sams.id)
        .unwrap()
        .sync_member = None;
    hub.integrations.sync_one(sams.id).await;
    assert_eq!(member_of(&hub, &sams.id), sams_member);
}

#[test]
fn the_gh_host_is_the_connections_own() {
    assert_eq!(github_host(None), "github.com");
    assert_eq!(github_host(Some("https://api.github.com")), "github.com");
    assert_eq!(
        github_host(Some("https://GHE.example.com/api/v3")),
        "ghe.example.com"
    );
    assert_eq!(
        github_host(Some("https://api.example-co.ghe.com")),
        "example-co.ghe.com"
    );
}

#[test]
fn utc_timestamps_are_githubs_shape() {
    assert_eq!(utc_rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(utc_rfc3339(1_767_225_600), "2026-01-01T00:00:00Z");
    assert_eq!(utc_rfc3339(1_772_323_199), "2026-02-28T23:59:59Z");
    assert_eq!(utc_rfc3339(951_782_400), "2000-02-29T00:00:00Z");
}
