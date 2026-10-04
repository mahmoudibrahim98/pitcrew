//! Projections: the work model's tables, derived from the event log.
//!
//! Each projection owns its tables (migrations `0200`–`0210`) and follows the store's rules, so
//! applying events one append at a time and rebuilding from the log give identical tables:
//! - `apply` has no side effects: it never appends events and never reads the clock; times come
//!   from the event (`at`), and authorship from `author` and `on_behalf_of`;
//! - `apply` reads and writes only its own tables, never another projection's, and there are no
//!   foreign keys between projections;
//! - `apply` never fails on what an event says, only on a database error: an event about
//!   something the projection does not know (a move of an unknown task) changes nothing, and an
//!   event that contradicts the tables (a task key already taken, a move from a status the task
//!   has left) is resolved deterministically (see [`Tasks`]). One odd event, or a second writer,
//!   never stalls the log;
//! - a list is returned in the order its rows were first created (`rev`), and a later event that
//!   re-states a row (a second `task_created`, `ask_raised` or `session_discovered` with the same
//!   id) updates it in place and keeps that order.
//!
//! Bump a projection's `VERSION` whenever its `apply` changes meaning or a migration reshapes its
//! tables; the store then rebuilds it on the next open.

mod asks;
mod briefs;
mod comments;
mod cursors;
mod directory;
mod projects;
mod refs;
mod sessions;
mod tasks;

pub use asks::Asks;
pub use briefs::Briefs;
pub(crate) use briefs::target_columns;
pub use comments::Comments;
pub use cursors::Cursors;
pub use directory::Directory;
pub use projects::Projects;
pub use refs::Refs;
pub use sessions::Sessions;
pub use tasks::Tasks;

use pitcrew_store::sql::{Params, Transaction};
use pitcrew_store::{BoxError, Projection};

/// Every projection of the work model. Pass them to `Store::open_with`.
#[must_use]
pub fn projections() -> Vec<Box<dyn Projection>> {
    vec![
        Box::new(Directory),
        Box::new(Projects),
        Box::new(Tasks),
        Box::new(Sessions),
        Box::new(Asks),
        Box::new(Comments),
        Box::new(Briefs),
        Box::new(Refs),
        Box::new(Cursors),
        Box::new(crate::safety::Safety),
    ]
}

/// The names of [`projections`], e.g. for `Store::rebuild`.
pub const NAMES: [&str; 10] = [
    Directory::NAME,
    Projects::NAME,
    Tasks::NAME,
    Sessions::NAME,
    Asks::NAME,
    Comments::NAME,
    Briefs::NAME,
    Refs::NAME,
    Cursors::NAME,
    crate::safety::Safety::NAME,
];

/// Every table the work model owns, children before parents (the order `reset` clears them in).
pub const TABLES: [&str; 25] = [
    "work_safety",
    "work_read_cursors",
    "work_team_members",
    "work_teams",
    "work_personas",
    "work_members",
    "work_machines",
    "work_project_members",
    "work_projects",
    "work_locations",
    "work_workstreams",
    "work_subtasks",
    "work_task_deps",
    "work_task_labels",
    "work_tasks",
    "work_task_clashes",
    "work_dispatches",
    "work_sessions",
    "work_asks",
    "work_comment_mentions",
    "work_comments",
    "work_brief_proposals",
    "work_briefs",
    "work_event_refs",
    "work_ref_parents",
];

type Applied = Result<(), BoxError>;

/// Runs one cached statement.
fn exec(tx: &Transaction<'_>, sql: &str, params: impl Params) -> Result<usize, BoxError> {
    Ok(tx.prepare_cached(sql)?.execute(params)?)
}

/// Deletes every row of `tables`, in order.
fn clear(tx: &Transaction<'_>, tables: &[&str]) -> Applied {
    for table in tables {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
    }
    Ok(())
}
