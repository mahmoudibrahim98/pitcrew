//! `work.refs`: the activity reference index. For every event, which project, workstream, task
//! and session it is about, so activity can be filtered by any of them
//! ([`crate::EventRefs`]).
//!
//! An event names some of them itself; the rest are filled in from what the projection knew when
//! it applied the event:
//! - a session event is about the session's task and workstream as linked **then** (a link made
//!   later does not reach back);
//! - a task event is about the task's workstream and project, a workstream event about its
//!   project; a `task_updated` that moves the task to another workstream is about the new one, and
//!   so is every later event about the task;
//! - `dispatch_finished` and `ask_answered` are about the task and session of their dispatch or
//!   ask;
//! - `write_proposed`, `write_started` and `write_finished` are about the task they name.
//!
//! The parents come from this projection's own `work_ref_parents`, never from the other work
//! tables (a projection reads only its own), and follow the same rules: a session keeps a firm
//! link (see [`super::Sessions`]), and a `task_created` refused for a key clash (see
//! [`super::Tasks`]) changes no task's parents; to tell, the index also keeps which task holds
//! each key (rows of kind `task_key`). Events about none of them (machines, members, personas,
//! teams) get no row.

use super::sessions::replaces_link;
use super::{Applied, clear, exec};
use crate::codec::{IdText, enum_text, opt_enum_col, sql_rev};
use pitcrew_protocol::events::{BriefTarget, EventBody};
use pitcrew_protocol::model::LinkBasis;
use pitcrew_store::sql::{OptionalExtension, Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// The activity reference index.
#[derive(Debug, Clone, Copy, Default)]
pub struct Refs;

impl Refs {
    /// The projection's name.
    pub const NAME: &'static str = "work.refs";
    const VERSION: u32 = 3;
}

/// What an event is about, as bare ULIDs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct About {
    project: Option<String>,
    workstream: Option<String>,
    task: Option<String>,
    session: Option<String>,
}

impl About {
    fn is_empty(&self) -> bool {
        self.project.is_none()
            && self.workstream.is_none()
            && self.task.is_none()
            && self.session.is_none()
    }
}

/// A row of `work_ref_parents`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Parents {
    project: Option<String>,
    workstream: Option<String>,
    task: Option<String>,
    session: Option<String>,
    link_basis: Option<LinkBasis>,
}

const WORKSTREAM: &str = "workstream";
const TASK: &str = "task";
/// Which task holds a key: `id` is the key (`PAP-4`), `task` the task. Kept to refuse clashing
/// `task_created` events as `work.tasks` does, without reading its tables.
const TASK_KEY: &str = "task_key";
const SESSION: &str = "session";
const DISPATCH: &str = "dispatch";
const ASK: &str = "ask";

fn parents(tx: &Transaction<'_>, kind: &str, id: &str) -> Result<Parents, BoxError> {
    Ok(tx
        .prepare_cached(
            "SELECT project, workstream, task, session, link_basis FROM work_ref_parents
             WHERE kind = ?1 AND id = ?2",
        )?
        .query_row(params![kind, id], |r| {
            Ok(Parents {
                project: r.get(0)?,
                workstream: r.get(1)?,
                task: r.get(2)?,
                session: r.get(3)?,
                link_basis: opt_enum_col(r, 4)?,
            })
        })
        .optional()?
        .unwrap_or_default())
}

fn remember(tx: &Transaction<'_>, kind: &str, id: &str, p: &Parents) -> Applied {
    exec(
        tx,
        "INSERT INTO work_ref_parents (kind, id, project, workstream, task, session, link_basis)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (kind, id) DO UPDATE SET project = excluded.project,
           workstream = excluded.workstream, task = excluded.task, session = excluded.session,
           link_basis = excluded.link_basis",
        params![
            kind,
            id,
            p.project,
            p.workstream,
            p.task,
            p.session,
            p.link_basis.as_ref().map(enum_text).transpose()?,
        ],
    )?;
    Ok(())
}

