//! The read verbs (api-v1.md, "Orchestrator", "The CLI's read verbs"): what an agent looks at to
//! answer a question about the work. Each is only `GET`s, so a reader token (which may only read)
//! runs every one. Text output names things by the ids an answer cites: `ses_…`, `wst_…`,
//! `prj_…`, task keys, and `recap:wst_…@YYYY-MM-DD`. `--json` prints the daemon's JSON (for
//! `session list --since`, filtered to those sessions; for `session show` and `search`, an object
//! of the daemon's lists).

use super::{Verb, query_string, task_ref, wire};
use crate::client::from_value;
use crate::display;
use crate::error::{Error, Result};
use crate::time;
use pitcrew_protocol::api::EventsPage;
use pitcrew_protocol::ids::{ProjectId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{Machine, Project, Session, SessionState, Task, Workstream};
use pitcrew_protocol::recap::{BlocksPage, DaysPage, RecapBlock};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt::Write as _;

/// The recap blocks `search` looks through: the newest.
const SEARCH_BLOCKS: u32 = 200;
/// The most matches of each kind `search` prints.
const SEARCH_SHOWN: usize = 20;
/// Recent blocks `session show` prints.
const SHOW_BLOCKS: u32 = 10;
/// The longest text an activity line quotes, in characters.
const QUOTE_CHARS: usize = 100;

const STATES: &str = "starting, working, waiting, idle, ended, unreachable";

/// `--session`, `--task`, `--workstream`, `--project`: what to narrow a read to.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scope {
    pub(crate) session: Option<String>,
    pub(crate) task: Option<String>,
    pub(crate) workstream: Option<String>,
    pub(crate) project: Option<String>,
}

/// `session list`'s filters.
#[derive(Clone, Debug, Default)]
pub(crate) struct SessionListArgs {
    pub(crate) since: Option<String>,
    pub(crate) states: Vec<String>,
    pub(crate) workstream: Option<String>,
    pub(crate) task: Option<String>,
}

fn session_id(text: &str) -> Result<SessionId> {
    text.trim()
        .parse()
        .map_err(|_| Error::invalid(format!("{text:?} is not a session id (ses_…)")))
}

fn workstream_id(text: &str) -> Result<WorkstreamId> {
    text.trim()
        .parse()
        .map_err(|_| Error::invalid(format!("{text:?} is not a workstream id (wst_…)")))
}

fn project_id(text: &str) -> Result<ProjectId> {
    text.trim()
        .parse()
        .map_err(|_| Error::invalid(format!("{text:?} is not a project id (prj_…)")))
}

