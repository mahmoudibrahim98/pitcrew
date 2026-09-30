//! One Codex CLI rollout record to [`TranscriptItem`]s and session facts.
//!
//! Codex writes each typed prompt twice (an `event_msg` / `user_message` and a `response_item`
//! user message) and each assistant message twice (`event_msg` / `agent_message` and a
//! `response_item` assistant message). Items come from the `response_item` side only: every Codex
//! version writes it (the oldest write nothing else), so each prompt and reply appears once and
//! parsing stays context-free. Context Codex injects as user messages is dropped by its wrapper.

use crate::bound::{
    Diff, MAX_DIFF_BYTES, MAX_ID_BYTES, MAX_INPUT_STRING_CHARS, MAX_PATH_BYTES, MAX_PLAN_ITEMS,
    MAX_TARGET_CHARS, MAX_TEXT_CHARS, MAX_TOOL_CHARS, SUMMARY_CHARS, SUMMARY_LINES, bounded,
    bounded_input, call_id, plan_item,
};
use crate::lines::SkipReason;
use crate::text::{first_line, summary, truncate_chars};
use crate::time::parse_rfc3339_ms;
use pitcrew_interfaces::source::{PlanItem, TranscriptItem};
use pitcrew_protocol::model::TimestampMs;
use serde_json::{Map, Value};

/// At most this many files are taken from one patch.
const MAX_PATCH_FILES: usize = 100;
/// The diffs of one patch share this many bytes; each file also has [`MAX_DIFF_BYTES`].
const MAX_PATCH_DIFF_BYTES: usize = 256 * 1024;

/// Text Codex writes into user messages that a person did not type.
const INJECTED_PREFIXES: &[&str] = &[
    "<environment_context>",
    "<user_instructions>",
    "<INSTRUCTIONS>",
    "# AGENTS.md instructions for ",
    "<user_shell_command>",
    "<turn_aborted>",
];

/// What one record contributes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodexRecord {
    /// Items, in order.
    pub items: Vec<TranscriptItem>,
    /// Session facts found on the record.
    pub facts: RecordFacts,
}

/// Session facts one record carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordFacts {
    /// `session_meta.id`.
    pub session_id: Option<String>,
    /// `session_meta.cwd`, or a `turn_context`'s.
    pub cwd: Option<String>,
    /// `session_meta.git.branch`.
    pub branch: Option<String>,
    /// `turn_context.model`.
    pub model: Option<String>,
    /// The session's start for `session_meta`, else the record's time.
    pub timestamp: Option<TimestampMs>,
    /// Whether `session_meta.source` names a sub-agent.
    pub is_subagent: Option<bool>,
}

/// Parses one line of a Codex rollout; `offset` is the line's byte offset.
///
/// Blank lines and records of unknown or wrong shape give no items. Only lines that are not
/// UTF-8, not JSON, or not a JSON object are errors, and callers skip them.
///
/// # Errors
///
/// The reason the line cannot be used.
pub fn parse_line(line: &[u8], offset: u64) -> Result<CodexRecord, SkipReason> {
    let text = std::str::from_utf8(line).map_err(|_| SkipReason::InvalidUtf8)?;
    if text.trim().is_empty() {
        return Ok(CodexRecord::default());
    }
    let value: Value =
        serde_json::from_str(text).map_err(|e| SkipReason::Malformed(e.to_string()))?;
    let Value::Object(rec) = value else {
        return Err(SkipReason::Malformed("not a JSON object".into()));
    };

    let ts = str_at(&rec, "timestamp").and_then(parse_rfc3339_ms);
    let at = ts.unwrap_or(0);
    let payload = rec.get("payload").and_then(Value::as_object);
    let mut out = CodexRecord::default();
    match (str_at(&rec, "type"), payload) {
        (Some("session_meta"), Some(p)) => out.facts = session_facts(p),
        (Some("turn_context"), Some(p)) => {
            out.facts.model = bounded(str_at(p, "model"), MAX_ID_BYTES);
            out.facts.cwd = bounded(str_at(p, "cwd"), MAX_PATH_BYTES);
        }
        (Some("response_item"), Some(p)) => response_item(p, at, offset, &mut out.items),
        (Some("event_msg"), Some(p)) => {
            if matches!(str_at(p, "type"), Some("task_complete" | "turn_aborted")) {
                out.items.push(TranscriptItem::TurnEnded { at, offset });
            }
        }
        (Some("session_meta" | "turn_context" | "response_item" | "event_msg"), None) => {}
        // Older rollouts: items unwrapped, after a header line with no type.
        (Some(_), _) => response_item(&rec, at, offset, &mut out.items),
        (None, _) if rec.get("id").is_some_and(Value::is_string) => {
            out.facts = session_facts(&rec);
        }
        (None, _) => {}
    }
    out.facts.timestamp = out.facts.timestamp.or(ts);
    Ok(out)
}

