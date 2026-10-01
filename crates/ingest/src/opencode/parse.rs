//! One OpenCode part (a row of the `part` table) to [`TranscriptItem`]s.
//!
//! Parts change while a turn streams: text grows until `time.end` is set, and a tool part goes
//! `pending` → `running` → `completed` or `error`. So a part gives items in up to two phases:
//! - the **head**, once its input is known (a running tool's `ToolUse`, and the `PlanUpdated` or
//!   `Question` it carries);
//! - the **tail**, once it is final (the `ToolResult` and any `FileEdit`s).
//!
//! Parts without a running phase (text, turn ends) give everything in the head once final.
//! Parsing is context-free apart from the owning message's facts ([`MessageInfo`]).

use crate::bound::{
    Diff, MAX_DIFF_BYTES, MAX_ID_BYTES, MAX_PATH_BYTES, MAX_PLAN_ITEMS, MAX_TARGET_CHARS,
    MAX_TEXT_CHARS, MAX_TOOL_CHARS, SUMMARY_CHARS, SUMMARY_LINES, bounded, bounded_input, call_id,
    capped_diff, plan_item,
};
use crate::lines::SkipReason;
use crate::patch::Patch;
use crate::text::{first_line, summary, truncate_chars};
use pitcrew_interfaces::source::{PlanItem, TranscriptItem};
use pitcrew_protocol::model::TimestampMs;
use serde_json::{Map, Value};

/// Questions: count per call, text, option count and option label length.
const MAX_QUESTIONS: usize = 20;
const MAX_QUESTION_CHARS: usize = 4000;
const MAX_OPTIONS: usize = 50;
const MAX_OPTION_CHARS: usize = 200;

/// Who wrote a message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Role {
    /// A person.
    User,
    /// The agent.
    Assistant,
    /// Missing or unreadable.
    #[default]
    Unknown,
}

/// What a part needs to know about its message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageInfo {
    /// Who wrote it.
    pub role: Role,
    /// Whether the message can no longer change: it completed or failed, or a newer assistant
    /// message exists in the session. Unfinished parts of a settled message are final as they
    /// are.
    pub settled: bool,
    /// The model, for assistant messages.
    pub model: Option<String>,
}

/// How far along a part is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Nothing to show yet.
    Waiting,
    /// The head can be shown; the tail will follow.
    Started,
    /// Final: head and tail.
    Done,
}

/// What one part gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartItems {
    /// How far along the part is.
    pub phase: Phase,
    /// Items shown once started.
    pub head: Vec<TranscriptItem>,
    /// Items shown once done.
    pub tail: Vec<TranscriptItem>,
}

impl PartItems {
    fn done(head: Vec<TranscriptItem>) -> Self {
        Self {
            phase: Phase::Done,
            head,
            tail: Vec::new(),
        }
    }

    fn waiting() -> Self {
        Self {
            phase: Phase::Waiting,
            head: Vec::new(),
            tail: Vec::new(),
        }
    }

    /// The items a reader of the current state shows: head and tail when done, the head when
    /// started, nothing while waiting.
    #[must_use]
    pub fn visible(self) -> Vec<TranscriptItem> {
        match self.phase {
            Phase::Waiting => Vec::new(),
            Phase::Started => self.head,
            Phase::Done => {
                let mut all = self.head;
                all.extend(self.tail);
                all
            }
        }
    }
}

/// Parses one part's `data`. `offset` is the part's position (see the module docs of
/// [`crate::opencode`]) and `created` its creation time, used where the payload has none.
///
/// # Errors
///
/// The reason the payload cannot be used: not UTF-8, not JSON, or not a JSON object.
pub fn parse_part(
    data: &[u8],
    msg: &MessageInfo,
    offset: u64,
    created: TimestampMs,
) -> Result<PartItems, SkipReason> {
    let text = std::str::from_utf8(data).map_err(|_| SkipReason::InvalidUtf8)?;
    let value: Value =
        serde_json::from_str(text).map_err(|e| SkipReason::Malformed(e.to_string()))?;
    let Value::Object(part) = value else {
        return Err(SkipReason::Malformed("not a JSON object".into()));
    };
    Ok(match str_at(&part, "type") {
        Some("text") => text_part(&part, msg, offset, created),
        Some("tool") => tool_part(&part, msg, offset, created),
        Some("step-finish") => {
            // Every step ends with one; only the last step of a turn has a reason other than
            // `tool-calls`.
            let reason = str_at(&part, "reason");
            if matches!(reason, Some("tool-calls" | "tool_calls")) {
                PartItems::done(Vec::new())
            } else {
                PartItems::done(vec![TranscriptItem::TurnEnded {
                    at: created,
                    offset,
                }])
            }
        }
        _ => PartItems::done(Vec::new()),
    })
}

