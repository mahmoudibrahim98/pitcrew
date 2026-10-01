//! Synthetic inputs. Everything here is generated; nothing comes from a real machine.
//!
//! Transcripts follow the shapes of the fixtures in `crates/fixtures/data/transcripts`, with one
//! large tool result per turn (as real transcripts have), so a 20 MiB file holds a few hundred
//! turns and several thousand lines.

use pitcrew_interfaces::source::TranscriptRef;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::EventId;
use pitcrew_protocol::model::{Engine, Liveness};
use pitcrew_protocol::{MachineId, MemberId, WorkspaceId};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// One mebibyte.
pub const MIB: u64 = 1 << 20;

/// The size of the large tool result in each generated turn.
pub const TOOL_RESULT_BYTES: usize = 24 * 1024;

const CWD: &str = "/work/bench-project";
const CLAUDE_SESSION: &str = "0b5e7c1a-2d3f-4a6b-8c9d-0e1f2a3b4c5d";
const CODEX_SESSION: &str = "1c6f8d2b-3e4a-4b7c-9d0e-1f2a3b4c5d6e";

/// Writes a Claude Code transcript of at least `min_bytes` to `path`. Returns its size.
///
/// # Errors
///
/// I/O errors.
pub fn claude_transcript(path: &Path, min_bytes: u64) -> io::Result<u64> {
    let big = file_listing(TOOL_RESULT_BYTES);
    write_lines(path, min_bytes, None, |turn, out| {
        claude_turn(turn, &big, out)
    })
}

/// Writes a Codex rollout of at least `min_bytes` to `path`. Returns its size.
///
/// # Errors
///
/// I/O errors.
pub fn codex_transcript(path: &Path, min_bytes: u64) -> io::Result<u64> {
    let big = file_listing(TOOL_RESULT_BYTES);
    let meta = json!({
        "timestamp": timestamp(0),
        "type": "session_meta",
        "payload": {
            "id": CODEX_SESSION,
            "timestamp": timestamp(0),
            "cwd": CWD,
            "originator": "codex_cli_rs",
            "cli_version": "0.50.0",
            "instructions": null,
            "git": {"branch": "main"}
        }
    });
    write_lines(path, min_bytes, Some(meta), |turn, out| {
        codex_turn(turn, &big, out);
    })
}

/// A reference to a generated transcript, as discovery would return it.
///
/// # Errors
///
/// The file cannot be read.
pub fn transcript_ref(engine: Engine, path: &Path) -> io::Result<TranscriptRef> {
    Ok(TranscriptRef {
        engine,
        path: path.to_path_buf(),
        inner_id: None,
        size: std::fs::metadata(path)?.len(),
        modified: 0,
    })
}

/// Writes `first`, then turns from `turn` until the file holds at least `min_bytes`.
fn write_lines(
    path: &Path,
    min_bytes: u64,
    first: Option<Value>,
    mut turn: impl FnMut(u64, &mut Vec<Value>),
) -> io::Result<u64> {
    let mut file = BufWriter::with_capacity(1 << 20, File::create(path)?);
    let mut written = 0u64;
    let mut records: Vec<Value> = first.into_iter().collect();
    let mut n = 0u64;
    loop {
        for record in records.drain(..) {
            let line = serde_json::to_vec(&record)?;
            file.write_all(&line)?;
            file.write_all(b"\n")?;
            written += line.len() as u64 + 1;
        }
        if written >= min_bytes {
            break;
        }
        turn(n, &mut records);
        n += 1;
    }
    file.flush()?;
    Ok(written)
}