fn str_at<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(Value::as_str)
}

fn session_facts(p: &Map<String, Value>) -> RecordFacts {
    RecordFacts {
        session_id: bounded(str_at(p, "id"), MAX_ID_BYTES),
        cwd: bounded(str_at(p, "cwd"), MAX_PATH_BYTES),
        branch: bounded(
            p.get("git")
                .and_then(|g| g.get("branch"))
                .and_then(Value::as_str),
            MAX_ID_BYTES,
        ),
        model: None,
        timestamp: str_at(p, "timestamp").and_then(parse_rfc3339_ms),
        // `"cli"`, `"exec"`, ... or `{"subagent": ...}`.
        is_subagent: p.get("source").map(|s| s.get("subagent").is_some()),
    }
}

fn response_item(
    p: &Map<String, Value>,
    at: TimestampMs,
    offset: u64,
    out: &mut Vec<TranscriptItem>,
) {
    match str_at(p, "type") {
        Some("message") => message(p, at, offset, out),
        Some("function_call") => {
            let (Some(name), Some(id)) = (str_at(p, "name"), str_at(p, "call_id")) else {
                return;
            };
            match p.get("arguments") {
                Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
                    Ok(v) => tool_call(name, id, Args::Json(&v), at, offset, out),
                    Err(_) => tool_call(name, id, Args::Text(s), at, offset, out),
                },
                Some(v) => tool_call(name, id, Args::Json(v), at, offset, out),
                None => tool_call(name, id, Args::Json(&Value::Null), at, offset, out),
            }
        }
        Some("custom_tool_call") => {
            let (Some(name), Some(id), Some(input)) =
                (str_at(p, "name"), str_at(p, "call_id"), str_at(p, "input"))
            else {
                return;
            };
            tool_call(name, id, Args::Text(input), at, offset, out);
        }
        Some("local_shell_call") => {
            let Some(id) = str_at(p, "call_id").or_else(|| str_at(p, "id")) else {
                return;
            };
            let action = p.get("action").unwrap_or(&Value::Null);
            tool_call("local_shell", id, Args::Json(action), at, offset, out);
        }
        Some("function_call_output" | "custom_tool_call_output") => {
            let Some(id) = str_at(p, "call_id") else {
                return;
            };
            let (summary, is_error) = output_summary(p.get("output"));
            out.push(TranscriptItem::ToolResult {
                at,
                call_id: call_id(id),
                is_error,
                summary,
                offset,
            });
        }
        _ => {}
    }
}

fn message(p: &Map<String, Value>, at: TimestampMs, offset: u64, out: &mut Vec<TranscriptItem>) {
    let texts: Vec<&str> = match p.get("content") {
        Some(Value::String(s)) => vec![s],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| {
                matches!(
                    b.get("type").and_then(Value::as_str),
                    Some("input_text" | "output_text" | "text")
                )
            })
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect(),
        _ => return,
    };
    match str_at(p, "role") {
        Some("user") => {
            let typed: Vec<&str> = texts
                .into_iter()
                .map(str::trim)
                .filter(|t| !t.is_empty() && !is_injected(t))
                .collect();
            if !typed.is_empty() {
                out.push(TranscriptItem::UserPrompt {
                    at,
                    text: truncate_chars(&typed.join("\n\n"), MAX_TEXT_CHARS),
                    offset,
                });
            }
        }
        Some("assistant") => {
            for text in texts.into_iter().filter(|t| !t.trim().is_empty()) {
                out.push(TranscriptItem::AssistantText {
                    at,
                    text: truncate_chars(text, MAX_TEXT_CHARS),
                    offset,
                });
            }
        }
        _ => {}
    }
}

