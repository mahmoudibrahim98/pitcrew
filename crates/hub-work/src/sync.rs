//! The hub's side of a tracker sync (GitHub, Jira; api-v1.md "Integrations"): the sync's own
//! member ([`WorkService::ensure_sync_member`]) and [`SyncCommands`], the commands a sync applies
//! upstream changes through.
//!
//! This crate knows nothing of GitHub or Jira: the daemon reads them (`pitcrew-sync-github`,
//! `pitcrew-sync-jira`), plans what each change implies, and calls these commands. Each one
//! re-checks the hub's own tables under the command lock, like any caller's command, and appends
//! its events authored by the sync's member (an agent) on behalf of its owner (the person who
//! connected the tracker):
//!
//! - **tasks** are created with their upstream `source`, in a workstream of their project, status
//!   `todo`, with text made to fit the hub's rules ([`fit_title`], [`fit_labels`]);
//! - **updates** change only upstream-owned fields (title, description, labels) and the
//!   workstream, and append nothing when nothing differs;
//! - **moves** pass `TaskStatus::can_move` with `Mover::Sync` from the status the task is in now;
//!   in-progress work is never touched;
//! - **a workstream** is shipped only while none of its tasks is in progress;
//! - **conflicts** are `decision` asks to the owner, never raised twice while one is open, and
//!   **notes** (a merged pull request) are comments, never posted twice on one task.
//!
//! A sync read again (after a restart, say) is safe: what is already so changes nothing.

use crate::error::{Result, WorkError};
use crate::query::{self, AskFilter, TaskFilter, TaskRef};
use crate::service::{WorkService, no_task};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{MemberId, TaskId, TaskKey, WorkstreamId};
use pitcrew_protocol::model::{
    Ask, AskKind, AskState, ExternalRef, Member, MemberKind, Mover, Priority, Task, TaskPatch,
    TaskStatus, Workstream, WorkstreamStatus,
};
use pitcrew_protocol::text::is_hidden;
use pitcrew_store::sql::{OptionalExtension, params};

/// The handle of the sync's member.
pub const SYNC_HANDLE: &str = "@sync";
/// The handle used when [`SYNC_HANDLE`] is someone else's.
pub const SYNC_FALLBACK_HANDLE: &str = "@tracker-sync";
/// The display name [`WorkService::ensure_sync_member`] gives a member it adds.
pub const SYNC_NAME: &str = "Tracker sync";

/// The longest ask title a conflict gets, in characters.
const ASK_TITLE_CHARS: usize = 200;

/// Text without hidden characters, trimmed, at most `max` characters.
fn fit(text: &str, max: usize) -> String {
    let clean: String = text.chars().filter(|c| !is_hidden(*c)).collect();
    clean
        .trim()
        .chars()
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// An upstream title made to fit the hub's (1 to [`crate::TITLE_CHARS`] characters, no hidden
/// characters); `fallback` (the upstream key, say) when nothing is left.
#[must_use]
pub fn fit_title(title: &str, fallback: &str) -> String {
    let fitted = fit(title, crate::TITLE_CHARS);
    if fitted.is_empty() {
        fit(fallback, crate::TITLE_CHARS)
    } else {
        fitted
    }
}

/// Upstream labels made to fit the hub's rules: each trimmed and cut to [`crate::LABEL_CHARS`]
/// characters, empty ones dropped, the first of equal ones kept, at most [`crate::MAX_LABELS`].
#[must_use]
pub fn fit_labels(labels: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for label in labels {
        let label = fit(label, crate::LABEL_CHARS);
        if !label.is_empty() && !out.contains(&label) {
            out.push(label);
            if out.len() == crate::MAX_LABELS {
                break;
            }
        }
    }
    out
}

/// What a move or a status change came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<T> {
    /// It was made.
    Changed(T),
    /// It was so already; nothing was appended.
    Unchanged,
    /// The rules refuse it; nothing was appended. Why, for a person to read.
    Refused(String),
}

