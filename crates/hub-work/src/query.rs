//! Reads of the work model's tables. Every function takes a connection from `Store::read`, so a
//! caller can combine several in one consistent snapshot.

use crate::codec::{
    IdText, col, enum_col, enum_text, json_col, opt_col, opt_enum_col, opt_json_col, sql_rev,
};
use crate::error::{Result, WorkError};
use crate::projection::{NAMES, target_columns};
use pitcrew_protocol::ids::{
    AskId, DispatchId, MachineId, MemberId, PersonaId, ProjectId, ProjectKey, SessionId, TaskId,
    TaskKey, TeamId, WorkstreamId,
};
use pitcrew_protocol::model::{
    Ask, AskState, Brief, BriefProposal, BriefTarget, Date, Dispatch, Location, Machine, Member,
    Persona, Project, Session, SessionState, Task, TaskStatus, Team, Workstream,
};
use pitcrew_store::sql::types::{Type, Value};
use pitcrew_store::sql::{self, Connection, OptionalExtension, Row, params, params_from_iter};
use std::collections::HashMap;

/// Which tasks to list. Empty fields match everything; `statuses` matches any of its values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFilter {
    /// Only tasks in this project.
    pub project: Option<ProjectId>,
    /// Only tasks in this workstream.
    pub workstream: Option<WorkstreamId>,
    /// Only tasks assigned to this member.
    pub assignee: Option<MemberId>,
    /// Only tasks with one of these statuses.
    pub statuses: Vec<TaskStatus>,
}

/// Which asks to list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AskFilter {
    /// Only asks addressed to this member.
    pub to: Option<MemberId>,
    /// Only asks in one of these states.
    pub states: Vec<AskState>,
}

/// Which sessions to list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionFilter {
    /// Only sessions on this machine.
    pub machine: Option<MachineId>,
    /// Only sessions linked to this workstream.
    pub workstream: Option<WorkstreamId>,
    /// Only sessions linked to this task.
    pub task: Option<TaskId>,
    /// Only sessions in one of these states.
    pub states: Vec<SessionState>,
}

/// A task named by id or by key, as task routes accept either.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TaskRef {
    /// By id.
    Id(TaskId),
    /// By key, e.g. `PAP-4`.
    Key(TaskKey),
}

impl TaskRef {
    /// Parses `tsk_01JB…`, a bare ULID, or a key such as `PAP-4`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        text.parse()
            .map(Self::Id)
            .or_else(|_| text.parse().map(Self::Key))
            .ok()
    }
}

impl From<TaskId> for TaskRef {
    fn from(id: TaskId) -> Self {
        Self::Id(id)
    }
}

impl std::fmt::Display for TaskRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Id(id) => write!(f, "{id}"),
            Self::Key(key) => write!(f, "{key}"),
        }
    }
}

// ─── Building WHERE clauses ──────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Where {
    clauses: Vec<String>,
    params: Vec<Value>,
}

impl Where {
    fn eq(&mut self, column: &str, value: Option<String>) {
        if let Some(value) = value {
            self.params.push(Value::Text(value));
            self.clauses
                .push(format!("{column} = ?{}", self.params.len()));
        }
    }

    fn any_of(&mut self, column: &str, mut values: Vec<String>) {
        if values.len() <= 1 {
            // `=` rather than a one-value `IN`, so the planner treats both alike.
            self.eq(column, values.pop());
            return;
        }
        let mut marks = Vec::with_capacity(values.len());
        for value in values {
            self.params.push(Value::Text(value));
            marks.push(format!("?{}", self.params.len()));
        }
        self.clauses
            .push(format!("{column} IN ({})", marks.join(", ")));
    }

    fn sql(&self) -> String {
        if self.clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", self.clauses.join(" AND "))
        }
    }
}

fn texts<T: serde::Serialize>(values: &[T]) -> Result<Vec<String>> {
    Ok(values.iter().map(enum_text).collect::<Result<_, _>>()?)
}

