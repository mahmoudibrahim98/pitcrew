//! Outward writes to GitHub and Jira (api-v1.md, "Outward writes: every one approved first").
//!
//! Three parts, all run by the integrations' loop, one at a time with the syncs:
//!
//! - **The planner** ([`Integrations::plan_writes`]) reads the event log from where it stopped
//!   (`integrations.json`'s `writes_rev`; the log's end the first time). A `task_moved` across the
//!   open/closed line, or a `task_updated` of a field upstream owns (the crates' ownership tables,
//!   [`pitcrew_sync_github::outward`]), on a task that mirrors an issue an integration syncs, and
//!   authored by anyone but a sync (any integration's own member), becomes a proposal: an approval
//!   ask from that integration's member and `write_proposed` (`SyncCommands::propose_write`, once
//!   per cause). `before` is upstream's value as the last
//!   sync read it; nothing is proposed when upstream already has the value.
//! - **Requests** ([`Integrations::request_write`]): a person asks to create an issue from a task,
//!   or to comment on its issue. Also only a proposal.
//! - **The executor** ([`Integrations::settle_writes`]) acts on answered approvals. A denied write
//!   is recorded as not sent. An approved one (or a failed one a person retries) is checked
//!   against the task as it is now, started (`write_started`, which hub-work allows only for the
//!   approval ask of the integration's own member, answered "Send" by a person), sent once with the
//!   integration's
//!   credential, and finished (`write_finished`). A write still `sending` when a pass begins was
//!   cut off (the hub stopped): it is finished as failed and never sent again by itself.
//!
//! Nothing here logs a credential, a request or an answer body.

use super::saved::{Files, Record};
use super::{Integrations, Refusal, github_host, lock};
use pitcrew_hub_work::links::{LinkScope, scope_of};
use pitcrew_hub_work::{SyncOutcome, TaskRef, WorkService, WriteFilter};
use pitcrew_protocol::api::{Caller, ErrorCode};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{AskId, EventId, IntegrationId, MemberId};
use pitcrew_protocol::integrations::{IntegrationSettings, JiraDeployment};
use pitcrew_protocol::model::{ExternalRef, ExternalSystem, Task, TaskStatus, Workstream};
use pitcrew_protocol::writes::{
    CloseReason, IssueState, MAX_BODY_CHARS, MAX_COMMENT_CHARS, NewWrite, UpstreamWrite,
    WriteFields, WriteOperation, WriteProposal, WriteResult, WriteState,
};
use pitcrew_sync_github::ownership::{ISSUE_FIELD_OWNERSHIP, Outward, outward};
use std::collections::HashMap;
use std::sync::Arc;

/// How many events the planner reads at once.
const PAGE: usize = 500;
/// How much of a long text an approval ask's body shows (the write itself holds all of it).
const SHOWN_CHARS: usize = 200;
/// The message for a write cut off while it was being sent.
const CUT_OFF: &str = "The hub stopped while sending; check upstream before you retry.";

fn tracker(system: ExternalSystem) -> &'static str {
    if system == ExternalSystem::Jira {
        "Jira"
    } else {
        "GitHub"
    }
}

fn system_of(settings: &IntegrationSettings) -> ExternalSystem {
    match settings {
        IntegrationSettings::Github { .. } => ExternalSystem::Github,
        IntegrationSettings::Jira { .. } => ExternalSystem::Jira,
    }
}

/// An issue's repository (`owner/repo#12`) or Jira project (`DEMO-12`), and its number or key.
fn container_of(system: ExternalSystem, key: &str) -> Option<&str> {
    match system {
        ExternalSystem::Github => key.split_once('#').map(|(repo, _)| repo),
        ExternalSystem::Jira => key.rsplit_once('-').map(|(project, _)| project),
        _ => None,
    }
}

/// The integration that syncs `container` of `system`, and that container as it spells it.
fn record_for<'a>(
    records: &'a [Record],
    system: ExternalSystem,
    container: &str,
) -> Option<(&'a Record, String)> {
    records.iter().find_map(|r| match (&r.settings, system) {
        (IntegrationSettings::Github { repos, .. }, ExternalSystem::Github) => repos
            .iter()
            .find(|repo| repo.eq_ignore_ascii_case(container))
            .map(|repo| (r, repo.clone())),
        (IntegrationSettings::Jira { projects, .. }, ExternalSystem::Jira) => projects
            .iter()
            .find(|p| *p == container)
            .map(|p| (r, p.clone())),
        _ => None,
    })
}

/// Whether a status is on the closed side of the line (`done`, `canceled`).
fn closed(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Done | TaskStatus::Canceled)
}

/// An issue as the last sync read it, from either tracker's snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    title: String,
    body: String,
    labels: Vec<String>,
    /// Its milestone (as a link key) or epic.
    parent: Option<String>,
    open: bool,
}

/// The sync states, read once per pass.
#[derive(Default)]
struct States {
    github: HashMap<IntegrationId, pitcrew_sync_github::SyncState>,
    jira: HashMap<IntegrationId, pitcrew_sync_jira::SyncState>,
}