/// What a sync changes in the hub. See the [module docs](self). Get one with
/// [`WorkService::sync_commands`].
#[derive(Debug)]
pub struct SyncCommands<'a> {
    work: &'a WorkService,
    member: MemberId,
    owner: MemberId,
}

impl WorkService {
    /// The sync's member for a workspace whose person is `owner`, found or added: the member
    /// holding [`SYNC_HANDLE`] (else [`SYNC_FALLBACK_HANDLE`]) when it is an agent of `owner`;
    /// otherwise a new one (agent, [`SYNC_NAME`], owned by `owner`) under the first of the two
    /// handles nobody holds, appended in a `member_added` authored by `owner`.
    ///
    /// # Errors
    ///
    /// `invalid` when `owner` is not a person of this workspace; `conflict` when both handles are
    /// held by other members; database errors.
    pub fn ensure_sync_member(&self, owner: MemberId) -> Result<Member> {
        let _guard = self.lock();
        let (person, found) = self.read(|c| {
            let mut found = Vec::new();
            for handle in [SYNC_HANDLE, SYNC_FALLBACK_HANDLE] {
                found.push((handle, query::member_with_handle(c, handle)?));
            }
            Ok((query::member(c, &owner)?, found))
        })?;
        if !person.is_some_and(|p| p.kind == MemberKind::Human) {
            return Err(WorkError::invalid(format!(
                "{owner} is not a person of this workspace; only a person owns the tracker sync."
            )));
        }
        let mut free = None;
        for (handle, member) in found {
            match member {
                Some(m) if m.kind == MemberKind::Agent && m.owner == Some(owner) => return Ok(m),
                Some(_) => {}
                None => {
                    free.get_or_insert(handle);
                }
            }
        }
        let Some(handle) = free else {
            return Err(WorkError::conflict(format!(
                "{SYNC_HANDLE} and {SYNC_FALLBACK_HANDLE} are both other members' handles."
            )));
        };
        let member = Member {
            id: MemberId::new(),
            kind: MemberKind::Agent,
            handle: handle.to_owned(),
            name: SYNC_NAME.to_owned(),
            owner: Some(owner),
            persona: None,
            avatar: None,
        };
        self.append(&[self.event(
            owner,
            None,
            EventBody::MemberAdded {
                member: member.clone(),
            },
        )])?;
        Ok(member)
    }

    /// The commands of the sync acting as `member` (an agent with an owner, from
    /// [`WorkService::ensure_sync_member`]).
    ///
    /// # Errors
    ///
    /// `invalid` when `member` is not an agent of this workspace with an owner; database errors.
    pub fn sync_commands(&self, member: MemberId) -> Result<SyncCommands<'_>> {
        match self.read(|c| query::member(c, &member))? {
            Some(Member {
                kind: MemberKind::Agent,
                owner: Some(owner),
                ..
            }) => Ok(SyncCommands {
                work: self,
                member,
                owner,
            }),
            _ => Err(WorkError::invalid(format!(
                "The tracker sync's member {member} must be an agent of this workspace with an \
                 owner."
            ))),
        }
    }
}

