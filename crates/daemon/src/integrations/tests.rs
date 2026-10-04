//! The sync loop's work, over the demo workspace and the recorded fixtures in
//! `apps/mock-hub/fixtures` (no network), with a stand-in `gh` (never the machine's own).

#![allow(clippy::unwrap_used)]

use super::*;
use pitcrew_hub_work::{TaskFilter, WorkstreamPatch};
use pitcrew_protocol::api::TokenScope;
use pitcrew_protocol::ids::{TaskId, WorkstreamId};
use pitcrew_protocol::model::{AskState, ExternalRef, Mover, Task, TaskStatus, WorkstreamStatus};
use std::path::PathBuf;

const SAM: &str = "01JB000000000000000MEM0001";
const SEED_RUNS: &str = "01JB000000000000000WST0002";
const SUBMISSION: &str = "01JB000000000000000WST0001";
const IDEA: &str = "01JB000000000000000WST0004";
const GH_TOKEN: &str = "synthetic-gh-credential-0001";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/mock-hub/fixtures")
}

fn sam() -> Caller {
    Caller {
        member: SAM.parse().unwrap(),
        scope: TokenScope::Device,
        on_behalf_of: None,
    }
}

struct Hub {
    _dir: tempfile::TempDir,
    root: PathBuf,
    work: Arc<WorkService>,
    integrations: Arc<Integrations>,
}

#[cfg(unix)]
fn stand_in_gh(dir: &Path) -> GhCli {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gh = bin.join("gh");
    std::fs::write(&gh, format!("#!/bin/sh\necho {GH_TOKEN}\n")).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
    GhCli::with_path(bin.into_os_string())
}

#[cfg(not(unix))]
fn stand_in_gh(dir: &Path) -> GhCli {
    // No stand-in on Windows: an empty PATH, so `gh` is missing (never the machine's own).
    GhCli::with_path(dir.as_os_str().to_owned())
}

fn hub() -> Hub {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    std::fs::create_dir(&root).unwrap();
    let demo = pitcrew_fixtures::demo_workspace().unwrap();
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
        http::FixtureTransport::load(&fixtures()).unwrap(),
    ));
    let gh = stand_in_gh(dir.path());
    let integrations = Arc::new(Integrations::open(&root, &work, upstream, gh).unwrap());
    Hub {
        _dir: dir,
        root,
        work,
        integrations,
    }
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

#[cfg(unix)]
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
    let sync_member = lock(&hub.integrations.saved).sync_member.unwrap();
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

#[cfg(unix)]
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
    let sync_member = lock(&hub.integrations.saved).sync_member.unwrap();
    let commands = hub.work.sync_commands(sync_member).unwrap();
    let mut applier = Applier::new(commands, ExternalSystem::Github).unwrap();
    let closed = pitcrew_sync_github::UpstreamChange::IssueClosed {
        source: task.source.clone().unwrap(),
        at: pitcrew_sync_github::GithubTimestamp::new("2026-01-03T00:00:00Z"),
        reason: pitcrew_sync_github::CloseReason::Completed,
    };
    apply::apply_github(&mut applier, std::slice::from_ref(&closed));
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
    apply::apply_github(&mut applier, std::slice::from_ref(&closed));
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

#[cfg(unix)]
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
    apply::apply_github(&mut applier, &[created, closed.clone()]);
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
    apply::apply_github(&mut applier, &[closed]);
    assert_eq!(
        hub.work.workstream(&idea).unwrap().status,
        WorkstreamStatus::Shipped
    );
}

#[cfg(unix)]
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
    assert_eq!(reopened.list().await.unwrap().len(), 1);
    let _ = TaskId::new();
}

#[test]
fn utc_timestamps_are_githubs_shape() {
    assert_eq!(utc_rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(utc_rfc3339(1_767_225_600), "2026-01-01T00:00:00Z");
    assert_eq!(utc_rfc3339(1_772_323_199), "2026-02-28T23:59:59Z");
    assert_eq!(utc_rfc3339(951_782_400), "2000-02-29T00:00:00Z");
}