/// At most `max` characters of one safe line, with `…` when cut.
fn quote(text: &str, max: usize) -> String {
    let line = display::line(text);
    let line = line.trim();
    if line.chars().count() <= max {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// A summary's text, or `-`.
fn line_of(block: &RecapBlock) -> String {
    let text = quote(&block.line.text, 300);
    if text.is_empty() {
        "-".to_owned()
    } else {
        text
    }
}

impl Verb<'_, '_> {
    /// A task's id, from its key or id (`GET /tasks/{key}` for a key).
    fn task_id(&mut self, reference: &str) -> Result<TaskId> {
        let reference = task_ref(reference)?;
        if let Ok(id) = reference.parse::<TaskId>() {
            return Ok(id);
        }
        Ok(self.task(&reference)?.0.id)
    }

    /// The scope as query pairs: ids bare, a task key resolved to its id.
    fn scope_query(&mut self, scope: &Scope) -> Result<Vec<(&'static str, String)>> {
        let mut out = Vec::new();
        if let Some(s) = &scope.session {
            out.push(("session", session_id(s)?.0.to_string()));
        }
        if let Some(t) = &scope.task {
            out.push(("task", self.task_id(t)?.0.to_string()));
        }
        if let Some(w) = &scope.workstream {
            out.push(("workstream", workstream_id(w)?.0.to_string()));
        }
        if let Some(p) = &scope.project {
            out.push(("project", project_id(p)?.0.to_string()));
        }
        Ok(out)
    }

    /// Workstream names by id.
    fn workstream_names(&mut self) -> Result<HashMap<WorkstreamId, String>> {
        let list: Vec<Workstream> = from_value(self.client.get("/workstreams")?)?;
        Ok(list
            .into_iter()
            .map(|w| (w.id, display::line(&w.name)))
            .collect())
    }

    /// `session list`: the hub's sessions, newest activity first.
    pub(crate) fn session_list(&mut self, args: &SessionListArgs) -> Result<()> {
        let since = args
            .since
            .as_deref()
            .map(|s| time::since(s, super::now_ms()).map_err(Error::invalid))
            .transpose()?;
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(w) = &args.workstream {
            query.push(("workstream", workstream_id(w)?.0.to_string()));
        }
        if let Some(t) = &args.task {
            query.push(("task", self.task_id(t)?.0.to_string()));
        }
        for state in &args.states {
            let state: SessionState = super::parse_enum("state", state, STATES)?;
            query.push(("state", wire(&state)));
        }
        let value = self
            .client
            .get(&format!("/sessions{}", query_string(&query)))?;
        let mut raw: Vec<Value> = from_value(value)?;
        if let Some(since) = since {
            raw.retain(|s| s["last_activity"].as_i64().is_some_and(|at| at >= since));
        }
        if self.json {
            return self.print_json(&Value::Array(raw));
        }
        let mut sessions: Vec<Session> = from_value(Value::Array(raw))?;
        if sessions.is_empty() {
            return self.print("No sessions.\n");
        }
        sessions.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
        let names = self.workstream_names()?;
        let keys = self.task_keys()?;
        let mut rows = vec![[
            "SESSION".to_owned(),
            "STATE".into(),
            "CLI".into(),
            "LAST ACTIVE (UTC)".into(),
            "WORKSTREAM".into(),
            "TASK".into(),
            "TITLE".into(),
        ]];
        for s in &sessions {
            rows.push([
                s.id.to_string(),
                wire(&s.state),
                wire(&s.engine),
                time::when(s.last_activity),
                s.workstream
                    .and_then(|w| names.get(&w).cloned())
                    .unwrap_or_else(|| "-".into()),
                s.task
                    .and_then(|t| keys.get(&t).cloned())
                    .unwrap_or_else(|| "-".into()),
                s.title
                    .as_deref()
                    .map_or_else(|| "-".into(), |t| quote(t, 120)),
            ]);
        }
        self.print(&super::table(&rows))
    }

    /// `session show <id>`: one session's facts and its recent blocks of work.
    pub(crate) fn session_show(&mut self, id: &str) -> Result<()> {
        let id = session_id(id)?;
        let value = self.client.get(&format!("/sessions/{}", id.0))?;
        let blocks = self.client.get(&format!(
            "/recaps/blocks?session={}&limit={SHOW_BLOCKS}",
            id.0
        ))?;
        if self.json {
            return self.print_json(&json!({ "session": value, "blocks": blocks }));
        }
        let session: Session = from_value(value)?;
        let page: BlocksPage = from_value(blocks)?;
        let mut out = format!(
            "{}  {}\n",
            session.id,
            session
                .title
                .as_deref()
                .map_or_else(|| "-".into(), |t| quote(t, 200))
        );
        let _ = writeln!(
            out,
            "{} · {} · started {} · last active {} (UTC)",
            wire(&session.engine),
            wire(&session.state),
            time::when(session.started),
            time::when(session.last_activity)
        );
        let machines: Vec<Machine> = from_value(self.client.get("/machines")?)?;
        let machine = machines
            .iter()
            .find(|m| m.id == session.machine)
            .map_or_else(|| session.machine.to_string(), |m| display::line(&m.name));
        let _ = writeln!(out, "machine: {machine}");
        if let Some(branch) = &session.branch {
            let _ = writeln!(out, "branch: {}", quote(branch, 100));
        }
        if let Some(w) = session.workstream {
            let name = self
                .workstream_names()?
                .get(&w)
                .cloned()
                .unwrap_or_default();
            let _ = writeln!(out, "workstream: {w} {name}");
        }
        if let Some(t) = session.task {
            let key = self
                .task_keys()?
                .get(&t)
                .cloned()
                .unwrap_or_else(|| t.to_string());
            let _ = writeln!(out, "task: {key}");
        }
        if let Some(agent) = session.agent {
            let _ = writeln!(out, "agent: {}", self.handle(agent)?);
        }
        if let Some(status) = &session.status_line {
            let _ = writeln!(out, "status: {}", quote(status, 200));
        }
        if !page.blocks.is_empty() {
            out.push_str("\nRecent work (newest first):\n");
            for block in &page.blocks {
                let _ = writeln!(
                    out,
                    "  {}  {}",
                    time::when(block.block.start),
                    line_of(block)
                );
                let files: Vec<String> = block
                    .block
                    .files
                    .iter()
                    .take(5)
                    .map(|f| quote(&f.path, 100))
                    .collect();
                if !files.is_empty() {
                    let _ = writeln!(out, "    files: {}", files.join(", "));
                }
            }
        }
        self.print(&out)
    }

    /// `recap blocks`: blocks of work, newest first, each with its line.
    pub(crate) fn recap_blocks(&mut self, scope: &Scope, limit: Option<u32>) -> Result<()> {
        let mut query = self.scope_query(scope)?;
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let value = self
            .client
            .get(&format!("/recaps/blocks{}", query_string(&query)))?;
        if self.json {
            return self.print_json(&value);
        }
        let page: BlocksPage = from_value(value)?;
        if page.blocks.is_empty() {
            return self.print("No work recorded.\n");
        }
        let mut out = String::new();
        for block in &page.blocks {
            let b = &block.block;
            let whose = match (b.session, b.workstream, b.project) {
                (Some(s), _, _) => s.to_string(),
                (None, Some(w), _) => w.to_string(),
                (None, None, Some(p)) => p.to_string(),
                (None, None, None) => "-".into(),
            };
            let _ = writeln!(
                out,
                "{} to {}  {whose}  {}",
                time::when(b.start),
                time::when(b.end),
                line_of(block)
            );
            if let Some(w) = b.workstream.filter(|_| b.session.is_some()) {
                let _ = writeln!(out, "    workstream {w}");
            }
        }
        if !page.at_start {
            out.push_str("(older blocks not shown: --limit, up to 200)\n");
        }
        self.print(&out)
    }

    /// `recap days`: day paragraphs of a workstream or a project, newest day first.
    pub(crate) fn recap_days(&mut self, scope: &Scope, limit: Option<u32>) -> Result<()> {
        if scope.session.is_some() || scope.task.is_some() {
            return Err(Error::invalid(
                "recap days takes --workstream or --project, not --session or --task",
            ));
        }
        let mut query = self.scope_query(scope)?;
        if query.len() != 1 {
            return Err(Error::invalid(
                "give exactly one of --workstream or --project",
            ));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let value = self
            .client
            .get(&format!("/recaps/days{}", query_string(&query)))?;
        if self.json {
            return self.print_json(&value);
        }
        let page: DaysPage = from_value(value)?;
        if page.days.is_empty() {
            return self.print("No days with work.\n");
        }
        let project = scope.project.as_deref().map(project_id).transpose()?;
        let names = self.workstream_names()?;
        let mut out = String::new();
        for day in &page.days {
            let (cite, name) = match (day.workstream, project) {
                (Some(w), _) => (w.to_string(), names.get(&w).cloned().unwrap_or_default()),
                (None, Some(p)) => (p.to_string(), "outside any workstream".into()),
                (None, None) => ("-".into(), String::new()),
            };
            let _ = writeln!(out, "recap:{cite}@{}  {name}", day.date.0);
            let _ = writeln!(out, "  {}", quote(&day.summary.text, 2000));
        }
        self.print(&out)
    }

    /// `activity`: the activity log, oldest first in the page (the newest page).
    pub(crate) fn activity(&mut self, scope: &Scope, limit: Option<u32>) -> Result<()> {
        let mut query = self.scope_query(scope)?;
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let value = self
            .client
            .get(&format!("/events{}", query_string(&query)))?;
        if self.json {
            return self.print_json(&value);
        }
        let page: EventsPage = from_value(value)?;
        if page.events.is_empty() {
            return self.print("No activity.\n");
        }
        let keys = self.task_keys()?;
        let mut out = String::new();
        for event in &page.events {
            let author = self.handle(event.author)?;
            let body = serde_json::to_value(&event.body).unwrap_or(Value::Null);
            let kind = body["type"].as_str().unwrap_or("event").to_owned();
            let _ = writeln!(
                out,
                "{}  {kind}  {author}{}",
                time::when(event.at),
                describe(&body["data"], &keys)
            );
        }
        self.print(&out)
    }

    /// `search <words…>`: projects, workstreams, tasks, sessions and recent recap lines whose
    /// text holds every word (any case).
    pub(crate) fn search(&mut self, words: &[String]) -> Result<()> {
        let words: Vec<String> = words
            .iter()
            .flat_map(|w| w.split_whitespace())
            .map(str::to_lowercase)
            .collect();
        if words.is_empty() {
            return Err(Error::invalid("give the words to search for"));
        }
        let holds = |text: &str| {
            let text = text.to_lowercase();
            words.iter().all(|w| text.contains(w.as_str()))
        };
        let projects: Vec<Project> = from_value(self.client.get("/projects")?)?;
        let workstreams: Vec<Workstream> = from_value(self.client.get("/workstreams")?)?;
        let tasks: Vec<Task> = from_value(self.client.get("/tasks")?)?;
        let sessions: Vec<Session> = from_value(self.client.get("/sessions")?)?;
        let page: BlocksPage = from_value(
            self.client
                .get(&format!("/recaps/blocks?limit={SEARCH_BLOCKS}"))?,
        )?;
        let projects: Vec<&Project> = projects
            .iter()
            .filter(|p| holds(&format!("{} {}", p.key, p.name)))
            .collect();
        let workstreams: Vec<&Workstream> = workstreams.iter().filter(|w| holds(&w.name)).collect();
        let tasks: Vec<&Task> = tasks
            .iter()
            .filter(|t| holds(&format!("{} {} {}", t.key, t.title, t.description)))
            .collect();
        let sessions: Vec<&Session> = sessions
            .iter()
            .filter(|s| {
                holds(&format!(
                    "{} {} {}",
                    s.title.as_deref().unwrap_or_default(),
                    s.branch.as_deref().unwrap_or_default(),
                    s.status_line.as_deref().unwrap_or_default()
                ))
            })
            .collect();
        let blocks: Vec<&RecapBlock> = page
            .blocks
            .iter()
            .filter(|b| {
                let files: Vec<&str> = b.block.files.iter().map(|f| f.path.as_str()).collect();
                holds(&format!("{} {}", b.line.text, files.join(" ")))
            })
            .collect();
        if self.json {
            return self.print_json(&json!({
                "projects": projects,
                "workstreams": workstreams,
                "tasks": tasks,
                "sessions": sessions,
                "blocks": blocks,
            }));
        }
        let mut out = String::new();
        let mut section = |title: &str, lines: Vec<String>| {
            if lines.is_empty() {
                return;
            }
            let _ = writeln!(out, "{title}:");
            let more = lines.len().saturating_sub(SEARCH_SHOWN);
            for line in lines.into_iter().take(SEARCH_SHOWN) {
                let _ = writeln!(out, "  {line}");
            }
            if more > 0 {
                let _ = writeln!(out, "  (and {more} more)");
            }
        };
        section(
            "Projects",
            projects
                .iter()
                .map(|p| format!("{}  {}  {}", p.id, p.key, quote(&p.name, 120)))
                .collect(),
        );
        section(
            "Workstreams",
            workstreams
                .iter()
                .map(|w| format!("{}  {}  (project {})", w.id, quote(&w.name, 120), w.project))
                .collect(),
        );
        section(
            "Tasks",
            tasks
                .iter()
                .map(|t| format!("{}  {}  {}", t.key, wire(&t.status), quote(&t.title, 120)))
                .collect(),
        );
        section(
            "Sessions",
            sessions
                .iter()
                .map(|s| {
                    format!(
                        "{}  {}  {}  {}",
                        s.id,
                        wire(&s.state),
                        time::when(s.last_activity),
                        s.title
                            .as_deref()
                            .map_or_else(|| "-".into(), |t| quote(t, 120))
                    )
                })
                .collect(),
        );
        section(
            "Recent work",
            blocks
                .iter()
                .map(|b| {
                    let whose = b
                        .block
                        .session
                        .map(|s| s.to_string())
                        .or_else(|| b.block.workstream.map(|w| w.to_string()))
                        .unwrap_or_else(|| "-".into());
                    format!("{}  {whose}  {}", time::when(b.block.start), line_of(b))
                })
                .collect(),
        );
        if out.is_empty() {
            out.push_str("Nothing matches.\n");
        }
        self.print(&out)
    }
}

/// An event's data, briefly: the session, task, workstream and text it names.
fn describe(data: &Value, keys: &HashMap<TaskId, String>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let id_of = |value: &Value| -> Option<String> {
        value
            .as_str()
            .map(str::to_owned)
            .or_else(|| value.get("id").and_then(Value::as_str).map(str::to_owned))
    };
    if let Some(s) = data.get("session").and_then(id_of)
        && let Ok(id) = s.parse::<SessionId>()
    {
        parts.push(id.to_string());
    }
    if let Some(t) = data.get("task").and_then(|task| {
        task.get("key")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                id_of(task)
                    .and_then(|id| id.parse::<TaskId>().ok())
                    .map(|id| keys.get(&id).cloned().unwrap_or_else(|| id.to_string()))
            })
    }) {
        parts.push(display::line(&t));
    }
    if let Some(w) = data.get("workstream").and_then(id_of)
        && let Ok(id) = w.parse::<WorkstreamId>()
    {
        parts.push(id.to_string());
    }
    if let (Some(from), Some(to)) = (
        data.get("from").and_then(Value::as_str),
        data.get("to").and_then(Value::as_str),
    ) {
        parts.push(format!("{} → {}", display::line(from), display::line(to)));
    }
    for field in ["title", "text", "path", "tool", "summary"] {
        if let Some(text) = data.get(field).and_then(Value::as_str) {
            parts.push(format!("\"{}\"", quote(text, QUOTE_CHARS)));
            break;
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(": {}", parts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_described_by_what_they_name() {
        let task: TaskId = "01JB000000000000000TSK0001".parse().unwrap();
        let keys = HashMap::from([(task, "PAP-1".to_owned())]);
        let data = json!({
            "task": task.0.to_string(),
            "from": "todo",
            "to": "in_progress",
            "by": "agent",
        });
        assert_eq!(describe(&data, &keys), ": PAP-1 todo → in_progress");
        let data = json!({
            "session": "01JB000000000000000SES0001",
            "text": "a\u{1b}[2J very long text ".repeat(20),
        });
        let said = describe(&data, &keys);
        assert!(said.starts_with(": ses_01JB000000000000000SES0001 \"a[2J very"));
        assert!(said.ends_with("…\""));
        assert_eq!(describe(&json!({}), &keys), "");
        assert_eq!(quote(" x\ny ", 10), "x y");
    }
}
