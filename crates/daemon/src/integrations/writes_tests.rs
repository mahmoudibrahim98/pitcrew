//! Outward writes, end to end in the hub (G's acceptance: "no outward write without an answered
//! approval ask"), over the demo workspace and the recorded fixtures in `apps/mock-hub/fixtures`.
//! The transport keeps every request it was sent, so each test shows what reached "upstream" and
//! when: nothing before an approval, nothing after a denial, and never twice for one approval or
//! one retry. Credentials are stored secrets (no `gh`), so these run on every platform.

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
    _dir: tempfile::TempDir,
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
        let upstream = http::FixtureTransport::load(&fixtures()).unwrap();
        // No `gh` on this PATH: these integrations keep a stored secret.
        let gh = GhCli::with_path(dir.path().as_os_str().to_owned());
        let integrations = Arc::new(
            Integrations::open(&root, &work, Ok(Upstream::Fixtures(upstream.clone())), gh).unwrap(),
        );
        Self {
            _dir: dir,
            work,
            integrations,
            upstream,
        }
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
    let member = lock(&hub.integrations.saved).sync_member.unwrap();
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
    for w in &writes {
        hub.answer(&w.proposal.ask, 0);
    }
    hub.pass().await;
    let sent = hub.writes_sent();
    let api = "https://jira.example.com/rest/api/3/issue/DEMO-6";
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
                serde_json::json!({"fields": {"labels": ["billing", "email"]}})
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
