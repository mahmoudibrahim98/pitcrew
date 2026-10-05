//! Outward writes, end to end in the hub (G's acceptance: "no outward write without an answered
//! approval ask"), over the demo workspace and the recorded fixtures in `apps/mock-hub/fixtures`.
//! The transport keeps every request it was sent, so each test shows what reached "upstream" and
//! when: nothing before an approval, nothing after a denial, never twice for one approval or one
//! retry, and nothing upstream changed since it was proposed. Each hub reads a copy of the
//! fixtures, so a test changes what "upstream" says by adding a file that sorts first.
//! Credentials are stored secrets (no `gh`), so these run on every platform.

#![allow(clippy::unwrap_used)]

use super::*;
use pitcrew_hub_work::{AnswerAsk, NewAsk, NewTask, TaskFilter, TaskRef, WorkstreamPatch};
use pitcrew_protocol::api::TokenScope;
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{AskId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{AskKind, AskState, ExternalRef, Task, TaskStatus};
use pitcrew_protocol::writes::{
    APPROVAL_OPTIONS, IssueState, NewWrite, UpstreamWrite, WriteOperation, WriteResult, WriteState,
};
use pitcrew_sync_github::transport::{Method, Request};
use std::path::PathBuf;

const SAM: &str = "01JB000000000000000MEM0001";
const WRITER: &str = "01JB000000000000000MEM0002";
const SEED_RUNS: &str = "01JB000000000000000WST0002";
const IDEA: &str = "01JB000000000000000WST0004";
const SECRET: &str = "synthetic-write-secret-0001";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/mock-hub/fixtures")
}

fn person(id: &str) -> Caller {
    Caller {
        member: id.parse().unwrap(),
        scope: TokenScope::Device,
        on_behalf_of: None,
    }
}

fn sam() -> Caller {
    person(SAM)
}

fn writer() -> Caller {
    Caller {
        member: WRITER.parse().unwrap(),
        scope: TokenScope::Agent,
        on_behalf_of: Some(SAM.parse().unwrap()),
    }
}

struct Hub {
    dir: tempfile::TempDir,
    work: Arc<WorkService>,
    integrations: Arc<Integrations>,
    upstream: http::FixtureTransport,
}

impl Hub {
    fn new() -> Self {
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
        let copy = dir.path().join("fixtures");
        std::fs::create_dir(&copy).unwrap();
        for entry in std::fs::read_dir(fixtures()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|x| x == "fixture") {
                std::fs::copy(&path, copy.join(path.file_name().unwrap())).unwrap();
            }
        }
        let upstream = http::FixtureTransport::load(&copy).unwrap();
        // No `gh` on this PATH: these integrations keep a stored secret.
        let gh = GhCli::with_path(dir.path().join("state").into_os_string());
        let integrations = Arc::new(
            Integrations::open(&root, &work, Ok(Upstream::Fixtures(upstream.clone())), gh).unwrap(),
        );
        Self {
            dir,
            work,
            integrations,
            upstream,
        }
    }

    /// From now on, upstream answers `method url` with `status` and `body` (a file that sorts
    /// before the recorded fixtures; a later call for the same request wins).
    fn upstream_says(&self, method: &str, url: &str, status: u16, body: &serde_json::Value) {
        let path = self.dir.path().join("fixtures/0-test.fixture");
        let before = std::fs::read_to_string(&path).unwrap_or_default();
        let block = format!(
            "{method} {url} HTTP/1.1\nAccept: application/json\n\nHTTP/1.1 {status}\n\n{body}\n"
        );
        let text = if before.is_empty() {
            block
        } else {
            format!("{block}### pitcrew-github-fixture ###\n{before}")
        };
        std::fs::write(&path, text).unwrap();
    }

    /// The hub started again over the same state: nothing kept in memory survives.
    fn restart(&mut self) {
        let root = self.dir.path().join("state");
        let gh = GhCli::with_path(root.clone().into_os_string());
        self.integrations = Arc::new(
            Integrations::open(
                &root,
                &self.work,
                Ok(Upstream::Fixtures(self.upstream.clone())),
                gh,
            )
            .unwrap(),
        );
    }

    /// Adds `settings` with a stored secret, links `workstream` to `links`, and syncs once.
    async fn connect(
        &self,
        settings: IntegrationSettings,
        workstream: &str,
        links: Vec<ExternalRef>,
    ) -> IntegrationId {
        let added = self
            .integrations
            .add(
                &sam(),
                NewIntegration {
                    name: "Synthetic".into(),
                    settings,
                    credential: CredentialSource::Stored,
                    interval_minutes: Some(1440),
                },
            )
            .await
            .unwrap();
        self.integrations
            .set_credential(&sam(), &added.id, SECRET)
            .unwrap();
        let id: WorkstreamId = workstream.parse().unwrap();
        self.work
            .patch_workstream(
                &sam(),
                &id,
                WorkstreamPatch {
                    external: Some(links),
                    ..WorkstreamPatch::default()
                },
            )
            .unwrap();
        // The planner starts at the log's end; a pass before any change sets it there.
        self.pass().await;
        self.integrations.sync_one(added.id).await;
        let synced = self.integrations.get(&added.id).await.unwrap();
        assert!(
            synced.status.problems.is_empty(),
            "{:?}",
            synced.status.problems
        );
        added.id
    }

    /// One pass of the loop's write work: propose, then settle.
    async fn pass(&self) {
        self.integrations.plan_writes().await;
        self.integrations.settle_writes().await;
    }

    /// Every request that was not a read.
    fn writes_sent(&self) -> Vec<Request> {
        self.upstream
            .sent()
            .into_iter()
            .filter(|r| r.method != Method::Get)
            .collect()
    }

    fn writes(&self) -> Vec<UpstreamWrite> {
        self.work
            .writes(&pitcrew_hub_work::WriteFilter::default())
            .unwrap()
    }

    fn mirrored(&self, key: &str) -> Task {
        self.work
            .tasks(&TaskFilter::default())
            .unwrap()
            .into_iter()
            .find(|t| t.source.as_ref().is_some_and(|s| s.key == key))
            .unwrap_or_else(|| panic!("a task mirrors {key}"))
    }

    fn answer(&self, ask: &AskId, option: usize) {
        self.work
            .answer_ask(
                &sam(),
                ask,
                AnswerAsk {
                    option: Some(option),
                    text: None,
                },
            )
            .unwrap();
    }

    fn move_task(&self, task: TaskId, to: TaskStatus) {
        self.work.move_task(&sam(), &TaskRef::Id(task), to).unwrap();
    }

    fn event_kinds(&self, ask: &AskId) -> Vec<&'static str> {
        self.work
            .store()
            .since(0, 100_000)
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.event.body {
                EventBody::WriteProposed { write } if write.ask == *ask => Some("write_proposed"),
                EventBody::WriteStarted { ask: a, .. } if a == *ask => Some("write_started"),
                EventBody::WriteRetryRequested { ask: a, .. } if a == *ask => {
                    Some("write_retry_requested")
                }
                EventBody::WriteFinished { ask: a, .. } if a == *ask => Some("write_finished"),
                _ => None,
            })
            .collect()
    }
}

