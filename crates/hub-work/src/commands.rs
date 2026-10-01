//! Commands: each validates against the current tables, then appends the events that make the
//! change. The rules are those of `docs/build/contracts/api-v1.md`:
//! - a `device` token is a person, who may change anything;
//! - an `agent` token writes only to its **own** tasks (it is the assignee, or holds an active
//!   dispatch on the task), else `forbidden`;
//! - a move is checked with `TaskStatus::can_move`; a refused move is a `conflict`;
//! - decisions, approvals and reviews are answered only by people, and an ask only by its
//!   addressee or the addressee's owner.

use crate::error::{Result, WorkError};
use crate::query::{self, TaskRef};
use crate::service::{WorkService, no_task};
use pitcrew_protocol::api::{Caller, TokenScope};
use pitcrew_protocol::events::{BriefTarget, Event, EventBody};
use pitcrew_protocol::ids::{
    AskId, DispatchId, MemberId, ProjectId, SessionId, SubtaskId, TaskId, TaskKey, WorkstreamId,
};
use pitcrew_protocol::model::{
    Answer, Ask, AskKind, AskState, Brief, Date, Health, MemberKind, Mover, Priority, Receipt,
    Subtask, SubtaskSource, Task, TaskStatus, Workstream, WorkstreamStatus,
};
use pitcrew_protocol::transcript::{PlanItem, PlanStatus};
use pitcrew_store::sql::Connection;
use serde::Deserialize;
use std::collections::HashSet;

/// `POST /v1/tasks`: a new task. The hub assigns the id and the next key in the project.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NewTask {
    /// The project.
    pub project: ProjectId,
    /// The workstream, which must belong to the project.
    #[serde(default)]
    pub workstream: Option<WorkstreamId>,
    /// Title; not empty.
    pub title: String,
    /// Description.
    #[serde(default)]
    pub description: Option<String>,
    /// Status; `todo` if absent.
    #[serde(default)]
    pub status: Option<TaskStatus>,
    /// Priority; `none` if absent.
    #[serde(default)]
    pub priority: Option<Priority>,
    /// Assignee.
    #[serde(default)]
    pub assignee: Option<MemberId>,
    /// Labels.
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    /// Due date, `YYYY-MM-DD`.
    #[serde(default)]
    pub due: Option<Date>,
}

/// `POST /v1/asks`: a new ask, from the caller.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NewAsk {
    /// Kind.
    pub kind: AskKind,
    /// Who must answer.
    pub to: MemberId,
    /// One-line title; not empty.
    pub title: String,
    /// Context.
    #[serde(default)]
    pub body: Option<String>,
    /// Offered options.
    #[serde(default)]
    pub options: Option<Vec<String>>,
    /// Related task.
    #[serde(default)]
    pub task: Option<TaskId>,
    /// Related session.
    #[serde(default)]
    pub session: Option<SessionId>,
    /// Evidence.
    #[serde(default)]
    pub receipts: Option<Vec<Receipt>>,
}

/// `POST /v1/asks/{id}/answer`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct AnswerAsk {
    /// The chosen option's index.
    #[serde(default)]
    pub option: Option<usize>,
    /// Free text.
    #[serde(default)]
    pub text: Option<String>,
}

/// `PUT /v1/briefs/{kind}/{id}`: a person's brief.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BriefEdit {
    /// The text.
    pub text: String,
    /// The next step. **Not stored yet**: `brief_accepted` has no field for it (a contract gap).
    #[serde(default)]
    pub next: Option<String>,
    /// Whether it is pinned.
    pub pinned: bool,
}

/// `PATCH /v1/workstreams/{id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct WorkstreamPatch {
    /// New status.
    #[serde(default)]
    pub status: Option<WorkstreamStatus>,
    /// New health.
    #[serde(default)]
    pub health: Option<Health>,
}

/// `POST /v1/tasks/{id}/comments`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NewComment {
    /// Text (markdown); not empty.
    pub text: String,
    /// Mentioned members.
    #[serde(default)]
    pub mentions: Vec<MemberId>,
}

fn require_person(caller: &Caller, what: &str) -> Result<()> {
    if caller.is_person() {
        Ok(())
    } else {
        Err(WorkError::forbidden(format!(
            "{what} needs a device token."
        )))
    }
}