fn conversion(idx: usize, e: impl std::error::Error + Send + Sync + 'static) -> sql::Error {
    sql::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e))
}

fn opt_date(row: &Row<'_>, idx: usize) -> sql::Result<Option<Date>> {
    Ok(row.get::<_, Option<String>>(idx)?.map(Date))
}

fn project_key(row: &Row<'_>, idx: usize) -> sql::Result<ProjectKey> {
    ProjectKey::new(row.get::<_, String>(idx)?).map_err(|e| conversion(idx, e))
}

/// Child rows grouped by their parent's id, each group in `position` order.
fn children<T>(
    conn: &Connection,
    sql: &str,
    params: &[Value],
    read: impl Fn(&Row<'_>) -> sql::Result<T>,
) -> Result<HashMap<String, Vec<T>>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let mut rows = stmt.query(params_from_iter(params.iter()))?;
    let mut out: HashMap<String, Vec<T>> = HashMap::new();
    while let Some(row) = rows.next()? {
        let parent: String = row.get(0)?;
        out.entry(parent).or_default().push(read(row)?);
    }
    Ok(out)
}

// ─── Directory ───────────────────────────────────────────────────────────────────────────────────

fn machine_row(r: &Row<'_>) -> sql::Result<Machine> {
    Ok(Machine {
        id: col(r, 0)?,
        name: r.get(1)?,
        kind: enum_col(r, 2)?,
        info: opt_json_col(r, 3)?,
        liveness: enum_col(r, 4)?,
    })
}

/// Every machine.
pub fn machines(conn: &Connection) -> Result<Vec<Machine>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, name, kind, info, liveness FROM work_machines ORDER BY rev, id",
    )?;
    let rows = stmt.query_map([], machine_row)?;
    Ok(rows.collect::<sql::Result<_>>()?)
}

/// One machine.
pub fn machine(conn: &Connection, id: &MachineId) -> Result<Option<Machine>> {
    Ok(conn
        .prepare_cached("SELECT id, name, kind, info, liveness FROM work_machines WHERE id = ?1")?
        .query_row(params![id.text()], machine_row)
        .optional()?)
}

fn member_row(r: &Row<'_>) -> sql::Result<Member> {
    Ok(Member {
        id: col(r, 0)?,
        kind: enum_col(r, 1)?,
        handle: r.get(2)?,
        name: r.get(3)?,
        owner: opt_col(r, 4)?,
        persona: opt_col(r, 5)?,
    })
}

const MEMBER_COLS: &str = "id, kind, handle, name, owner, persona";

/// Every member, people and agents.
pub fn members(conn: &Connection) -> Result<Vec<Member>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {MEMBER_COLS} FROM work_members ORDER BY rev, id"
    ))?;
    let rows = stmt.query_map([], member_row)?;
    Ok(rows.collect::<sql::Result<_>>()?)
}

/// One member.
pub fn member(conn: &Connection, id: &MemberId) -> Result<Option<Member>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {MEMBER_COLS} FROM work_members WHERE id = ?1"
        ))?
        .query_row(params![id.text()], member_row)
        .optional()?)
}

const PERSONA_COLS: &str = "id, name, engine, model, instructions, permission_mode";

fn persona_row(r: &Row<'_>) -> sql::Result<Persona> {
    Ok(Persona {
        id: col(r, 0)?,
        name: r.get(1)?,
        engine: enum_col(r, 2)?,
        model: r.get(3)?,
        instructions: r.get(4)?,
        permission_mode: enum_col(r, 5)?,
    })
}

/// Every persona.
pub fn personas(conn: &Connection) -> Result<Vec<Persona>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {PERSONA_COLS} FROM work_personas ORDER BY rev, id"
    ))?;
    let rows = stmt.query_map([], persona_row)?;
    Ok(rows.collect::<sql::Result<_>>()?)
}