fn github() -> IntegrationSettings {
    IntegrationSettings::Github {
        repos: vec!["example-org/demo-repo".into()],
        api_base: None,
    }
}

fn link(key: &str) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: key.into(),
        url: None,
    }
}

fn body(request: &Request) -> serde_json::Value {
    serde_json::from_slice(&request.body).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_write_is_sent_before_approval_after_a_denial_or_twice() {
    let hub = Hub::new();
    hub.connect(
        github(),
        SEED_RUNS,
        vec![link("example-org/demo-repo#milestone:1")],
    )
    .await;
    let task = hub.mirrored("example-org/demo-repo#1");
    assert!(hub.writes_sent().is_empty(), "a sync only reads");

    // A person closes the task: an approval ask, and nothing sent.
    hub.move_task(task.id, TaskStatus::Done);
    hub.pass().await;
    hub.pass().await;
    let writes = hub.writes();
    assert_eq!(writes.len(), 1, "one change, one proposal: {writes:?}");
    let first = &writes[0];
    assert_eq!(first.state, WriteState::Pending);
    assert_eq!(first.proposal.operation, WriteOperation::Close);
    assert_eq!(first.proposal.before.state, Some(IssueState::Open));
    assert_eq!(first.proposal.after.state, Some(IssueState::Closed));
    assert_eq!(first.proposal.requested_by, sam().member);
    let ask = hub.work.ask(&first.proposal.ask).unwrap();
    assert_eq!(ask.kind, AskKind::Approval);
    assert_eq!(ask.to, sam().member);
    assert_eq!(ask.task, Some(task.id));
    assert_eq!(ask.options, APPROVAL_OPTIONS.map(str::to_owned).to_vec());
    assert!(
        ask.body.contains("state: open → closed (completed)"),
        "{}",
        ask.body
    );
    assert!(hub.writes_sent().is_empty(), "nothing before an answer");

    // "Don't send": recorded as not sent, and still nothing sent.
    hub.answer(&first.proposal.ask, 1);
    hub.pass().await;
    let denied = hub.work.write(&first.proposal.ask).unwrap();
    assert_eq!(denied.state, WriteState::NotSent);
    assert!(
        matches!(&denied.result, Some(WriteResult::NotSent { reason }) if reason.contains("chose not to")),
        "{denied:?}"
    );
    assert_eq!(
        hub.event_kinds(&first.proposal.ask),
        vec!["write_proposed", "write_finished"]
    );
    assert!(hub.writes_sent().is_empty(), "nothing after a denial");

    // Back to todo: upstream is still open, so nothing to reopen.
    hub.move_task(task.id, TaskStatus::Todo);
    hub.pass().await;
    assert_eq!(hub.writes().len(), 1);

    // Done again, and approved: sent exactly once, exactly as shown.
    hub.move_task(task.id, TaskStatus::Done);
    hub.pass().await;
    let second = hub
        .writes()
        .into_iter()
        .find(|w| w.state == WriteState::Pending)
        .unwrap();
    assert!(hub.writes_sent().is_empty());
    hub.answer(&second.proposal.ask, 0);
    hub.pass().await;
    hub.pass().await;
    let sent = hub.writes_sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].method, Method::Patch);
    assert_eq!(
        sent[0].url,
        "https://api.github.com/repos/example-org/demo-repo/issues/1"
    );
    assert_eq!(
        body(&sent[0]),
        serde_json::json!({"state": "closed", "state_reason": "completed"})
    );
    assert_eq!(
        sent[0].header("authorization"),
        Some(format!("Bearer {SECRET}").as_str())
    );
    let done = hub.work.write(&second.proposal.ask).unwrap();
    assert_eq!((done.state, done.attempts), (WriteState::Sent, 1));
    assert_eq!(
        hub.event_kinds(&second.proposal.ask),
        vec!["write_proposed", "write_started", "write_finished"]
    );
    // A sent write is never sent again.
    assert_eq!(
        hub.integrations
            .retry_write(&sam(), second.proposal.ask)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    hub.pass().await;
    assert_eq!(hub.writes_sent().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_created_issue_becomes_the_tasks_source_and_a_failure_is_retried_once_per_retry() {
    let hub = Hub::new();
    hub.connect(
        github(),
        IDEA,
        vec![link("example-org/demo-repo#milestone:2")],
    )
    .await;
    let created = hub
        .work
        .create_task(
            &sam(),
            NewTask {
                project: hub.work.workstream(&IDEA.parse().unwrap()).unwrap().project,
                workstream: Some(IDEA.parse().unwrap()),
                title: "Synthetic new issue".into(),
                description: Some("Written in PitCrew.".into()),
                status: None,
                priority: None,
                assignee: None,
                labels: Some(vec!["docs".into()]),
                due: None,
            },
        )
        .unwrap();
    hub.pass().await;
    assert!(hub.writes().is_empty(), "creating a task creates no issue");

    // A comment needs an issue first; an agent cannot ask at all (the route), and other
    // operations are the hub's own.
    let comment = NewWrite {
        task: created.id,
        operation: WriteOperation::Comment,
        text: Some("Hello".into()),
    };
    assert_eq!(
        hub.integrations
            .request_write(&sam(), comment.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let close = NewWrite {
        task: created.id,
        operation: WriteOperation::Close,
        text: None,
    };
    assert_eq!(
        hub.integrations
            .request_write(&sam(), close)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Invalid
    );

    let create = hub
        .integrations
        .request_write(
            &sam(),
            NewWrite {
                task: created.id,
                operation: WriteOperation::CreateIssue,
                text: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(create.state, WriteState::Pending);
    assert_eq!(
        create.proposal.after.milestone.as_deref(),
        Some("example-org/demo-repo#milestone:2")
    );
    hub.pass().await;
    assert!(hub.writes_sent().is_empty());
    hub.answer(&create.proposal.ask, 0);
    hub.pass().await;
    let sent = hub.writes_sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        body(&sent[0]),
        serde_json::json!({"title": "Synthetic new issue", "body": "Written in PitCrew.",
            "labels": ["docs"], "milestone": 2})
    );
    let task = hub.work.task(&TaskRef::Id(created.id)).unwrap();
    assert_eq!(
        task.source.as_ref().map(|s| s.key.as_str()),
        Some("example-org/demo-repo#8")
    );
    assert_eq!(
        hub.work.write(&create.proposal.ask).unwrap().result,
        Some(WriteResult::Sent {
            created: task.source.clone(),
            url: Some("https://github.com/example-org/demo-repo/issues/8".into()),
        })
    );

    // A comment the fixture refuses: failed, with upstream's status; never resent by itself.
    let comment = hub
        .integrations
        .request_write(&sam(), comment)
        .await
        .unwrap();
    hub.answer(&comment.proposal.ask, 0);
    hub.pass().await;
    hub.pass().await;
    let failed = hub.work.write(&comment.proposal.ask).unwrap();
    assert_eq!((failed.state, failed.attempts), (WriteState::Failed, 1));
    assert!(
        matches!(&failed.result, Some(WriteResult::Failed { status: Some(422), message }) if message.contains("Validation Failed")),
        "{failed:?}"
    );
    assert_eq!(hub.writes_sent().len(), 2);

    // Only a person who may answer it retries it; each retry sends it once more.
    assert_eq!(
        hub.integrations
            .retry_write(&writer(), comment.proposal.ask)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    hub.integrations
        .retry_write(&sam(), comment.proposal.ask)
        .await
        .unwrap();
    hub.integrations
        .retry_write(&sam(), comment.proposal.ask)
        .await
        .unwrap();
    hub.pass().await;
    hub.pass().await;
    assert_eq!(
        hub.writes_sent().len(),
        3,
        "two retries asked at once send once"
    );
    let again = hub.work.write(&comment.proposal.ask).unwrap();
    assert_eq!((again.state, again.attempts), (WriteState::Failed, 2));
    // The request is in the log, once; the attempt used it, so no pass sends it again.
    assert_eq!(
        hub.event_kinds(&comment.proposal.ask),
        vec![
            "write_proposed",
            "write_started",
            "write_finished",
            "write_retry_requested",
            "write_started",
            "write_finished"
        ]
    );
    assert_eq!(again.retry_requested_by, None);
    hub.pass().await;
    assert_eq!(hub.writes_sent().len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_cut_off_and_crafted_approvals_send_nothing() {
    let hub = Hub::new();
    hub.connect(
        github(),
        SEED_RUNS,
        vec![link("example-org/demo-repo#milestone:1")],
    )
    .await;
    let task = hub.mirrored("example-org/demo-repo#1");

    // Two title edits, approved oldest first: the first is no longer what the task says.
    for title in ["Fix the flaky login test", "Fix the login test for good"] {
        hub.work
            .patch_task(
                &sam(),
                &TaskRef::Id(task.id),
                pitcrew_hub_work::TaskPatch {
                    title: Some(title.into()),
                    ..pitcrew_hub_work::TaskPatch::default()
                },
            )
            .unwrap();
    }
    hub.pass().await;
    let edits = hub.writes();
    assert_eq!(edits.len(), 2);
    assert_eq!(
        edits[0].proposal.before.title.as_deref(),
        Some("Fix flaky login test"),
        "before is upstream's, as last read"
    );
    hub.answer(&edits[0].proposal.ask, 0);
    hub.pass().await;
    let stale = hub.work.write(&edits[0].proposal.ask).unwrap();
    assert_eq!(stale.state, WriteState::NotSent);
    assert!(hub.writes_sent().is_empty());

    // A write cut off while being sent (the hub stopped): failed, and not sent again by itself.
    hub.answer(&edits[1].proposal.ask, 0);
    let member = lock(&hub.integrations.saved).integrations[0]
        .sync_member
        .unwrap();
    hub.work
        .sync_commands(member)
        .unwrap()
        .start_write(&edits[1].proposal.ask)
        .unwrap();
    hub.pass().await;
    let cut = hub.work.write(&edits[1].proposal.ask).unwrap();
    assert_eq!(cut.state, WriteState::Failed);
    assert!(
        matches!(&cut.result, Some(WriteResult::Failed { message, .. }) if message.contains("stopped while sending"))
    );
    assert!(hub.writes_sent().is_empty());

    // An approval ask an agent raises itself, answered "Send": nothing.
    let crafted = hub
        .work
        .raise_ask(
            &writer(),
            NewAsk {
                kind: AskKind::Approval,
                to: sam().member,
                title: "GitHub: close example-org/demo-repo#1".into(),
                body: None,
                options: Some(APPROVAL_OPTIONS.map(str::to_owned).to_vec()),
                task: None,
                session: None,
                receipts: None,
            },
        )
        .unwrap();
    hub.answer(&crafted.id, 0);
    hub.pass().await;
    assert_eq!(hub.work.ask(&crafted.id).unwrap().state, AskState::Answered);
    assert!(hub.writes_sent().is_empty());
    assert_eq!(hub.writes().len(), 2);

    // The sync's own changes are never sent back.
    let events = hub.work.store().since(0, 100_000).unwrap();
    assert!(events.iter().all(|e| match &e.event.body {
        EventBody::WriteProposed { write } => write.requested_by != member,
        _ => true,
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jira_closes_through_its_workflow_and_sends_its_labels() {
    let hub = Hub::new();
    hub.connect(
        IntegrationSettings::Jira {
            deployment: JiraDeployment::Cloud,
            site: "https://jira.example.com".into(),
            projects: vec!["DEMO".into()],
            email: Some("sam@example.com".into()),
            epic_link_field: None,
        },
        SEED_RUNS,
        vec![ExternalRef {
            system: ExternalSystem::Jira,
            key: "DEMO".into(),
            url: None,
        }],
    )
    .await;
    let story = hub.mirrored("DEMO-6");
    hub.move_task(story.id, TaskStatus::Canceled);
    hub.work
        .patch_task(
            &sam(),
            &TaskRef::Id(story.id),
            pitcrew_hub_work::TaskPatch {
                labels: Some(vec!["billing".into(), "email".into()]),
                ..pitcrew_hub_work::TaskPatch::default()
            },
        )
        .unwrap();
    hub.pass().await;
    let writes = hub.writes();
    assert_eq!(
        writes
            .iter()
            .map(|w| w.proposal.operation)
            .collect::<Vec<_>>(),
        vec![WriteOperation::Close, WriteOperation::Update]
    );
    assert_eq!(writes[0].proposal.after.close_reason, None, "GitHub only");
    assert_eq!(
        writes[1].proposal.before.labels,
        Some(vec!["billing".to_string()])
    );
    assert_eq!(
        writes[1].proposal.after.add_labels,
        Some(vec!["email".to_string()])
    );
    assert_eq!(writes[1].proposal.after.remove_labels, None);
    for w in &writes {
        hub.answer(&w.proposal.ask, 0);
    }
    hub.pass().await;
    let sent = hub.writes_sent();
    let api = "https://jira.example.com/rest/api/3/issue/DEMO-6";
    // Each was checked against the issue as Jira has it now, first.
    assert_eq!(
        hub.upstream
            .sent()
            .iter()
            .filter(|r| r.method == Method::Get && r.url.starts_with(&format!("{api}?fields=")))
            .count(),
        2
    );
    assert_eq!(
        sent.iter()
            .map(|r| (r.method, r.url.clone(), body(r)))
            .collect::<Vec<_>>(),
        vec![
            (
                Method::Post,
                format!("{api}/transitions"),
                serde_json::json!({"transition": {"id": "31"}})
            ),
            (
                Method::Put,
                api.to_owned(),
                serde_json::json!({"update": {"labels": [{"add": "email"}]}})
            ),
        ]
    );
    assert!(hub.writes().iter().all(|w| w.state == WriteState::Sent));
    // The secret went only in Authorization, never in a URL, a body or a saved write.
    let everything = serde_json::to_string(&hub.writes()).unwrap();
    assert!(!everything.contains(SECRET));
    for request in &sent {
        assert!(!request.url.contains(SECRET));
        assert!(!String::from_utf8_lossy(&request.body).contains(SECRET));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moving_a_task_to_another_milestones_workstream_asks_to_set_it() {
    let hub = Hub::new();
    hub.connect(
        github(),
        SEED_RUNS,
        vec![link("example-org/demo-repo#milestone:1")],
    )
    .await;
    let idea: WorkstreamId = IDEA.parse().unwrap();
    hub.work
        .patch_workstream(
            &sam(),
            &idea,
            WorkstreamPatch {
                external: Some(vec![link("example-org/demo-repo#milestone:2")]),
                ..WorkstreamPatch::default()
            },
        )
        .unwrap();
    let task = hub.mirrored("example-org/demo-repo#1");
    hub.work
        .patch_task(
            &sam(),
            &TaskRef::Id(task.id),
            pitcrew_hub_work::TaskPatch {
                workstream: Some(Some(idea)),
                ..pitcrew_hub_work::TaskPatch::default()
            },
        )
        .unwrap();
    hub.pass().await;
    let writes = hub.writes();
    assert_eq!(writes.len(), 1);
    let w = &writes[0].proposal;
    assert_eq!(w.operation, WriteOperation::Update);
    assert_eq!(
        (w.before.milestone.as_deref(), w.after.milestone.as_deref()),
        (
            Some("example-org/demo-repo#milestone:1"),
            Some("example-org/demo-repo#milestone:2")
        )
    );
    assert_eq!(w.after.names(), vec!["milestone"]);
    hub.answer(&w.ask, 0);
    hub.pass().await;
    let sent = hub.writes_sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(body(&sent[0]), serde_json::json!({"milestone": 2}));
}

/// Write events and approval asks reach activity and `/v1/stream` only through the hub's shared
/// visibility check, as `serve.rs` mounts it. None of them names a session, so excluding every
/// session (`/v1/import`, mode `none`) hides none of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_pass_the_shared_visibility_check() {
    use pitcrew_protocol::import::{ImportFilter, ImportMode};
    let hub = Hub::new();
    hub.connect(
        github(),
        SEED_RUNS,
        vec![link("example-org/demo-repo#milestone:1")],
    )
    .await;
    let task = hub.mirrored("example-org/demo-repo#1");
    hub.move_task(task.id, TaskStatus::Done);
    hub.pass().await;
    let write = hub.writes().remove(0);
    hub.answer(&write.proposal.ask, 0);
    hub.pass().await;
    assert_eq!(
        hub.work.write(&write.proposal.ask).unwrap().state,
        WriteState::Sent
    );

    let visibility = pitcrew_api::visibility::Visibility(Some(Arc::new(
        crate::visibility::WorkVisibility(Arc::clone(&hub.work)),
    )));
    hub.work
        .commit_import(ImportFilter {
            mode: ImportMode::None,
            ..ImportFilter::default()
        })
        .unwrap();
    let person = Some(sam().member);
    let mut kinds = Vec::new();
    for stored in hub.work.store().since(0, 100_000).unwrap() {
        let event = &stored.event;
        let kind = match &event.body {
            EventBody::WriteProposed { .. } => "write_proposed",
            EventBody::WriteStarted { .. } => "write_started",
            EventBody::WriteFinished { .. } => "write_finished",
            EventBody::AskRaised { ask } if ask.id == write.proposal.ask => "ask_raised",
            EventBody::AskAnswered { ask, .. } if *ask == write.proposal.ask => "ask_answered",
            _ => continue,
        };
        assert!(
            visibility.visible(event, None).unwrap(),
            "activity hides {kind}"
        );
        assert!(
            visibility.visible(event, person).unwrap(),
            "the stream hides {kind}"
        );
        kinds.push(kind);
    }
    assert_eq!(
        kinds,
        vec![
            "ask_raised",
            "write_proposed",
            "ask_answered",
            "write_started",
            "write_finished"
        ]
    );
}

const ISSUES: &str = "https://api.github.com/repos/example-org/demo-repo/issues?state=all&sort=updated&direction=asc&per_page=100";
const ISSUE_1: &str = "https://api.github.com/repos/example-org/demo-repo/issues/1";

/// Issue #1 as the recorded fixtures have it, with `change` made to it.
fn issue_1(change: impl FnOnce(&mut serde_json::Value)) -> serde_json::Value {
    let mut issue = serde_json::json!({
        "number": 1, "title": "Fix flaky login test", "body": "Login fails one run in ten on CI.",
        "state": "open", "labels": [{"name": "bug"}, {"name": "tests"}],
        "assignees": [], "milestone": {"number": 1, "title": "v1 launch", "state": "open"},
        "updated_at": "2026-01-02T09:00:00Z", "created_at": "2026-01-01T09:00:00Z",
        "html_url": "https://github.com/example-org/demo-repo/issues/1"
    });
    change(&mut issue);
    issue
}

fn patch(hub: &Hub, task: TaskId, patch: pitcrew_hub_work::TaskPatch) {
    hub.work
        .patch_task(&sam(), &TaskRef::Id(task), patch)
        .unwrap();
}

/// Review item 1: PitCrew never writes back a copy it holds lossily. A body with a hidden
/// character is never proposed, a title with one is left out (and the ask says so), and labels go
/// as a change: the labels upstream has that the hub does not hold (beyond its 32, or added since)
/// are never touched, and nothing sends the whole list.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lossy_text_is_never_written_back_and_labels_go_as_a_change() {
    let hub = Hub::new();
    // Upstream's #1: a zero-width joiner in its title and body, and 40 labels.
    let many: Vec<serde_json::Value> = ["bug", "tests"]
        .into_iter()
        .map(str::to_owned)
        .chain((1..=38).map(|n| format!("zz-{n:02}")))
        .map(|name| serde_json::json!({"name": name}))
        .collect();
    let lossy = issue_1(|i| {
        i["title"] = "Fix flaky login test \u{1F9D1}\u{200D}\u{1F4BB}".into();
        i["body"] = "Login fails one run in ten on CI.\u{200D}".into();
        i["labels"] = many.clone().into();
    });
    hub.upstream_says("GET", ISSUES, 200, &serde_json::json!([lossy]));
    hub.connect(
        github(),
        SEED_RUNS,
        vec![link("example-org/demo-repo#milestone:1")],
    )
    .await;
    let task = hub.mirrored("example-org/demo-repo#1");
    assert_eq!(task.labels.len(), 32, "the hub holds 32 of upstream's 40");

    // A description fix alone: upstream's body is not held exactly, so nothing is proposed.
    patch(
        &hub,
        task.id,
        pitcrew_hub_work::TaskPatch {
            description: Some("Login fails one run in ten on CI, on Linux.".into()),
            ..pitcrew_hub_work::TaskPatch::default()
        },
    );
    hub.pass().await;
    assert!(hub.writes().is_empty(), "{:?}", hub.writes());

    // A title change and a label change: labels go as a change, the title is left out (the
    // ask says why), and the body is not sent.
    let mut labels: Vec<String> = task
        .labels
        .iter()
        .filter(|l| *l != "tests")
        .cloned()
        .collect();
    labels.push("docs".into());
    patch(
        &hub,
        task.id,
        pitcrew_hub_work::TaskPatch {
            title: Some("Fix the flaky login test".into()),
            labels: Some(labels),
            ..pitcrew_hub_work::TaskPatch::default()
        },
    );
    hub.pass().await;
    let writes = hub.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    let w = &writes[0].proposal;
    assert_eq!(w.operation, WriteOperation::Update);
    assert_eq!(w.after.names(), vec!["add_labels", "remove_labels"]);
    assert_eq!(w.after.add_labels, Some(vec!["docs".to_string()]));
    assert_eq!(w.after.remove_labels, Some(vec!["tests".to_string()]));
    assert_eq!(w.before.labels.as_ref().map(Vec::len), Some(40));
    let ask = hub.work.ask(&w.ask).unwrap();
    assert!(ask.body.contains("+ docs, − tests"), "{}", ask.body);
    assert!(ask.body.contains("Not sent: the title."), "{}", ask.body);

    // Meanwhile a colleague adds `security` upstream. Approved: the labels change and nothing
    // else is sent; `security` and the 8 labels the hub never held stay.
    let mut now = many.clone();
    now.push(serde_json::json!({"name": "security"}));
    hub.upstream_says("GET", ISSUE_1, 200, &issue_1(|i| i["labels"] = now.into()));
    hub.answer(&w.ask, 0);
    hub.pass().await;
    let sent = hub.writes_sent();
    assert_eq!(
        sent.iter()
            .map(|r| (r.method, r.url.clone()))
            .collect::<Vec<_>>(),
        vec![
            (Method::Post, format!("{ISSUE_1}/labels")),
            (Method::Delete, format!("{ISSUE_1}/labels/tests")),
        ]
    );
    assert_eq!(body(&sent[0]), serde_json::json!({"labels": ["docs"]}));
    assert_eq!(hub.work.write(&w.ask).unwrap().state, WriteState::Sent);
}

/// Review item 1, Jira: a description with formatting (a list) is never written back as the plain
/// paragraphs PitCrew holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_jira_description_with_formatting_is_never_written_back() {
    let hub = Hub::new();
    let search = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29%20ORDER%20BY%20updated%20ASC%2C%20key%20ASC&maxResults=100&fields=summary%2Cdescription%2Cstatus%2Cresolution%2Clabels%2Cassignee%2Cparent%2Cissuetype%2Cupdated";
    hub.upstream_says(
        "GET",
        search,
        200,
        &serde_json::json!({"issues": [{"id": "10006", "key": "DEMO-6", "fields": {
            "summary": "Send invoices by e-mail",
            "description": {"type": "doc", "version": 1, "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "Monthly:"}]},
                {"type": "bulletList", "content": [{"type": "listItem", "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "as a PDF"}]}]}]}
            ]},
            "status": {"statusCategory": {"key": "new"}}, "labels": ["billing"],
            "parent": {"key": "DEMO-5"}, "issuetype": {"name": "Story"},
            "updated": "2026-01-02T09:05:00.000+0000"}}]}),
    );
    hub.connect(
        IntegrationSettings::Jira {
            deployment: JiraDeployment::Cloud,
            site: "https://jira.example.com".into(),
            projects: vec!["DEMO".into()],
            email: Some("sam@example.com".into()),
            epic_link_field: None,
        },
        SEED_RUNS,
        vec![ExternalRef {
            system: ExternalSystem::Jira,
            key: "DEMO".into(),
            url: None,
        }],
    )
    .await;
    let story = hub.mirrored("DEMO-6");
    assert!(
        story.description.contains("as a PDF"),
        "{}",
        story.description
    );
    patch(
        &hub,
        story.id,
        pitcrew_hub_work::TaskPatch {
            description: Some(story.description.replace("as a PDF", "as a PDF, by e-mail")),
            ..pitcrew_hub_work::TaskPatch::default()
        },
    );
    hub.pass().await;
    assert!(hub.writes().is_empty(), "{:?}", hub.writes());
    assert!(hub.writes_sent().is_empty());
}

/// Review item 2: an approved edit is checked against the issue as upstream has it now, just
/// before it is sent. A field upstream changed since it was read sends nothing ("changed upstream
/// since"); a change upstream already has is not sent again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approved_edit_is_checked_against_upstream_as_it_is_now() {
    let hub = Hub::new();
    hub.connect(
        github(),
        SEED_RUNS,
        vec![link("example-org/demo-repo#milestone:1")],
    )
    .await;
    let task = hub.mirrored("example-org/demo-repo#1");
    patch(
        &hub,
        task.id,
        pitcrew_hub_work::TaskPatch {
            title: Some("Fix the flaky login test".into()),
            ..pitcrew_hub_work::TaskPatch::default()
        },
    );
    hub.pass().await;
    let retitle = hub.writes().remove(0);
    assert_eq!(
        retitle.proposal.before.title.as_deref(),
        Some("Fix flaky login test")
    );
    // A colleague retitles it upstream after the proposal.
    hub.upstream_says(
        "GET",
        ISSUE_1,
        200,
        &issue_1(|i| i["title"] = "Fix flaky login test on Linux".into()),
    );
    hub.answer(&retitle.proposal.ask, 0);
    hub.pass().await;
    let stopped = hub.work.write(&retitle.proposal.ask).unwrap();
    assert_eq!((stopped.state, stopped.attempts), (WriteState::NotSent, 0));
    assert!(
        matches!(&stopped.result, Some(WriteResult::NotSent { reason }) if reason.contains("changed upstream since") && reason.contains("(title)")),
        "{stopped:?}"
    );
    assert!(
        hub.writes_sent().is_empty(),
        "nothing over the colleague's title"
    );
    assert_eq!(
        hub.event_kinds(&retitle.proposal.ask),
        vec!["write_proposed", "write_finished"]
    );

    // A close upstream already made is recorded as sent, and nothing is sent.
    hub.move_task(task.id, TaskStatus::Done);
    hub.pass().await;
    let close = hub
        .writes()
        .into_iter()
        .find(|w| w.state == WriteState::Pending)
        .unwrap();
    hub.upstream_says(
        "GET",
        ISSUE_1,
        200,
        &issue_1(|i| i["state"] = "closed".into()),
    );
    hub.answer(&close.proposal.ask, 0);
    hub.pass().await;
    let done = hub.work.write(&close.proposal.ask).unwrap();
    assert_eq!(done.state, WriteState::Sent);
    assert!(hub.writes_sent().is_empty());
}

/// Review item 3: a result the store could not record when it came back is kept, and recorded
/// first at the next pass, so a write already sent is not swept as cut off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_result_the_store_could_not_record_is_recorded_first() {
    let hub = Hub::new();
    hub.connect(
        github(),
        SEED_RUNS,
        vec![link("example-org/demo-repo#milestone:1")],
    )
    .await;
    let task = hub.mirrored("example-org/demo-repo#1");
    hub.move_task(task.id, TaskStatus::Done);
    hub.pass().await;
    let close = hub.writes().remove(0);
    hub.answer(&close.proposal.ask, 0);
    hub.integrations
        .fail_finishes
        .store(1, std::sync::atomic::Ordering::SeqCst);
    hub.pass().await;
    assert_eq!(hub.writes_sent().len(), 1);
    assert_eq!(
        hub.work.write(&close.proposal.ask).unwrap().state,
        WriteState::Sending,
        "sent, but not recorded yet"
    );
    hub.pass().await;
    let done = hub.work.write(&close.proposal.ask).unwrap();
    assert_eq!(
        (done.state, done.attempts),
        (WriteState::Sent, 1),
        "{done:?}"
    );
    assert_eq!(hub.writes_sent().len(), 1);
}

/// Review items 3 and 4: an issue created upstream whose answer was never recorded (the hub
/// stopped) is failed as cut off; a person's retry is in the log, and it finds the earlier
/// attempt upstream instead of creating a second issue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retried_create_finds_its_earlier_attempt_upstream() {
    let mut hub = Hub::new();
    hub.connect(
        github(),
        IDEA,
        vec![link("example-org/demo-repo#milestone:2")],
    )
    .await;
    let created = hub
        .work
        .create_task(
            &sam(),
            NewTask {
                project: hub.work.workstream(&IDEA.parse().unwrap()).unwrap().project,
                workstream: Some(IDEA.parse().unwrap()),
                title: "Synthetic new issue".into(),
                description: Some("Written in PitCrew.".into()),
                status: None,
                priority: None,
                assignee: None,
                labels: Some(vec!["docs".into()]),
                due: None,
            },
        )
        .unwrap();
    let create = hub
        .integrations
        .request_write(
            &sam(),
            NewWrite {
                task: created.id,
                operation: WriteOperation::CreateIssue,
                text: None,
            },
        )
        .await
        .unwrap();
    hub.answer(&create.proposal.ask, 0);
    // Upstream creates #8, then the hub stops before recording it.
    hub.integrations
        .fail_finishes
        .store(1, std::sync::atomic::Ordering::SeqCst);
    hub.pass().await;
    assert_eq!(hub.writes_sent().len(), 1);
    hub.restart();
    hub.pass().await;
    let cut = hub.work.write(&create.proposal.ask).unwrap();
    assert_eq!(cut.state, WriteState::Failed);
    assert!(
        matches!(&cut.result, Some(WriteResult::Failed { message, .. }) if message.contains("stopped while sending")),
        "{cut:?}"
    );
    hub.pass().await;
    assert_eq!(hub.writes_sent().len(), 1, "never sent again by itself");

    // Sam retries: the request is in the log, and the earlier attempt is found upstream.
    hub.integrations
        .retry_write(&sam(), create.proposal.ask)
        .await
        .unwrap();
    hub.pass().await;
    let found = hub.work.write(&create.proposal.ask).unwrap();
    assert_eq!(
        (found.state, found.attempts),
        (WriteState::Sent, 2),
        "{found:?}"
    );
    assert_eq!(hub.writes_sent().len(), 1, "no second issue");
    let task = hub.work.task(&TaskRef::Id(created.id)).unwrap();
    assert_eq!(
        task.source.as_ref().map(|s| s.key.as_str()),
        Some("example-org/demo-repo#8")
    );
    assert_eq!(
        hub.event_kinds(&create.proposal.ask),
        vec![
            "write_proposed",
            "write_started",
            "write_finished",
            "write_retry_requested",
            "write_started",
            "write_finished"
        ]
    );
    let requested = hub
        .work
        .store()
        .since(0, 100_000)
        .unwrap()
        .into_iter()
        .find(|e| matches!(e.event.body, EventBody::WriteRetryRequested { .. }))
        .unwrap();
    assert_eq!(requested.event.author, sam().member);
    assert!(
        hub.upstream.sent().iter().any(|r| r.method == Method::Get
            && r.url
                .contains("/issues?state=all&sort=created&direction=desc")),
        "it looked upstream first"
    );
}

/// Review item 6: a planner place beyond the event log (a store restored from an older copy)
/// starts again from the log's end instead of never planning again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_planner_place_beyond_the_log_starts_again_from_its_end() {
    let hub = Hub::new();
    hub.connect(
        github(),
        SEED_RUNS,
        vec![link("example-org/demo-repo#milestone:1")],
    )
    .await;
    let latest = hub.work.store().latest_rev().unwrap();
    lock(&hub.integrations.saved).writes_rev = Some(latest + 1_000);
    hub.pass().await;
    assert_eq!(lock(&hub.integrations.saved).writes_rev, Some(latest));
    let task = hub.mirrored("example-org/demo-repo#1");
    hub.move_task(task.id, TaskStatus::Done);
    hub.pass().await;
    assert_eq!(hub.writes().len(), 1, "planning goes on");
}