impl States {
    fn seen(&mut self, files: &Files, record: &Record, container: &str, key: &str) -> Option<Seen> {
        match &record.settings {
            IntegrationSettings::Github { .. } => {
                let state = self
                    .github
                    .entry(record.id)
                    .or_insert_with(|| files.load_state(&record.id).unwrap_or_default());
                let number: u64 = key.rsplit_once('#')?.1.parse().ok()?;
                let issue = state.repos.get(container)?.issue_snapshots.get(&number)?;
                Some(Seen {
                    title: issue.title().to_owned(),
                    body: issue.body().to_owned(),
                    labels: issue.labels().to_vec(),
                    parent: issue
                        .milestone_number()
                        .map(|n| format!("{container}#milestone:{n}")),
                    open: issue.open(),
                })
            }
            IntegrationSettings::Jira { .. } => {
                let state = self
                    .jira
                    .entry(record.id)
                    .or_insert_with(|| files.load_state(&record.id).unwrap_or_default());
                let issue = state.projects.get(container)?.issue_snapshots.get(key)?;
                Some(Seen {
                    title: issue.title().to_owned(),
                    body: issue.body().to_owned(),
                    labels: issue.labels().to_vec(),
                    parent: issue.epic_key().map(str::to_owned),
                    open: issue.category() != pitcrew_sync_jira::StatusCategory::Done,
                })
            }
        }
    }
}

/// Whether the integration can write an epic (Jira Data Center needs its epic link field).
fn writes_parent(settings: &IntegrationSettings) -> bool {
    !matches!(
        settings,
        IntegrationSettings::Jira {
            deployment: JiraDeployment::DataCenter,
            epic_link_field: None,
            ..
        }
    )
}

/// The milestone (as a link key) or epic `workstream` links in `container`, if it links one.
fn parent_in(workstream: &Workstream, system: ExternalSystem, container: &str) -> Option<String> {
    workstream
        .external
        .iter()
        .filter(|l| l.system == system)
        .filter_map(scope_of)
        .find_map(|scope| match scope {
            LinkScope::GithubMilestone { repo, number } if repo.eq_ignore_ascii_case(container) => {
                Some(format!("{container}#milestone:{number}"))
            }
            LinkScope::JiraEpic { project, key } if project == container => Some(key),
            _ => None,
        })
}

fn set_parent(fields: &mut WriteFields, system: ExternalSystem, value: Option<String>) {
    if system == ExternalSystem::Jira {
        fields.epic = value;
    } else {
        fields.milestone = value;
    }
}

fn parent_of(fields: &WriteFields) -> Option<&String> {
    fields.milestone.as_ref().or(fields.epic.as_ref())
}

fn sorted(labels: &[String]) -> Vec<String> {
    let mut out = labels.to_vec();
    out.sort();
    out
}