/// Records a session's link, unless it would replace a firm link with a weaker one. Returns the
/// link the session has afterwards.
fn link_session(
    tx: &Transaction<'_>,
    session: &str,
    incoming: Parents,
) -> Result<Parents, BoxError> {
    let existing = parents(tx, SESSION, session)?;
    if replaces_link(existing.link_basis, incoming.link_basis) {
        remember(tx, SESSION, session, &incoming)?;
        Ok(incoming)
    } else {
        Ok(existing)
    }
}

impl Projection for Refs {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn version(&self) -> u32 {
        Self::VERSION
    }

    fn reset(&self, tx: &Transaction<'_>) -> Result<(), BoxError> {
        clear(tx, &["work_event_refs", "work_ref_parents"])
    }

    fn apply(&self, tx: &Transaction<'_>, stored: &StoredEvent) -> Result<(), BoxError> {
        let about = direct(tx, &stored.event.body)?;
        if about.is_empty() {
            return Ok(());
        }
        let about = complete(tx, about)?;
        exec(
            tx,
            "INSERT INTO work_event_refs (rev, project, workstream, task, session)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                sql_rev(stored.rev),
                about.project,
                about.workstream,
                about.task,
                about.session
            ],
        )?;
        Ok(())
    }
}

/// What the event names itself, after recording what it says about parents.
fn direct(tx: &Transaction<'_>, body: &EventBody) -> Result<About, BoxError> {
    let mut about = About::default();
    match body {
        EventBody::ProjectCreated { project } => about.project = Some(project.id.text()),
        EventBody::WorkstreamCreated { workstream } => {
            let id = workstream.id.text();
            let p = Parents {
                project: Some(workstream.project.text()),
                ..Parents::default()
            };
            remember(tx, WORKSTREAM, &id, &p)?;
            about.workstream = Some(id);
        }
        EventBody::WorkstreamChanged { workstream, .. }
        | EventBody::WorkstreamLinked { workstream, .. } => {
            about.workstream = Some(workstream.text());
        }
        EventBody::TaskCreated { task } => {
            let id = task.id.text();
            let key = task.key.to_string();
            // `work.tasks` refuses a `task_created` whose key another task holds (a clash); so
            // does the index. The refused event is still about the task it names, but changes
            // no task's parents.
            let holder = parents(tx, TASK_KEY, &key)?.task;
            if holder.as_ref().is_none_or(|h| *h == id) {
                if parents(tx, TASK, &id)?.project.is_some() {
                    // A re-stated task may come with another key, freeing its old one.
                    exec(
                        tx,
                        "DELETE FROM work_ref_parents WHERE kind = ?1 AND task = ?2 AND id <> ?3",
                        params![TASK_KEY, id, key],
                    )?;
                }
                let p = Parents {
                    project: Some(task.project.text()),
                    workstream: task.workstream.as_ref().map(IdText::text),
                    ..Parents::default()
                };
                remember(tx, TASK, &id, &p)?;
                let holds = Parents {
                    task: Some(id.clone()),
                    ..Parents::default()
                };
                remember(tx, TASK_KEY, &key, &holds)?;
            }
            about.task = Some(id);
        }
        EventBody::TaskUpdated { task, patch } => {
            let id = task.text();
            if let Some(workstream) = &patch.workstream {
                let mut p = parents(tx, TASK, &id)?;
                // Every task the index knows has a project; an unknown task gets no parents.
                if p.project.is_some() {
                    p.workstream = workstream.as_ref().map(IdText::text);
                    remember(tx, TASK, &id, &p)?;
                }
            }
            about.task = Some(id);
        }
        EventBody::TaskMoved { task, .. }
        | EventBody::TaskAssigned { task, .. }
        | EventBody::SubtasksReplaced { task, .. } => about.task = Some(task.text()),
        EventBody::SessionDiscovered { session } => {
            let id = session.id.text();
            let link = link_session(
                tx,
                &id,
                Parents {
                    workstream: session.workstream.as_ref().map(IdText::text),
                    task: session.task.as_ref().map(IdText::text),
                    link_basis: session.link_basis,
                    ..Parents::default()
                },
            )?;
            about.workstream = link.workstream;
            about.task = link.task;
            about.session = Some(id);
        }
        EventBody::SessionLinked {
            session,
            workstream,
            task,
            basis,
        } => {
            let id = session.text();
            let link = link_session(
                tx,
                &id,
                Parents {
                    workstream: workstream.as_ref().map(IdText::text),
                    task: task.as_ref().map(IdText::text),
                    link_basis: Some(*basis),
                    ..Parents::default()
                },
            )?;
            about.workstream = link.workstream;
            about.task = link.task;
            about.session = Some(id);
        }
        EventBody::SessionStateChanged { session, .. }
        | EventBody::TurnEnded { session, .. }
        | EventBody::ToolRan { session, .. }
        | EventBody::FileEdited { session, .. }
        | EventBody::SessionUpdated { session, .. }
        | EventBody::SessionEnded { session } => about.session = Some(session.text()),
        EventBody::DispatchStarted { dispatch } => {
            let p = Parents {
                task: Some(dispatch.task.text()),
                session: dispatch.session.as_ref().map(IdText::text),
                ..Parents::default()
            };
            remember(tx, DISPATCH, &dispatch.id.text(), &p)?;
            about.task = p.task;
            about.session = p.session;
        }
        EventBody::DispatchFinished { dispatch, .. } => {
            let p = parents(tx, DISPATCH, &dispatch.text())?;
            about.task = p.task;
            about.session = p.session;
        }
        EventBody::AskRaised { ask } => {
            let p = Parents {
                task: ask.task.as_ref().map(IdText::text),
                session: ask.session.as_ref().map(IdText::text),
                ..Parents::default()
            };
            remember(tx, ASK, &ask.id.text(), &p)?;
            about.task = p.task;
            about.session = p.session;
        }
        EventBody::AskAnswered { ask, .. } => {
            let p = parents(tx, ASK, &ask.text())?;
            about.task = p.task;
            about.session = p.session;
        }
        EventBody::CommentPosted {
            task, workstream, ..
        } => {
            about.task = task.as_ref().map(IdText::text);
            about.workstream = workstream.as_ref().map(IdText::text);
        }
        EventBody::BriefProposed { target, .. } | EventBody::BriefAccepted { target, .. } => {
            match target {
                BriefTarget::Project(id) => about.project = Some(id.text()),
                BriefTarget::Workstream(id) => about.workstream = Some(id.text()),
            }
        }
        EventBody::DecisionRecorded { workstream, .. } => {
            about.workstream = workstream.as_ref().map(IdText::text);
        }
        EventBody::WriteProposed { write } => about.task = write.task.as_ref().map(IdText::text),
        EventBody::WriteStarted { task, .. }
        | EventBody::WriteRetryRequested { task, .. }
        | EventBody::WriteFinished { task, .. } => {
            about.task = task.as_ref().map(IdText::text);
        }
        // Machines, members, personas and teams belong to the workspace, not to any work.
        _ => {}
    }
    Ok(about)
}

/// Fills in what the event's session, task and workstream belong to:
/// task ← the session's link; workstream ← the task's, else the session's; project ← the
/// workstream's, else the task's.
fn complete(tx: &Transaction<'_>, mut about: About) -> Result<About, BoxError> {
    let session = match &about.session {
        Some(id) => parents(tx, SESSION, id)?,
        None => Parents::default(),
    };
    if about.task.is_none() {
        about.task.clone_from(&session.task);
    }
    let task = match &about.task {
        Some(id) => parents(tx, TASK, id)?,
        None => Parents::default(),
    };
    if about.workstream.is_none() {
        about.workstream = task.workstream.or(session.workstream);
    }
    if about.project.is_none() {
        let workstream = match &about.workstream {
            Some(id) => parents(tx, WORKSTREAM, id)?,
            None => Parents::default(),
        };
        about.project = workstream.project.or(task.project);
    }
    Ok(about)
}