fn require_own_task(conn: &Connection, caller: &Caller, task: &Task) -> Result<()> {
    if caller.scope == TokenScope::Agent && !query::is_own_task(conn, task, &caller.member)? {
        return Err(WorkError::forbidden(format!(
            "An agent may only change its own tasks; {} is not one of them.",
            task.key
        )));
    }
    Ok(())
}

fn not_empty(text: &str, field: &str) -> Result<()> {
    if text.trim().is_empty() {
        Err(WorkError::invalid(format!("{field} must not be empty.")))
    } else {
        Ok(())
    }
}

fn known_member(
    conn: &Connection,
    id: &MemberId,
    field: &str,
) -> Result<pitcrew_protocol::model::Member> {
    query::member(conn, id)?.ok_or_else(|| WorkError::invalid(format!("{field}: no member {id}.")))
}

fn move_refusal(task: &Task, to: TaskStatus, mover: Mover) -> String {
    let name = |s: TaskStatus| crate::codec::enum_text(&s).unwrap_or_default();
    if task.status == to {
        return format!("{} is already {}.", task.key, name(to));
    }
    match mover {
        Mover::Agent { .. } => format!(
            "An agent may only move a task from backlog or todo to in_progress, or from \
             in_progress to review; not {} → {}.",
            name(task.status),
            name(to)
        ),
        _ => format!(
            "The rules do not allow moving {} from {} to {}.",
            task.key,
            name(task.status),
            name(to)
        ),
    }
}

/// Whether `s` is a line of `agent`'s plan.
fn is_plan_of(s: &Subtask, agent: MemberId) -> bool {
    matches!(s.source, SubtaskSource::AgentPlan { agent: a } if a == agent)
}

/// Replaces `agent`'s plan lines in `existing` with `plan`, where the first of them stood (at the
/// end if there were none). Every other line keeps its place.
fn splice_plan(existing: &[Subtask], agent: MemberId, plan: Vec<Subtask>) -> Vec<Subtask> {
    let mut out = Vec::with_capacity(existing.len() + plan.len());
    let mut plan = Some(plan);
    for s in existing {
        if is_plan_of(s, agent) {
            if let Some(lines) = plan.take() {
                out.extend(lines);
            }
        } else {
            out.push(s.clone());
        }
    }
    if let Some(lines) = plan {
        out.extend(lines);
    }
    out
}

/// `agent`'s plan as subtask lines. A line keeps the id of an earlier line with the same text, so
/// ids stay stable while the agent works through its plan.
fn plan_lines(existing: &[Subtask], agent: MemberId, items: &[PlanItem]) -> Vec<Subtask> {
    let mut old: Vec<&Subtask> = existing.iter().filter(|s| is_plan_of(s, agent)).collect();
    items
        .iter()
        .filter(|item| !item.text.trim().is_empty())
        .map(|item| {
            let id = old
                .iter()
                .position(|s| s.text == item.text)
                .map_or_else(SubtaskId::new, |i| old.remove(i).id);
            Subtask {
                id,
                text: item.text.clone(),
                done: item.status == PlanStatus::Completed,
                source: SubtaskSource::AgentPlan { agent },
            }
        })
        .collect()
}

