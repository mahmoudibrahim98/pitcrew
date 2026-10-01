//! `work.projects`: projects and workstreams, with workstream locations.

use super::{Applied, clear, exec};
use crate::codec::{IdText, enum_text, json, opt_json, sql_rev};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::model::{Project, Workstream};
use pitcrew_store::sql::{Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Projects and workstreams.
#[derive(Debug, Clone, Copy, Default)]
pub struct Projects;

impl Projects {
    /// The projection's name.
    pub const NAME: &'static str = "work.projects";
    const VERSION: u32 = 1;
}

impl Projection for Projects {
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
                "work_project_members",
                "work_projects",
                "work_locations",
                "work_workstreams",
            ],
        )
    }

    fn apply(&self, tx: &Transaction<'_>, stored: &StoredEvent) -> Result<(), BoxError> {
        let rev = sql_rev(stored.rev);
        match &stored.event.body {
            EventBody::ProjectCreated { project } => project_created(tx, rev, project),
            EventBody::WorkstreamCreated { workstream } => workstream_created(tx, rev, workstream),
            EventBody::WorkstreamChanged {
                workstream,
                status,
                health,
            } => {
                exec(
                    tx,
                    "UPDATE work_workstreams SET status = ?2, health = ?3 WHERE id = ?1",
                    params![workstream.text(), enum_text(status)?, enum_text(health)?],
                )?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn project_created(tx: &Transaction<'_>, rev: i64, p: &Project) -> Applied {
    let id = p.id.text();
    exec(
        tx,
        "INSERT INTO work_projects (id, rev, key, name, status, lead, start, due, root, external)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (id) DO UPDATE SET key = excluded.key, name = excluded.name,
           status = excluded.status, lead = excluded.lead, start = excluded.start,
           due = excluded.due, root = excluded.root, external = excluded.external",
        params![
            id,
            rev,
            p.key.as_str(),
            p.name,
            enum_text(&p.status)?,
            p.lead.text(),
            p.start.as_ref().map(|d| d.0.as_str()),
            p.due.as_ref().map(|d| d.0.as_str()),
            opt_json(p.root.as_ref())?,
            json(&p.external)?,
        ],
    )?;
    exec(
        tx,
        "DELETE FROM work_project_members WHERE project = ?1",
        params![id],
    )?;
    for (position, member) in p.members.iter().enumerate() {
        exec(
            tx,
            "INSERT INTO work_project_members (project, position, member) VALUES (?1, ?2, ?3)",
            params![id, i64::try_from(position)?, member.text()],
        )?;
    }
    Ok(())
}

fn workstream_created(tx: &Transaction<'_>, rev: i64, w: &Workstream) -> Applied {
    let id = w.id.text();
    exec(
        tx,
        "INSERT INTO work_workstreams (id, rev, project, name, status, health, external)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (id) DO UPDATE SET project = excluded.project, name = excluded.name,
           status = excluded.status, health = excluded.health, external = excluded.external",
        params![
            id,
            rev,
            w.project.text(),
            w.name,
            enum_text(&w.status)?,
            enum_text(&w.health)?,
            json(&w.external)?,
        ],
    )?;
    exec(
        tx,
        "DELETE FROM work_locations WHERE workstream = ?1",
        params![id],
    )?;
    for (position, l) in w.locations.iter().enumerate() {
        exec(
            tx,
            "INSERT INTO work_locations (workstream, position, machine, path, branch)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                i64::try_from(position)?,
                l.machine.text(),
                l.path,
                l.branch
            ],
        )?;
    }
    Ok(())
}