/// One persona.
pub fn persona(conn: &Connection, id: &PersonaId) -> Result<Option<Persona>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {PERSONA_COLS} FROM work_personas WHERE id = ?1"
        ))?
        .query_row(params![id.text()], persona_row)
        .optional()?)
}

/// Every team, with its members in order.
pub fn teams(conn: &Connection) -> Result<Vec<Team>> {
    let mut members = children(
        conn,
        "SELECT team, member FROM work_team_members ORDER BY team, position",
        &[],
        |r| col::<MemberId>(r, 1),
    )?;
    let mut stmt = conn.prepare_cached("SELECT id, name, lead FROM work_teams ORDER BY rev, id")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Team {
                id: col::<TeamId>(r, 0)?,
                name: r.get(1)?,
                lead: col(r, 2)?,
                members: Vec::new(),
            },
        ))
    })?;
    let mut teams = Vec::new();
    for row in rows {
        let (key, mut team) = row?;
        team.members = members.remove(&key).unwrap_or_default();
        teams.push(team);
    }
    Ok(teams)
}

// ─── Projects and workstreams ────────────────────────────────────────────────────────────────────

const PROJECT_COLS: &str = "id, key, name, status, lead, start, due, root, external";

fn project_row(r: &Row<'_>) -> sql::Result<(String, Project)> {
    Ok((
        r.get(0)?,
        Project {
            id: col(r, 0)?,
            key: project_key(r, 1)?,
            name: r.get(2)?,
            status: enum_col(r, 3)?,
            lead: col(r, 4)?,
            members: Vec::new(),
            start: opt_date(r, 5)?,
            due: opt_date(r, 6)?,
            root: opt_json_col(r, 7)?,
            external: json_col(r, 8)?,
        },
    ))
}

fn load_projects(conn: &Connection, filter: &Where) -> Result<Vec<Project>> {
    let where_sql = filter.sql();
    let mut members = children(
        conn,
        &format!(
            "SELECT m.project, m.member FROM work_project_members m
             JOIN work_projects p ON p.id = m.project {where_sql}
             ORDER BY m.project, m.position"
        ),
        &filter.params,
        |r| col::<MemberId>(r, 1),
    )?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {PROJECT_COLS} FROM work_projects p {where_sql} ORDER BY p.rev, p.id"
    ))?;
    let rows = stmt.query_map(params_from_iter(filter.params.iter()), project_row)?;
    let mut out = Vec::new();
    for row in rows {
        let (key, mut project) = row?;
        project.members = members.remove(&key).unwrap_or_default();
        out.push(project);
    }
    Ok(out)
}

/// Every project.
pub fn projects(conn: &Connection) -> Result<Vec<Project>> {
    load_projects(conn, &Where::default())
}

/// One project.
pub fn project(conn: &Connection, id: &ProjectId) -> Result<Option<Project>> {
    let mut filter = Where::default();
    filter.eq("p.id", Some(id.text()));
    Ok(load_projects(conn, &filter)?.pop())
}

/// The project that holds `key`, if any. Keys are unique (see [`crate::projection::Projects`]).
pub fn project_with_key(conn: &Connection, key: &ProjectKey) -> Result<Option<Project>> {
    let mut filter = Where::default();
    filter.eq("p.key", Some(key.as_str().to_owned()));
    Ok(load_projects(conn, &filter)?.pop())
}

/// Whether `task`, or a task it waits on directly or through others, is `target`: so `target`
/// waiting on `task` would close a cycle. Existing cycles (from a log imported from elsewhere) do
/// not loop.
pub fn waits_on(conn: &Connection, task: &TaskId, target: &TaskId) -> Result<bool> {
    let found: Option<i64> = conn
        .prepare_cached(
            "WITH RECURSIVE waiting(id) AS (
               SELECT ?1
               UNION
               SELECT d.blocked_by FROM work_task_deps d JOIN waiting w ON d.task = w.id
             )
             SELECT 1 FROM waiting WHERE id = ?2 LIMIT 1",
        )?
        .query_row(params![task.text(), target.text()], |r| r.get(0))
        .optional()?;
    Ok(found.is_some())
}