impl WorkService {
    /// Creates a task with the next key in its project. People only.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent; `invalid` for an empty title, an unknown project, workstream or
    /// assignee, a workstream of another project, or a malformed date.
    pub fn create_task(&self, caller: &Caller, new: NewTask) -> Result<Task> {
        require_person(caller, "Creating a task")?;
        not_empty(&new.title, "title")?;
        if let Some(due) = &new.due
            && !due.is_well_formed()
        {
            return Err(WorkError::invalid("due must be a date written YYYY-MM-DD."));
        }
        let _guard = self.lock();
        let (project, number) = self.read(|c| {
            let project = query::project(c, &new.project)?.ok_or_else(|| {
                WorkError::invalid(format!("project: no project {}.", new.project))
            })?;
            if let Some(id) = &new.workstream {
                let workstream = query::workstream(c, id)?.ok_or_else(|| {
                    WorkError::invalid(format!("workstream: no workstream {id}."))
                })?;
                if workstream.project != project.id {
                    return Err(WorkError::invalid(format!(
                        "workstream \"{}\" belongs to another project.",
                        workstream.name
                    )));
                }
            }
            if let Some(id) = &new.assignee {
                known_member(c, id, "assignee")?;
            }
            Ok((project, query::highest_task_number(c, &new.project)?))
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
            workstream: new.workstream,
            title: new.title,
            description: new.description.unwrap_or_default(),
            status: new.status.unwrap_or(TaskStatus::Todo),
            priority: new.priority.unwrap_or_default(),
            assignee: new.assignee,
            labels: new.labels.unwrap_or_default(),
            start: None,
            due: new.due,
            blocked_by: Vec::new(),
            source: None,
            accept_auto: false,
            subtasks: Vec::new(),
        };
        let id = task.id;
        self.append(&[self.by(caller, EventBody::TaskCreated { task })])?;
        self.reload_task(id)
    }

    /// Moves a task. The mover comes from the caller: a person, or an agent on its own task.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown task; `forbidden` for an agent on a task not its own;
    /// `conflict` when `TaskStatus::can_move` refuses the move.
    pub fn move_task(&self, caller: &Caller, task: &TaskRef, to: TaskStatus) -> Result<Task> {
        let _guard = self.lock();
        let task = self.read(|c| {
            let task = query::task(c, task)?.ok_or_else(|| no_task(task))?;
            require_own_task(c, caller, &task)?;
            Ok(task)
        })?;
        // An agent that got past the check above is on its own task.
        let mover = if caller.is_person() {
            Mover::Person
        } else {
            Mover::Agent { on_own_task: true }
        };
        if !task.status.can_move(to, mover) {
            return Err(WorkError::conflict(move_refusal(&task, to, mover)));
        }
        let body = EventBody::TaskMoved {
            task: task.id,
            from: task.status,
            to,
            mover,
        };
        self.append(&[self.by(caller, body)])?;
        self.reload_task(task.id)
    }

    /// Assigns a task, or unassigns it with `None`. People only. Nothing is appended when the
    /// assignee does not change.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent; `not_found` for an unknown task; `invalid` for an unknown
    /// assignee.
    pub fn assign_task(
        &self,
        caller: &Caller,
        task: &TaskRef,
        assignee: Option<MemberId>,
    ) -> Result<Task> {
        require_person(caller, "Assigning a task")?;
        let _guard = self.lock();
        let task = self.read(|c| {
            let task = query::task(c, task)?.ok_or_else(|| no_task(task))?;
            if let Some(id) = &assignee {
                known_member(c, id, "assignee")?;
            }
            Ok(task)
        })?;
        if task.assignee == assignee {
            return Ok(task);
        }
        self.append(&[self.by(
            caller,
            EventBody::TaskAssigned {
                task: task.id,
                assignee,
            },
        )])?;
        self.reload_task(task.id)
    }

    /// Replaces a task's subtasks. A person replaces the whole list. An agent, on its own task,
    /// replaces only its own plan lines (every line it sends must be `agent_plan` naming itself),
    /// and every other line stays where it was.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown task; `forbidden` for an agent on a task not its own or sending
    /// a line that is not its own plan; `invalid` for an empty text, an `agent_plan` naming
    /// something other than a known agent, or repeated ids.
    pub fn replace_subtasks(
        &self,
        caller: &Caller,
        task: &TaskRef,
        incoming: Vec<Subtask>,
    ) -> Result<Task> {
        let _guard = self.lock();
        let task = self.read(|c| {
            let task = query::task(c, task)?.ok_or_else(|| no_task(task))?;
            require_own_task(c, caller, &task)?;
            for (i, s) in incoming.iter().enumerate() {
                not_empty(&s.text, &format!("[{i}].text"))?;
                if let SubtaskSource::AgentPlan { agent } = &s.source {
                    let field = format!("[{i}].source.agent");
                    let member = known_member(c, agent, &field)?;
                    if member.kind != MemberKind::Agent {
                        return Err(WorkError::invalid(format!(
                            "{field} must be an agent; {} is a person.",
                            member.handle
                        )));
                    }
                }
            }
            Ok(task)
        })?;
        let subtasks = if caller.is_person() {
            incoming
        } else {
            let me = caller.member;
            if !incoming.iter().all(|s| is_plan_of(s, me)) {
                return Err(WorkError::forbidden(format!(
                    "An agent may only write its own plan: every subtask needs source \
                     {{\"kind\":\"agent_plan\",\"agent\":\"{}\"}}.",
                    me.0
                )));
            }
            splice_plan(&task.subtasks, me, incoming)
        };
        let mut seen = HashSet::new();
        if !subtasks.iter().all(|s| seen.insert(s.id)) {
            return Err(WorkError::invalid("Subtask ids must be unique."));
        }
        self.append(&[self.by(
            caller,
            EventBody::SubtasksReplaced {
                task: task.id,
                subtasks,
            },
        )])?;
        self.reload_task(task.id)
    }

    /// Posts a comment on a task, and returns its event.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown task; `forbidden` for an agent on a task not its own;
    /// `invalid` for an empty text or an unknown mentioned member.
    pub fn post_comment(
        &self,
        caller: &Caller,
        task: &TaskRef,
        comment: NewComment,
    ) -> Result<Event> {
        not_empty(&comment.text, "text")?;
        let _guard = self.lock();
        let task = self.read(|c| {
            let task = query::task(c, task)?.ok_or_else(|| no_task(task))?;
            require_own_task(c, caller, &task)?;
            for (i, id) in comment.mentions.iter().enumerate() {
                known_member(c, id, &format!("mentions[{i}]"))?;
            }
            Ok(task)
        })?;
        let event = self.by(
            caller,
            EventBody::CommentPosted {
                task: Some(task.id),
                workstream: None,
                text: comment.text,
                mentions: comment.mentions,
            },
        );
        self.append(std::slice::from_ref(&event))?;
        Ok(event)
    }

    /// Raises an ask from the caller.
    ///
    /// # Errors
    ///
    /// `invalid` for an empty title or an unknown addressee, task or session; `forbidden` for an
    /// agent naming a task or session that is not its own.
    pub fn raise_ask(&self, caller: &Caller, new: NewAsk) -> Result<Ask> {
        not_empty(&new.title, "title")?;
        let _guard = self.lock();
        self.read(|c| {
            known_member(c, &new.to, "to")?;
            let task = match &new.task {
                Some(id) => Some(
                    query::task(c, &TaskRef::Id(*id))?
                        .ok_or_else(|| WorkError::invalid(format!("task: no task {id}.")))?,
                ),
                None => None,
            };
            let session = match &new.session {
                Some(id) => Some(
                    query::session(c, id)?
                        .ok_or_else(|| WorkError::invalid(format!("session: no session {id}.")))?,
                ),
                None => None,
            };
            if let Some(task) = &task {
                require_own_task(c, caller, task)?;
            }
            if let Some(session) = &session
                && caller.scope == TokenScope::Agent
                && session.agent != Some(caller.member)
            {
                return Err(WorkError::forbidden(
                    "An agent may only act on its own sessions.",
                ));
            }
            Ok(())
        })?;
        let ask = Ask {
            id: AskId::new(),
            kind: new.kind,
            from: caller.member,
            to: new.to,
            task: new.task,
            session: new.session,
            title: new.title,
            body: new.body.unwrap_or_default(),
            options: new.options.unwrap_or_default(),
            receipts: new.receipts.unwrap_or_default(),
            state: AskState::Open,
            answer: None,
            created: self.now(),
        };
        let id = ask.id;
        self.append(&[self.by(caller, EventBody::AskRaised { ask })])?;
        self.ask(&id)
    }

    /// Answers an open ask.
    ///
    /// Who may answer: a person answers asks addressed to them or to an agent they own; an agent
    /// answers only questions and mentions addressed to itself. Decisions, approvals and reviews
    /// always need a person.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown ask; `invalid` without an option or a text, or with an option
    /// out of range; `forbidden` when the caller may not answer it; `conflict` when it is no
    /// longer open.
    pub fn answer_ask(&self, caller: &Caller, id: &AskId, answer: AnswerAsk) -> Result<Ask> {
        let _guard = self.lock();
        let ask = self.read(|c| {
            let ask =
                query::ask(c, id)?.ok_or_else(|| WorkError::not_found(format!("No ask {id}.")))?;
            if answer.option.is_none() && answer.text.is_none() {
                return Err(WorkError::invalid("Give an option, a text, or both."));
            }
            if let Some(option) = answer.option
                && option >= ask.options.len()
            {
                return Err(WorkError::invalid(if ask.options.is_empty() {
                    "This ask offers no options; answer with text.".to_owned()
                } else {
                    format!("option must be below {}.", ask.options.len())
                }));
            }
            if let Some(refusal) = answer_refusal(c, caller, &ask)? {
                return Err(WorkError::forbidden(refusal));
            }
            Ok(ask)
        })?;
        if ask.state != AskState::Open {
            let state = crate::codec::enum_text(&ask.state).unwrap_or_default();
            return Err(WorkError::conflict(format!("This ask is already {state}.")));
        }
        let answer = Answer {
            by: caller.member,
            option: answer.option,
            text: answer.text,
            at: self.now(),
        };
        self.append(&[self.by(
            caller,
            EventBody::AskAnswered {
                ask: ask.id,
                answer,
            },
        )])?;
        self.ask(&ask.id)
    }

    /// Puts a person's brief ("Where it stands") in force. People only.
    ///
    /// `edit.next` is accepted but **not stored**: `brief_accepted` has no field for it yet.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent; `not_found` for an unknown project or workstream.
    pub fn put_brief(
        &self,
        caller: &Caller,
        target: BriefTarget,
        edit: BriefEdit,
    ) -> Result<Brief> {
        require_person(caller, "Editing a brief")?;
        let _guard = self.lock();
        self.read(|c| {
            let found = match &target {
                BriefTarget::Project(id) => query::project(c, id)?.is_some(),
                BriefTarget::Workstream(id) => query::workstream(c, id)?.is_some(),
            };
            if found {
                Ok(())
            } else {
                Err(WorkError::not_found(match &target {
                    BriefTarget::Project(id) => format!("No project {id}."),
                    BriefTarget::Workstream(id) => format!("No workstream {id}."),
                }))
            }
        })?;
        self.append(&[self.by(
            caller,
            EventBody::BriefAccepted {
                target,
                text: edit.text,
                pinned: edit.pinned,
            },
        )])?;
        self.brief(&target)?
            .ok_or_else(|| WorkError::internal("the brief is missing after it was written"))
    }

    /// Changes a workstream's status or health. People only. Nothing is appended when neither
    /// changes.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent; `not_found` for an unknown workstream; `invalid` when the patch
    /// has neither a status nor a health.
    pub fn patch_workstream(
        &self,
        caller: &Caller,
        id: &WorkstreamId,
        patch: WorkstreamPatch,
    ) -> Result<Workstream> {
        require_person(caller, "Changing a workstream")?;
        if patch.status.is_none() && patch.health.is_none() {
            return Err(WorkError::invalid("Give a status, a health, or both."));
        }
        let _guard = self.lock();
        let current = self.workstream(id)?;
        let status = patch.status.unwrap_or(current.status);
        let health = patch.health.unwrap_or(current.health);
        if status == current.status && health == current.health {
            return Ok(current);
        }
        self.append(&[self.by(
            caller,
            EventBody::WorkstreamChanged {
                workstream: current.id,
                status,
                health,
            },
        )])?;
        self.workstream(id)
    }

    /// A dispatched session started working: its task moves to in progress, as the dispatch's
    /// agent (`task_moved` with mover `agent`, authored by the agent on behalf of its owner).
    /// For the runner or the hub's runner link to call; there is no route.
    ///
    /// The task stays where it is when it is already in progress, or when the rules do not let
    /// an agent move it (it is in review, done or canceled). The task is returned either way.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown dispatch or task; `conflict` when the dispatch has ended.
    pub fn dispatch_working(&self, id: &DispatchId) -> Result<Task> {
        let _guard = self.lock();
        let (dispatch, task, owner) = self.read(|c| {
            let dispatch = query::dispatch(c, id)?
                .ok_or_else(|| WorkError::not_found(format!("No dispatch {id}.")))?;
            let task = query::task(c, &TaskRef::Id(dispatch.task))?
                .ok_or_else(|| no_task(&TaskRef::Id(dispatch.task)))?;
            let owner = query::member(c, &dispatch.agent)?.and_then(|m| m.owner);
            Ok((dispatch, task, owner))
        })?;
        if dispatch.ended.is_some() {
            return Err(WorkError::conflict(format!("Dispatch {id} has ended.")));
        }
        // The dispatch is active, so the task is the agent's own.
        let mover = Mover::Agent { on_own_task: true };
        if !task.status.can_move(TaskStatus::InProgress, mover) {
            return Ok(task);
        }
        let body = EventBody::TaskMoved {
            task: task.id,
            from: task.status,
            to: TaskStatus::InProgress,
            mover,
        };
        self.append(&[self.event(dispatch.agent, owner, body)])?;
        self.reload_task(task.id)
    }

    /// Mirrors an agent's live plan (a `PlanUpdated` from its session) as the subtasks of the
    /// session's task: the agent's `agent_plan` lines are replaced, every other line is kept.
    /// For the runner or the hub's runner link to call; there is no route.
    ///
    /// Only the agent's **own** task is changed (it is the assignee or holds an active dispatch).
    /// Returns `None`, and changes nothing, when the session has no agent or no task, or the task
    /// is not the agent's. Nothing is appended when the plan is unchanged.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown session; database errors.
    pub fn mirror_plan(&self, session: &SessionId, items: &[PlanItem]) -> Result<Option<Task>> {
        let _guard = self.lock();
        let found = self.read(|c| {
            let session = query::session(c, session)?
                .ok_or_else(|| WorkError::not_found(format!("No session {session}.")))?;
            let (Some(agent), Some(task)) = (session.agent, session.task) else {
                return Ok(None);
            };
            let Some(task) = query::task(c, &TaskRef::Id(task))? else {
                return Ok(None);
            };
            if !query::is_own_task(c, &task, &agent)? {
                return Ok(None);
            }
            let owner = query::member(c, &agent)?.and_then(|m| m.owner);
            Ok(Some((agent, owner, task)))
        })?;
        let Some((agent, owner, task)) = found else {
            return Ok(None);
        };
        let plan = plan_lines(&task.subtasks, agent, items);
        let subtasks = splice_plan(&task.subtasks, agent, plan);
        if subtasks == task.subtasks {
            return Ok(Some(task));
        }
        let body = EventBody::SubtasksReplaced {
            task: task.id,
            subtasks,
        };
        self.append(&[self.event(agent, owner, body)])?;
        self.reload_task(task.id).map(Some)
    }
}

/// Why `caller` may not answer `ask`, or `None` if it may.
fn answer_refusal(conn: &Connection, caller: &Caller, ask: &Ask) -> Result<Option<String>> {
    let me = caller.member;
    if caller.scope == TokenScope::Agent {
        if ask.to != me {
            return Ok(Some(
                "An agent may only answer asks addressed to itself.".to_owned(),
            ));
        }
        if !matches!(ask.kind, AskKind::Question | AskKind::Mention) {
            let kind = crate::codec::enum_text(&ask.kind).unwrap_or_default();
            return Ok(Some(format!(
                "A {kind} must be answered with a device token."
            )));
        }
        return Ok(None);
    }
    if ask.to == me {
        return Ok(None);
    }
    let owner = query::member(conn, &ask.to)?.and_then(|m| m.owner);
    Ok((owner != Some(me))
        .then(|| "A person may only answer asks addressed to them or to their agents.".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, source: SubtaskSource) -> Subtask {
        Subtask {
            id: SubtaskId::new(),
            text: text.to_owned(),
            done: false,
            source,
        }
    }

    #[test]
    fn an_agent_plan_replaces_only_its_own_lines_in_place() {
        let me = MemberId::new();
        let other = MemberId::new();
        let mine = SubtaskSource::AgentPlan { agent: me };
        let existing = vec![
            line("human first", SubtaskSource::Human),
            line("old 1", mine),
            line("other agent", SubtaskSource::AgentPlan { agent: other }),
            line("old 2", mine),
            line("human last", SubtaskSource::Human),
        ];
        let plan = vec![
            line("new 1", mine),
            line("new 2", mine),
            line("new 3", mine),
        ];
        let out = splice_plan(&existing, me, plan);
        let texts: Vec<_> = out.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "human first",
                "new 1",
                "new 2",
                "new 3",
                "other agent",
                "human last"
            ]
        );
        // With no earlier plan, the plan goes last.
        let out = splice_plan(&existing[..1], me, vec![line("new", mine)]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].text, "new");
    }

    #[test]
    fn plan_lines_keep_ids_by_text() {
        let me = MemberId::new();
        let mine = SubtaskSource::AgentPlan { agent: me };
        let existing = vec![line("Read", mine), line("Write", mine)];
        let items = [
            PlanItem {
                text: "Write".into(),
                status: PlanStatus::Completed,
            },
            PlanItem {
                text: "Test".into(),
                status: PlanStatus::Pending,
            },
            PlanItem {
                text: "  ".into(),
                status: PlanStatus::Pending,
            },
        ];
        let lines = plan_lines(&existing, me, &items);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].id, existing[1].id);
        assert!(lines[0].done);
        assert_ne!(lines[1].id, existing[0].id);
        assert!(!lines[1].done);
    }
}