/// Injected context: a known prefix, or text wholly wrapped in one tag whose name looks like
/// Codex's (`<snake_case>` or `<UPPER>`), which a person would rarely type.
fn is_injected(text: &str) -> bool {
    if INJECTED_PREFIXES.iter().any(|p| text.starts_with(p)) {
        return true;
    }
    let Some(rest) = text.strip_prefix('<') else {
        return false;
    };
    let Some(name) = rest.split_once('>').map(|(n, _)| n) else {
        return false;
    };
    let codex_like = !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_')
        && (name.contains('_') || name.bytes().all(|b| b.is_ascii_uppercase()));
    codex_like && text.ends_with(&format!("</{name}>"))
}

/// A tool call's arguments: parsed JSON, or text (a custom tool's input, or arguments that are
/// not JSON).
#[derive(Clone, Copy)]
enum Args<'a> {
    Json(&'a Value),
    Text(&'a str),
}

fn tool_call(
    tool: &str,
    id: &str,
    args: Args<'_>,
    at: TimestampMs,
    offset: u64,
    out: &mut Vec<TranscriptItem>,
) {
    let patch = patch_text(tool, args).map(Patch::parse);
    let files = patch.as_ref().map_or(&[][..], |p| &p.files[..]);
    let target = if files.is_empty() {
        target(tool, args)
    } else {
        files_target(files)
    };
    let input = match args {
        Args::Json(Value::Null) => None,
        Args::Json(v) => Some(bounded_input(v)),
        Args::Text(s) => Some(Value::String(truncate_chars(s, MAX_INPUT_STRING_CHARS))),
    };
    out.push(TranscriptItem::ToolUse {
        at,
        call_id: call_id(id),
        tool: truncate_chars(tool, MAX_TOOL_CHARS),
        target,
        input,
        offset,
    });
    if tool == "update_plan"
        && let Args::Json(v) = args
        && let Some(items) = plan(v)
    {
        out.push(TranscriptItem::PlanUpdated { at, items, offset });
    }
    if let Some(patch) = patch {
        patch.file_edits(at, offset, out);
    }
}

/// The patch a call applies: `apply_patch`'s input, or a patch passed to `apply_patch` through a
/// shell command (as older Codex versions did).
fn patch_text<'a>(tool: &str, args: Args<'a>) -> Option<&'a str> {
    match (tool, args) {
        ("apply_patch", Args::Text(s)) => Some(s),
        ("apply_patch", Args::Json(v)) => v
            .get("input")
            .or_else(|| v.get("patch"))
            .and_then(Value::as_str)
            .or_else(|| v.as_str()),
        (_, Args::Json(v)) => {
            let parts = command_parts(v.get("command").or_else(|| v.get("cmd"))?);
            if !parts
                .iter()
                .any(|p| p.contains("apply_patch") || p.contains("applypatch"))
            {
                return None;
            }
            parts
                .iter()
                .find_map(|p| p.find("*** Begin Patch").map(|i| &p[i..]))
        }
        (_, Args::Text(_)) => None,
    }
}

/// The strings of a command: an array's elements, or a single string.
fn command_parts(cmd: &Value) -> Vec<&str> {
    match cmd {
        Value::String(s) => vec![s],
        Value::Array(parts) => parts.iter().filter_map(Value::as_str).take(64).collect(),
        _ => Vec::new(),
    }
}