const WORKSTREAM_COLS: &str = "w.id, w.project, w.name, w.status, w.health, w.external";

fn load_workstreams(conn: &Connection, filter: &Where) -> Result<Vec<Workstream>> {
    let where_sql = filter.sql();
    let mut locations = children(
        conn,
        &format!(
            "SELECT l.workstream, l.machine, l.path, l.branch FROM work_locations l
             JOIN work_workstreams w ON w.id = l.workstream {where_sql}
             ORDER BY l.workstream, l.position"
        ),
        &filter.params,
        |r| {
            Ok(Location {
                machine: col(r, 1)?,
                path: r.get(2)?,
                branch: r.get(3)?,
            })
        },
    )?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {WORKSTREAM_COLS} FROM work_workstreams w {where_sql} ORDER BY w.rev, w.id"
    ))?;
    let rows = stmt.query_map(params_from_iter(filter.params.iter()), |r| {
        Ok((
            r.get::<_, String>(0)?,
            Workstream {
                id: col(r, 0)?,
                project: col(r, 1)?,
                name: r.get(2)?,
                status: enum_col(r, 3)?,
                health: enum_col(r, 4)?,
                locations: Vec::new(),
                external: json_col(r, 5)?,
            },
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (key, mut workstream) = row?;
        workstream.locations = locations.remove(&key).unwrap_or_default();
        out.push(workstream);
    }
    Ok(out)
}

/// Workstreams, all or those of one project.
pub fn workstreams(conn: &Connection, project: Option<&ProjectId>) -> Result<Vec<Workstream>> {
    let mut filter = Where::default();
    filter.eq("w.project", project.map(IdText::text));
    load_workstreams(conn, &filter)
}

/// One workstream.
pub fn workstream(conn: &Connection, id: &WorkstreamId) -> Result<Option<Workstream>> {
    let mut filter = Where::default();
    filter.eq("w.id", Some(id.text()));
    Ok(load_workstreams(conn, &filter)?.pop())
}

// ─── Tasks ───────────────────────────────────────────────────────────────────────────────────────

/// Calls `each` with the JSON document of every task matching `filter` (a WHERE over
/// `work_tasks t`), in creation order. One indexed query, however many tasks match.
fn task_docs(
    conn: &Connection,
    filter: &Where,
    mut each: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT t.doc FROM work_tasks t {} ORDER BY t.rev",
        filter.sql()
    ))?;
    let mut rows = stmt.query(params_from_iter(filter.params.iter()))?;
    while let Some(row) = rows.next()? {
        each(row.get_ref(0)?.as_str().map_err(|e| conversion(0, e))?)?;
    }
    Ok(())
}

fn task_where(filter: &TaskFilter) -> Result<Where> {
    let mut w = Where::default();
    w.eq("t.project", filter.project.as_ref().map(IdText::text));
    w.eq("t.workstream", filter.workstream.as_ref().map(IdText::text));
    w.eq("t.assignee", filter.assignee.as_ref().map(IdText::text));
    w.any_of("t.status", texts(&filter.statuses)?);
    Ok(w)
}

fn load_tasks(conn: &Connection, filter: &Where) -> Result<Vec<Task>> {
    let mut out = Vec::new();
    task_docs(conn, filter, |doc| {
        out.push(serde_json::from_str(doc)?);
        Ok(())
    })?;
    Ok(out)
}

/// Tasks matching `filter`, in creation order.
pub fn tasks(conn: &Connection, filter: &TaskFilter) -> Result<Vec<Task>> {
    load_tasks(conn, &task_where(filter)?)
}

