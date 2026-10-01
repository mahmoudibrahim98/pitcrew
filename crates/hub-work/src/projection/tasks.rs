//! `work.tasks`: tasks, subtasks, dependencies and labels.
//!
//! Each task row holds the whole task as JSON (`doc`, the API's shape) next to the columns lists
//! filter on; subtasks, dependencies and labels are also indexed one per row. Every change
//! rewrites the document and the columns together, from the document as it was.

use super::{Applied, clear, exec};
use crate::codec::{IdText, enum_text, json, opt_text, sql_rev};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::TaskId;
use pitcrew_protocol::model::{SubtaskSource, Task};
use pitcrew_store::sql::{OptionalExtension, Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Tasks with their subtasks, dependencies and labels.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tasks;

impl Tasks {
    /// The projection's name.
    pub const NAME: &'static str = "work.tasks";
    /// Bump with any change to `apply`, to the tables, or to the shape of `Task` (the documents
    /// are serialized protocol values).
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
        match &stored.event.body {
            EventBody::TaskCreated { task } => save(tx, sql_rev(stored.rev), task),
            EventBody::TaskMoved { task, to, .. } => change(tx, task, |t| t.status = *to),
            EventBody::TaskAssigned { task, assignee } => {
                change(tx, task, |t| t.assignee = *assignee)
            }
            EventBody::SubtasksReplaced { task, subtasks } => {
                change(tx, task, |t| t.subtasks.clone_from(subtasks))
            }
            _ => Ok(()),
        }
    }
}

/// Applies `f` to a known task and saves it; a task the hub never saw is left alone.
fn change(tx: &Transaction<'_>, id: &TaskId, f: impl FnOnce(&mut Task)) -> Applied {
    let found: Option<(i64, String)> = tx
        .prepare_cached("SELECT rev, doc FROM work_tasks WHERE id = ?1")?
        .query_row(params![id.text()], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((rev, doc)) = found else {
        return Ok(());
    };
    let mut task: Task = serde_json::from_str(&doc)?;
    f(&mut task);
    save(tx, rev, &task)
}

/// Writes a task's row and its child rows. `rev` is the revision that first created it.
fn save(tx: &Transaction<'_>, rev: i64, t: &Task) -> Applied {
    let id = t.id.text();
    exec(
        tx,
        "INSERT INTO work_tasks (id, rev, project, key_prefix, number, workstream, status,
           assignee, doc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (id) DO UPDATE SET project = excluded.project,
           key_prefix = excluded.key_prefix, number = excluded.number,
           workstream = excluded.workstream, status = excluded.status,
           assignee = excluded.assignee, doc = excluded.doc",
        params![
            id,
            rev,
            t.project.text(),
            t.key.project.as_str(),
            t.key.number,
            opt_text(t.workstream.as_ref()),
            enum_text(&t.status)?,
            opt_text(t.assignee.as_ref()),
            json(t)?,
        ],
    )?;
    for table in ["work_subtasks", "work_task_deps", "work_task_labels"] {
        exec(
            tx,
            &format!("DELETE FROM {table} WHERE task = ?1"),
            params![id],
        )?;
    }
    for (position, s) in t.subtasks.iter().enumerate() {
        let agent = match &s.source {
            SubtaskSource::Human => None,
            SubtaskSource::AgentPlan { agent } => Some(agent.text()),
        };
        exec(
            tx,
            "INSERT INTO work_subtasks (task, position, id, text, done, agent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                i64::try_from(position)?,
                s.id.text(),
                s.text,
                s.done,
                agent
            ],
        )?;
    }
    for (position, blocker) in t.blocked_by.iter().enumerate() {
        exec(
            tx,
            "INSERT INTO work_task_deps (task, position, blocked_by) VALUES (?1, ?2, ?3)",
            params![id, i64::try_from(position)?, blocker.text()],
        )?;
    }
    for (position, label) in t.labels.iter().enumerate() {
        exec(
            tx,
            "INSERT INTO work_task_labels (task, position, label) VALUES (?1, ?2, ?3)",
            params![id, i64::try_from(position)?, label],
        )?;
    }
    Ok(())
}
