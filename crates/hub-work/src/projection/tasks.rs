//! `work.tasks`: tasks, subtasks, dependencies and labels.

use super::{Applied, clear, exec};
use crate::codec::{IdText, enum_text, opt_json, opt_text, sql_rev};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::TaskId;
use pitcrew_protocol::model::{Subtask, SubtaskSource, Task};
use pitcrew_store::sql::{OptionalExtension, Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Tasks with their subtasks, dependencies and labels.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tasks;

impl Tasks {
    /// The projection's name.
    pub const NAME: &'static str = "work.tasks";
    const VERSION: u32 = 1;
}

impl Projection for Tasks {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn version(&self) -> u32 {
        Self::VERSION
    }

    fn reset(&self, tx: &Transaction<'_>) -> Result<(), BoxError> {
        clear(
            tx,
            &[
                "work_subtasks",
                "work_task_deps",
                "work_task_labels",
                "work_tasks",
            ],
        )
    }

    fn apply(&self, tx: &Transaction<'_>, stored: &StoredEvent) -> Result<(), BoxError> {
        let rev = sql_rev(stored.rev);
        match &stored.event.body {
            EventBody::TaskCreated { task } => task_created(tx, rev, task),
            EventBody::TaskMoved { task, to, .. } => {
                exec(
                    tx,
                    "UPDATE work_tasks SET status = ?2 WHERE id = ?1",
                    params![task.text(), enum_text(to)?],
                )?;
                Ok(())
            }
            EventBody::TaskAssigned { task, assignee } => {
                exec(
                    tx,
                    "UPDATE work_tasks SET assignee = ?2 WHERE id = ?1",
                    params![task.text(), opt_text(assignee.as_ref())],
                )?;
                Ok(())
            }
            EventBody::SubtasksReplaced { task, subtasks } => {
                if exists(tx, task)? {
                    replace_subtasks(tx, &task.text(), subtasks)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn exists(tx: &Transaction<'_>, task: &TaskId) -> Result<bool, BoxError> {
    Ok(tx
        .prepare_cached("SELECT 1 FROM work_tasks WHERE id = ?1")?
        .query_row(params![task.text()], |_| Ok(()))
        .optional()?
        .is_some())
}

fn task_created(tx: &Transaction<'_>, rev: i64, t: &Task) -> Applied {
    let id = t.id.text();
    exec(
        tx,
        "INSERT INTO work_tasks (id, rev, project, key_prefix, number, workstream, title,
           description, status, priority, assignee, start, due, source, accept_auto)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT (id) DO UPDATE SET project = excluded.project,
           key_prefix = excluded.key_prefix, number = excluded.number,
           workstream = excluded.workstream, title = excluded.title,
           description = excluded.description, status = excluded.status,
           priority = excluded.priority, assignee = excluded.assignee, start = excluded.start,
           due = excluded.due, source = excluded.source, accept_auto = excluded.accept_auto",
        params![
            id,
            rev,
            t.project.text(),
            t.key.project.as_str(),
            t.key.number,
            opt_text(t.workstream.as_ref()),
            t.title,
            t.description,
            enum_text(&t.status)?,
            enum_text(&t.priority)?,
            opt_text(t.assignee.as_ref()),
            t.start.as_ref().map(|d| d.0.as_str()),
            t.due.as_ref().map(|d| d.0.as_str()),
            opt_json(t.source.as_ref())?,
            t.accept_auto,
        ],
    )?;
    replace_subtasks(tx, &id, &t.subtasks)?;
    exec(
        tx,
        "DELETE FROM work_task_deps WHERE task = ?1",
        params![id],
    )?;
    for (position, blocker) in t.blocked_by.iter().enumerate() {
        exec(
            tx,
            "INSERT INTO work_task_deps (task, position, blocked_by) VALUES (?1, ?2, ?3)",
            params![id, i64::try_from(position)?, blocker.text()],
        )?;
    }
    exec(
        tx,
        "DELETE FROM work_task_labels WHERE task = ?1",
        params![id],
    )?;
    for (position, label) in t.labels.iter().enumerate() {
        exec(
            tx,
            "INSERT INTO work_task_labels (task, position, label) VALUES (?1, ?2, ?3)",
            params![id, i64::try_from(position)?, label],
        )?;
    }
    Ok(())
}

fn replace_subtasks(tx: &Transaction<'_>, task: &str, subtasks: &[Subtask]) -> Applied {
    exec(
        tx,
        "DELETE FROM work_subtasks WHERE task = ?1",
        params![task],
    )?;
    for (position, s) in subtasks.iter().enumerate() {
        let agent = match &s.source {
            SubtaskSource::Human => None,
            SubtaskSource::AgentPlan { agent } => Some(agent.text()),
        };
        exec(
            tx,
            "INSERT INTO work_subtasks (task, position, id, text, done, agent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                task,
                i64::try_from(position)?,
                s.id.text(),
                s.text,
                s.done,
                agent
            ],
        )?;
    }
    Ok(())
}