/// Tasks matching `filter`, in creation order, as a JSON array: exactly what serializing
/// [`tasks`] gives, without decoding and re-encoding each task. For lists served as they are.
pub fn tasks_json(conn: &Connection, filter: &TaskFilter) -> Result<String> {
    let mut out = String::from("[");
    task_docs(conn, &task_where(filter)?, |doc| {
        if out.len() > 1 {
            out.push(',');
        }
        out.push_str(doc);
        Ok(())
    })?;
    out.push(']');
    Ok(out)
}

/// One task, by id or key.
pub fn task(conn: &Connection, task: &TaskRef) -> Result<Option<Task>> {
    let mut w = Where::default();
    match task {
        TaskRef::Id(id) => w.eq("t.id", Some(id.text())),
        TaskRef::Key(key) => {
            w.eq("t.key_prefix", Some(key.project.as_str().to_owned()));
            w.params.push(Value::Integer(i64::from(key.number)));
            w.clauses.push(format!("t.number = ?{}", w.params.len()));
        }
    }
    Ok(load_tasks(conn, &w)?.pop())
}

/// The highest task number used with the key prefix `key` (`PAP` in `PAP-4`), or 0.
///
/// Keys are unique by prefix and number, whichever project a task is in, so this is what the
/// next key is allocated from: two projects that share a key never hand out the same task key.
pub fn highest_task_number(conn: &Connection, key: &ProjectKey) -> Result<u32> {
    let n: Option<i64> = conn
        .prepare_cached("SELECT MAX(number) FROM work_tasks WHERE key_prefix = ?1")?
        .query_row(params![key.as_str()], |r| r.get(0))?;
    Ok(n.and_then(|n| u32::try_from(n).ok()).unwrap_or(0))
}

/// Whether a `task_created` for `task` was refused because another task held its key (see
/// [`crate::projection::Tasks`]).
pub fn key_clashed(conn: &Connection, task: &TaskId) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM work_task_clashes WHERE task = ?1 LIMIT 1")?
        .query_row(params![task.text()], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Whether `agent` holds an active (not ended) dispatch on `task`.
pub fn has_active_dispatch(conn: &Connection, task: &TaskId, agent: &MemberId) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT 1 FROM work_dispatches WHERE task = ?1 AND agent = ?2 AND ended IS NULL
             LIMIT 1",
        )?
        .query_row(params![task.text(), agent.text()], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Whether the task is `member`'s own: it is the assignee, or holds an active dispatch on it.
pub fn is_own_task(conn: &Connection, task: &Task, member: &MemberId) -> Result<bool> {
    Ok(task.assignee.as_ref() == Some(member) || has_active_dispatch(conn, &task.id, member)?)
}

// ─── Sessions and dispatches ─────────────────────────────────────────────────────────────────────

const SESSION_COLS: &str = "id, engine, native_id, machine, cwd, branch, title, agent, workstream, \
                            task, link_basis, state, status_line, started, last_activity, \
                            terminal, parent";

fn session_row(r: &Row<'_>) -> sql::Result<Session> {
    Ok(Session {
        id: col(r, 0)?,
        engine: enum_col(r, 1)?,
        native_id: r.get(2)?,
        machine: col(r, 3)?,
        cwd: r.get(4)?,
        branch: r.get(5)?,
        title: r.get(6)?,
        agent: opt_col(r, 7)?,
        workstream: opt_col(r, 8)?,
        task: opt_col(r, 9)?,
        link_basis: opt_enum_col(r, 10)?,
        state: enum_col(r, 11)?,
        status_line: r.get(12)?,
        started: r.get(13)?,
        last_activity: r.get(14)?,
        terminal: opt_col(r, 15)?,
        parent: opt_col(r, 16)?,
    })
}

/// Sessions matching `filter`, in the order they were first seen.
pub fn sessions(conn: &Connection, filter: &SessionFilter) -> Result<Vec<Session>> {
    let mut w = Where::default();
    w.eq("machine", filter.machine.as_ref().map(IdText::text));
    w.eq("workstream", filter.workstream.as_ref().map(IdText::text));
    w.eq("task", filter.task.as_ref().map(IdText::text));
    w.any_of("state", texts(&filter.states)?);
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {SESSION_COLS} FROM work_sessions {} ORDER BY rev, id",
        w.sql()
    ))?;
    let rows = stmt.query_map(params_from_iter(w.params.iter()), session_row)?;
    Ok(rows.collect::<sql::Result<_>>()?)
}

