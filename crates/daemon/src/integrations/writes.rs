//! Outward writes to GitHub and Jira (api-v1.md, "Outward writes: every one approved first").
//!
//! Three parts, all run by the integrations' loop, one at a time with the syncs:
//!
//! - **The planner** ([`Integrations::plan_writes`]) reads the event log from where it stopped
//!   (`integrations.json`'s `writes_rev`; the log's end the first time, and again when the saved
//!   revision is beyond the log). A `task_moved` across the open/closed line, or a `task_updated`
//!   of a field upstream owns (the crates' ownership tables, [`pitcrew_sync_github::outward`]),
//!   on a task that mirrors an issue an integration syncs, and authored by anyone but a sync (any
//!   integration's own member), becomes a proposal: an approval ask from that integration's
//!   member and `write_proposed` (`SyncCommands::propose_write`, once per cause). `before` is
//!   upstream's value as the last sync read it; nothing is proposed when upstream already has the
//!   value. Only what the hub holds exactly is sent back: a title or description the last read
//!   held lossily (hidden characters stripped, cut, Jira rich text) is left out, and labels go as
//!   the labels added and removed, never the whole list.
//! - **Requests** ([`Integrations::request_write`]): a person asks to create an issue from a task,
//!   or to comment on its issue. Also only a proposal.
//! - **The executor** ([`Integrations::settle_writes`]) acts on answered approvals. A denied write
//!   is recorded as not sent. An approved one (or a failed one with a person's retry request) is
//!   checked against the task as it is now; an edit, close or reopen is then checked against the
//!   issue as upstream has it now (one read): what upstream already holds is not sent, and a field
//!   upstream changed since `before` means nothing is sent. A retried create or comment first
//!   looks upstream for the earlier attempt. Then it is started (`write_started`, which hub-work
//!   allows only for the approval ask of the integration's own member, answered "Send" by a
//!   person, or for a person's retry request), sent once with the integration's credential, and
//!   finished (`write_finished`). A result the store cannot record is kept in memory and recorded
//!   first at the next pass; a write still `sending` after that was cut off (the hub stopped): it
//!   is finished as failed and never sent again by itself.
//!
//! Nothing here logs a credential, a request or an answer body.

use super::saved::{Files, Record};
use super::{Integrations, Refusal, github_host, lock, utc_rfc3339};
use pitcrew_hub_work::links::{LinkScope, scope_of};
use pitcrew_hub_work::{SyncOutcome, TaskRef, WorkService, WriteFilter, fit_labels, fit_title};
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
const CUT_OFF: &str =
    "The hub stopped while sending; a retry first looks upstream for this attempt.";
/// How long before a write's approval its earlier attempt is looked for upstream (clock skew).
const EARLIER_MARGIN_MS: i64 = 10 * 60 * 1000;

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

/// The integration that syncs `container` of `system`, and that container as it spells it. A
/// repository or Jira project is synced by one integration at most, on any host (`add` refuses a
/// second), so the container alone names it.
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
    /// Whether the hub holds `title` exactly as upstream has it (nothing stripped, cut or
    /// trimmed), so sending the hub's title back loses nothing.
    title_exact: bool,
    /// Whether `body` is upstream's whole body or description.
    body_exact: bool,
}