fn capped(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// A proposal ready for `propose_write`: its ask's addressee, the write, and the ask's text.
struct Draft {
    to: MemberId,
    write: WriteProposal,
}

/// What a change to `task` implies upstream, if anything. `event` is the change; `seen` the
/// issue as the last sync read it.
#[allow(clippy::too_many_arguments)]
fn plan_change(
    body: &EventBody,
    cause: EventId,
    author: MemberId,
    task: &Task,
    workstreams: &[Workstream],
    record: &Record,
    scope: &str,
    seen: Option<&Seen>,
) -> Option<Draft> {
    let target = task.source.clone()?;
    let system = system_of(&record.settings);
    let mut before = WriteFields::default();
    let mut after = WriteFields::default();
    let operation = match body {
        EventBody::TaskMoved { from, to, .. } => {
            if closed(*from) == closed(*to)
                || outward(ISSUE_FIELD_OWNERSHIP, "status") != Outward::AskToCloseOrReopen
            {
                return None;
            }
            let closing = closed(*to);
            if seen.is_some_and(|s| s.open != closing) {
                // Upstream is already there.
                return None;
            }
            let (from_state, to_state) = if closing {
                (IssueState::Open, IssueState::Closed)
            } else {
                (IssueState::Closed, IssueState::Open)
            };
            before.state = seen.map(|_| from_state);
            after.state = Some(to_state);
            if closing && system == ExternalSystem::Github {
                after.close_reason = Some(if *to == TaskStatus::Canceled {
                    CloseReason::NotPlanned
                } else {
                    CloseReason::Completed
                });
            }
            if closing {
                WriteOperation::Close
            } else {
                WriteOperation::Reopen
            }
        }
        EventBody::TaskUpdated { patch, .. } => {
            let sends = |field| outward(ISSUE_FIELD_OWNERSHIP, field) == Outward::AskToSend;
            if let Some(title) = &patch.title
                && sends("title")
                && seen.is_none_or(|s| s.title != *title)
            {
                after.title = Some(title.clone());
                before.title = seen.map(|s| s.title.clone());
            }
            if let Some(description) = &patch.description
                && sends("body")
                && seen.is_none_or(|s| s.body != *description)
            {
                after.body = Some(capped(description, MAX_BODY_CHARS));
                before.body = seen.map(|s| s.body.clone());
            }
            if let Some(labels) = &patch.labels
                && sends("labels")
                && seen.is_none_or(|s| sorted(&s.labels) != sorted(labels))
            {
                after.labels = Some(labels.clone());
                before.labels = seen.map(|s| s.labels.clone());
            }
            if let Some(Some(moved_to)) = &patch.workstream
                && sends("milestone")
                && writes_parent(&record.settings)
                && let Some(workstream) = workstreams.iter().find(|w| w.id == *moved_to)
                && let Some(parent) = parent_in(workstream, system, scope)
                && seen.is_none_or(|s| s.parent.as_ref() != Some(&parent))
            {
                set_parent(&mut after, system, Some(parent));
                set_parent(&mut before, system, seen.and_then(|s| s.parent.clone()));
            }
            if after.is_empty() {
                return None;
            }
            WriteOperation::Update
        }
        _ => return None,
    };
    Some(Draft {
        to: record.added_by,
        write: WriteProposal {
            ask: AskId::new(),
            integration: record.id,
            system,
            scope: scope.to_owned(),
            target: Some(target),
            task: Some(task.id),
            operation,
            before,
            after,
            requested_by: author,
            cause: Some(cause),
        },
    })
}

fn shown(text: &str) -> String {
    let one_line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut out: String = one_line.chars().take(SHOWN_CHARS).collect();
    if one_line.chars().count() > SHOWN_CHARS {
        out.push('…');
    }
    format!("\"{out}\"")
}

/// One line per field `after` sets: `field: before → after` for a change (`diff`), else
/// `field: value` (a new issue, a comment).
fn field_lines(before: &WriteFields, after: &WriteFields, diff: bool) -> Vec<String> {
    let unread = "(not read yet)".to_owned();
    let labels = |l: &Vec<String>| {
        if l.is_empty() {
            "(none)".to_owned()
        } else {
            l.join(", ")
        }
    };
    let state = |s: &IssueState| match s {
        IssueState::Open => "open".to_owned(),
        IssueState::Closed => "closed".to_owned(),
    };
    let mut out = Vec::new();
    let mut line = |name: &str, b: Option<String>, a: String, change: bool| {
        if change {
            out.push(format!(
                "{name}: {} → {a}",
                b.unwrap_or_else(|| unread.clone())
            ));
        } else {
            out.push(format!("{name}: {a}"));
        }
    };
    let change = diff;
    if let Some(a) = &after.title {
        line(
            "title",
            before.title.as_deref().map(shown),
            shown(a),
            change,
        );
    }
    if let Some(a) = &after.body {
        line("body", before.body.as_deref().map(shown), shown(a), change);
    }
    if let Some(a) = &after.labels {
        line(
            "labels",
            before.labels.as_ref().map(labels),
            labels(a),
            change,
        );
    }
    if let Some(a) = &after.milestone {
        line("milestone", before.milestone.clone(), a.clone(), change);
    }
    if let Some(a) = &after.epic {
        line("epic", before.epic.clone(), a.clone(), change);
    }
    if let Some(a) = &after.state {
        let reason = match after.close_reason {
            Some(CloseReason::Completed) => " (completed)",
            Some(CloseReason::NotPlanned) => " (not planned)",
            None => "",
        };
        line(
            "state",
            before.state.as_ref().map(state),
            format!("{}{reason}", state(a)),
            change,
        );
    }
    if let Some(a) = &after.comment {
        out.push(format!("comment: {}", shown(a)));
    }
    out
}

/// The approval ask's title and body for `write`.
fn ask_text(write: &WriteProposal, task_key: &str, why: &str) -> (String, String) {
    let tracker = tracker(write.system);
    let target = write
        .target
        .as_ref()
        .map_or_else(|| write.scope.clone(), |t| t.key.clone());
    let title = match write.operation {
        WriteOperation::CreateIssue => {
            format!(
                "{tracker}: create an issue in {} from {task_key}",
                write.scope
            )
        }
        WriteOperation::Comment => format!("{tracker}: comment on {target}"),
        WriteOperation::Update => format!(
            "{tracker}: change {} of {target}",
            write
                .after
                .names()
                .iter()
                .map(
                    |n| if *n == "body" && write.system == ExternalSystem::Jira {
                        "description"
                    } else {
                        n
                    }
                )
                .collect::<Vec<_>>()
                .join(", ")
        ),
        WriteOperation::Close => format!("{tracker}: close {target}"),
        WriteOperation::Reopen => format!("{tracker}: reopen {target}"),
    };
    let mut body = format!("PitCrew sends this to {tracker} only if you choose Send.\n\n");
    let diff = matches!(
        write.operation,
        WriteOperation::Update | WriteOperation::Close | WriteOperation::Reopen
    );
    for line in field_lines(&write.before, &write.after, diff) {
        body.push_str(&line);
        body.push('\n');
    }
    body.push('\n');
    body.push_str(why);
    if let Some(url) = write.target.as_ref().and_then(|t| t.url.as_deref()) {
        body.push_str("\n\n");
        body.push_str(url);
    }
    (title, body)
}

/// Why a change implied it, for the ask's body.
fn because(body: &EventBody, handle: &str, task: &Task) -> String {
    match body {
        EventBody::TaskMoved { to, .. } => format!(
            "Because {handle} moved {} to {}.",
            task.key,
            serde_json::to_value(to)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        ),
        _ => format!("Because {handle} changed {} in PitCrew.", task.key),
    }
}

impl Integrations {
    /// Reads the log from where the planner stopped and proposes what each change implies (see
    /// the [module docs](self)). Problems are logged; the next pass reads from where this one
    /// stopped.
    pub(super) async fn plan_writes(&self) {
        let Ok(work) = self.work() else { return };
        let (records, from) = {
            let saved = lock(&self.saved);
            (saved.integrations.clone(), saved.writes_rev)
        };
        let mut members = HashMap::new();
        for record in &records {
            match self.member_of(record).await {
                Ok(member) => {
                    members.insert(record.id, member);
                }
                Err(problem) => {
                    tracing::warn!(integration = %record.id, problem = %problem.message, "no sync member to propose writes as");
                }
            }
        }
        let files = self.files.clone();
        let planned = tokio::task::spawn_blocking(move || {
            plan_from(&work, &files, &records, &members, from)
        })
        .await;
        let to = match planned {
            Ok(Ok(to)) => to,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "cannot plan outward writes; trying again later");
                return;
            }
            Err(_) => return,
        };
        let mut saved = lock(&self.saved);
        if saved.writes_rev != Some(to) {
            saved.writes_rev = Some(to);
            if let Err(e) = self.files.save(&saved) {
                tracing::warn!(error = %e, "cannot save the integrations");
            }
        }
    }

    /// Acts on answered approvals and asked-for retries (see the [module docs](self)).
    pub(super) async fn settle_writes(&self) {
        let Ok(work) = self.work() else { return };
        let filter = WriteFilter {
            task: None,
            states: vec![
                WriteState::Approved,
                WriteState::Denied,
                WriteState::Sending,
            ],
        };
        let waiting = {
            let work = Arc::clone(&work);
            match tokio::task::spawn_blocking(move || work.writes(&filter)).await {
                Ok(Ok(found)) => found,
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "cannot read the outward writes");
                    return;
                }
                Err(_) => return,
            }
        };
        for write in waiting {
            let Some(member) = self.proposer(&work, &write).await else {
                continue;
            };
            match write.state {
                WriteState::Sending => {
                    self.finish(
                        &work,
                        member,
                        write.proposal.ask,
                        WriteResult::Failed {
                            message: CUT_OFF.into(),
                            status: None,
                        },
                    )
                    .await;
                }
                WriteState::Denied => {
                    let name = match write.answered_by {
                        Some(by) => {
                            let work = Arc::clone(&work);
                            tokio::task::spawn_blocking(move || work.member(&by))
                                .await
                                .ok()
                                .and_then(Result::ok)
                                .map_or_else(|| "the person".to_owned(), |m| m.name)
                        }
                        None => "the person".to_owned(),
                    };
                    self.finish(
                        &work,
                        member,
                        write.proposal.ask,
                        WriteResult::NotSent {
                            reason: format!("Not sent: {name} chose not to."),
                        },
                    )
                    .await;
                }
                WriteState::Approved => self.send_write(&work, write).await,
                _ => {}
            }
        }
        let retries: Vec<AskId> = lock(&self.retries).drain().collect();
        for ask in retries {
            let found = {
                let work = Arc::clone(&work);
                tokio::task::spawn_blocking(move || work.write(&ask)).await
            };
            if let Ok(Ok(write)) = found
                && write.state == WriteState::Failed
            {
                self.send_write(&work, write).await;
            }
        }
    }

    /// The member that proposed `write`: its integration's sync member while it is connected,
    /// else the one its approval ask came from.
    async fn proposer(&self, work: &Arc<WorkService>, write: &UpstreamWrite) -> Option<MemberId> {
        if let Ok(record) = self.record(&write.proposal.integration) {
            return self.member_of(&record).await.ok();
        }
        let work = Arc::clone(work);
        let ask = write.proposal.ask;
        tokio::task::spawn_blocking(move || work.ask(&ask))
            .await
            .ok()
            .and_then(Result::ok)
            .map(|a| a.from)
    }

    async fn finish(
        &self,
        work: &Arc<WorkService>,
        member: MemberId,
        ask: AskId,
        result: WriteResult,
    ) {
        let work = Arc::clone(work);
        let done = tokio::task::spawn_blocking(move || {
            work.sync_commands(member)?.finish_write(&ask, result)
        })
        .await;
        match done {
            Ok(Ok(SyncOutcome::Changed(w))) => {
                tracing::info!(write = %ask, state = ?w.state, attempts = w.attempts, "outward write finished");
            }
            Ok(Ok(SyncOutcome::Refused(reason))) => {
                tracing::warn!(write = %ask, %reason, "an outward write could not be finished");
            }
            Ok(Ok(SyncOutcome::Unchanged)) => {}
            Ok(Err(e)) => {
                tracing::warn!(write = %ask, error = %e, "cannot finish an outward write")
            }
            Err(_) => {}
        }
    }

    /// Sends one approved (or retried) write: checks it, starts it, sends it once, finishes it.
    /// It starts only as its integration's own sync member, so hub-work refuses an approval ask
    /// any other member raised.
    async fn send_write(&self, work: &Arc<WorkService>, write: UpstreamWrite) {
        let Some(member) = self.proposer(work, &write).await else {
            return;
        };
        let ask = write.proposal.ask;
        let record = self.record(&write.proposal.integration).ok();
        let stale = {
            let work = Arc::clone(work);
            let proposal = write.proposal.clone();
            let has_record = record.is_some();
            tokio::task::spawn_blocking(move || stale_reason(&work, &proposal, has_record))
                .await
                .unwrap_or_else(|_| Some("It could not be checked; nothing was sent.".into()))
        };
        let Some(record) = record.filter(|_| stale.is_none()) else {
            let reason = stale
                .unwrap_or_else(|| "Its integration was removed; nothing was sent.".to_owned());
            self.finish(work, member, ask, WriteResult::NotSent { reason })
                .await;
            return;
        };
        let started = {
            let work = Arc::clone(work);
            tokio::task::spawn_blocking(move || work.sync_commands(member)?.start_write(&ask)).await
        };
        match started {
            Ok(Ok(SyncOutcome::Changed(_))) => {}
            Ok(Ok(SyncOutcome::Refused(reason))) => {
                tracing::warn!(write = %ask, %reason, "an outward write was not started");
                return;
            }
            Ok(Ok(SyncOutcome::Unchanged)) => return,
            Ok(Err(e)) => {
                tracing::warn!(write = %ask, error = %e, "cannot start an outward write");
                return;
            }
            Err(_) => return,
        }
        let result = self.deliver(&record, &write.proposal).await;
        self.finish(work, member, ask, result).await;
    }

    /// Sends `write` once through the integration and says what came of it.
    async fn deliver(&self, record: &Record, write: &WriteProposal) -> WriteResult {
        let failed = |message: String| WriteResult::Failed {
            message,
            status: None,
        };
        let secret = match self.credential(record).await {
            Ok(secret) => secret,
            Err(problem) => return failed(problem.message),
        };
        let upstream = match self.upstream() {
            Ok(upstream) => upstream.clone(),
            Err(problem) => return failed(problem.message),
        };
        match &record.settings {
            IntegrationSettings::Github { api_base, .. } => {
                deliver_github(&upstream, api_base.clone(), &secret, write).await
            }
            IntegrationSettings::Jira {
                deployment,
                site,
                email,
                epic_link_field,
                ..
            } => {
                let auth = match deployment {
                    JiraDeployment::Cloud => match email {
                        Some(email) => pitcrew_sync_jira::JiraAuth::Basic {
                            email: email.clone(),
                            api_token: secret.expose().to_owned(),
                        },
                        None => return failed("The integration's settings are incomplete.".into()),
                    },
                    JiraDeployment::DataCenter => pitcrew_sync_jira::JiraAuth::Bearer {
                        token: secret.expose().to_owned(),
                    },
                };
                let config = pitcrew_sync_jira::write::WriteConfig {
                    site: site.clone(),
                    flavor: match deployment {
                        JiraDeployment::Cloud => pitcrew_sync_jira::write::Flavor::Cloud,
                        JiraDeployment::DataCenter => pitcrew_sync_jira::write::Flavor::DataCenter,
                    },
                    auth,
                    epic_link_field: epic_link_field.clone(),
                };
                deliver_jira(&upstream, &config, write).await
            }
        }
    }

    /// `POST /v1/writes`: a person asks to create an issue from a task, or to comment on its
    /// issue. Proposes it; nothing is sent until the person approves it.
    ///
    /// # Errors
    ///
    /// `invalid` for an unknown task, another operation, a malformed comment, or a task whose
    /// workstream links no scope an integration syncs; `conflict` for a create on a task that
    /// already mirrors an issue, or a comment on one that mirrors none an integration syncs.
    pub async fn request_write(
        &self,
        caller: &Caller,
        new: NewWrite,
    ) -> Result<UpstreamWrite, Refusal> {
        let work = self.work()?;
        let records = lock(&self.saved).integrations.clone();
        let mut members = HashMap::new();
        for record in &records {
            if let Ok(member) = self.member_of(record).await {
                members.insert(record.id, member);
            }
        }
        let caller = *caller;
        tokio::task::spawn_blocking(move || -> Result<UpstreamWrite, Refusal> {
            let invalid = |m: String| Refusal::new(ErrorCode::Invalid, m);
            let conflict = |m: &str| Refusal::new(ErrorCode::Conflict, m);
            let task = work
                .task(&TaskRef::Id(new.task))
                .map_err(|_| invalid(format!("task: no task {}.", new.task)))?;
            let (record, write) = match new.operation {
                WriteOperation::CreateIssue => {
                    if new.text.is_some() {
                        return Err(invalid("text is for a comment only.".into()));
                    }
                    if task.source.is_some() {
                        return Err(conflict(
                            "This task already mirrors an issue; it cannot create another.",
                        ));
                    }
                    let workstream = match task.workstream {
                        Some(id) => Some(work.workstream(&id)?),
                        None => None,
                    };
                    let (record, scope, parent) = workstream
                        .iter()
                        .flat_map(|w| w.external.iter())
                        .find_map(|link| {
                            let linked = scope_of(link)?;
                            let (record, container) =
                                record_for(&records, link.system, linked.container())?;
                            let parent = match linked {
                                LinkScope::GithubMilestone { number, .. } => {
                                    Some(format!("{container}#milestone:{number}"))
                                }
                                LinkScope::JiraEpic { key, .. } => Some(key),
                                _ => None,
                            }
                            .filter(|_| writes_parent(&record.settings));
                            Some((record.clone(), container, parent))
                        })
                        .ok_or_else(|| {
                            invalid(
                                "The task's workstream links no repository, milestone, Jira \
                                 project or epic an integration syncs."
                                    .into(),
                            )
                        })?;
                    let system = system_of(&record.settings);
                    let mut after = WriteFields {
                        title: Some(task.title.clone()),
                        body: Some(capped(&task.description, MAX_BODY_CHARS)),
                        labels: Some(task.labels.clone()),
                        ..WriteFields::default()
                    };
                    set_parent(&mut after, system, parent);
                    let write = WriteProposal {
                        ask: AskId::new(),
                        integration: record.id,
                        system,
                        scope,
                        target: None,
                        task: Some(task.id),
                        operation: WriteOperation::CreateIssue,
                        before: WriteFields::default(),
                        after,
                        requested_by: caller.member,
                        cause: None,
                    };
                    (record, write)
                }
                WriteOperation::Comment => {
                    let text = new.text.unwrap_or_default();
                    let length = text.chars().count();
                    if text.trim().is_empty()
                        || length > MAX_COMMENT_CHARS
                        || text
                            .chars()
                            .any(|c| c.is_control() && c != '\n' && c != '\t')
                    {
                        return Err(invalid(format!(
                            "text must be 1 to {MAX_COMMENT_CHARS} characters, with no control \
                             characters but line breaks and tabs."
                        )));
                    }
                    let target = task.source.clone().ok_or_else(|| {
                        conflict("This task mirrors no issue; create one from it first.")
                    })?;
                    let (record, scope) = container_of(target.system, &target.key)
                        .and_then(|c| record_for(&records, target.system, c))
                        .map(|(r, c)| (r.clone(), c))
                        .ok_or_else(|| {
                            conflict("No integration syncs the issue this task mirrors.")
                        })?;
                    let write = WriteProposal {
                        ask: AskId::new(),
                        integration: record.id,
                        system: target.system,
                        scope,
                        target: Some(target),
                        task: Some(task.id),
                        operation: WriteOperation::Comment,
                        before: WriteFields::default(),
                        after: WriteFields {
                            comment: Some(text),
                            ..WriteFields::default()
                        },
                        requested_by: caller.member,
                        cause: None,
                    };
                    (record, write)
                }
                _ => {
                    return Err(invalid(
                        "operation must be create_issue or comment; the hub proposes the others \
                         itself."
                            .into(),
                    ));
                }
            };
            let member = *members
                .get(&record.id)
                .ok_or_else(|| conflict("The integration's sync member cannot be found."))?;
            let handle = work
                .member(&caller.member)
                .map_or_else(|_| "a person".to_owned(), |m| m.handle);
            let (title, body) = ask_text(
                &write,
                &task.key.to_string(),
                &format!("Asked for by {handle}."),
            );
            work.sync_commands(member)?
                .propose_write(record.added_by, write, &title, &body)?
                .ok_or_else(|| Refusal::internal("Proposing the write"))
        })
        .await
        .map_err(|_| Refusal::internal("Proposing the write"))?
    }

    /// `POST /v1/writes/{id}/retry`: checks the caller may retry it, then sends it again on the
    /// loop's next pass.
    ///
    /// # Errors
    ///
    /// `not_found`, `forbidden` or `conflict`, as [`WorkService::check_retry`].
    pub async fn retry_write(&self, caller: &Caller, ask: AskId) -> Result<UpstreamWrite, Refusal> {
        let work = self.work()?;
        let caller = *caller;
        let write = tokio::task::spawn_blocking(move || work.check_retry(&caller, &ask))
            .await
            .map_err(|_| Refusal::internal("Retrying the write"))??;
        lock(&self.retries).insert(ask);
        self.wake.notify_one();
        Ok(write)
    }

    /// `GET /v1/writes`.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub async fn list_writes(&self, filter: WriteFilter) -> Result<Vec<UpstreamWrite>, Refusal> {
        let work = self.work()?;
        tokio::task::spawn_blocking(move || work.writes(&filter))
            .await
            .map_err(|_| Refusal::internal("Reading the writes"))?
            .map_err(Refusal::from)
    }

    /// `GET /v1/writes/{id}`.
    ///
    /// # Errors
    ///
    /// `not_found`; database errors.
    pub async fn get_write(&self, ask: AskId) -> Result<UpstreamWrite, Refusal> {
        let work = self.work()?;
        tokio::task::spawn_blocking(move || work.write(&ask))
            .await
            .map_err(|_| Refusal::internal("Reading the write"))?
            .map_err(Refusal::from)
    }
}