/// One session.
pub fn session(conn: &Connection, id: &SessionId) -> Result<Option<Session>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {SESSION_COLS} FROM work_sessions WHERE id = ?1"
        ))?
        .query_row(params![id.text()], session_row)
        .optional()?)
}

const DISPATCH_COLS: &str = "id, task, agent, session, brief, started, ended, outcome, summary";

fn dispatch_row(r: &Row<'_>) -> sql::Result<Dispatch> {
    Ok(Dispatch {
        id: col(r, 0)?,
        task: col(r, 1)?,
        agent: col(r, 2)?,
        session: opt_col(r, 3)?,
        brief: r.get(4)?,
        started: r.get(5)?,
        ended: r.get(6)?,
        outcome: opt_enum_col(r, 7)?,
        summary: r.get(8)?,
    })
}

/// Every dispatch, oldest first.
pub fn dispatches(conn: &Connection) -> Result<Vec<Dispatch>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {DISPATCH_COLS} FROM work_dispatches ORDER BY rev, id"
    ))?;
    let rows = stmt.query_map([], dispatch_row)?;
    Ok(rows.collect::<sql::Result<_>>()?)
}

/// One dispatch.
pub fn dispatch(conn: &Connection, id: &DispatchId) -> Result<Option<Dispatch>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {DISPATCH_COLS} FROM work_dispatches WHERE id = ?1"
        ))?
        .query_row(params![id.text()], dispatch_row)
        .optional()?)
}

// ─── Asks ────────────────────────────────────────────────────────────────────────────────────────

const ASK_COLS: &str = "id, kind, from_member, to_member, task, session, title, body, options, \
                        receipts, state, answer, created";

fn ask_row(r: &Row<'_>) -> sql::Result<Ask> {
    Ok(Ask {
        id: col(r, 0)?,
        kind: enum_col(r, 1)?,
        from: col(r, 2)?,
        to: col(r, 3)?,
        task: opt_col(r, 4)?,
        session: opt_col(r, 5)?,
        title: r.get(6)?,
        body: r.get(7)?,
        options: json_col(r, 8)?,
        receipts: json_col(r, 9)?,
        state: enum_col(r, 10)?,
        answer: opt_json_col(r, 11)?,
        created: r.get(12)?,
    })
}

/// Asks matching `filter`, oldest first.
pub fn asks(conn: &Connection, filter: &AskFilter) -> Result<Vec<Ask>> {
    let mut w = Where::default();
    w.eq("to_member", filter.to.as_ref().map(IdText::text));
    w.any_of("state", texts(&filter.states)?);
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {ASK_COLS} FROM work_asks {} ORDER BY rev, id",
        w.sql()
    ))?;
    let rows = stmt.query_map(params_from_iter(w.params.iter()), ask_row)?;
    Ok(rows.collect::<sql::Result<_>>()?)
}

/// One ask.
pub fn ask(conn: &Connection, id: &AskId) -> Result<Option<Ask>> {
    Ok(conn
        .prepare_cached(&format!("SELECT {ASK_COLS} FROM work_asks WHERE id = ?1"))?
        .query_row(params![id.text()], ask_row)
        .optional()?)
}

// ─── Briefs ──────────────────────────────────────────────────────────────────────────────────────