/// Whether the hub's own copy of `title` (`fit_title`) is `title` itself.
fn hub_holds_title(title: &str) -> bool {
    fit_title(title, title) == title
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
                    title_exact: issue.title_lossless() && hub_holds_title(issue.title()),
                    body_exact: issue.body_lossless(),
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
                    title_exact: issue.title_lossless() && hub_holds_title(issue.title()),
                    body_exact: issue.body_lossless(),
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

fn capped(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// A proposal ready for `propose_write`: its ask's addressee, the write, and the fields left out
/// because the hub does not hold upstream's copy exactly.
struct Draft {
    to: MemberId,
    write: WriteProposal,
    left_out: Vec<&'static str>,
}

/// The labels `labels` (the task's, after a person's change) adds to and removes from upstream's
/// `seen` labels as the hub holds them (`fit_labels`: at most 32, each cut). Labels upstream has
/// that the hub does not hold are in neither list, so they are never touched.
fn label_change(seen: &[String], labels: &[String]) -> (Vec<String>, Vec<String>) {
    let held = fit_labels(seen);
    let mut add: Vec<String> = Vec::new();
    for label in labels {
        if !held.contains(label) && !add.contains(label) {
            add.push(label.clone());
        }
    }
    let remove = held.into_iter().filter(|l| !labels.contains(l)).collect();
    (add, remove)
}

/// What a change to `task` implies upstream, if anything. `event` is the change; `seen` the
/// issue as the last sync read it. An `update` needs `seen`: it is checked against upstream's
/// values as read, sends only what the hub holds exactly, and labels as a change.
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
    let mut left_out = Vec::new();
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
            let Some(seen) = seen else {
                tracing::info!(task = %task.key, "no sync has read this task's issue yet; nothing is proposed");
                return None;
            };
            let sends = |field| outward(ISSUE_FIELD_OWNERSHIP, field) == Outward::AskToSend;
            if let Some(title) = &patch.title
                && sends("title")
                && seen.title != *title
            {
                if seen.title_exact {
                    after.title = Some(title.clone());
                    before.title = Some(seen.title.clone());
                } else {
                    left_out.push("title");
                }
            }
            if let Some(description) = &patch.description
                && sends("body")
                && seen.body != *description
            {
                if seen.body_exact {
                    after.body = Some(capped(description, MAX_BODY_CHARS));
                    before.body = Some(seen.body.clone());
                } else {
                    left_out.push("body");
                }
            }
            if let Some(labels) = &patch.labels
                && sends("labels")
            {
                let (add, remove) = label_change(&seen.labels, labels);
                if !add.is_empty() || !remove.is_empty() {
                    after.add_labels = (!add.is_empty()).then_some(add);
                    after.remove_labels = (!remove.is_empty()).then_some(remove);
                    before.labels = Some(seen.labels.clone());
                }
            }
            if let Some(Some(moved_to)) = &patch.workstream
                && sends("milestone")
                && writes_parent(&record.settings)
                && let Some(workstream) = workstreams.iter().find(|w| w.id == *moved_to)
                && let Some(parent) = parent_in(workstream, system, scope)
                && seen.parent.as_ref() != Some(&parent)
            {
                set_parent(&mut after, system, Some(parent));
                set_parent(&mut before, system, seen.parent.clone());
            }
            if after.is_empty() {
                if !left_out.is_empty() {
                    tracing::info!(task = %task.key, fields = ?left_out, "upstream's copy is not held exactly; nothing is proposed");
                }
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
        left_out,
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
    if after.add_labels.is_some() || after.remove_labels.is_some() {
        let added = after.add_labels.iter().flatten().map(|l| format!("+ {l}"));
        let removed = after
            .remove_labels
            .iter()
            .flatten()
            .map(|l| format!("− {l}"));
        line(
            "labels",
            before.labels.as_ref().map(labels),
            added.chain(removed).collect::<Vec<_>>().join(", "),
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

/// The approval ask's title and body for `write`. `left_out` names the fields a person changed
/// that are not sent, because the hub does not hold upstream's copy exactly.
fn ask_text(
    write: &WriteProposal,
    task_key: &str,
    why: &str,
    left_out: &[&str],
) -> (String, String) {
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
                .map(|n| field_name(n, write.system))
                .fold(Vec::new(), |mut names, n| {
                    if !names.contains(&n) {
                        names.push(n);
                    }
                    names
                })
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
    if !left_out.is_empty() {
        let names: Vec<&str> = left_out
            .iter()
            .map(|n| field_name(n, write.system))
            .collect();
        body.push_str(&format!(
            "\nNot sent: the {}. {tracker}'s copy holds formatting or characters PitCrew does not \
             keep, and sending PitCrew's would replace them; change it in {tracker}.\n",
            names.join(" and the ")
        ));
    }
    body.push('\n');
    body.push_str(why);
    if let Some(url) = write.target.as_ref().and_then(|t| t.url.as_deref()) {
        body.push_str("\n\n");
        body.push_str(url);
    }
    (title, body)
}

/// A field's name as the tracker calls it.
fn field_name(field: &str, system: ExternalSystem) -> &str {
    match (field, system) {
        ("body", ExternalSystem::Jira) => "description",
        ("title", ExternalSystem::Jira) => "summary",
        ("add_labels" | "remove_labels", _) => "labels",
        _ => field,
    }
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
        let planned =
            tokio::task::spawn_blocking(move || plan_from(&work, &files, &records, &members, from))
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

    /// Acts on answered approvals and asked-for retries (see the [module docs](self)). Results
    /// the store could not record before come first, so a write already sent is never swept as
    /// cut off.
    pub(super) async fn settle_writes(&self) {
        let Ok(work) = self.work() else { return };
        let unfinished: Vec<(AskId, (MemberId, WriteResult))> =
            lock(&self.unfinished).drain().collect();
        for (ask, (member, result)) in unfinished {
            self.finish(&work, member, ask, result).await;
        }
        let filter = WriteFilter {
            task: None,
            states: vec![
                WriteState::Approved,
                WriteState::Denied,
                WriteState::Sending,
                WriteState::Failed,
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
            if lock(&self.unfinished).contains_key(&write.proposal.ask) {
                // Its result is still waiting to be recorded: neither cut off nor sent again.
                continue;
            }
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
                WriteState::Approved => self.send_write(&work, member, write).await,
                WriteState::Failed if write.retry_requested_by.is_some() => {
                    self.send_write(&work, member, write).await;
                }
                _ => {}
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

    /// Records what came of `ask`. When the store cannot (a busy database, say), the result is
    /// kept in memory and recorded first at the next pass.
    async fn finish(
        &self,
        work: &Arc<WorkService>,
        member: MemberId,
        ask: AskId,
        result: WriteResult,
    ) {
        #[cfg(test)]
        {
            let mut failing = lock(&self.fail_finishes);
            if *failing > 0 {
                *failing -= 1;
                lock(&self.unfinished).insert(ask, (member, result));
                return;
            }
        }
        let done = {
            let work = Arc::clone(work);
            let result = result.clone();
            tokio::task::spawn_blocking(move || {
                work.sync_commands(member)?.finish_write(&ask, result)
            })
            .await
        };
        match done {
            Ok(Ok(SyncOutcome::Changed(w))) => {
                tracing::info!(write = %ask, state = ?w.state, attempts = w.attempts, "outward write finished");
            }
            Ok(Ok(SyncOutcome::Refused(reason))) => {
                tracing::warn!(write = %ask, %reason, "an outward write could not be finished");
            }
            Ok(Ok(SyncOutcome::Unchanged)) => {}
            Ok(Err(e)) => {
                tracing::warn!(write = %ask, error = %e, "cannot record an outward write's result yet; keeping it");
                lock(&self.unfinished).insert(ask, (member, result));
            }
            Err(_) => {
                lock(&self.unfinished).insert(ask, (member, result));
            }
        }
    }

    /// Starts `ask` (`write_started`); whether it started.
    async fn start(&self, work: &Arc<WorkService>, member: MemberId, ask: AskId) -> bool {
        let started = {
            let work = Arc::clone(work);
            tokio::task::spawn_blocking(move || work.sync_commands(member)?.start_write(&ask)).await
        };
        match started {
            Ok(Ok(SyncOutcome::Changed(_))) => true,
            Ok(Ok(SyncOutcome::Refused(reason))) => {
                tracing::warn!(write = %ask, %reason, "an outward write was not started");
                false
            }
            Ok(Ok(SyncOutcome::Unchanged)) | Err(_) => false,
            Ok(Err(e)) => {
                tracing::warn!(write = %ask, error = %e, "cannot start an outward write");
                false
            }
        }
    }

    /// Sends one approved (or retried) write: checks it against the task, then against upstream
    /// as it is now, starts it, sends what is left once, and finishes it. It starts only as its
    /// integration's own sync member (`member`), so hub-work refuses an approval ask any other
    /// member raised.
    async fn send_write(&self, work: &Arc<WorkService>, member: MemberId, write: UpstreamWrite) {
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
        let (tracker, step) = match self.tracker(&record).await {
            Ok(tracker) => {
                let step = self.check_upstream(&tracker, &write).await;
                (Some(tracker), step)
            }
            Err(message) => (None, Step::Finish(failed(message, None))),
        };
        if let Step::NotSent(reason) = step {
            // Nothing was started, and nothing is sent.
            self.finish(work, member, ask, WriteResult::NotSent { reason })
                .await;
            return;
        }
        if !self.start(work, member, ask).await {
            return;
        }
        let result = match (step, tracker) {
            (Step::Send(fields), Some(tracker)) => tracker.deliver(&write.proposal, &fields).await,
            (Step::Finish(result), _) => result,
            (Step::Send(_) | Step::NotSent(_), _) => failed("Nothing could be sent.".into(), None),
        };
        self.finish(work, member, ask, result).await;
    }

    /// What is left to send of `write`, given upstream as it is now: an edit, close or reopen is
    /// checked against the issue (one read; see [`reconcile`]); a retried create or comment first
    /// looks for its earlier attempt.
    async fn check_upstream(&self, tracker: &Tracker, write: &UpstreamWrite) -> Step<WriteFields> {
        let proposal = &write.proposal;
        match proposal.operation {
            WriteOperation::CreateIssue | WriteOperation::Comment if write.attempts > 0 => {
                let since = write
                    .answered_at
                    .unwrap_or(write.proposed_at)
                    .saturating_sub(EARLIER_MARGIN_MS)
                    / 1000;
                match tracker.find_earlier(proposal, since).await {
                    Ok(Some(found)) => Step::Finish(found),
                    Ok(None) => Step::Send(proposal.after.clone()),
                    Err(e) => Step::Finish(failed(
                        format!(
                            "Could not look upstream for the earlier attempt, so nothing was \
                             sent: {}",
                            e.0
                        ),
                        e.1,
                    )),
                }
            }
            WriteOperation::CreateIssue | WriteOperation::Comment => {
                Step::Send(proposal.after.clone())
            }
            WriteOperation::Update | WriteOperation::Close | WriteOperation::Reopen => {
                let key = proposal
                    .target
                    .as_ref()
                    .map_or_else(|| "The issue".to_owned(), |t| t.key.clone());
                match tracker.read(proposal).await {
                    Err(e) => Step::Finish(failed(
                        format!(
                            "Could not read {key} before sending, so nothing was sent: {}",
                            e.0
                        ),
                        e.1,
                    )),
                    Ok(now) => match reconcile(proposal, &now) {
                        Err(fields) => Step::NotSent(format!(
                            "Not sent: {key} changed upstream since this was proposed ({}). The \
                             next sync brings that change into PitCrew.",
                            fields.join(", ")
                        )),
                        Ok(rest) if rest.is_empty() => Step::Finish(WriteResult::Sent {
                            created: None,
                            url: now
                                .url
                                .or_else(|| proposal.target.as_ref().and_then(|t| t.url.clone())),
                        }),
                        Ok(rest) => Step::Send(rest),
                    },
                }
            }
        }
    }

    /// How to reach `record`'s tracker: its transport and credential.
    async fn tracker(&self, record: &Record) -> Result<Tracker, String> {
        let secret = self.credential(record).await.map_err(|p| p.message)?;
        let upstream = self.upstream().map_err(|p| p.message)?.clone();
        match &record.settings {
            IntegrationSettings::Github { api_base, .. } => Ok(Tracker::Github {
                upstream,
                config: pitcrew_sync_github::write::WriteConfig {
                    api_base: api_base.clone(),
                    token: pitcrew_sync_github::AuthToken::new(secret.expose()),
                },
            }),
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
                        None => return Err("The integration's settings are incomplete.".into()),
                    },
                    JiraDeployment::DataCenter => pitcrew_sync_jira::JiraAuth::Bearer {
                        token: secret.expose().to_owned(),
                    },
                };
                Ok(Tracker::Jira {
                    upstream,
                    config: pitcrew_sync_jira::write::WriteConfig {
                        site: site.clone(),
                        flavor: match deployment {
                            JiraDeployment::Cloud => pitcrew_sync_jira::write::Flavor::Cloud,
                            JiraDeployment::DataCenter => {
                                pitcrew_sync_jira::write::Flavor::DataCenter
                            }
                        },
                        auth,
                        epic_link_field: epic_link_field.clone(),
                    },
                })
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
                &[],
            );
            work.sync_commands(member)?
                .propose_write(record.added_by, write, &title, &body)?
                .ok_or_else(|| Refusal::internal("Proposing the write"))
        })
        .await
        .map_err(|_| Refusal::internal("Proposing the write"))?
    }

    /// `POST /v1/writes/{id}/retry`: records the caller's request to send it again
    /// (`write_retry_requested`), which the loop's next pass uses.
    ///
    /// # Errors
    ///
    /// `not_found`, `forbidden` or `conflict`, as [`WorkService::request_retry`].
    pub async fn retry_write(&self, caller: &Caller, ask: AskId) -> Result<UpstreamWrite, Refusal> {
        let work = self.work()?;
        let caller = *caller;
        let write = tokio::task::spawn_blocking(move || work.request_retry(&caller, &ask))
            .await
            .map_err(|_| Refusal::internal("Retrying the write"))??;
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
    let Some(mut from) = from else {
        return Ok(latest);
    };
    if from > latest {
        // A store restored from an older copy, or another one: planning from beyond its end would
        // never read anything again.
        tracing::warn!(
            saved = from,
            latest,
            "the outward-write planner's place is beyond the event log; planning from its end"
        );
        from = latest;
    }
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
            let (title, body) =
                ask_text(&draft.write, &task.key.to_string(), &why, &draft.left_out);
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
            if after.labels.is_some() {
                // Proposed before labels were sent as a change: never sent as a whole list.
                return Some(format!(
                    "{} was proposed in an older form; nothing was sent.",
                    task.key
                ));
            }
            if after.title.as_ref().is_some_and(|t| *t != task.title)
                || after
                    .body
                    .as_ref()
                    .is_some_and(|b| *b != capped(&task.description, MAX_BODY_CHARS))
                || after
                    .add_labels
                    .iter()
                    .flatten()
                    .any(|l| !task.labels.contains(l))
                || after
                    .remove_labels
                    .iter()
                    .flatten()
                    .any(|l| task.labels.contains(l))
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

/// What to do with a write after checking it against upstream.
#[derive(Debug)]
enum Step<T> {
    /// Send this.
    Send(T),
    /// Start it, and record this without sending anything (found upstream already, or upstream
    /// could not be read).
    Finish(WriteResult),
    /// Record it as not sent, without starting it.
    NotSent(String),
}

/// A crate's error, as a message and upstream's status.
type Problem = (String, Option<u16>);

/// An issue as upstream has it now, from either tracker: raw text (Jira's description as the
/// sync reads it), labels as upstream spells them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Now {
    title: String,
    body: String,
    /// Whether `body` is upstream's whole body or description.
    body_exact: bool,
    labels: Vec<String>,
    /// Its milestone (as a link key) or epic.
    parent: Option<String>,
    open: bool,
    url: Option<String>,
}

/// What is left of `write` to send, given upstream `now`, field by field:
/// - a field upstream already holds as `after` is dropped;
/// - a field still as `before` is kept;
/// - any other value (upstream changed it since it was read, or its description now has
///   formatting) is a conflict: `Err` names those fields, and nothing is sent;
/// - labels: an added label upstream has, or a removed one it no longer has, is dropped; a label to
///   remove is named as upstream spells it;
/// - a close or reopen upstream already made is dropped.
///
/// `Ok` with nothing left means upstream already has it all.
fn reconcile(write: &WriteProposal, now: &Now) -> Result<WriteFields, Vec<&'static str>> {
    let (after, before) = (&write.after, &write.before);
    let jira = write.system == ExternalSystem::Jira;
    let mut rest = WriteFields::default();
    let mut changed = Vec::new();
    if let Some(title) = &after.title
        && now.title != *title
    {
        if before.title.as_deref() == Some(now.title.as_str()) {
            rest.title = Some(title.clone());
        } else {
            changed.push(if jira { "summary" } else { "title" });
        }
    }
    if let Some(body) = &after.body
        && now.body != *body
    {
        if now.body_exact && before.body.as_deref() == Some(now.body.as_str()) {
            rest.body = Some(body.clone());
        } else {
            changed.push(if jira { "description" } else { "body" });
        }
    }
    if let Some(parent) = parent_of(after)
        && now.parent.as_ref() != Some(parent)
    {
        if now.parent.as_ref() == parent_of(before) {
            set_parent(&mut rest, write.system, Some(parent.clone()));
        } else {
            changed.push(if jira { "epic" } else { "milestone" });
        }
    }
    if let Some(state) = after.state
        && now.open != (state == IssueState::Open)
    {
        rest.state = Some(state);
        rest.close_reason = after.close_reason;
    }
    // Upstream's labels as the hub holds them (`fit_labels`), so they compare with the change.
    let held = |raw: &String| fit_labels(std::slice::from_ref(raw)).pop();
    if let Some(add) = &after.add_labels {
        let missing: Vec<String> = add
            .iter()
            .filter(|l| !now.labels.iter().any(|r| held(r).as_ref() == Some(*l)))
            .cloned()
            .collect();
        rest.add_labels = (!missing.is_empty()).then_some(missing);
    }
    if let Some(remove) = &after.remove_labels {
        let present: Vec<String> = now
            .labels
            .iter()
            .filter(|r| held(r).is_some_and(|h| remove.contains(&h)))
            .cloned()
            .collect();
        rest.remove_labels = (!present.is_empty()).then_some(present);
    }
    if changed.is_empty() {
        Ok(rest)
    } else {
        Err(changed)
    }
}

/// An integration's tracker, ready for one write: its transport and credential.
enum Tracker {
    Github {
        upstream: super::http::Upstream,
        config: pitcrew_sync_github::write::WriteConfig,
    },
    Jira {
        upstream: super::http::Upstream,
        config: pitcrew_sync_jira::write::WriteConfig,
    },
}

fn github_problem(e: &pitcrew_sync_github::write::WriteError) -> Problem {
    (e.to_string(), e.status())
}

fn jira_problem(e: &pitcrew_sync_jira::write::WriteError) -> Problem {
    (e.to_string(), e.status())
}

/// The repository and issue number of a GitHub write.
fn github_issue(write: &WriteProposal) -> Result<(pitcrew_sync_github::RepoRef, u64), Problem> {
    let repo = pitcrew_sync_github::RepoRef::new(write.scope.clone())
        .map_err(|_| ("The repository's name is malformed.".to_owned(), None))?;
    let number = write
        .target
        .as_ref()
        .and_then(|t| t.key.rsplit_once('#'))
        .and_then(|(_, n)| n.parse().ok())
        .ok_or_else(|| ("The issue's number is malformed.".to_owned(), None))?;
    Ok((repo, number))
}

fn milestone_number(fields: &WriteFields) -> Option<u64> {
    fields
        .milestone
        .as_ref()
        .and_then(|m| m.rsplit_once("#milestone:"))
        .and_then(|(_, n)| n.parse().ok())
}

impl Tracker {
    /// The issue `write` changes, as upstream has it now.
    async fn read(&self, write: &WriteProposal) -> Result<Now, Problem> {
        match self {
            Self::Github { upstream, config } => {
                let (repo, number) = github_issue(write)?;
                let issue = pitcrew_sync_github::write::read_issue(upstream, config, &repo, number)
                    .await
                    .map_err(|e| github_problem(&e))?;
                Ok(Now {
                    title: issue.title,
                    body: issue.body,
                    body_exact: true,
                    labels: issue.labels,
                    parent: issue
                        .milestone
                        .map(|n| format!("{}#milestone:{n}", write.scope)),
                    open: issue.open,
                    url: issue.url,
                })
            }
            Self::Jira { upstream, config } => {
                let key = write
                    .target
                    .as_ref()
                    .map(|t| t.key.clone())
                    .unwrap_or_default();
                let issue = pitcrew_sync_jira::write::read_issue(upstream, config, &key)
                    .await
                    .map_err(|e| jira_problem(&e))?;
                Ok(Now {
                    title: issue.summary,
                    body: issue.description,
                    body_exact: issue.description_lossless,
                    labels: issue.labels,
                    parent: issue.epic,
                    open: issue.category != pitcrew_sync_jira::StatusCategory::Done,
                    url: Some(format!(
                        "{}/browse/{key}",
                        config.site.trim_end_matches('/')
                    )),
                })
            }
        }
    }

    /// An earlier attempt at `write` (a create or a comment) made since `since_unix`, as the
    /// result it would have had.
    async fn find_earlier(
        &self,
        write: &WriteProposal,
        since_unix: i64,
    ) -> Result<Option<WriteResult>, Problem> {
        match self {
            Self::Github { upstream, config } => {
                let request = github_request(write, &write.after)?;
                let since = pitcrew_sync_github::GithubTimestamp::new(utc_rfc3339(since_unix));
                let found =
                    pitcrew_sync_github::write::find_earlier(upstream, config, &request, &since)
                        .await
                        .map_err(|e| github_problem(&e))?;
                Ok(found.map(|w| github_sent(config, write, w)))
            }
            Self::Jira { upstream, config } => {
                let request = jira_request(write, &write.after)?;
                let found =
                    pitcrew_sync_jira::write::find_earlier(upstream, config, &request, since_unix)
                        .await
                        .map_err(|e| jira_problem(&e))?;
                Ok(found.map(jira_sent))
            }
        }
    }

    /// Sends `fields` (what is left of `write`) once and says what came of it.
    async fn deliver(&self, write: &WriteProposal, fields: &WriteFields) -> WriteResult {
        match self {
            Self::Github { upstream, config } => {
                let request = match github_request(write, fields) {
                    Ok(request) => request,
                    Err((message, status)) => return failed(message, status),
                };
                match pitcrew_sync_github::write::send(upstream, config, &request).await {
                    Ok(written) => github_sent(config, write, written),
                    Err(e) => {
                        let mut message = e.to_string();
                        if matches!(e, pitcrew_sync_github::write::WriteError::Malformed(_)) {
                            message.push_str("; a retry first looks upstream for it.");
                        }
                        failed(message, e.status())
                    }
                }
            }
            Self::Jira { upstream, config } => {
                let request = match jira_request(write, fields) {
                    Ok(request) => request,
                    Err((message, status)) => return failed(message, status),
                };
                match pitcrew_sync_jira::write::send(upstream, config, &request).await {
                    Ok(written) => jira_sent(written),
                    Err(e) => failed(e.to_string(), e.status()),
                }
            }
        }
    }
}

/// The GitHub write that sends `fields` of `write`.
fn github_request(
    write: &WriteProposal,
    fields: &WriteFields,
) -> Result<pitcrew_sync_github::write::IssueWrite, Problem> {
    use pitcrew_sync_github::write::{IssueEdit, IssueWrite, StateChange};
    if write.operation == WriteOperation::CreateIssue {
        let repo = pitcrew_sync_github::RepoRef::new(write.scope.clone())
            .map_err(|_| ("The repository's name is malformed.".to_owned(), None))?;
        return Ok(IssueWrite::Create {
            repo,
            title: fields.title.clone().unwrap_or_default(),
            body: fields.body.clone().unwrap_or_default(),
            labels: fields.labels.clone().unwrap_or_default(),
            milestone: milestone_number(fields),
        });
    }
    let (repo, number) = github_issue(write)?;
    if write.operation == WriteOperation::Comment {
        return Ok(IssueWrite::Comment {
            repo,
            number,
            body: fields.comment.clone().unwrap_or_default(),
        });
    }
    let state = fields.state.map(|state| match state {
        IssueState::Open => StateChange::Reopen,
        IssueState::Closed => StateChange::Close(match fields.close_reason {
            Some(CloseReason::NotPlanned) => pitcrew_sync_github::CloseReason::NotPlanned,
            _ => pitcrew_sync_github::CloseReason::Completed,
        }),
    });
    Ok(IssueWrite::Edit {
        repo,
        number,
        edit: IssueEdit {
            title: fields.title.clone(),
            body: fields.body.clone(),
            add_labels: fields.add_labels.clone().unwrap_or_default(),
            remove_labels: fields.remove_labels.clone().unwrap_or_default(),
            milestone: milestone_number(fields),
            state,
        },
    })
}

/// What GitHub said it wrote, as a result: a created issue becomes `created`.
fn github_sent(
    config: &pitcrew_sync_github::write::WriteConfig,
    write: &WriteProposal,
    written: pitcrew_sync_github::write::Written,
) -> WriteResult {
    let created = written.number.map(|n| ExternalRef {
        system: ExternalSystem::Github,
        key: format!("{}#{n}", write.scope),
        url: written.url.clone().or_else(|| {
            (github_host(config.api_base.as_deref()) == "github.com")
                .then(|| format!("https://github.com/{}/issues/{n}", write.scope))
        }),
    });
    let url = written
        .url
        .or_else(|| created.as_ref().and_then(|c| c.url.clone()))
        .or_else(|| write.target.as_ref().and_then(|t| t.url.clone()));
    WriteResult::Sent { created, url }
}

/// The Jira write that sends `fields` of `write`.
fn jira_request(
    write: &WriteProposal,
    fields: &WriteFields,
) -> Result<pitcrew_sync_jira::write::IssueWrite, Problem> {
    use pitcrew_sync_jira::write::{IssueEdit, IssueWrite};
    let key = || {
        write
            .target
            .as_ref()
            .map(|t| t.key.clone())
            .unwrap_or_default()
    };
    Ok(match write.operation {
        WriteOperation::CreateIssue => {
            let project = pitcrew_sync_jira::ProjectRef::new(write.scope.clone())
                .map_err(|_| ("The Jira project's key is malformed.".to_owned(), None))?;
            IssueWrite::Create {
                project,
                summary: fields.title.clone().unwrap_or_default(),
                description: fields.body.clone().unwrap_or_default(),
                labels: fields.labels.clone().unwrap_or_default(),
                epic: fields.epic.clone(),
            }
        }
        WriteOperation::Comment => IssueWrite::Comment {
            key: key(),
            body: fields.comment.clone().unwrap_or_default(),
        },
        WriteOperation::Close | WriteOperation::Reopen if fields.state.is_some() => {
            IssueWrite::Transition {
                key: key(),
                to: if fields.state == Some(IssueState::Open) {
                    pitcrew_sync_jira::StatusCategory::New
                } else {
                    pitcrew_sync_jira::StatusCategory::Done
                },
            }
        }
        _ => IssueWrite::Edit {
            key: key(),
            edit: IssueEdit {
                summary: fields.title.clone(),
                description: fields.body.clone(),
                add_labels: fields.add_labels.clone().unwrap_or_default(),
                remove_labels: fields.remove_labels.clone().unwrap_or_default(),
                epic: fields.epic.clone(),
            },
        },
    })
}

/// What Jira said it wrote, as a result: a created issue becomes `created`.
fn jira_sent(written: pitcrew_sync_jira::write::Written) -> WriteResult {
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
        let (title, body) = ask_text(
            &write,
            "PAP-3",
            "Because @sam changed PAP-3 in PitCrew.",
            &[],
        );
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
    fn a_label_change_touches_only_the_labels_the_hub_holds() {
        // Upstream has 40; the hub holds the first 32. A person removes one and adds one.
        let seen: Vec<String> = (1..=40).map(|n| format!("l{n:02}")).collect();
        let mut labels = fit_labels(&seen);
        assert_eq!(labels.len(), 32);
        labels.retain(|l| l != "l05");
        labels.push("new".into());
        let (add, remove) = label_change(&seen, &labels);
        assert_eq!(
            (add, remove),
            (vec!["new".to_string()], vec!["l05".to_string()])
        );
        // The 8 the hub never held are neither added nor removed.
        let (add, remove) = label_change(&seen, &fit_labels(&seen));
        assert!(add.is_empty() && remove.is_empty());
    }

    fn update(before: WriteFields, after: WriteFields) -> WriteProposal {
        WriteProposal {
            ask: AskId::new(),
            integration: IntegrationId::new(),
            system: ExternalSystem::Github,
            scope: "example-org/demo-repo".into(),
            target: None,
            task: None,
            operation: WriteOperation::Update,
            before,
            after,
            requested_by: MemberId::new(),
            cause: None,
        }
    }

    #[test]
    fn upstream_now_decides_what_is_left_to_send() {
        let write = update(
            WriteFields {
                title: Some("Old".into()),
                body: Some("Body".into()),
                labels: Some(vec!["bug".into(), "tests".into()]),
                milestone: Some("example-org/demo-repo#milestone:1".into()),
                ..WriteFields::default()
            },
            WriteFields {
                title: Some("New".into()),
                body: Some("Body, fixed".into()),
                add_labels: Some(vec!["docs".into(), "bug".into()]),
                remove_labels: Some(vec!["tests".into(), "gone".into()]),
                milestone: Some("example-org/demo-repo#milestone:2".into()),
                ..WriteFields::default()
            },
        );
        let now = Now {
            title: "Old".into(),
            body: "Body".into(),
            body_exact: true,
            labels: vec!["bug".into(), "tests\u{200b}".into(), "security".into()],
            parent: Some("example-org/demo-repo#milestone:1".into()),
            open: true,
            url: None,
        };
        // Everything still as read: all of it, labels against upstream's own spelling.
        let rest = reconcile(&write, &now).unwrap();
        assert_eq!(rest.title.as_deref(), Some("New"));
        assert_eq!(rest.body.as_deref(), Some("Body, fixed"));
        assert_eq!(rest.add_labels, Some(vec!["docs".to_string()]));
        assert_eq!(rest.remove_labels, Some(vec!["tests\u{200b}".to_string()]));
        assert_eq!(
            rest.milestone.as_deref(),
            Some("example-org/demo-repo#milestone:2")
        );
        // Upstream already has it: nothing left.
        let there = Now {
            title: "New".into(),
            body: "Body, fixed".into(),
            labels: vec!["bug".into(), "docs".into()],
            parent: Some("example-org/demo-repo#milestone:2".into()),
            ..now.clone()
        };
        assert!(reconcile(&write, &there).unwrap().is_empty());
        // Changed upstream since: nothing is sent, and the fields are named.
        let moved = Now {
            title: "Someone else's".into(),
            body: "Body".into(),
            body_exact: false,
            parent: None,
            ..now.clone()
        };
        assert_eq!(
            reconcile(&write, &moved).unwrap_err(),
            vec!["title", "body", "milestone"]
        );
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