/// One Claude Code turn: a prompt, a plan, a large file read, an edit, a test run and a reply.
fn claude_turn(turn: u64, big: &str, out: &mut Vec<Value>) {
    let t = turn * 60;
    let id = |k: u32| format!("t{turn}-{k:02}");
    let base = |k: u32, kind: &str| {
        json!({
            "parentUuid": if k == 0 { Value::Null } else { Value::String(id(k - 1)) },
            "isSidechain": false,
            "userType": "external",
            "cwd": CWD,
            "sessionId": CLAUDE_SESSION,
            "version": "2.1.0",
            "gitBranch": "main",
            "type": kind,
            "uuid": id(k),
            "timestamp": timestamp(t + u64::from(k)),
        })
    };
    let user = |k: u32, content: Value| {
        with(
            base(k, "user"),
            "message",
            json!({"role": "user", "content": content}),
        )
    };
    let assistant = |k: u32, content: Value, stop: &str| {
        with(
            base(k, "assistant"),
            "message",
            json!({
                "id": format!("msg_{turn}_{k}"),
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-5-5",
                "content": content,
                "stop_reason": stop,
                "usage": {"input_tokens": 1200 + turn, "output_tokens": 180}
            }),
        )
    };
    let result = |k: u32, tool: &str, content: &str| {
        user(
            k,
            json!([{"tool_use_id": tool, "type": "tool_result", "content": content}]),
        )
    };
    let tool = |n: u32| format!("toolu_{turn}_{n}");

    out.push(user(
        0,
        json!(format!(
            "Turn {turn}: tidy the parser module, then run the tests."
        )),
    ));
    out.push(assistant(
        1,
        json!([
            {"type": "text", "text": "I will read the module, make the change, then run the tests."},
            {"type": "tool_use", "id": tool(1), "name": "TodoWrite", "input": {"todos": [
                {"content": "Read src/parser.rs", "status": "in_progress", "activeForm": "Reading"},
                {"content": "Tidy the parser", "status": "pending", "activeForm": "Tidying"},
                {"content": "Run the tests", "status": "pending", "activeForm": "Testing"}
            ]}}
        ]),
        "tool_use",
    ));
    out.push(result(
        2,
        &tool(1),
        "Todos have been modified successfully.",
    ));
    out.push(assistant(
        3,
        json!([{"type": "tool_use", "id": tool(2), "name": "Read",
                "input": {"file_path": format!("{CWD}/src/parser.rs")}}]),
        "tool_use",
    ));
    out.push(result(4, &tool(2), big));
    out.push(assistant(
        5,
        json!([{"type": "tool_use", "id": tool(3), "name": "Edit", "input": {
            "file_path": format!("{CWD}/src/parser.rs"),
            "old_string": format!("let value_{turn} = compute({turn});"),
            "new_string": format!("let value_{turn} = compute_checked({turn})?;")
        }}]),
        "tool_use",
    ));
    out.push(with(
        result(6, &tool(3), "The file has been updated."),
        "toolUseResult",
        json!({"filePath": format!("{CWD}/src/parser.rs"), "structuredPatch": [{
            "oldStart": 10, "oldLines": 1, "newStart": 10, "newLines": 1,
            "lines": [
                format!("-let value_{turn} = compute({turn});"),
                format!("+let value_{turn} = compute_checked({turn})?;")
            ]
        }]}),
    ));
    out.push(assistant(
        7,
        json!([{"type": "tool_use", "id": tool(4), "name": "Bash",
                "input": {"command": "cargo test", "description": "Run the tests"}}]),
        "tool_use",
    ));
    out.push(result(8, &tool(4), "test result: ok. 42 passed; 0 failed"));
    out.push(assistant(
        9,
        json!([{"type": "text", "text": format!("Turn {turn} is done: the parser is tidier and the tests pass.")}]),
        "end_turn",
    ));
    out.push(json!({
        "type": "system", "subtype": "turn_duration", "durationMs": 42_000,
        "parentUuid": id(9), "uuid": id(10), "timestamp": timestamp(t + 10),
        "sessionId": CLAUDE_SESSION, "cwd": CWD, "isSidechain": false
    }));
}

