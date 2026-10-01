//! `work.tasks`: tasks, subtasks, dependencies and labels.
//!
//! Each task row holds the whole task as JSON (`doc`, the API's shape) next to the columns lists
//! filter on; subtasks, dependencies and labels are also indexed one per row. Every change
//! rewrites the document and the columns together, from the document as it was.
//!
//! Two rules keep `apply` total and deterministic when a second writer races the service (see
//! "One writer" in the crate docs), so the log never stalls on one of them:
//! - **Keys are never shared.** A `task_created` whose key another task already holds is not
//!   applied: the first task keeps the key, and the refused event is recorded in
//!   `work_task_clashes`. The same goes for a re-stated task that would take another's key.
//! - **Moves name where they start.** A `task_moved` whose `from` is not the task's status now
//!   lost a race with another move, and is ignored.
//!
//! A `task_updated` writes its patch into the task as it is (`TaskPatch::apply`): the command that
//! appended it checked the rules, and `apply` never second-guesses an event.

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
    /// are serialized protocol values; `tests/task_shape.rs` pins the shape to this number).
    ///
    /// 2: key clashes are recorded instead of failing, and stale moves are ignored.
    /// 3: `task_updated` is applied.
    pub const VERSION: u32 = 3;
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
                "work_task_clashes",
            ],
        )
    }

    fn apply(&self, tx: &Transaction<'_>, stored: &StoredEvent) -> Result<(), BoxError> {
        match &stored.event.body {
            EventBody::TaskCreated { task } => created(tx, sql_rev(stored.rev), task),
            EventBody::TaskMoved { task, from, to, .. } => change(tx, task, |t| {
                if t.status == *from {
                    t.status = *to;
                }
            }),
            EventBody::TaskAssigned { task, assignee } => {
                change(tx, task, |t| t.assignee = *assignee)
            }
            EventBody::TaskUpdated { task, patch } => change(tx, task, |t| patch.apply(t)),
            EventBody::SubtasksReplaced { task, subtasks } => {
                change(tx, task, |t| t.subtasks.clone_from(subtasks))
            }
            _ => Ok(()),
        }
    }
}

/// A new task, or a re-stated one; refused (and recorded) if another task holds its key.
fn created(tx: &Transaction<'_>, rev: i64, task: &Task) -> Applied {
    let id = task.id.text();
    let holder: Option<String> = tx
        .prepare_cached(
            "SELECT id FROM work_tasks WHERE key_prefix = ?1 AND number = ?2 AND id <> ?3",
        )?
        .query_row(
            params![task.key.project.as_str(), task.key.number, id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(holder) = holder {
        exec(
            tx,
            "INSERT INTO work_task_clashes (rev, task, key_prefix, number, holder)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![rev, id, task.key.project.as_str(), task.key.number, holder],
        )?;
        return Ok(());
    }
    save(tx, rev, task)
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
    let before = task.clone();
    f(&mut task);
    if task == before {
        return Ok(());
    }
    save(tx, rev, &task)
}

/// Writes a task's row and its child rows. `rev` is the revision that first created it (an
/// update keeps the stored one).
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
