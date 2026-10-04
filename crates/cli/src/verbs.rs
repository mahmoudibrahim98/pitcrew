//! The agent verbs. Each prints short, plain text, or with `--json` the daemon's JSON.
//!
//! They act only with an agent token: the first thing each does is ask the daemon whose token
//! it holds, and a person's token is refused (stream I never acts with device tokens). In text
//! output every string from the daemon goes through [`crate::display`].

use crate::client::{Client, from_value};
use crate::display;
use crate::error::{Error, Kind, Result};
use crate::http::encode;
use crate::plan;
use crate::{Io, MAX_STDIN};
use pitcrew_protocol::board::{BoardDraft, MAX_PROPOSAL_BYTES};
use pitcrew_protocol::ids::{AskId, DraftId, MemberId, TaskId, TaskKey};
use pitcrew_protocol::model::{Ask, AskKind, AskState, Member, MemberKind, Task, TaskStatus};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read as _;

/// Mentions and answers this recent are shown by `check`.
const RECENT_MS: i64 = 7 * 24 * 60 * 60 * 1000;

const STATUSES: &str = "backlog, todo, in_progress, review, done, canceled";
const ASK_KINDS: &str = "question, decision, review, approval, mention";

/// A verb's working state: the client, the output, the caller, and what it has fetched.
pub(crate) struct Verb<'io, 'a> {
    client: Client,
    io: &'io mut Io<'a>,
    json: bool,
    /// The token's member, an agent.
    me: Member,
    /// The same, as the daemon sent it.
    me_value: Value,
    members: Option<Vec<Member>>,
}

impl<'io, 'a> Verb<'io, 'a> {
    /// Asks the daemon whose token this is, and refuses anyone but an agent.
    ///
    /// # Errors
    /// `invalid` for a person's token; any error from the daemon.
    pub(crate) fn connect(client: Client, io: &'io mut Io<'a>, json: bool) -> Result<Self> {
        let me_value = client.get("/me")?;
        let me: Member = from_value(me_value.clone())?;
        if me.kind != MemberKind::Agent {
            return Err(Error::invalid(format!(
                "only agent tokens are accepted, and this one belongs to {}, a person; use the \
                 agent token the runner sets in PITCREW_TOKEN",
                display::line(&me.handle)
            )));
        }
        Ok(Self {
            client,
            io,
            json,
            me,
            me_value,
            members: None,
        })
    }

    // ─── Output ──────────────────────────────────────────────────────────────────────────────

    fn print(&mut self, text: &str) -> Result<()> {
        self.io
            .stdout
            .write_all(text.as_bytes())
            .and_then(|()| self.io.stdout.flush())
            .map_err(|e| Error::internal(format!("cannot write the output: {e}")))
    }

    fn print_json(&mut self, value: &Value) -> Result<()> {
        let mut text = serde_json::to_string_pretty(value)
            .map_err(|e| Error::internal(format!("cannot encode the output: {e}")))?;
        text.push('\n');
        self.print(&text)
    }

    // ─── Lookups ─────────────────────────────────────────────────────────────────────────────

    fn members(&mut self) -> Result<&[Member]> {
        if self.members.is_none() {
            self.members = Some(from_value(self.client.get("/members")?)?);
        }
        Ok(self.members.as_deref().unwrap_or_default())
    }

    /// `@handle`, safe to print, or the id when the member is unknown.
    fn handle(&mut self, id: MemberId) -> Result<String> {
        Ok(self
            .members()?
            .iter()
            .find(|m| m.id == id)
            .map_or_else(|| id.to_string(), |m| display::line(&m.handle)))
    }

    /// A member by `@handle`, handle, or id.
    fn member_id(&mut self, who: &str) -> Result<MemberId> {
        let members = self.members()?;
        if let Ok(id) = who.parse::<MemberId>()
            && members.iter().any(|m| m.id == id)
        {
            return Ok(id);
        }
        let want = who.trim().trim_start_matches('@');
        members
            .iter()
            .find(|m| m.handle.trim_start_matches('@').eq_ignore_ascii_case(want))
            .map(|m| m.id)
            .ok_or_else(|| {
                let known: Vec<String> = members.iter().map(|m| display::line(&m.handle)).collect();
                Error::invalid(format!("no member {who}; members: {}", known.join(", ")))
            })
    }

    fn task(&mut self, reference: &str) -> Result<(Task, Value)> {
        let value = self.client.get(&task_path(reference, "")?)?;
        Ok((from_value(value.clone())?, value))
    }