fn str_at<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(Value::as_str)
}

fn int_at(v: Option<&Value>, key: &str) -> Option<TimestampMs> {
    v.and_then(|v| v.get(key)).and_then(Value::as_i64)
}

fn text_part(
    part: &Map<String, Value>,
    msg: &MessageInfo,
    offset: u64,
    created: TimestampMs,
) -> PartItems {
    let flag = |k: &str| part.get(k).and_then(Value::as_bool).unwrap_or(false);
    let text = str_at(part, "text").unwrap_or("");
    let at = int_at(part.get("time"), "start").unwrap_or(created);
    match msg.role {
        // Written whole; synthetic and ignored parts are context OpenCode adds.
        Role::User => {
            let text = text.trim();
            if flag("synthetic") || flag("ignored") || text.is_empty() {
                return PartItems::done(Vec::new());
            }
            PartItems::done(vec![TranscriptItem::UserPrompt {
                at,
                text: truncate_chars(text, MAX_TEXT_CHARS),
                offset,
            }])
        }
        Role::Assistant | Role::Unknown => {
            let ended = int_at(part.get("time"), "end").is_some();
            if !ended && !msg.settled {
                return PartItems::waiting();
            }
            if text.trim().is_empty() {
                return PartItems::done(Vec::new());
            }
            PartItems::done(vec![TranscriptItem::AssistantText {
                at,
                text: truncate_chars(text, MAX_TEXT_CHARS),
                offset,
            }])
        }
    }
}

fn tool_part(
    part: &Map<String, Value>,
    msg: &MessageInfo,
    offset: u64,
    created: TimestampMs,
) -> PartItems {
    let (Some(tool), Some(id)) = (str_at(part, "tool"), str_at(part, "callID")) else {
        return PartItems::done(Vec::new());
    };
    let state = part.get("state").and_then(Value::as_object);
    let status = state.and_then(|s| str_at(s, "status"));
    let input = state.and_then(|s| s.get("input")).unwrap_or(&Value::Null);
    let at = int_at(state.and_then(|s| s.get("time")), "start").unwrap_or(created);
    let call_id = call_id(id);

    // A pending tool's input may still be streaming, so it shows nothing until it runs.
    let known = matches!(status, Some("running" | "completed" | "error"));
    if !known && !msg.settled {
        return PartItems::waiting();
    }
    let patch = matches!(tool, "apply_patch" | "patch")
        .then(|| {
            input
                .get("patchText")
                .or_else(|| input.get("patch"))
                .or_else(|| input.get("input"))
                .and_then(Value::as_str)
        })
        .flatten()
        .map(Patch::parse);

    let mut head = vec![TranscriptItem::ToolUse {
        at,
        call_id: call_id.clone(),
        tool: truncate_chars(tool, MAX_TOOL_CHARS),
        target: match &patch {
            Some(p) if !p.is_empty() => p.target(),
            _ => target(tool, input),
        },
        input: (!input.is_null()).then(|| bounded_input(input)),
        offset,
    }];
    match tool {
        "todowrite" => {
            if let Some(items) = plan(input) {
                head.push(TranscriptItem::PlanUpdated { at, items, offset });
            }
        }
        "question" => questions(input, at, offset, &mut head),
        _ => {}
    }

    let finished = matches!(status, Some("completed" | "error"));
    if !finished && !msg.settled {
        return PartItems {
            phase: Phase::Started,
            head,
            tail: Vec::new(),
        };
    }
    let end = int_at(state.and_then(|s| s.get("time")), "end").unwrap_or(at);
    let metadata = state.and_then(|s| s.get("metadata"));
    let (is_error, text) = match status {
        Some("completed") => {
            // A shell that exited non-zero completed, but failed.
            let exit = metadata.and_then(|m| m.get("exit")).and_then(Value::as_i64);
            (
                exit.is_some_and(|c| c != 0),
                state.and_then(|s| str_at(s, "output")).unwrap_or(""),
            )
        }
        Some("error") => (true, state.and_then(|s| str_at(s, "error")).unwrap_or("")),
        // The message ended while the tool never finished.
        _ => (true, "interrupted: the tool did not finish"),
    };
    let mut tail = vec![TranscriptItem::ToolResult {
        at: end,
        call_id,
        is_error,
        summary: summary(text, SUMMARY_LINES, SUMMARY_CHARS),
        offset,
    }];
    if status == Some("completed") {
        match &patch {
            Some(p) => p.file_edits(end, offset, &mut tail),
            None => tail.extend(file_edit(tool, input, metadata, end, offset)),
        }
    }
    PartItems {
        phase: Phase::Done,
        head,
        tail,
    }
}