/// A short description of what a tool call acts on.
fn target(tool: &str, args: Args<'_>) -> String {
    let v = match args {
        Args::Text(s) => return first_line(s, MAX_TARGET_CHARS),
        Args::Json(v) => v,
    };
    if tool == "update_plan" {
        let n = v.get("plan").and_then(Value::as_array).map_or(0, Vec::len);
        return format!("{n} steps");
    }
    if let Some(cmd) = v.get("command").or_else(|| v.get("cmd"))
        && let Some(shown) = command_display(cmd)
    {
        return shown;
    }
    let found = [
        "path",
        "file_path",
        "pattern",
        "url",
        "query",
        "description",
        "prompt",
        "input",
    ]
    .into_iter()
    .find_map(|k| v.get(k).and_then(Value::as_str));
    first_line(found.unwrap_or(""), MAX_TARGET_CHARS)
}

/// A command for display: a string as is, an array joined with shell quoting.
fn command_display(cmd: &Value) -> Option<String> {
    match cmd {
        Value::String(s) => Some(first_line(s, MAX_TARGET_CHARS)),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts.iter().filter_map(Value::as_str) {
                if out.len() > MAX_TARGET_CHARS * 4 {
                    break;
                }
                if !out.is_empty() {
                    out.push(' ');
                }
                push_quoted(&mut out, &truncate_chars(part, MAX_TARGET_CHARS));
            }
            Some(first_line(&out, MAX_TARGET_CHARS))
        }
        _ => None,
    }
}

fn push_quoted(out: &mut String, part: &str) {
    let plain = !part.is_empty()
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b));
    if plain {
        out.push_str(part);
    } else {
        out.push('\'');
        out.push_str(&part.replace('\'', r"'\''"));
        out.push('\'');
    }
}

fn files_target(files: &[PatchFile<'_>]) -> String {
    let first = first_line(files[0].dest(), MAX_TARGET_CHARS);
    match files.len() {
        1 => first,
        n => format!("{first} (+{} more)", n - 1),
    }
}

fn plan(args: &Value) -> Option<Vec<PlanItem>> {
    let steps = args.get("plan")?.as_array()?;
    Some(
        steps
            .iter()
            .filter_map(|s| {
                let text = s.get("step").and_then(Value::as_str)?;
                Some(plan_item(text, s.get("status").and_then(Value::as_str)))
            })
            .take(MAX_PLAN_ITEMS)
            .collect(),
    )
}

/// A tool output's summary and whether it reports an error.
///
/// Shapes seen: a JSON string `{"output": ..., "metadata": {"exit_code": ...}}`; plain text,
/// possibly starting `Exit code: N` with the output after `Output:`; an object with `content`
/// and `success`; or an array of content items.
fn output_summary(output: Option<&Value>) -> (String, bool) {
    match output {
        Some(Value::String(s)) => {
            if s.trim_start().starts_with('{')
                && let Ok(Value::Object(o)) = serde_json::from_str::<Value>(s)
            {
                return object_summary(&o, s);
            }
            text_summary(s)
        }
        Some(Value::Object(o)) => object_summary(o, ""),
        Some(Value::Array(items)) => (short(&content_text(items)), false),
        _ => (String::new(), false),
    }
}

fn object_summary(o: &Map<String, Value>, raw: &str) -> (String, bool) {
    let exit = o
        .get("metadata")
        .and_then(|m| m.get("exit_code"))
        .and_then(Value::as_i64);
    let success = o.get("success").and_then(Value::as_bool);
    let is_error = exit.is_some_and(|c| c != 0) || success == Some(false);
    let text = match o.get("output").or_else(|| o.get("content")) {
        Some(Value::String(s)) => short(s),
        Some(Value::Array(items)) => short(&content_text(items)),
        _ => short(raw),
    };
    (text, is_error)
}

fn text_summary(s: &str) -> (String, bool) {
    let Some(rest) = s.strip_prefix("Exit code: ") else {
        return (short(s), false);
    };
    let exit = rest
        .lines()
        .next()
        .and_then(|l| l.trim().parse::<i64>().ok());
    let body = s.find("\nOutput:\n").map_or(s, |i| &s[i + 9..]);
    (short(body), exit.is_some_and(|c| c != 0))
}

fn content_text(items: &[Value]) -> String {
    items
        .iter()
        .filter_map(|i| i.get("text").and_then(Value::as_str))
        .take(100)
        .map(|t| truncate_chars(t, SUMMARY_CHARS))
        .collect::<Vec<_>>()
        .join("\n")
}

fn short(s: &str) -> String {
    summary(s, SUMMARY_LINES, SUMMARY_CHARS)
}

/// What a patch does to one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Add,
    Delete,
    Update,
}