/// Plans from the log after `from` (or starts at its end), returning where it got to. `members`
/// holds each integration's own sync member: their changes came from upstream, so none of them
/// is ever proposed back.
fn plan_from(
    work: &WorkService,
    files: &Files,
    records: &[Record],
    members: &HashMap<IntegrationId, MemberId>,
    from: Option<u64>,
) -> pitcrew_hub_work::Result<u64> {
    let latest = work.store().latest_rev()?;
    let Some(from) = from else {
        return Ok(latest);
    };
    if records.is_empty() {
        return Ok(latest);
    }
    let mut states = States::default();
    let mut at = from;
    while at < latest {
        let page = work.store().since(at, PAGE)?;
        let Some(last) = page.last().map(|s| s.rev) else {
            break;
        };
        for stored in page {
            let event = &stored.event;
            if members.values().any(|m| *m == event.author) {
                continue;
            }
            let task_id = match &event.body {
                EventBody::TaskMoved { task, .. } | EventBody::TaskUpdated { task, .. } => *task,
                _ => continue,
            };
            let Ok(task) = work.task(&TaskRef::Id(task_id)) else {
                continue;
            };
            let Some(source) = task.source.clone() else {
                continue;
            };
            let Some((record, scope)) = container_of(source.system, &source.key)
                .and_then(|c| record_for(records, source.system, c))
            else {
                continue;
            };
            let seen = states.seen(files, record, &scope, &source.key);
            let workstreams = work.workstreams(Some(&task.project))?;
            let Some(draft) = plan_change(
                &event.body,
                event.id,
                event.author,
                &task,
                &workstreams,
                record,
                &scope,
                seen.as_ref(),
            ) else {
                continue;
            };
            let handle = work
                .member(&event.author)
                .map_or_else(|_| "someone".to_owned(), |m| m.handle);
            let why = because(&event.body, &handle, &task);
            let (title, body) = ask_text(&draft.write, &task.key.to_string(), &why);
            let Some(member) = members.get(&record.id) else {
                continue;
            };
            let proposed = work
                .sync_commands(*member)
                .and_then(|c| c.propose_write(draft.to, draft.write, &title, &body));
            if let Err(e) = proposed {
                tracing::warn!(error = %e, task = %task.key, "cannot propose an outward write");
            }
        }
        at = last;
    }
    Ok(at.max(from))
}