/// A short description of what a tool call acts on.
fn target(tool: &str, input: &Value) -> String {
    let get = |k: &str| input.get(k).and_then(Value::as_str);
    let found = match tool {
        "bash" => get("command"),
        "read" | "edit" | "write" | "multiedit" => get("filePath"),
        "list" | "ls" => get("path"),
        "grep" | "glob" => get("pattern"),
        "webfetch" => get("url"),
        "websearch" | "codesearch" => get("query"),
        "task" => get("description"),
        "skill" => get("name"),
        "todowrite" => {
            let n = input
                .get("todos")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            return format!("{n} items");
        }
        "question" => input
            .get("questions")
            .and_then(|q| q.get(0))
            .and_then(|q| q.get("question"))
            .and_then(Value::as_str),
        _ => None,
    };
    let found = found.or_else(|| {
        [
            "command",
            "filePath",
            "path",
            "pattern",
            "url",
            "query",
            "description",
            "prompt",
            "name",
        ]
        .into_iter()
        .find_map(get)
    });
    first_line(found.unwrap_or(""), MAX_TARGET_CHARS)
}

fn plan(input: &Value) -> Option<Vec<PlanItem>> {
    let todos = input.get("todos")?.as_array()?;
    Some(
        todos
            .iter()
            .filter_map(|t| {
                let text = t.get("content").and_then(Value::as_str)?;
                Some(plan_item(text, t.get("status").and_then(Value::as_str)))
            })
            .take(MAX_PLAN_ITEMS)
            .collect(),
    )
}

fn questions(input: &Value, at: TimestampMs, offset: u64, out: &mut Vec<TranscriptItem>) {
    let Some(qs) = input.get("questions").and_then(Value::as_array) else {
        return;
    };
    for q in qs.iter().take(MAX_QUESTIONS) {
        let Some(text) = q.get("question").and_then(Value::as_str) else {
            continue;
        };
        let options = q
            .get("options")
            .and_then(Value::as_array)
            .map(|opts| {
                opts.iter()
                    .filter_map(|o| {
                        o.get("label")
                            .and_then(Value::as_str)
                            .or_else(|| o.as_str())
                    })
                    .take(MAX_OPTIONS)
                    .map(|label| truncate_chars(label, MAX_OPTION_CHARS))
                    .collect()
            })
            .unwrap_or_default();
        out.push(TranscriptItem::Question {
            at,
            text: truncate_chars(text, MAX_QUESTION_CHARS),
            options,
            offset,
        });
    }
}

/// The edit a completed `edit` or `write` made.
///
/// OpenCode records `metadata.filediff` (`file`, `additions`, `deletions`, and a unified
/// `patch`, or the whole `before` and `after`), or only `metadata.diff`. A `write` without either
/// counts its content as added lines.
fn file_edit(
    tool: &str,
    input: &Value,
    metadata: Option<&Value>,
    at: TimestampMs,
    offset: u64,
) -> Option<TranscriptItem> {
    let path_of = |p: &str| truncate_chars(p, MAX_PATH_BYTES);
    let input_path = input.get("filePath").and_then(Value::as_str);
    let text_diff = metadata.and_then(|m| m.get("diff")).and_then(Value::as_str);
    if let Some(fd) = metadata
        .and_then(|m| m.get("filediff"))
        .filter(|f| f.is_object())
    {
        let path = fd.get("file").and_then(Value::as_str).or(input_path)?;
        let count = |k: &str| {
            fd.get(k)
                .and_then(Value::as_u64)
                .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX))
        };
        let diff = fd
            .get("patch")
            .and_then(Value::as_str)
            .or(text_diff)
            .map(capped_diff);
        return Some(TranscriptItem::FileEdit {
            at,
            path: path_of(path),
            added: count("additions"),
            removed: count("deletions"),
            diff,
            offset,
        });
    }
    if let Some(diff) = text_diff {
        let (added, removed) = count_diff(diff);
        return Some(TranscriptItem::FileEdit {
            at,
            path: path_of(input_path?),
            added,
            removed,
            diff: Some(capped_diff(diff)),
            offset,
        });
    }
    if tool == "write" {
        let path = input_path.or_else(|| {
            metadata
                .and_then(|m| m.get("filepath"))
                .and_then(Value::as_str)
        })?;
        let content = input.get("content").and_then(Value::as_str).unwrap_or("");
        let n = content.lines().count();
        let created = metadata
            .and_then(|m| m.get("exists"))
            .and_then(Value::as_bool)
            == Some(false);
        let diff = created.then(|| {
            let path = path_of(path);
            let mut diff = Diff::new("/dev/null", &path, MAX_DIFF_BYTES);
            diff.push(&format!("@@ -0,0 +1,{n} @@"));
            for line in content.lines() {
                diff.push(&format!("+{line}"));
            }
            diff.finish()
        });
        return Some(TranscriptItem::FileEdit {
            at,
            path: path_of(path),
            added: u32::try_from(n).unwrap_or(u32::MAX),
            removed: 0,
            diff,
            offset,
        });
    }
    None
}