/// One file's section of a patch. The body is kept as a byte range of the patch text, so a large
/// patch is not copied while it is counted.
#[derive(Debug)]
struct PatchFile<'a> {
    change: Change,
    path: &'a str,
    move_to: Option<&'a str>,
    body: (usize, usize),
    added: u32,
    removed: u32,
}

impl PatchFile<'_> {
    fn dest(&self) -> &str {
        self.move_to.unwrap_or(self.path)
    }
}

/// A Codex patch (`*** Begin Patch` ... `*** End Patch`), split into files.
#[derive(Debug)]
struct Patch<'a> {
    text: &'a str,
    files: Vec<PatchFile<'a>>,
}

impl<'a> Patch<'a> {
    /// Anything that does not start with `*** Begin Patch` has no files. Lines outside a file
    /// section are ignored, and a patch without `*** End Patch` keeps the files it has.
    fn parse(text: &'a str) -> Self {
        let mut files = Vec::new();
        let mut cur: Option<PatchFile<'a>> = None;
        let mut began = false;
        let mut pos = 0;
        for raw in text.split_inclusive('\n') {
            pos += raw.len();
            let line = raw.trim_end_matches(['\n', '\r']);
            if !began {
                match line.trim() {
                    "" => continue,
                    "*** Begin Patch" => {
                        began = true;
                        continue;
                    }
                    _ => break,
                }
            }
            if line.trim_end() == "*** End Patch" {
                break;
            }
            let header = [
                ("*** Add File: ", Change::Add),
                ("*** Delete File: ", Change::Delete),
                ("*** Update File: ", Change::Update),
            ]
            .into_iter()
            .find_map(|(prefix, change)| line.strip_prefix(prefix).map(|p| (change, p.trim())));
            if let Some((change, path)) = header {
                files.extend(cur.take());
                if files.len() >= MAX_PATCH_FILES {
                    break;
                }
                cur = (!path.is_empty()).then_some(PatchFile {
                    change,
                    path,
                    move_to: None,
                    body: (pos, pos),
                    added: 0,
                    removed: 0,
                });
                continue;
            }
            let Some(file) = cur.as_mut() else { continue };
            if let Some(to) = line.strip_prefix("*** Move to: ") {
                let to = to.trim();
                if file.body.0 == file.body.1 && file.change == Change::Update && !to.is_empty() {
                    file.move_to = Some(to);
                    file.body = (pos, pos);
                }
                continue;
            }
            match line.as_bytes().first() {
                Some(b'+') => file.added = file.added.saturating_add(1),
                Some(b'-') => file.removed = file.removed.saturating_add(1),
                _ => {}
            }
            file.body.1 = pos;
        }
        files.extend(cur);
        Self { text, files }
    }