/// Why an approved write is no longer what the task says, if it is not.
fn stale_reason(work: &WorkService, write: &WriteProposal, has_record: bool) -> Option<String> {
    if !has_record {
        return Some("Its integration was removed; nothing was sent.".into());
    }
    let task_id = write.task?;
    let Ok(task) = work.task(&TaskRef::Id(task_id)) else {
        return Some("Its task is gone; nothing was sent.".into());
    };
    let changed = || Some(format!("{} changed since; nothing was sent.", task.key));
    match write.operation {
        WriteOperation::CreateIssue => task
            .source
            .is_some()
            .then(|| format!("{} already mirrors an issue; nothing was sent.", task.key)),
        WriteOperation::Comment => (task.source != write.target)
            .then(|| format!("{} mirrors another issue now; nothing was sent.", task.key)),
        WriteOperation::Close | WriteOperation::Reopen => {
            if task.source.as_ref().map(|s| &s.key) != write.target.as_ref().map(|t| &t.key) {
                return Some(format!(
                    "{} mirrors another issue now; nothing was sent.",
                    task.key
                ));
            }
            let closing = write.operation == WriteOperation::Close;
            (closed(task.status) != closing)
                .then(|| format!("{} was moved back since; nothing was sent.", task.key))
        }
        WriteOperation::Update => {
            if task.source.as_ref().map(|s| &s.key) != write.target.as_ref().map(|t| &t.key) {
                return Some(format!(
                    "{} mirrors another issue now; nothing was sent.",
                    task.key
                ));
            }
            let after = &write.after;
            if after.title.as_ref().is_some_and(|t| *t != task.title)
                || after
                    .body
                    .as_ref()
                    .is_some_and(|b| *b != capped(&task.description, MAX_BODY_CHARS))
                || after.labels.as_ref().is_some_and(|l| *l != task.labels)
            {
                return changed();
            }
            if let Some(parent) = parent_of(after) {
                let linked = task
                    .workstream
                    .and_then(|id| work.workstream(&id).ok())
                    .and_then(|w| parent_in(&w, write.system, &write.scope));
                if linked.as_ref() != Some(parent) {
                    return changed();
                }
            }
            None
        }
    }
}