impl SyncCommands<'_> {
    /// The sync's member.
    #[must_use]
    pub fn member(&self) -> MemberId {
        self.member
    }

    /// The person the sync acts for, whom conflicts are asked of.
    #[must_use]
    pub fn owner(&self) -> MemberId {
        self.owner
    }

    fn event(&self, body: EventBody) -> pitcrew_protocol::events::Event {
        self.work.event(self.member, Some(self.owner), body)
    }

    /// The service, for the write commands (`crate::writes`).
    pub(crate) fn work(&self) -> &WorkService {
        self.work
    }

    /// An event authored by the sync on behalf of its owner, for the write commands.
    pub(crate) fn sync_event(&self, body: EventBody) -> pitcrew_protocol::events::Event {
        self.event(body)
    }

    /// The task mirroring the upstream item `source` (same system and key), if there is one.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn task_by_source(&self, source: &ExternalRef) -> Result<Option<Task>> {
        let system = crate::codec::enum_text(&source.system)
            .map_err(|e| WorkError::internal(e.to_string()))?;
        self.work.read(|c| {
            let id: Option<String> = c
                .prepare_cached(
                    "SELECT id FROM work_tasks
                     WHERE json_extract(doc, '$.source.system') = ?1
                       AND json_extract(doc, '$.source.key') = ?2
                     ORDER BY rev LIMIT 1",
                )?
                .query_row(params![system, source.key], |r| r.get(0))
                .optional()?;
            match id.and_then(|id| id.parse::<TaskId>().ok()) {
                Some(id) => query::task(c, &TaskRef::Id(id)),
                None => Ok(None),
            }
        })
    }

    /// The task `task`.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown task; database errors.
    pub fn task_by_id(&self, task: &TaskId) -> Result<Task> {
        let reference = TaskRef::Id(*task);
        self.work
            .read(|c| query::task(c, &reference)?.ok_or_else(|| no_task(&reference)))
    }

    /// Every workstream, for routing upstream items by their links.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn workstreams(&self) -> Result<Vec<Workstream>> {
        self.work.workstreams(None)
    }

    /// Whether a task of `workstream` is in progress.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn work_in_progress(&self, workstream: &WorkstreamId) -> Result<bool> {
        let filter = TaskFilter {
            workstream: Some(*workstream),
            statuses: vec![TaskStatus::InProgress],
            ..TaskFilter::default()
        };
        Ok(!self.work.read(|c| query::tasks(c, &filter))?.is_empty())
    }

    /// Creates a task mirroring `source` in `workstream` (and its project), status `todo`, with
    /// the title, description and labels made to fit ([`fit_title`], [`fit_labels`]). When a task
    /// already mirrors `source`, that task is returned and nothing is appended.
    ///
    /// # Errors
    ///
    /// `invalid` for an unknown workstream or project; `conflict` when another writer took the
    /// key; database errors.
    pub fn create_task(
        &self,
        workstream: &WorkstreamId,
        source: ExternalRef,
        title: &str,
        description: &str,
        labels: &[String],
    ) -> Result<Task> {
        if let Some(task) = self.task_by_source(&source)? {
            return Ok(task);
        }
        let _guard = self.work.lock();
        let (project, number) = self.work.read(|c| {
            let workstream = query::workstream(c, workstream)?.ok_or_else(|| {
                WorkError::invalid(format!("workstream: no workstream {workstream}."))
            })?;
            let project = query::project(c, &workstream.project)?.ok_or_else(|| {
                WorkError::invalid(format!("project: no project {}.", workstream.project))
            })?;
            let highest = query::highest_task_number(c, &project.key)?;
            Ok((project, highest))
        })?;
        let number = number.checked_add(1).ok_or_else(|| {
            WorkError::conflict(format!("{} has no task numbers left.", project.key))
        })?;
        let key = TaskKey::new(project.key.clone(), number)
            .map_err(|e| WorkError::internal(e.to_string()))?;
        let task = Task {
            id: TaskId::new(),
            key,
            project: project.id,
            workstream: Some(*workstream),
            title: fit_title(title, &source.key),
            description: description.to_owned(),
            status: TaskStatus::Todo,
            priority: Priority::None,
            assignee: None,
            labels: fit_labels(labels),
            start: None,
            due: None,
            blocked_by: Vec::new(),
            source: Some(source),
            accept_auto: false,
            subtasks: Vec::new(),
        };
        let (id, key) = (task.id, task.key.clone());
        self.work
            .append(&[self.event(EventBody::TaskCreated { task })])?;
        match self.work.read(|c| {
            Ok((
                query::task(c, &TaskRef::Id(id))?,
                query::key_clashed(c, &id)?,
            ))
        })? {
            (Some(task), _) => Ok(task),
            (None, true) => Err(WorkError::conflict(format!(
                "{key} was taken by another change at the same moment."
            ))),
            (None, false) => Err(WorkError::internal(format!(
                "task {id} is missing after it was created"
            ))),
        }
    }

    /// Changes upstream-owned fields of `task`: each `Some` field is made to fit and set; the
    /// workstream must be one of the task's project. Appends `task_updated` with the fields that
    /// differ, or nothing.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown task; `invalid` for a workstream of another project; database
    /// errors.
    pub fn update_task(
        &self,
        task: &TaskId,
        title: Option<&str>,
        description: Option<&str>,
        labels: Option<&[String]>,
        workstream: Option<WorkstreamId>,
    ) -> Result<Outcome<Task>> {
        let _guard = self.work.lock();
        let reference = TaskRef::Id(*task);
        let current = self.work.read(|c| {
            let current = query::task(c, &reference)?.ok_or_else(|| no_task(&reference))?;
            if let Some(id) = &workstream {
                let found = query::workstream(c, id)?.ok_or_else(|| {
                    WorkError::invalid(format!("workstream: no workstream {id}."))
                })?;
                if found.project != current.project {
                    return Err(WorkError::invalid(format!(
                        "workstream \"{}\" belongs to another project than {}.",
                        found.name, current.key
                    )));
                }
            }
            Ok(current)
        })?;
        let fallback = current
            .source
            .as_ref()
            .map_or_else(|| current.key.to_string(), |s| s.key.clone());
        let wanted = TaskPatch {
            workstream: workstream.map(Some),
            title: title.map(|t| fit_title(t, &fallback)),
            description: description.map(str::to_owned),
            labels: labels.map(fit_labels),
            ..TaskPatch::default()
        };
        let patch = crate::edits::changed_fields(&current, wanted);
        if patch == TaskPatch::default() {
            return Ok(Outcome::Unchanged);
        }
        self.work.append(&[self.event(EventBody::TaskUpdated {
            task: current.id,
            patch,
        })])?;
        Ok(Outcome::Changed(self.work.reload_task(current.id)?))
    }

    /// Moves `task` to `to` as the sync (`Mover::Sync`), checked against the status it is in
    /// now.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown task; `conflict` when another writer moved it first; database
    /// errors.
    pub fn move_task(&self, task: &TaskId, to: TaskStatus) -> Result<Outcome<Task>> {
        let _guard = self.work.lock();
        let reference = TaskRef::Id(*task);
        let current = self
            .work
            .read(|c| query::task(c, &reference)?.ok_or_else(|| no_task(&reference)))?;
        if current.status == to {
            return Ok(Outcome::Unchanged);
        }
        if !current.status.can_move(to, Mover::Sync) {
            let name = |s: TaskStatus| crate::codec::enum_text(&s).unwrap_or_default();
            return Ok(Outcome::Refused(format!(
                "{} is {}, and a sync may not move it to {}.",
                current.key,
                name(current.status),
                name(to)
            )));
        }
        self.work.append(&[self.event(EventBody::TaskMoved {
            task: current.id,
            from: current.status,
            to,
            mover: Mover::Sync,
        })])?;
        Ok(Outcome::Changed(self.work.reload_moved(current.id, to)?))
    }

    /// Sets `workstream`'s status to `to` (a closed milestone or epic: `shipped`), keeping its
    /// health. Refused while one of its tasks is in progress.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown workstream; database errors.
    pub fn set_workstream_status(
        &self,
        workstream: &WorkstreamId,
        to: WorkstreamStatus,
    ) -> Result<Outcome<Workstream>> {
        let _guard = self.work.lock();
        let current = self.work.workstream(workstream)?;
        if current.status == to {
            return Ok(Outcome::Unchanged);
        }
        if self.work_in_progress(workstream)? {
            return Ok(Outcome::Refused(format!(
                "\"{}\" has a task in progress, and a sync does not change its status.",
                current.name
            )));
        }
        self.work
            .append(&[self.event(EventBody::WorkstreamChanged {
                workstream: current.id,
                status: to,
                health: current.health,
            })])?;
        Ok(Outcome::Changed(self.work.workstream(workstream)?))
    }

    /// Posts `text` on `task` (a merged pull request, say), unless the sync already posted that
    /// same text there. Whether it posted.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown task; `invalid` for an empty text; database errors.
    pub fn note(&self, task: &TaskId, text: &str) -> Result<bool> {
        crate::commands::not_empty(text, "text")?;
        let _guard = self.work.lock();
        let reference = TaskRef::Id(*task);
        let posted = self.work.read(|c| {
            query::task(c, &reference)?.ok_or_else(|| no_task(&reference))?;
            Ok(c.prepare_cached(
                "SELECT 1 FROM work_comments WHERE task = ?1 AND author = ?2 AND text = ?3",
            )?
            .query_row(
                params![task.0.to_string(), self.member.0.to_string(), text],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
        })?;
        if posted {
            return Ok(false);
        }
        self.work.append(&[self.event(EventBody::CommentPosted {
            task: Some(*task),
            workstream: None,
            text: text.to_owned(),
            mentions: Vec::new(),
        })])?;
        Ok(true)
    }

    /// Raises a conflict for the owner to decide: an ask of kind `decision` from the sync, with
    /// the task when there is one. `None` when the same ask (same title and task) is still open.
    ///
    /// # Errors
    ///
    /// `invalid` for an unknown task; database errors.
    pub fn raise_conflict(
        &self,
        task: Option<TaskId>,
        title: &str,
        body: &str,
    ) -> Result<Option<Ask>> {
        let title = fit_title(title, "A tracker sync needs a decision");
        let title: String = title.chars().take(ASK_TITLE_CHARS).collect();
        let _guard = self.work.lock();
        let filter = AskFilter {
            to: Some(self.owner),
            states: vec![AskState::Open],
        };
        let open = self.work.read(|c| {
            if let Some(id) = task {
                query::task(c, &TaskRef::Id(id))?
                    .ok_or_else(|| WorkError::invalid(format!("task: no task {id}.")))?;
            }
            query::asks(c, &filter)
        })?;
        if open
            .iter()
            .any(|a| a.from == self.member && a.task == task && a.title == title)
        {
            return Ok(None);
        }
        let ask = Ask {
            id: pitcrew_protocol::ids::AskId::new(),
            kind: AskKind::Decision,
            from: self.member,
            to: self.owner,
            task,
            session: None,
            title,
            body: body.to_owned(),
            options: Vec::new(),
            receipts: Vec::new(),
            state: AskState::Open,
            answer: None,
            created: self.work.now(),
        };
        let id = ask.id;
        self.work
            .append(&[self.event(EventBody::AskRaised { ask })])?;
        self.work.ask(&id).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_labels_fit_the_hubs_rules() {
        assert_eq!(fit_title("  Fix\u{202e} login  ", "R#1"), "Fix login");
        assert_eq!(
            fit_title(" \u{200b} ", "example-org/demo-repo#1"),
            "example-org/demo-repo#1"
        );
        assert_eq!(
            fit_title(&"x".repeat(900), "k").chars().count(),
            crate::TITLE_CHARS
        );
        let labels: Vec<String> = ["bug", " bug ", "", &"l".repeat(100)]
            .iter()
            .map(|s| (*s).to_string())
            .chain((0..100).map(|i| format!("label-{i}")))
            .collect();
        let fitted = fit_labels(&labels);
        assert_eq!(fitted[0], "bug");
        assert_eq!(fitted[1].chars().count(), crate::LABEL_CHARS);
        assert_eq!(fitted.len(), crate::MAX_LABELS);
    }
}