/// Added and removed lines of a unified diff, headers excluded.
fn count_diff(diff: &str) -> (u32, u32) {
    let (mut added, mut removed) = (0u32, 0u32);
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added = added.saturating_add(1);
        } else if line.starts_with('-') {
            removed = removed.saturating_add(1);
        }
    }
    (added, removed)
}

/// The model from a session's `model` column: JSON `{"id": ...}` (or `modelID`), or plain text.
pub(crate) fn model_from_json(s: &str) -> Option<String> {
    match serde_json::from_str::<Value>(s) {
        Ok(v @ Value::Object(_)) => bounded(
            v.get("id")
                .or_else(|| v.get("modelID"))
                .and_then(Value::as_str),
            MAX_ID_BYTES,
        ),
        Ok(Value::String(m)) => bounded(Some(&m), MAX_ID_BYTES),
        Ok(_) => None,
        Err(_) => bounded(Some(s), MAX_ID_BYTES),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bound::MAX_INPUT_JSON_BYTES;
    use serde_json::json;

    const USER: MessageInfo = MessageInfo {
        role: Role::User,
        settled: false,
        model: None,
    };
    const LIVE: MessageInfo = MessageInfo {
        role: Role::Assistant,
        settled: false,
        model: None,
    };
    const SETTLED: MessageInfo = MessageInfo {
        role: Role::Assistant,
        settled: true,
        model: None,
    };

    fn parse(v: &Value, msg: &MessageInfo) -> PartItems {
        parse_part(v.to_string().as_bytes(), msg, 9, 1000).expect("parses")
    }

    fn tool(status: &str, extra: Value) -> Value {
        let mut state =
            json!({"status": status, "input": {"command": "ls -la"}, "time": {"start": 2000}});
        if let (Some(s), Some(e)) = (state.as_object_mut(), extra.as_object()) {
            s.extend(e.clone());
        }
        json!({"type": "tool", "tool": "bash", "callID": "c1", "state": state})
    }

    #[test]
    fn user_text_is_a_prompt_unless_synthetic() {
        let got = parse(&json!({"type": "text", "text": "  fix it  "}), &USER);
        assert_eq!(got.phase, Phase::Done);
        assert_eq!(
            got.head,
            vec![TranscriptItem::UserPrompt {
                at: 1000,
                text: "fix it".into(),
                offset: 9
            }]
        );
        for flag in ["synthetic", "ignored"] {
            let v = json!({"type": "text", "text": "context", flag: true});
            assert!(parse(&v, &USER).visible().is_empty(), "{flag}");
        }
    }

    #[test]
    fn assistant_text_waits_for_its_end() {
        let streaming = json!({"type": "text", "text": "partial", "time": {"start": 5}});
        assert_eq!(parse(&streaming, &LIVE).phase, Phase::Waiting);
        // Settled messages show what they have.
        assert!(matches!(
            &parse(&streaming, &SETTLED).visible()[..],
            [TranscriptItem::AssistantText { text, at: 5, .. }] if text == "partial"
        ));
        let ended = json!({"type": "text", "text": "whole", "time": {"start": 5, "end": 6}});
        assert_eq!(parse(&ended, &LIVE).phase, Phase::Done);
    }

    #[test]
    fn a_tool_goes_from_waiting_to_started_to_done() {
        assert_eq!(
            parse(&tool("pending", json!({})), &LIVE).phase,
            Phase::Waiting
        );

        let running = parse(&tool("running", json!({})), &LIVE);
        assert_eq!(running.phase, Phase::Started);
        assert!(matches!(
            &running.head[..],
            [TranscriptItem::ToolUse { call_id, target, at: 2000, .. }] if call_id == "c1" && target == "ls -la"
        ));

        let done = parse(
            &tool(
                "completed",
                json!({"output": "a\nb\nc\nd", "metadata": {"exit": 0}, "time": {"start": 2000, "end": 3000}}),
            ),
            &LIVE,
        );
        assert_eq!(done.phase, Phase::Done);
        assert_eq!(done.head, running.head, "the head does not change");
        assert_eq!(
            done.tail,
            vec![TranscriptItem::ToolResult {
                at: 3000,
                call_id: "c1".into(),
                is_error: false,
                summary: "a\nb\nc".into(),
                offset: 9
            }]
        );
    }

    #[test]
    fn errors_and_exit_codes() {
        let result = |v: &Value, msg: &MessageInfo| match &parse(v, msg).tail[..] {
            [
                TranscriptItem::ToolResult {
                    is_error, summary, ..
                },
            ] => (*is_error, summary.clone()),
            other => panic!("{other:?}"),
        };
        let failed = tool(
            "completed",
            json!({"output": "no", "metadata": {"exit": 2}}),
        );
        assert_eq!(result(&failed, &LIVE), (true, "no".into()));
        let killed = tool(
            "completed",
            json!({"output": "x", "metadata": {"exit": null}}),
        );
        assert_eq!(result(&killed, &LIVE), (false, "x".into()));
        let error = tool("error", json!({"error": "denied"}));
        assert_eq!(result(&error, &LIVE), (true, "denied".into()));
        let stuck = tool("running", json!({}));
        assert!(
            result(&stuck, &SETTLED).0,
            "an unfinished tool of a settled message failed"
        );
    }

    #[test]
    fn edits_from_filediff_diff_or_write() {
        let edit = json!({"type": "tool", "tool": "edit", "callID": "e", "state": {
            "status": "completed", "input": {"filePath": "/w/a.rs"}, "output": "ok",
            "metadata": {"diff": "x", "filediff": {"file": "/w/a.rs", "additions": 2, "deletions": 1,
                "patch": "Index: /w/a.rs\n===\n--- /w/a.rs\n+++ /w/a.rs\n@@ -1,1 +1,2 @@\n-a\n+b\n+c\n"}}}});
        let got = parse(&edit, &LIVE).tail;
        assert!(matches!(
            &got[1],
            TranscriptItem::FileEdit { path, added: 2, removed: 1, diff: Some(d), .. } if path == "/w/a.rs" && d.ends_with("+c\n")
        ));

        let only_diff = json!({"type": "tool", "tool": "edit", "callID": "e", "state": {
            "status": "completed", "input": {"filePath": "/w/b.rs"}, "output": "ok",
            "metadata": {"diff": "--- /w/b.rs\n+++ /w/b.rs\n@@ -1 +1 @@\n-a\n+b\n"}}});
        assert!(matches!(
            &parse(&only_diff, &LIVE).tail[1],
            TranscriptItem::FileEdit {
                added: 1,
                removed: 1,
                ..
            }
        ));

        let write = json!({"type": "tool", "tool": "write", "callID": "w", "state": {
            "status": "completed", "input": {"filePath": "/w/new.md", "content": "one\ntwo\n"}, "output": "",
            "metadata": {"exists": false, "filepath": "/w/new.md"}}});
        assert!(matches!(
            &parse(&write, &LIVE).tail[1],
            TranscriptItem::FileEdit { added: 2, removed: 0, diff: Some(d), .. } if d.ends_with("+one\n+two\n")
        ));

        // A failed edit changed nothing.
        let failed = json!({"type": "tool", "tool": "edit", "callID": "e", "state": {
            "status": "error", "input": {"filePath": "/w/a.rs"}, "error": "not found"}});
        assert_eq!(parse(&failed, &LIVE).tail.len(), 1);
    }

    #[test]
    fn a_patch_tool_gives_one_edit_per_file() {
        let v = json!({"type": "tool", "tool": "apply_patch", "callID": "p", "state": {
            "status": "completed", "output": "ok", "input": {"patchText":
            "*** Begin Patch\n*** Add File: a\n+1\n*** Update File: b\n@@\n-x\n+y\n*** End Patch"}}});
        let got = parse(&v, &LIVE);
        assert!(
            matches!(&got.head[0], TranscriptItem::ToolUse { target, .. } if target == "a (+1 more)")
        );
        assert_eq!(got.tail.len(), 3);
    }

    #[test]
    fn todos_and_questions_come_with_the_head() {
        let todo = json!({"type": "tool", "tool": "todowrite", "callID": "t", "state": {
            "status": "running", "input": {"todos": [
                {"content": "a", "status": "completed", "priority": "high"},
                {"content": "b", "status": "in_progress", "priority": "low"}]}}});
        let got = parse(&todo, &LIVE);
        assert_eq!(got.phase, Phase::Started);
        assert!(
            matches!(&got.head[1], TranscriptItem::PlanUpdated { items, .. } if items.len() == 2)
        );

        let q = json!({"type": "tool", "tool": "question", "callID": "q", "state": {
            "status": "running", "input": {"questions": [{"question": "Which?", "header": "Pick",
                "options": [{"label": "A", "description": "a"}, {"label": "B", "description": "b"}]}]}}});
        let got = parse(&q, &LIVE);
        assert!(matches!(
            &got.head[1],
            TranscriptItem::Question { text, options, .. } if text == "Which?" && options == &["A", "B"]
        ));
    }

    #[test]
    fn turn_ends_only_on_the_last_step() {
        let step = |reason: &str| {
            parse(&json!({"type": "step-finish", "reason": reason}), &LIVE).visible()
        };
        assert!(step("tool-calls").is_empty());
        assert!(matches!(
            &step("stop")[..],
            [TranscriptItem::TurnEnded { .. }]
        ));
        assert!(matches!(
            &step("length")[..],
            [TranscriptItem::TurnEnded { .. }]
        ));
        for kind in [
            "reasoning",
            "step-start",
            "patch",
            "file",
            "compaction",
            "who-knows",
        ] {
            assert!(
                parse(&json!({"type": kind}), &LIVE).visible().is_empty(),
                "{kind}"
            );
        }
    }

    #[test]
    fn wrong_shapes_give_no_items() {
        for v in [
            json!({"type": "tool"}),
            json!({"type": "tool", "tool": 1, "callID": "c"}),
            json!({"type": "tool", "tool": "bash", "callID": "c", "state": 5}),
            json!({"type": "text", "text": ["x"]}),
            json!({"type": ["text"]}),
            json!({}),
        ] {
            let got = parse(&v, &SETTLED).visible();
            assert!(
                got.iter().all(|i| !matches!(
                    i,
                    TranscriptItem::UserPrompt { .. } | TranscriptItem::AssistantText { .. }
                )),
                "{v}"
            );
        }
        assert!(matches!(
            parse_part(b"[1]", &LIVE, 0, 0),
            Err(SkipReason::Malformed(_))
        ));
        assert!(matches!(
            parse_part(b"{\"a\":", &LIVE, 0, 0),
            Err(SkipReason::Malformed(_))
        ));
        assert_eq!(
            parse_part(b"{\"a\":\"\xff\"}", &LIVE, 0, 0),
            Err(SkipReason::InvalidUtf8)
        );
    }

    #[test]
    fn payloads_are_capped() {
        let big = "x".repeat(200_000);
        let v = json!({"type": "tool", "tool": "bash", "callID": big, "state": {
            "status": "completed", "input": {"command": big}, "output": big,
            "metadata": {"filediff": {"file": big, "patch": format!("{big}\n").repeat(3)}}}});
        let got = parse(&v, &LIVE).visible();
        let TranscriptItem::ToolUse {
            call_id,
            target,
            input: Some(input),
            ..
        } = &got[0]
        else {
            panic!("tool use first");
        };
        assert!(call_id.chars().count() <= MAX_ID_BYTES + 1);
        assert!(target.chars().count() <= MAX_TARGET_CHARS + 1);
        assert!(input.to_string().len() <= MAX_INPUT_JSON_BYTES + 64);
        let TranscriptItem::ToolResult { summary, .. } = &got[1] else {
            panic!("result second");
        };
        assert!(summary.chars().count() <= SUMMARY_CHARS + 1);
        let TranscriptItem::FileEdit {
            path,
            diff: Some(diff),
            ..
        } = &got[2]
        else {
            panic!("edit third");
        };
        assert!(path.chars().count() <= MAX_PATH_BYTES + 1);
        assert!(diff.len() <= MAX_DIFF_BYTES + 32);
    }
}