/// One Codex turn: a prompt, a plan, a shell command with a large output, a patch and a reply.
fn codex_turn(turn: u64, big: &str, out: &mut Vec<Value>) {
    let t = turn * 60;
    let mut k = 0u64;
    let mut rec = |kind: &str, payload: Value| {
        k += 1;
        json!({"timestamp": timestamp(t + k), "type": kind, "payload": payload})
    };
    let call = |n: u32| format!("call_{turn}_{n}");
    let prompt = format!("Turn {turn}: run the test suite and fix what fails.");
    let reply = format!("Turn {turn} is done: all tests pass.");
    let args = |v: Value| v.to_string();

    out.push(rec(
        "turn_context",
        json!({"cwd": CWD, "approval_policy": "on-request",
               "sandbox_policy": {"mode": "workspace-write"}, "model": "gpt-5-codex", "summary": "auto"}),
    ));
    out.push(rec(
        "response_item",
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": prompt}]}),
    ));
    out.push(rec(
        "event_msg",
        json!({"type": "user_message", "message": prompt, "kind": "plain"}),
    ));
    out.push(rec(
        "response_item",
        json!({"type": "function_call", "name": "update_plan", "call_id": call(1),
        "arguments": args(json!({"plan": [
            {"step": "Run the tests", "status": "in_progress"},
            {"step": "Fix failures", "status": "pending"}
        ]}))}),
    ));
    out.push(rec(
        "response_item",
        json!({"type": "function_call_output", "call_id": call(1), "output": "Plan updated"}),
    ));
    out.push(rec(
        "response_item",
        json!({"type": "function_call", "name": "shell", "call_id": call(2),
               "arguments": args(json!({"command": ["bash", "-lc", "cargo test"], "workdir": CWD}))}),
    ));
    out.push(rec(
        "response_item",
        json!({"type": "function_call_output", "call_id": call(2),
               "output": args(json!({"output": big, "metadata": {"exit_code": 0, "duration_seconds": 1.5}}))}),
    ));
    out.push(rec(
        "response_item",
        json!({"type": "custom_tool_call", "status": "completed", "call_id": call(3), "name": "apply_patch",
               "input": format!("*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-let v = {turn};\n+let v = {turn} + 1;\n*** End Patch")}),
    ));
    out.push(rec(
        "response_item",
        json!({"type": "custom_tool_call_output", "call_id": call(3),
               "output": args(json!({"output": "Success. Updated the following files:\nM src/lib.rs\n",
                                     "metadata": {"exit_code": 0, "duration_seconds": 0.0}}))}),
    ));
    out.push(rec(
        "event_msg",
        json!({"type": "agent_message", "message": reply}),
    ));
    out.push(rec(
        "response_item",
        json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": reply}]}),
    ));
    out.push(rec(
        "event_msg",
        json!({"type": "token_count", "info": {"total_token_usage":
            {"input_tokens": 5200 + turn, "output_tokens": 410, "total_tokens": 5610 + turn}}}),
    ));
    out.push(rec(
        "event_msg",
        json!({"type": "task_complete", "last_agent_message": reply}),
    ));
}

fn with(mut record: Value, key: &str, value: Value) -> Value {
    if let Some(map) = record.as_object_mut() {
        map.insert(key.to_owned(), value);
    }
    record
}

/// A numbered source listing of about `bytes` bytes, like a file read's result.
fn file_listing(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes + 64);
    let mut n = 1u32;
    while out.len() < bytes {
        out.push_str(&format!(
            "{n:>6}\tlet value_{n} = compute({n}); // generated line\n"
        ));
        n += 1;
    }
    out
}