/// A failure from a crate's error: its message, and upstream's status when it answered.
fn failed(message: String, status: Option<u16>) -> WriteResult {
    WriteResult::Failed { message, status }
}

async fn deliver_github(
    upstream: &super::http::Upstream,
    api_base: Option<String>,
    secret: &super::secret::Secret,
    write: &WriteProposal,
) -> WriteResult {
    use pitcrew_sync_github::write::{IssueEdit, IssueWrite, StateChange};
    let Ok(repo) = pitcrew_sync_github::RepoRef::new(write.scope.clone()) else {
        return failed("The repository's name is malformed.".into(), None);
    };
    let number = || -> Option<u64> {
        write
            .target
            .as_ref()
            .and_then(|t| t.key.rsplit_once('#'))
            .and_then(|(_, n)| n.parse().ok())
    };
    let milestone = || -> Option<u64> {
        write
            .after
            .milestone
            .as_ref()
            .and_then(|m| m.rsplit_once("#milestone:"))
            .and_then(|(_, n)| n.parse().ok())
    };
    let after = &write.after;
    let request = match write.operation {
        WriteOperation::CreateIssue => IssueWrite::Create {
            repo: repo.clone(),
            title: after.title.clone().unwrap_or_default(),
            body: after.body.clone().unwrap_or_default(),
            labels: after.labels.clone().unwrap_or_default(),
            milestone: milestone(),
        },
        operation => {
            let Some(number) = number() else {
                return failed("The issue's number is malformed.".into(), None);
            };
            match operation {
                WriteOperation::Comment => IssueWrite::Comment {
                    repo: repo.clone(),
                    number,
                    body: after.comment.clone().unwrap_or_default(),
                },
                WriteOperation::Close | WriteOperation::Reopen => IssueWrite::Edit {
                    repo: repo.clone(),
                    number,
                    edit: IssueEdit {
                        state: Some(if operation == WriteOperation::Reopen {
                            StateChange::Reopen
                        } else {
                            StateChange::Close(match after.close_reason {
                                Some(CloseReason::NotPlanned) => {
                                    pitcrew_sync_github::CloseReason::NotPlanned
                                }
                                _ => pitcrew_sync_github::CloseReason::Completed,
                            })
                        }),
                        ..IssueEdit::default()
                    },
                },
                _ => IssueWrite::Edit {
                    repo: repo.clone(),
                    number,
                    edit: IssueEdit {
                        title: after.title.clone(),
                        body: after.body.clone(),
                        labels: after.labels.clone(),
                        milestone: milestone(),
                        state: None,
                    },
                },
            }
        }
    };
    let config = pitcrew_sync_github::write::WriteConfig {
        api_base: api_base.clone(),
        token: pitcrew_sync_github::AuthToken::new(secret.expose()),
    };
    match pitcrew_sync_github::write::send(upstream, &config, &request).await {
        Ok(written) => {
            let created = written.number.map(|n| ExternalRef {
                system: ExternalSystem::Github,
                key: format!("{}#{n}", repo.as_str()),
                url: written.url.clone().or_else(|| {
                    (github_host(api_base.as_deref()) == "github.com")
                        .then(|| format!("https://github.com/{}/issues/{n}", repo.as_str()))
                }),
            });
            let url = written
                .url
                .or_else(|| created.as_ref().and_then(|c| c.url.clone()));
            WriteResult::Sent { created, url }
        }
        Err(e) => {
            let status = e.status();
            let mut message = e.to_string();
            if matches!(e, pitcrew_sync_github::write::WriteError::Malformed(_)) {
                message.push_str("; check upstream before you retry.");
            }
            failed(message, status)
        }
    }
}