/// A brief in force with its pending proposal, if any (the `LEFT JOIN` in [`BRIEFS`]).
fn brief_row(r: &Row<'_>) -> sql::Result<Brief> {
    let kind: String = r.get(0)?;
    let target = match kind.as_str() {
        "project" => BriefTarget::Project(col(r, 1)?),
        "workstream" => BriefTarget::Workstream(col(r, 1)?),
        other => {
            return Err(conversion(
                0,
                std::io::Error::other(format!("unknown brief target {other:?}")),
            ));
        }
    };
    let proposal = match r.get::<_, Option<String>>(8)? {
        Some(text) => Some(BriefProposal {
            text,
            next: r.get(9)?,
            receipts: json_col(r, 10)?,
            at: r.get(11)?,
        }),
        None => None,
    };
    Ok(Brief {
        target,
        text: r.get(2)?,
        next: r.get(3)?,
        pinned: r.get(4)?,
        source: enum_col(r, 5)?,
        updated: r.get(6)?,
        receipts: json_col(r, 7)?,
        proposal,
    })
}

/// Briefs in force, each with its pending proposal: a proposal is pending exactly while its row
/// exists (see [`crate::projection::Briefs`]).
const BRIEFS: &str = "SELECT b.target_kind, b.target_id, b.text, b.next, b.pinned, b.source,
       b.updated, b.receipts, p.text, p.next, p.receipts, p.at
     FROM work_briefs b
     LEFT JOIN work_brief_proposals p
       ON p.target_kind = b.target_kind AND p.target_id = b.target_id";

/// Every brief in force, in the order they were first written, each with its pending proposal.
/// A target with a proposal but no brief in force is not listed.
pub fn briefs(conn: &Connection) -> Result<Vec<Brief>> {
    let mut stmt = conn.prepare_cached(&format!(
        "{BRIEFS} ORDER BY b.rev, b.target_kind, b.target_id"
    ))?;
    let rows = stmt.query_map([], brief_row)?;
    Ok(rows.collect::<sql::Result<_>>()?)
}

/// The brief in force for `target`, if any, with its pending proposal.
pub fn brief(conn: &Connection, target: &BriefTarget) -> Result<Option<Brief>> {
    let (kind, id) = target_columns(target);
    Ok(conn
        .prepare_cached(&format!(
            "{BRIEFS} WHERE b.target_kind = ?1 AND b.target_id = ?2"
        ))?
        .query_row(params![kind, id], brief_row)
        .optional()?)
}

/// The pending proposal for `target`, whether or not a brief is in force for it yet.
pub fn pending_proposal(conn: &Connection, target: &BriefTarget) -> Result<Option<BriefProposal>> {
    let (kind, id) = target_columns(target);
    Ok(conn
        .prepare_cached(
            "SELECT text, next, receipts, at FROM work_brief_proposals
             WHERE target_kind = ?1 AND target_id = ?2",
        )?
        .query_row(params![kind, id], |r| {
            Ok(BriefProposal {
                text: r.get(0)?,
                next: r.get(1)?,
                receipts: json_col(r, 2)?,
                at: r.get(3)?,
            })
        })
        .optional()?)
}

/// Whether the work model holds any members, machines or projects yet.
pub fn has_data(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM work_members) OR EXISTS (SELECT 1 FROM work_machines)
           OR EXISTS (SELECT 1 FROM work_projects)",
        [],
        |r| r.get(0),
    )?)
}

/// The revision every work table reflects: the lowest checkpoint of the work projections
/// (`projection_state.rev`, as the store README says to read it), or 0 before any.
pub fn work_rev(conn: &Connection) -> Result<u64> {
    let marks = vec!["?"; NAMES.len()].join(", ");
    let rev: Option<i64> = conn
        .prepare_cached(&format!(
            "SELECT MIN(rev) FROM projection_state WHERE name IN ({marks})"
        ))?
        .query_row(params_from_iter(NAMES), |r| r.get(0))?;
    Ok(rev.and_then(|r| u64::try_from(r).ok()).unwrap_or(0))
}

// ─── Activity references ─────────────────────────────────────────────────────────────────────────