    /// One `FileEdit` per file, the diffs sharing [`MAX_PATCH_DIFF_BYTES`].
    fn file_edits(&self, at: TimestampMs, offset: u64, out: &mut Vec<TranscriptItem>) {
        let mut budget = MAX_PATCH_DIFF_BYTES;
        for file in &self.files {
            let path = truncate_chars(file.path, MAX_PATH_BYTES);
            let dest = truncate_chars(file.dest(), MAX_PATH_BYTES);
            let diff = (budget > 0).then(|| {
                let (old, new) = match file.change {
                    Change::Add => ("/dev/null", dest.as_str()),
                    Change::Delete => (path.as_str(), "/dev/null"),
                    Change::Update => (path.as_str(), dest.as_str()),
                };
                let mut diff = Diff::new(old, new, budget.min(MAX_DIFF_BYTES));
                if file.change == Change::Add {
                    diff.push(&format!("@@ -0,0 +1,{} @@", file.added));
                }
                let body = self.text.get(file.body.0..file.body.1).unwrap_or("");
                for line in body.lines().filter(|l| !l.starts_with("*** ")) {
                    diff.push(line);
                }
                let diff = diff.finish();
                budget = budget.saturating_sub(diff.len());
                diff
            });
            out.push(TranscriptItem::FileEdit {
                at,
                path: dest,
                added: file.added,
                removed: file.removed,
                diff,
                offset,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn items(v: &Value) -> Vec<TranscriptItem> {
        parse_line(v.to_string().as_bytes(), 7)
            .expect("parses")
            .items
    }

    fn item(payload: Value) -> Value {
        json!({"timestamp": "1970-01-01T00:00:01Z", "type": "response_item", "payload": payload})
    }

    fn edits(got: &[TranscriptItem]) -> Vec<(String, u32, u32, Option<String>)> {
        got.iter()
            .filter_map(|i| match i {
                TranscriptItem::FileEdit {
                    path,
                    added,
                    removed,
                    diff,
                    ..
                } => Some((path.clone(), *added, *removed, diff.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn prompts_and_replies_come_from_response_items_only() {
        let user =
            json!({"type": "event_msg", "payload": {"type": "user_message", "message": "hi"}});
        let agent =
            json!({"type": "event_msg", "payload": {"type": "agent_message", "message": "yo"}});
        assert!(items(&user).is_empty());
        assert!(items(&agent).is_empty());
        let got = items(&item(
            json!({"type": "message", "role": "user", "content": [
            {"type": "input_text", "text": "<environment_context>\n  <cwd>/w</cwd>\n</environment_context>"},
            {"type": "input_text", "text": "  fix it  "}]}),
        ));
        assert_eq!(
            got,
            vec![TranscriptItem::UserPrompt {
                at: 1000,
                text: "fix it".into(),
                offset: 7
            }]
        );
    }

    #[test]
    fn injected_context_is_not_a_prompt() {
        for text in [
            "<user_instructions>be brief</user_instructions>",
            "# AGENTS.md instructions for /w\n\n<INSTRUCTIONS>x</INSTRUCTIONS>",
            "<some_new_wrapper>\nstuff\n</some_new_wrapper>",
            "<NOTE>x</NOTE>",
        ] {
            let v = item(json!({"type": "message", "role": "user",
                                "content": [{"type": "input_text", "text": text}]}));
            assert!(items(&v).is_empty(), "{text}");
        }
        for text in ["<b>bold</b> please", "use <div>x</div>", "x < y"] {
            let v = item(json!({"type": "message", "role": "user",
                                "content": [{"type": "input_text", "text": text}]}));
            assert_eq!(items(&v).len(), 1, "{text}");
        }
        let dev = item(json!({"type": "message", "role": "developer",
                              "content": [{"type": "input_text", "text": "rules"}]}));
        assert!(items(&dev).is_empty());
    }

    #[test]
    fn shell_target_and_exit_codes() {
        let call = item(
            json!({"type": "function_call", "name": "shell", "call_id": "c1",
            "arguments": json!({"command": ["bash", "-lc", "echo 'a b'"], "workdir": "/w"}).to_string()}),
        );
        let [TranscriptItem::ToolUse { target, input, .. }] = &items(&call)[..] else {
            panic!("one tool use");
        };
        assert_eq!(target, r"bash -lc 'echo '\''a b'\'''");
        assert_eq!(input.as_ref().map(|i| &i["workdir"]), Some(&json!("/w")));

        let result = |output: Value| {
            let v =
                item(json!({"type": "function_call_output", "call_id": "c1", "output": output}));
            match &items(&v)[..] {
                [
                    TranscriptItem::ToolResult {
                        is_error,
                        summary,
                        call_id,
                        ..
                    },
                ] => {
                    assert_eq!(call_id, "c1");
                    (summary.clone(), *is_error)
                }
                other => panic!("{other:?}"),
            }
        };
        let ok = json!({"output": "done\n", "metadata": {"exit_code": 0}}).to_string();
        assert_eq!(result(json!(ok)), ("done".into(), false));
        let bad = json!({"output": "boom", "metadata": {"exit_code": 2}}).to_string();
        assert_eq!(result(json!(bad)), ("boom".into(), true));
        assert_eq!(
            result(json!(
                "Exit code: 1\nWall time: 0.1 seconds\nOutput:\nnope\n"
            )),
            ("nope".into(), true)
        );
        assert_eq!(
            result(json!("Plan updated")),
            ("Plan updated".into(), false)
        );
        assert_eq!(
            result(json!({"content": "denied", "success": false})),
            ("denied".into(), true)
        );
        assert_eq!(result(json!("{not json")), ("{not json".into(), false));
    }

    #[test]
    fn apply_patch_gives_one_edit_per_file() {
        let patch = "*** Begin Patch\n\
            *** Add File: new.txt\n+one\n+two\n\
            *** Update File: src/a.rs\n*** Move to: src/b.rs\n@@ fn main\n-old\n+new\n context\n*** End of File\n\
            *** Delete File: gone.md\n\
            *** End Patch";
        let call = item(json!({"type": "custom_tool_call", "name": "apply_patch",
                               "call_id": "p1", "input": patch}));
        let got = items(&call);
        let TranscriptItem::ToolUse { target, .. } = &got[0] else {
            panic!("tool use first");
        };
        assert_eq!(target, "new.txt (+2 more)");
        assert_eq!(
            edits(&got),
            vec![
                (
                    "new.txt".into(),
                    2,
                    0,
                    Some("--- /dev/null\n+++ new.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n".into())
                ),
                (
                    "src/b.rs".into(),
                    1,
                    1,
                    Some("--- src/a.rs\n+++ src/b.rs\n@@ fn main\n-old\n+new\n context\n".into())
                ),
                (
                    "gone.md".into(),
                    0,
                    0,
                    Some("--- gone.md\n+++ /dev/null\n".into())
                ),
            ]
        );
    }

    #[test]
    fn a_patch_through_the_shell_is_an_edit_too() {
        let script = "apply_patch <<'EOF'\n*** Begin Patch\n*** Update File: x\n@@\n-a\n+b\n*** End Patch\nEOF\n";
        let call = item(
            json!({"type": "function_call", "name": "shell", "call_id": "s",
            "arguments": json!({"command": ["bash", "-lc", script]}).to_string()}),
        );
        let got = items(&call);
        assert_eq!(edits(&got).len(), 1);
        assert!(matches!(&got[0], TranscriptItem::ToolUse { target, .. } if target == "x"));

        let two = item(
            json!({"type": "function_call", "name": "shell", "call_id": "s",
            "arguments": json!({"command": ["apply_patch", "*** Begin Patch\n*** Add File: y\n+1\n*** End Patch"]}).to_string()}),
        );
        assert_eq!(edits(&items(&two))[0].0, "y");
    }

    #[test]
    fn malformed_patches_give_no_edits_or_what_they_can() {
        for patch in [
            "",
            "just text",
            "*** Update File: x\n-a\n+b\n",
            "*** Begin Patch\n+orphan line\n*** End Patch",
            "*** Begin Patch\n*** Update File:   \n+a\n*** End Patch",
        ] {
            let call = item(json!({"type": "custom_tool_call", "name": "apply_patch",
                                   "call_id": "p", "input": patch}));
            let got = items(&call);
            assert_eq!(got.len(), 1, "{patch:?}");
            assert!(edits(&got).is_empty(), "{patch:?}");
        }
        // No end marker: the files so far are kept.
        let cut = item(
            json!({"type": "custom_tool_call", "name": "apply_patch", "call_id": "p",
                              "input": "*** Begin Patch\n*** Update File: a\n+x\n*** Update File: b\n-y"}),
        );
        let got = edits(&items(&cut));
        assert_eq!(
            got.iter()
                .map(|e| (e.0.as_str(), e.1, e.2))
                .collect::<Vec<_>>(),
            [("a", 1, 0), ("b", 0, 1)]
        );
    }

    #[test]
    fn big_patches_are_capped() {
        let mut patch = String::from("*** Begin Patch\n");
        for f in 0..MAX_PATCH_FILES + 20 {
            patch.push_str(&format!("*** Update File: f{f}\n@@\n"));
            for _ in 0..500 {
                patch.push_str("+a fairly long line of added text, repeated many times\n");
            }
        }
        patch.push_str("*** End Patch");
        let call = item(json!({"type": "custom_tool_call", "name": "apply_patch",
                               "call_id": "p", "input": patch}));
        let got = edits(&items(&call));
        assert_eq!(got.len(), MAX_PATCH_FILES);
        assert!(got.iter().all(|e| e.1 == 500));
        let total: usize = got
            .iter()
            .filter_map(|e| e.3.as_ref())
            .map(String::len)
            .sum();
        assert!(
            total <= MAX_PATCH_DIFF_BYTES + MAX_PATCH_FILES * 64,
            "{total}"
        );
        assert!(
            got.iter()
                .filter_map(|e| e.3.as_ref())
                .all(|d| d.len() <= MAX_DIFF_BYTES + 64)
        );
    }

    #[test]
    fn update_plan_and_turn_ends() {
        let call = item(
            json!({"type": "function_call", "name": "update_plan", "call_id": "u",
            "arguments": json!({"plan": [{"step": "a", "status": "completed"}, {"step": "b", "status": "in_progress"}, {"status": "pending"}]}).to_string()}),
        );
        let got = items(&call);
        assert!(matches!(&got[0], TranscriptItem::ToolUse { target, .. } if target == "3 steps"));
        let TranscriptItem::PlanUpdated { items: plan, .. } = &got[1] else {
            panic!("plan second");
        };
        assert_eq!(plan.len(), 2);

        for kind in ["task_complete", "turn_aborted"] {
            let v = json!({"type": "event_msg", "payload": {"type": kind}});
            assert!(matches!(&items(&v)[..], [TranscriptItem::TurnEnded { .. }]));
        }
        for kind in ["token_count", "agent_reasoning"] {
            let v = json!({"type": "event_msg", "payload": {"type": kind}});
            assert!(items(&v).is_empty());
        }
        let reasoning = item(
            json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": "hmm"}]}),
        );
        assert!(items(&reasoning).is_empty());
    }

    #[test]
    fn wrong_shapes_give_no_items() {
        for v in [
            json!({"type": "response_item", "payload": 5}),
            json!({"type": "response_item", "payload": {"type": "function_call", "name": 1}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": 3}}),
            json!({"type": "response_item", "payload": {"type": "function_call_output", "output": "x"}}),
            json!({"type": "session_meta", "payload": []}),
            json!({"type": ["x"]}),
            json!({}),
        ] {
            assert!(items(&v).is_empty(), "{v}");
        }
        assert!(matches!(
            parse_line(b"[1]", 0),
            Err(SkipReason::Malformed(_))
        ));
        assert_eq!(
            parse_line(b"{\"a\":\"\xff\"}", 0),
            Err(SkipReason::InvalidUtf8)
        );
    }

    #[test]
    fn older_unwrapped_rollouts_parse() {
        let header = json!({"id": "legacy-id", "timestamp": "2025-01-01T00:00:00Z",
                            "git": {"branch": "dev"}});
        let facts = parse_line(header.to_string().as_bytes(), 0)
            .expect("parses")
            .facts;
        assert_eq!(facts.session_id.as_deref(), Some("legacy-id"));
        assert_eq!(facts.branch.as_deref(), Some("dev"));
        let msg = json!({"type": "message", "role": "assistant",
                         "content": [{"type": "output_text", "text": "hello"}]});
        assert!(
            matches!(&items(&msg)[..], [TranscriptItem::AssistantText { text, .. }] if text == "hello")
        );
    }

    #[test]
    fn oversized_facts_are_dropped() {
        let long = "f".repeat(10_000);
        let v = json!({"type": "session_meta", "payload": {"id": long, "cwd": "/w", "git": {"branch": long},
                       "source": {"subagent": "review"}}});
        let facts = parse_line(v.to_string().as_bytes(), 0)
            .expect("parses")
            .facts;
        assert_eq!((facts.session_id, facts.branch), (None, None));
        assert_eq!(facts.cwd.as_deref(), Some("/w"));
        assert_eq!(facts.is_subagent, Some(true));
    }
}