    /// Task keys by id, for tasks shown only by id.
    fn task_keys(&mut self) -> Result<HashMap<TaskId, String>> {
        let tasks: Vec<Task> = from_value(self.client.get("/tasks")?)?;
        Ok(tasks
            .into_iter()
            .map(|t| (t.id, t.key.to_string()))
            .collect())
    }

    /// Moves a task. When the daemon says no because the task is already there, that counts
    /// as done (`claim` and `report --review` are safe to repeat). Returns the task and whether
    /// it moved.
    fn move_to(
        &mut self,
        reference: &str,
        to: TaskStatus,
        repeatable: bool,
    ) -> Result<(Value, bool)> {
        match self
            .client
            .post(&task_path(reference, "/move")?, &json!({ "to": to }))
        {
            Ok(value) => Ok((value, true)),
            Err(e) if repeatable && e.kind == Kind::Conflict => {
                let (task, value) = self.task(reference)?;
                if task.status == to {
                    Ok((value, false))
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    // ─── Verbs ───────────────────────────────────────────────────────────────────────────────

    pub(crate) fn whoami(&mut self) -> Result<()> {
        if self.json {
            let value = self.me_value.clone();
            return self.print_json(&value);
        }
        let what = match self.me.owner {
            Some(owner) => format!("an agent of {}", self.handle(owner)?),
            None => "an agent".to_owned(),
        };
        let text = format!(
            "{} ({}), {what}\n",
            display::line(&self.me.handle),
            display::line(&self.me.name)
        );
        self.print(&text)
    }

    pub(crate) fn task_list(&mut self, mine: bool, statuses: &[String]) -> Result<()> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if mine {
            query.push(("assignee", self.me.id.0.to_string()));
        }
        for status in statuses {
            let status: TaskStatus = parse_enum("status", status, STATUSES)?;
            query.push(("status", wire(&status)));
        }
        let value = self
            .client
            .get(&format!("/tasks{}", query_string(&query)))?;
        if self.json {
            return self.print_json(&value);
        }
        let tasks: Vec<Task> = from_value(value)?;
        if tasks.is_empty() {
            return self.print("No tasks.\n");
        }
        let mut rows = Vec::with_capacity(tasks.len());
        for task in &tasks {
            let assignee = match task.assignee {
                Some(id) => self.handle(id)?,
                None => "-".to_owned(),
            };
            rows.push([
                task.key.to_string(),
                wire(&task.status),
                assignee,
                display::line(&task.title),
            ]);
        }
        self.print(&table(&rows))
    }

    pub(crate) fn task_show(&mut self, reference: &str) -> Result<()> {
        let (task, value) = self.task(reference)?;
        if self.json {
            return self.print_json(&value);
        }
        let mut out = format!("{}  {}\n", task.key, display::line(&task.title));
        let mut facts = vec![format!("status {}", wire(&task.status))];
        if task.priority != pitcrew_protocol::model::Priority::None {
            facts.push(format!("priority {}", wire(&task.priority)));
        }
        if let Some(id) = task.assignee {
            facts.push(format!("assignee {}", self.handle(id)?));
        }
        if let Some(due) = &task.due {
            facts.push(format!("due {}", display::line(&due.0)));
        }
        if !task.labels.is_empty() {
            let labels: Vec<String> = task.labels.iter().map(|l| display::line(l)).collect();
            facts.push(format!("labels {}", labels.join(", ")));
        }
        let _ = writeln!(out, "{}", facts.join(" · "));
        if !task.blocked_by.is_empty() {
            let keys = self.task_keys()?;
            let blockers: Vec<String> = task
                .blocked_by
                .iter()
                .map(|id| keys.get(id).cloned().unwrap_or_else(|| id.to_string()))
                .collect();
            let _ = writeln!(out, "blocked by {}", blockers.join(", "));
        }
        let description = display::text(&task.description);
        if !description.trim().is_empty() {
            let _ = write!(out, "\n{}\n", description.trim_end());
        }
        if !task.subtasks.is_empty() {
            out.push_str("\nSubtasks:\n");
            for sub in &task.subtasks {
                let mark = if sub.done { "x" } else { " " };
                let origin = match sub.source {
                    pitcrew_protocol::model::SubtaskSource::AgentPlan { agent } => {
                        format!("  (plan of {})", self.handle(agent)?)
                    }
                    pitcrew_protocol::model::SubtaskSource::Human => String::new(),
                };
                let _ = writeln!(out, "  [{mark}] {}{origin}", display::line(&sub.text));
            }
        }
        self.print(&out)
    }

    pub(crate) fn task_move(&mut self, reference: &str, status: &str) -> Result<()> {
        let to: TaskStatus = parse_enum("status", status, STATUSES)?;
        let (value, _) = self.move_to(reference, to, false)?;
        if self.json {
            return self.print_json(&value);
        }
        let task: Task = from_value(value)?;
        self.print(&format!("{} is now {}.\n", task.key, wire(&task.status)))
    }

    pub(crate) fn task_plan(&mut self, reference: &str) -> Result<()> {
        if self.io.stdin_is_terminal {
            return Err(Error::invalid(
                "pipe the plan on stdin: one step per line, `[x]` for done steps",
            ));
        }
        let text = read_stdin(self.io)?;
        let steps = plan::parse(&text)?;
        let (task, _) = self.task(reference)?;
        let subtasks = plan::to_subtasks(steps, self.me.id, &task.subtasks);
        let (total, done) = (subtasks.len(), subtasks.iter().filter(|s| s.done).count());
        let body = serde_json::to_value(&subtasks)
            .map_err(|e| Error::internal(format!("cannot encode the plan: {e}")))?;
        let value = self
            .client
            .put(&task_path(&task.id.0.to_string(), "/subtasks")?, &body)?;
        if self.json {
            return self.print_json(&value);
        }
        self.print(&format!(
            "{}: plan updated, {done} of {total} steps done.\n",
            task.key
        ))
    }

    /// `board submit <draft>`: the proposal a board draft asks for, as JSON on stdin. Checked
    /// here for its size and shape (a JSON object) before anything is sent; the daemon checks the
    /// rest, and creates nothing until a person reviews it.
    pub(crate) fn board_submit(&mut self, draft: &str) -> Result<()> {
        if self.io.stdin_is_terminal {
            return Err(Error::invalid(
                "pipe the proposal on stdin: {\"tasks\": [{\"title\", \"status\", \"evidence\"}], \"note\"}",
            ));
        }
        let id = draft_ref(draft)?;
        let text = read_stdin(self.io)?;
        if text.len() > MAX_PROPOSAL_BYTES {
            return Err(Error::invalid(format!(
                "the proposal is {} bytes; at most {MAX_PROPOSAL_BYTES} are accepted",
                text.len()
            )));
        }
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| Error::invalid(format!("the proposal is not JSON: {e}")))?;
        if !body.is_object() {
            return Err(Error::invalid(
                "the proposal must be a JSON object: {\"tasks\": [...], \"note\": \"...\"}",
            ));
        }
        let value = self
            .client
            .post(&format!("/board-drafts/{}/proposal", encode(&id)), &body)?;
        if self.json {
            return self.print_json(&value);
        }
        let draft: BoardDraft = from_value(value)?;
        let count = draft.proposal.as_ref().map_or(0, |p| p.tasks.len());
        self.print(&format!(
            "Proposed {count} task{} for {}. Nothing is created until a person reviews it.\n",
            if count == 1 { "" } else { "s" },
            draft.id
        ))
    }

    pub(crate) fn claim(&mut self, reference: &str) -> Result<()> {
        let (value, moved) = self.move_to(reference, TaskStatus::InProgress, true)?;
        if self.json {
            return self.print_json(&value);
        }
        let task: Task = from_value(value)?;
        if moved {
            self.print(&format!("Claimed {}: it is now in_progress.\n", task.key))
        } else {
            self.print(&format!("{} is already in_progress.\n", task.key))
        }
    }

    pub(crate) fn report(
        &mut self,
        reference: &str,
        note: Option<&str>,
        review: bool,
    ) -> Result<()> {
        if note.is_none() && !review {
            return Err(Error::invalid("give --note <text>, --review, or both"));
        }
        let key = task_ref(reference)?;
        let note = note
            .map(|n| text_arg(&[n.to_owned()], self.io))
            .transpose()?;
        let comment = match note {
            Some(text) => Some(self.client.post(
                &task_path(reference, "/comments")?,
                &json!({ "text": text, "mentions": [] }),
            )?),
            None => None,
        };
        let moved = if review {
            Some(self.move_to(reference, TaskStatus::Review, true)?)
        } else {
            None
        };
        if self.json {
            let task = moved.as_ref().map(|(v, _)| v.clone());
            return self.print_json(&json!({ "comment": comment, "task": task }));
        }
        let mut out = String::new();
        if comment.is_some() {
            let _ = writeln!(out, "Noted on {key}.");
        }
        match moved {
            Some((_, true)) => {
                let _ = writeln!(out, "{key} is now in review.");
            }
            Some((_, false)) => {
                let _ = writeln!(out, "{key} was already in review.");
            }
            None => {}
        }
        self.print(&out)
    }

    pub(crate) fn comment(
        &mut self,
        reference: &str,
        text: &[String],
        mentions: &[String],
    ) -> Result<()> {
        let key = task_ref(reference)?;
        let text = text_arg(text, self.io)?;
        let ids = mentions
            .iter()
            .map(|m| self.member_id(m))
            .collect::<Result<Vec<_>>>()?;
        let value = self.client.post(
            &task_path(reference, "/comments")?,
            &json!({ "text": text, "mentions": ids }),
        )?;
        if self.json {
            return self.print_json(&value);
        }
        let mut out = format!("Commented on {key}.");
        if !ids.is_empty() {
            let handles = ids
                .iter()
                .map(|id| self.handle(*id))
                .collect::<Result<Vec<_>>>()?;
            let _ = write!(out, " Mentioned {}.", handles.join(", "));
        }
        out.push('\n');
        self.print(&out)
    }

    pub(crate) fn ask(&mut self, args: &AskArgs) -> Result<()> {
        let to = self.member_id(&args.to)?;
        let kind: AskKind = parse_enum("kind", &args.kind, ASK_KINDS)?;
        let title = text_arg(&args.title, self.io)?;
        let mut body = json!({ "kind": kind, "to": to, "title": title });
        if let Some(text) = &args.body {
            body["body"] = json!(text);
        }
        if !args.options.is_empty() {
            body["options"] = json!(args.options);
        }
        if let Some(reference) = &args.task {
            body["task"] = json!(self.task(reference)?.0.id);
        }
        let value = self.client.post("/asks", &body)?;
        if self.json {
            return self.print_json(&value);
        }
        let ask: Ask = from_value(value)?;
        let mut out = format!(
            "Asked {} ({}): {}\n",
            self.handle(ask.to)?,
            ask.id,
            display::line(&ask.title)
        );
        if !ask.options.is_empty() {
            let _ = writeln!(out, "  options: {}", numbered(&ask.options));
        }
        self.print(&out)
    }

    pub(crate) fn reply(
        &mut self,
        ask: &str,
        text: &[String],
        option: Option<usize>,
    ) -> Result<()> {
        let id: AskId = ask
            .trim()
            .parse()
            .map_err(|_| Error::invalid(format!("{ask:?} is not an ask id (ask_…)")))?;
        if text.is_empty() && option.is_none() {
            return Err(Error::invalid("give a text, --option <n>, or both"));
        }
        let mut body = json!({});
        if let Some(n) = option {
            let index = n
                .checked_sub(1)
                .ok_or_else(|| Error::invalid("options are numbered from 1"))?;
            body["option"] = json!(index);
        }
        if !text.is_empty() {
            body["text"] = json!(text_arg(text, self.io)?);
        }
        let value = self.client.post(&format!("/asks/{}/answer", id.0), &body)?;
        if self.json {
            return self.print_json(&value);
        }
        self.print(&format!("Answered {id}.\n"))
    }

    pub(crate) fn check(&mut self) -> Result<()> {
        let me = self.me.clone();
        let asks: Vec<Ask> = from_value(self.client.get("/asks")?)?;
        let now = now_ms();
        let recent = |at: i64| now.saturating_sub(at) <= RECENT_MS;
        let mut for_me = Vec::new();
        let mut mentions = Vec::new();
        let mut waiting = Vec::new();
        let mut answered = Vec::new();
        for ask in asks {
            if ask.to == me.id {
                if ask.kind == AskKind::Mention {
                    if recent(ask.created) {
                        mentions.push(ask);
                    }
                } else if ask.state == AskState::Open {
                    for_me.push(ask);
                }
            } else if ask.from == me.id {
                match (&ask.state, &ask.answer) {
                    (AskState::Open, _) => waiting.push(ask),
                    (AskState::Answered, Some(answer)) if recent(answer.at) => answered.push(ask),
                    _ => {}
                }
            }
        }
        mentions.sort_by_key(|a| std::cmp::Reverse(a.created));

        if self.json {
            let value = json!({
                "for_me": for_me,
                "mentions": mentions,
                "waiting": waiting,
                "answered": answered,
            });
            return self.print_json(&value);
        }
        if for_me.is_empty() && mentions.is_empty() && waiting.is_empty() && answered.is_empty() {
            return self.print("Nothing needs you.\n");
        }
        let keys = if [&for_me, &mentions, &waiting, &answered]
            .iter()
            .any(|group| group.iter().any(|a| a.task.is_some()))
        {
            self.task_keys()?
        } else {
            HashMap::new()
        };
        let mut out = String::new();
        let sections: [(&str, &[Ask], bool); 4] = [
            ("For you", &for_me, true),
            ("Mentions (last 7 days)", &mentions, true),
            ("Waiting for an answer", &waiting, false),
            ("Answered (last 7 days)", &answered, false),
        ];
        for (title, group, incoming) in sections {
            if group.is_empty() {
                continue;
            }
            let _ = writeln!(out, "{title}:");
            for ask in group {
                let line = self.ask_line(ask, incoming, &keys)?;
                let _ = writeln!(out, "  {line}");
            }
        }
        self.print(&out)
    }

    /// `ask_…  question from @sam on PAP-1: Title  [1) A  2) B]` and, if answered, the answer.
    fn ask_line(
        &mut self,
        ask: &Ask,
        incoming: bool,
        keys: &HashMap<TaskId, String>,
    ) -> Result<String> {
        let who = if incoming {
            format!("from {}", self.handle(ask.from)?)
        } else {
            format!("to {}", self.handle(ask.to)?)
        };
        let mut line = format!("{}  {} {who}", ask.id, wire(&ask.kind));
        if let Some(task) = ask.task {
            let key = keys.get(&task).cloned().unwrap_or_else(|| task.to_string());
            let _ = write!(line, " on {key}");
        }
        let _ = write!(line, ": {}", display::line(&ask.title));
        if !ask.options.is_empty() {
            let _ = write!(line, "  [{}]", numbered(&ask.options));
        }
        if let Some(answer) = &ask.answer {
            let mut parts = Vec::new();
            if let Some(i) = answer.option {
                let chosen = ask.options.get(i).map_or("", String::as_str);
                parts.push(format!(
                    "{}) {}",
                    i.saturating_add(1),
                    display::line(chosen)
                ));
            }
            if let Some(text) = &answer.text {
                parts.push(format!("\"{}\"", display::line(text)));
            }
            let _ = write!(line, "\n      → {}", parts.join("; "));
        }
        Ok(line)
    }
}

/// `pitcrew ask`'s arguments.
#[derive(Clone, Debug)]
pub(crate) struct AskArgs {
    pub(crate) to: String,
    pub(crate) title: Vec<String>,
    pub(crate) options: Vec<String>,
    pub(crate) body: Option<String>,
    pub(crate) task: Option<String>,
    pub(crate) kind: String,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────────────────────

/// A task reference as the daemon takes it: a key in upper case (`pap-1` → `PAP-1`), or an id
/// (`tsk_…` or a bare ULID) as a bare ULID. Anything else is refused here, so nothing but these
/// shapes (never `.` or `..`) reaches a path.
///
/// # Errors
/// `invalid` for anything that is neither a key nor an id.
pub(crate) fn task_ref(reference: &str) -> Result<String> {
    let trimmed = reference.trim();
    let upper = trimmed.to_ascii_uppercase();
    if let Ok(key) = upper.parse::<TaskKey>() {
        return Ok(key.to_string());
    }
    if let Ok(id) = trimmed.parse::<TaskId>() {
        return Ok(id.0.to_string());
    }
    Err(Error::invalid(format!(
        "{reference:?} is not a task key (like PAP-4) or a task id (tsk_…)"
    )))
}

/// A board draft's id as the daemon takes it: a bare ULID, from `drf_…` or a bare ULID.
///
/// # Errors
/// `invalid` for anything else.
pub(crate) fn draft_ref(reference: &str) -> Result<String> {
    reference
        .trim()
        .parse::<DraftId>()
        .map(|id| id.0.to_string())
        .map_err(|_| Error::invalid(format!("{reference:?} is not a board draft id (drf_…)")))
}

fn task_path(reference: &str, rest: &str) -> Result<String> {
    Ok(format!("/tasks/{}{rest}", encode(&task_ref(reference)?)))
}

fn query_string(pairs: &[(&str, String)]) -> String {
    let mut out = String::new();
    for (i, (name, value)) in pairs.iter().enumerate() {
        out.push(if i == 0 { '?' } else { '&' });
        let _ = write!(out, "{}={}", encode(name), encode(value));
    }
    out
}

/// A value's wire name, e.g. `in_progress`.
fn wire<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

/// Parses an enum by its wire name; `In-Progress` and `in progress` work too.
fn parse_enum<T: DeserializeOwned>(what: &str, raw: &str, choices: &str) -> Result<T> {
    let name = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    serde_json::from_value(Value::String(name))
        .map_err(|_| Error::invalid(format!("unknown {what} {raw:?}; use one of: {choices}")))
}

/// The words of a text argument joined by spaces, or stdin when it is `-`.
fn text_arg(words: &[String], io: &mut Io<'_>) -> Result<String> {
    let text = if words.len() == 1 && words[0] == "-" {
        read_stdin(io)?
    } else {
        words.join(" ")
    };
    if text.trim().is_empty() {
        return Err(Error::invalid("the text is empty"));
    }
    Ok(text)
}

fn read_stdin(io: &mut Io<'_>) -> Result<String> {
    let mut text = String::new();
    io.stdin
        .take(MAX_STDIN as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|e| Error::invalid(format!("cannot read stdin: {e}")))?;
    if text.len() > MAX_STDIN {
        return Err(Error::invalid("stdin is over 1 MiB"));
    }
    Ok(text)
}

fn numbered(options: &[String]) -> String {
    options
        .iter()
        .enumerate()
        .map(|(i, o)| format!("{}) {}", i.saturating_add(1), display::line(o)))
        .collect::<Vec<_>>()
        .join("  ")
}

/// Rows as aligned columns; the last column is not padded.
fn table<const N: usize>(rows: &[[String; N]]) -> String {
    let mut widths = [0usize; N];
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == N {
                line.push_str(cell);
            } else {
                let _ = write!(line, "{cell:<width$}  ", width = widths[i]);
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_refs_are_keys_or_ids_only() {
        assert_eq!(task_ref(" pap-12 ").unwrap(), "PAP-12");
        assert_eq!(task_ref("PAP-1").unwrap(), "PAP-1");
        assert_eq!(
            task_ref("tsk_01JB000000000000000TSK0001").unwrap(),
            "01JB000000000000000TSK0001"
        );
        assert_eq!(
            task_ref("01JB000000000000000TSK0001").unwrap(),
            "01JB000000000000000TSK0001"
        );
        assert_eq!(task_path("pap-3", "/move").unwrap(), "/tasks/PAP-3/move");
        for bad in [
            ".", "..", "", " ", "a/b", "../me", "PAP", "PAP-0", "PAP-1/..", "p-1", "PAP-1?x",
            "tsk_", "%2e%2e",
        ] {
            let err = task_ref(bad).unwrap_err();
            assert_eq!(err.kind, Kind::Invalid, "{bad:?}");
            assert!(task_path(bad, "/move").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn draft_refs_are_ids_only() {
        let bare = "01J00000000000000000000000";
        assert_eq!(draft_ref(bare).unwrap(), bare);
        assert_eq!(draft_ref(&format!(" drf_{bare} ")).unwrap(), bare);
        for bad in ["", "..", "PAP-1", "drf_", "tsk_01J0", "drf_01J0000000000000000000000/x"] {
            assert!(draft_ref(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn enums_by_friendly_names() {
        let s: TaskStatus = parse_enum("status", "In-Progress", STATUSES).unwrap();
        assert_eq!(s, TaskStatus::InProgress);
        let err = parse_enum::<TaskStatus>("status", "doing", STATUSES).unwrap_err();
        assert_eq!(err.kind, Kind::Invalid);
        assert!(err.message.contains("in_progress"));
        assert_eq!(wire(&TaskStatus::InProgress), "in_progress");
    }

    #[test]
    fn queries_are_encoded() {
        let q = query_string(&[("status", "in_progress".into()), ("assignee", "a b".into())]);
        assert_eq!(q, "?status=in_progress&assignee=a%20b");
        assert_eq!(query_string(&[]), "");
    }

    #[test]
    fn tables_align() {
        let rows = [
            ["PAP-1".to_owned(), "todo".into(), "Short".into()],
            ["PAP-10".into(), "in_progress".into(), "Longer title".into()],
        ];
        assert_eq!(
            table(&rows),
            "PAP-1   todo         Short\nPAP-10  in_progress  Longer title\n"
        );
    }
}