/// Which events to find: those about **all** of the given project, workstream, task and session
/// (see [`crate::projection::Refs`] for what "about" means). At least one must be given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefFilter {
    /// Events about this project.
    pub project: Option<ProjectId>,
    /// Events about this workstream.
    pub workstream: Option<WorkstreamId>,
    /// Events about this task, including those of sessions linked to it.
    pub task: Option<TaskId>,
    /// Events about this session.
    pub session: Option<SessionId>,
}

/// The most index rows one [`revs_matching`] call examines by default, like the activity route's
/// scan budget.
pub const REF_SCAN_BUDGET: usize = 10_000;

/// The newest revisions below `before` (exclusive) of events matching `filter`: at most `limit`
/// of them, oldest first, and where the search stopped, `scanned_to`.
///
/// - Pass `scanned_to` as the next call's `before` to page back. **`scanned_to` is 0 only when
///   the search reached the start of the log**, so nothing older matches. A non-zero
///   `scanned_to` says where the search stopped, not that something older matches.
/// - With more than `limit` matches below `before`, `scanned_to` is the oldest returned revision.
/// - The search walks the index of the filter's most specific field (session, then task, then
///   workstream, then project) and checks the other fields row by row. It examines at most
///   `budget` rows; when the budget runs out first, fewer than `limit` revisions come back (even
///   none) and `scanned_to` is the last revision examined, whether or not anything older
///   matches. With one field given, every row examined matches, so the budget never cuts a page
///   short.
///
/// # Errors
///
/// `invalid` for an empty filter (every event would match; page the log itself) or a `limit` of
/// 0; database errors.
pub fn revs_matching(
    conn: &Connection,
    filter: &RefFilter,
    before: u64,
    limit: usize,
    budget: usize,
) -> Result<(Vec<u64>, u64)> {
    // (column, index in the SELECT below, value), most specific first.
    let fields: Vec<(&str, usize, String)> = [
        ("session", 4, filter.session.as_ref().map(IdText::text)),
        ("task", 3, filter.task.as_ref().map(IdText::text)),
        (
            "workstream",
            2,
            filter.workstream.as_ref().map(IdText::text),
        ),
        ("project", 1, filter.project.as_ref().map(IdText::text)),
    ]
    .into_iter()
    .filter_map(|(column, idx, value)| value.map(|v| (column, idx, v)))
    .collect();
    let Some(((column, _, value), rest)) = fields.split_first() else {
        return Err(WorkError::invalid(
            "Give a project, workstream, task or session to filter by.",
        ));
    };
    if limit == 0 {
        return Err(WorkError::invalid("limit must be at least 1."));
    }
    if before <= 1 || budget == 0 {
        return Ok((Vec::new(), 0));
    }
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT rev, project, workstream, task, session FROM work_event_refs
         WHERE {column} = ?1 AND rev < ?2 ORDER BY rev DESC LIMIT ?3"
    ))?;
    let mut rows = stmt.query(params![
        value,
        sql_rev(before),
        i64::try_from(budget).unwrap_or(i64::MAX)
    ])?;
    let mut found = Vec::new(); // newest first
    let mut examined = 0;
    let mut last = 0;
    while let Some(row) = rows.next()? {
        examined += 1;
        let rev: i64 = row.get(0)?;
        last = u64::try_from(rev).unwrap_or(0);
        let mut matches = true;
        for (_, idx, want) in rest {
            let got = row
                .get_ref(*idx)?
                .as_str_or_null()
                .map_err(|e| conversion(*idx, e))?;
            if got != Some(want.as_str()) {
                matches = false;
                break;
            }
        }
        if matches {
            found.push(last);
            if found.len() > limit {
                found.truncate(limit);
                let oldest = found.last().copied().unwrap_or(0);
                found.reverse();
                return Ok((found, oldest));
            }
        }
    }
    // Every row below `before` was examined, unless the budget ran out first.
    let scanned_to = if examined >= budget { last } else { 0 };
    found.reverse();
    Ok((found, scanned_to))
}