async fn deliver_jira(
    upstream: &super::http::Upstream,
    config: &pitcrew_sync_jira::write::WriteConfig,
    write: &WriteProposal,
) -> WriteResult {
    use pitcrew_sync_jira::write::{IssueEdit, IssueWrite};
    let after = &write.after;
    let key = || {
        write
            .target
            .as_ref()
            .map(|t| t.key.clone())
            .unwrap_or_default()
    };
    let request = match write.operation {
        WriteOperation::CreateIssue => {
            let Ok(project) = pitcrew_sync_jira::ProjectRef::new(write.scope.clone()) else {
                return failed("The Jira project's key is malformed.".into(), None);
            };
            IssueWrite::Create {
                project,
                summary: after.title.clone().unwrap_or_default(),
                description: after.body.clone().unwrap_or_default(),
                labels: after.labels.clone().unwrap_or_default(),
                epic: after.epic.clone(),
            }
        }
        WriteOperation::Comment => IssueWrite::Comment {
            key: key(),
            body: after.comment.clone().unwrap_or_default(),
        },
        WriteOperation::Update => IssueWrite::Edit {
            key: key(),
            edit: IssueEdit {
                summary: after.title.clone(),
                description: after.body.clone(),
                labels: after.labels.clone(),
                epic: after.epic.clone(),
            },
        },
        WriteOperation::Close => IssueWrite::Transition {
            key: key(),
            to: pitcrew_sync_jira::StatusCategory::Done,
        },
        WriteOperation::Reopen => IssueWrite::Transition {
            key: key(),
            to: pitcrew_sync_jira::StatusCategory::New,
        },
    };
    match pitcrew_sync_jira::write::send(upstream, config, &request).await {
        Ok(written) => {
            let created = written.key.map(|key| ExternalRef {
                system: ExternalSystem::Jira,
                key,
                url: written.url.clone(),
            });
            WriteResult::Sent {
                created,
                url: written.url,
            }
        }
        Err(e) => failed(e.to_string(), e.status()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_show_every_field_before_and_after() {
        let write = WriteProposal {
            ask: AskId::new(),
            integration: IntegrationId::new(),
            system: ExternalSystem::Github,
            scope: "example-org/demo-repo".into(),
            target: Some(ExternalRef {
                system: ExternalSystem::Github,
                key: "example-org/demo-repo#1".into(),
                url: Some("https://github.com/example-org/demo-repo/issues/1".into()),
            }),
            task: None,
            operation: WriteOperation::Update,
            before: WriteFields {
                title: Some("Old".into()),
                labels: Some(vec!["bug".into()]),
                ..WriteFields::default()
            },
            after: WriteFields {
                title: Some("New\u{0007}".into()),
                labels: Some(vec![]),
                milestone: Some("example-org/demo-repo#milestone:2".into()),
                ..WriteFields::default()
            },
            requested_by: MemberId::new(),
            cause: None,
        };
        let (title, body) = ask_text(&write, "PAP-3", "Because @sam changed PAP-3 in PitCrew.");
        assert_eq!(
            title,
            "GitHub: change title, labels, milestone of example-org/demo-repo#1"
        );
        assert!(body.contains("title: \"Old\" → \"New \""), "{body}");
        assert!(body.contains("labels: bug → (none)"), "{body}");
        assert!(
            body.contains("milestone: (not read yet) → example-org/demo-repo#milestone:2"),
            "{body}"
        );
        assert!(body.ends_with("https://github.com/example-org/demo-repo/issues/1"));
        assert!(body.starts_with("PitCrew sends this to GitHub only if you choose Send."));
    }

    #[test]
    fn issues_belong_to_their_repository_or_project() {
        assert_eq!(
            container_of(ExternalSystem::Github, "example-org/demo-repo#3"),
            Some("example-org/demo-repo")
        );
        assert_eq!(container_of(ExternalSystem::Jira, "DEMO-12"), Some("DEMO"));
        assert_eq!(container_of(ExternalSystem::Linear, "X-1"), None);
        assert!(closed(TaskStatus::Done) && closed(TaskStatus::Canceled));
        assert!(!closed(TaskStatus::Review));
    }
}