/// An RFC 3339 time on a fixed day, `seconds` after midnight (wrapping at a day).
fn timestamp(seconds: u64) -> String {
    let s = seconds % 86_400;
    format!(
        "2026-09-30T{:02}:{:02}:{:02}.000Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// `tmux -C` output of at least `min_bytes`: mostly pane output (with the octal escapes tmux
/// uses), plus command replies and window notifications, across four panes.
#[must_use]
pub fn control_stream(min_bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(min_bytes + 4096);
    let mut line = 0u64;
    let mut command = 0u64;
    while out.len() < min_bytes {
        line += 1;
        let pane = line % 4 + 1;
        out.extend_from_slice(
            format!(
                "%output %{pane} \\033[32m   Compiling\\033[0m bench-crate-{line} v0.1.0 \u{2713} (/work/bench-project)\\015\\012\n"
            )
            .as_bytes(),
        );
        if line.is_multiple_of(64) {
            command += 1;
            out.extend_from_slice(format!("%begin 1790000000 {command} 1\n").as_bytes());
            for s in 0..4 {
                out.extend_from_slice(
                    format!("{s}: bench-{s} (1 windows) (created Wed Sep 30 08:00:00 2026)\n")
                        .as_bytes(),
                );
            }
            out.extend_from_slice(format!("%end 1790000000 {command} 1\n").as_bytes());
        }
        if line.is_multiple_of(256) {
            out.extend_from_slice(format!("%window-renamed @{pane} build-{line}\n").as_bytes());
            out.extend_from_slice(
                b"%layout-change @1 b25d,80x24,0,0,1 b25d,80x24,0,0,1 *\n".as_slice(),
            );
        }
        if line.is_multiple_of(1024) {
            out.extend_from_slice(b"%sessions-changed\n");
        }
    }
    out
}

/// `n` events shaped like the demo workspace's event log, each with a fresh id.
///
/// # Errors
///
/// The fixture does not parse, which its own tests rule out.
pub fn events(n: usize) -> Result<Vec<Event>, serde_json::Error> {
    let fixture = pitcrew_fixtures::demo_workspace()?.events;
    let cycled: Vec<Event> = fixture.iter().cycle().take(n).cloned().collect();
    Ok(with_fresh_ids(&cycled))
}

/// Copies of `events`, each with a new id, so they can be appended again.
#[must_use]
pub fn with_fresh_ids(events: &[Event]) -> Vec<Event> {
    events
        .iter()
        .map(|e| {
            let mut e = e.clone();
            e.id = EventId::new();
            e
        })
        .collect()
}

/// A small event, as a runner sends when a machine's liveness changes.
#[must_use]
pub fn liveness_event() -> Event {
    Event::now(
        WorkspaceId::new(),
        MemberId::new(),
        EventBody::MachineLiveness {
            machine: MachineId::new(),
            liveness: Liveness::Live,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_ingest::claude::ClaudeAdapter;
    use pitcrew_ingest::codex::CodexAdapter;
    use pitcrew_interfaces::source::{Cursor, SourceAdapter, TranscriptItem};
    use pitcrew_runtime::{ControlParser, Notification};

    fn check(engine: Engine, bytes: u64) -> (u64, usize, usize) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let len = match engine {
            Engine::Claude => claude_transcript(&path, bytes),
            _ => codex_transcript(&path, bytes),
        }
        .unwrap();
        assert_eq!(len, std::fs::metadata(&path).unwrap().len());
        let tref = transcript_ref(engine, &path).unwrap();
        let (skipped, items) = match engine {
            Engine::Claude => {
                let r = ClaudeAdapter::new()
                    .read(&tref, &Cursor::default())
                    .unwrap();
                (r.skipped_total, r.chunk.items)
            }
            _ => {
                let r = CodexAdapter::new().read(&tref, &Cursor::default()).unwrap();
                (r.skipped_total, r.chunk.items)
            }
        };
        assert_eq!(skipped, 0, "every generated line parses");
        let page = match engine {
            Engine::Claude => ClaudeAdapter::new().read_page(&tref, None, 200),
            _ => CodexAdapter::new().read_page(&tref, None, 200),
        }
        .unwrap();
        let turns = items
            .iter()
            .filter(|i| matches!(i, TranscriptItem::TurnEnded { .. }))
            .count();
        (len, turns, page.items.len())
    }

    #[test]
    fn claude_transcripts_parse_cleanly() {
        let (len, turns, page) = check(Engine::Claude, MIB);
        assert!(len >= MIB);
        assert!(turns >= 20, "{turns} turns");
        assert!(page >= 200, "a full page, got {page}");
    }

    #[test]
    fn codex_transcripts_parse_cleanly() {
        let (len, turns, page) = check(Engine::Codex, MIB);
        assert!(len >= MIB);
        assert!(turns >= 20, "{turns} turns");
        assert!(page >= 200, "a full page, got {page}");
    }

    #[test]
    fn control_output_is_all_understood() {
        let data = control_stream(256 * 1024);
        let mut parser = ControlParser::new();
        let mut all = Vec::new();
        for chunk in data.chunks(1000) {
            all.extend(parser.feed(chunk));
        }
        assert!(
            !all.iter().any(|n| matches!(n, Notification::Other { .. })),
            "no unknown lines"
        );
        let outputs = all
            .iter()
            .filter(|n| matches!(n, Notification::Output { .. }))
            .count();
        let replies = all
            .iter()
            .filter(|n| matches!(n, Notification::CommandReply(_)))
            .count();
        assert!(
            outputs > 1000 && replies > 10,
            "{outputs} outputs, {replies} replies"
        );
        if let Some(Notification::Output { data, .. }) = all.first() {
            assert!(data.starts_with(b"\x1b[32m"), "escapes are decoded");
        }
    }

    #[test]
    fn events_have_fresh_ids() {
        let events = events(50).unwrap();
        assert_eq!(events.len(), 50);
        let ids: std::collections::HashSet<_> = events.iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), 50);
    }

    #[test]
    fn timestamps_are_rfc3339() {
        assert_eq!(timestamp(0), "2026-09-30T00:00:00.000Z");
        assert_eq!(timestamp(3_723), "2026-09-30T01:02:03.000Z");
        assert_eq!(timestamp(86_400 + 1), "2026-09-30T00:00:01.000Z");
    }
}
