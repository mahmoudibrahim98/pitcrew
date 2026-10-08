//! `work.directory`: machines, members, personas and teams.

use super::{Applied, clear, exec};
use crate::codec::{IdText, enum_text, opt_json, opt_text, sql_rev};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::model::{Machine, Member, Persona, Team};
use pitcrew_store::sql::{Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Machines, members, personas and teams.
#[derive(Debug, Clone, Copy, Default)]
pub struct Directory;

impl Directory {
    /// The projection's name.
    pub const NAME: &'static str = "work.directory";
    const VERSION: u32 = 2;
}

impl Projection for Directory {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn version(&self) -> u32 {
        Self::VERSION
    }

    fn reset(&self, tx: &Transaction<'_>) -> Result<(), BoxError> {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS work_avatars (member TEXT PRIMARY KEY, avatar TEXT NOT NULL); DELETE FROM work_avatars;")?;
        clear(
            tx,
            &[
                "work_team_members",
                "work_teams",
                "work_personas",
                "work_members",
                "work_machines",
            ],
        )
    }

    fn apply(&self, tx: &Transaction<'_>, stored: &StoredEvent) -> Result<(), BoxError> {
        let rev = sql_rev(stored.rev);
        match &stored.event.body {
            EventBody::MachineAdded { machine } => machine_added(tx, rev, machine),
            EventBody::MachineLiveness { machine, liveness } => {
                exec(
                    tx,
                    "UPDATE work_machines SET liveness = ?2 WHERE id = ?1",
                    params![machine.text(), enum_text(liveness)?],
                )?;
                Ok(())
            }
            EventBody::MemberAdded { member } => member_added(tx, rev, member),
            EventBody::PersonaSaved { persona } => persona_saved(tx, rev, persona),
            EventBody::TeamSaved { team } => team_saved(tx, rev, team),
            _ => Ok(()),
        }
    }
}

fn machine_added(tx: &Transaction<'_>, rev: i64, m: &Machine) -> Applied {
    exec(
        tx,
        "INSERT INTO work_machines (id, rev, name, kind, info, liveness)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (id) DO UPDATE SET name = excluded.name, kind = excluded.kind,
           info = excluded.info, liveness = excluded.liveness",
        params![
            m.id.text(),
            rev,
            m.name,
            enum_text(&m.kind)?,
            opt_json(m.info.as_ref())?,
            enum_text(&m.liveness)?,
        ],
    )?;
    Ok(())
}

fn member_added(tx: &Transaction<'_>, rev: i64, m: &Member) -> Applied {
    if let Some(avatar) = &m.avatar {
        exec(
            tx,
            "INSERT INTO work_avatars VALUES (?1, ?2) ON CONFLICT(member) DO UPDATE SET avatar=excluded.avatar",
            params![m.id.text(), serde_json::to_string(avatar)?],
        )?;
    } else {
        exec(
            tx,
            "DELETE FROM work_avatars WHERE member=?1",
            params![m.id.text()],
        )?;
    }
    exec(
        tx,
        "INSERT INTO work_members (id, rev, kind, handle, name, owner, persona)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (id) DO UPDATE SET kind = excluded.kind, handle = excluded.handle,
           name = excluded.name, owner = excluded.owner, persona = excluded.persona",
        params![
            m.id.text(),
            rev,
            enum_text(&m.kind)?,
            m.handle,
            m.name,
            opt_text(m.owner.as_ref()),
            opt_text(m.persona.as_ref()),
        ],
    )?;
    Ok(())
}

fn persona_saved(tx: &Transaction<'_>, rev: i64, p: &Persona) -> Applied {
    exec(
        tx,
        "INSERT INTO work_personas (id, rev, name, engine, model, instructions, permission_mode)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (id) DO UPDATE SET name = excluded.name, engine = excluded.engine,
           model = excluded.model, instructions = excluded.instructions,
           permission_mode = excluded.permission_mode",
        params![
            p.id.text(),
            rev,
            p.name,
            enum_text(&p.engine)?,
            p.model,
            p.instructions,
            enum_text(&p.permission_mode)?,
        ],
    )?;
    Ok(())
}

fn team_saved(tx: &Transaction<'_>, rev: i64, t: &Team) -> Applied {
    let id = t.id.text();
    exec(
        tx,
        "INSERT INTO work_teams (id, rev, name, lead) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (id) DO UPDATE SET name = excluded.name, lead = excluded.lead",
        params![id, rev, t.name, t.lead.text()],
    )?;
    exec(
        tx,
        "DELETE FROM work_team_members WHERE team = ?1",
        params![id],
    )?;
    for (position, member) in t.members.iter().enumerate() {
        exec(
            tx,
            "INSERT INTO work_team_members (team, position, member) VALUES (?1, ?2, ?3)",
            params![id, i64::try_from(position)?, member.text()],
        )?;
    }
    Ok(())
}
