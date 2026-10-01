#!/usr/bin/env python3
"""Writes the seed corpora for the fuzz targets, from crates/fixtures and a few hand-made shapes.

Usage: python3 fuzz/seed.py [OUT_DIR]   (default: fuzz/corpus; one folder per target)

Only synthetic data: the fixtures are made up, and so is everything below. Standard library only.
"""

import json
import pathlib
import struct
import sys

HERE = pathlib.Path(__file__).resolve().parent
FIXTURES = HERE.parent / "crates" / "fixtures" / "data"
OUT = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else HERE / "corpus"

counts = {}


def seed(target, name, data):
    if isinstance(data, str):
        data = data.encode("utf-8")
    folder = OUT / target
    folder.mkdir(parents=True, exist_ok=True)
    (folder / name).write_bytes(data)
    counts[target] = counts.get(target, 0) + 1


def line(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


claude = (FIXTURES / "transcripts" / "claude" / "demo-session.jsonl").read_bytes()
codex = (FIXTURES / "transcripts" / "codex" / "rollout-demo.jsonl").read_bytes()
workspace = json.loads((FIXTURES / "demo-workspace.json").read_text("utf-8"))

# Line parsers: every fixture line, plus shapes the fixtures lack.
for i, text in enumerate(claude.splitlines()):
    seed("claude_parse_line", f"fixture-{i:03}", text)
for i, text in enumerate(codex.splitlines()):
    seed("codex_parse_line", f"fixture-{i:03}", text)

extra_claude = [
    {"type": "summary", "summary": "Method section drafted", "leafUuid": "x"},
    {"type": "custom-title", "customTitle": "Draft the method"},
    {"type": "system", "subtype": "turn_duration", "durationMs": 1200,
     "timestamp": "2026-09-30T08:00:09.5+02:00"},
    {"type": "user", "isMeta": True, "message": {"content": "<command-name>/clear</command-name>"}},
    {"type": "assistant", "message": {"model": "m", "stop_reason": "end_turn", "content": [
        {"type": "tool_use", "id": "t1", "name": "AskUserQuestion", "input": {"questions": [
            {"question": "Which one?", "options": [{"label": "A"}, "B"]}]}}]}},
    {"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "t2", "is_error": False, "content": [{"type": "text", "text": "ok"}]}]},
     "toolUseResult": {"type": "create", "filePath": "/w/a.txt", "content": "one\ntwo\n", "structuredPatch": []}},
]
for i, value in enumerate(extra_claude):
    seed("claude_parse_line", f"shape-{i:03}", line(value))

patch = "*** Begin Patch\n*** Update File: a.rs\n*** Move to: b.rs\n@@ fn main\n-old\n+new\n" \
        "*** Add File: c.txt\n+hello\n*** Delete File: d.txt\n*** End Patch\n"
extra_codex = [
    {"id": "7c1e9d2a-0b3f-4e6a-8d5c-1f2e3a4b5c6d", "timestamp": "2026-09-29T08:00:00Z"},
    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "legacy item"}]},
    {"timestamp": "2026-09-30T08:00:00Z", "type": "response_item", "payload": {
        "type": "custom_tool_call", "name": "apply_patch", "call_id": "c1", "input": patch}},
    {"timestamp": "2026-09-30T08:00:00Z", "type": "response_item", "payload": {
        "type": "function_call", "name": "shell", "call_id": "c2",
        "arguments": line({"command": ["bash", "-lc", "apply_patch <<'EOF'\n" + patch + "EOF\n"]})}},
    {"timestamp": "2026-09-30T08:00:00Z", "type": "response_item", "payload": {
        "type": "function_call_output", "call_id": "c2",
        "output": line({"output": "done", "metadata": {"exit_code": 1}})}},
    {"timestamp": "2026-09-30T08:00:00Z", "type": "response_item", "payload": {
        "type": "function_call", "name": "update_plan", "call_id": "c3",
        "arguments": line({"plan": [{"step": "one", "status": "in_progress"}]})}},
    {"timestamp": "2026-09-30T08:00:00Z", "type": "event_msg", "payload": {"type": "turn_aborted"}},
]
for i, value in enumerate(extra_codex):
    seed("codex_parse_line", f"shape-{i:03}", line(value))

def windows(data, size=3, step=2):
    """Short files of `size` consecutive lines: whole-file targets run much faster on them."""
    lines = data.splitlines(keepends=True)
    return [b"".join(lines[i:i + size]) for i in range(0, max(len(lines) - size, 0) + 1, step)]


# Adapters: [engine, split, page size, page end] + the file.
for engine, name, data in ((0, "claude", claude), (1, "codex", codex)):
    seed("adapter_read", f"{name}-whole", bytes([engine, 128, 3, 200]) + data)
    controls = ((128, 3, 200), (0, 0, 255), (255, 15, 64), (77, 1, 128))
    for i, window in enumerate(windows(data)):
        split, limit, before = controls[i % len(controls)]
        seed("adapter_read", f"{name}-{i:02}", bytes([engine, split, limit, before]) + window)
    window = windows(data)[1]
    seed("adapter_read", f"{name}-crlf", bytes([engine, 100, 4, 180]) + window.replace(b"\n", b"\r\n"))
    seed("adapter_read", f"{name}-partial", bytes([engine, 200, 2, 255]) + window[: len(window) * 2 // 3])

    # Cursors: [engine, offset (u16 LE), state length (u16 LE)] + state + file.
    data = windows(data)[0]
    first_line = data.index(b"\n") + 1
    states = [
        b"",
        b"null",
        b"{}",
        line({"pending": {"len": 5, "hex": data[first_line:first_line + 5].hex()}}).encode(),
        line({"pending": {"len": 40, "too_long": True}}).encode(),
        line({"pending": {"len": 12}}).encode(),
        line({"meta": {"session_id": "s"}, "last": "turn_ended"}).encode(),
    ]
    for i, state in enumerate(states):
        for offset in (0, first_line, 10, len(data), len(data) + 1):
            head = bytes([engine]) + struct.pack("<HH", offset % 65536, len(state))
            seed("adapter_cursor", f"{name}-{i}-{offset}", head + state + data)

# tmux control mode: [k] + k % 32 chunk sizes + the stream.
tmux = [
    b"%begin 1790755200 12 1\n0: main* (1 panes) [80x24] @0 (active)\n%end 1790755200 12 1\n",
    b"%begin 1790755200 13 0\nunknown command: nope\n%error 1790755200 13 0\n",
    b"%output %1 hello\\015\\012world \\134 done\n%output %2 \\033[1mbold\\033[0m\n",
    b"%extended-output %1 250 : late\\015\\012\n%pause %1\n%continue %1\n",
    b"%window-add @3\n%window-renamed @3 build\n%window-close @3\n%unlinked-window-add @4\n",
    b"%layout-change @1 b25d,80x24,0,0,0 b25d,80x24,0,0,0 *\n%session-changed $1 main\n"
    b"%sessions-changed\n%pane-mode-changed %1\n",
    b"%begin 1790755200 14 1\n%exit forged\n%output %9 forged\n%end 1790755200 14 1\n%exit\n",
]
for i, stream in enumerate(tmux):
    seed("tmux_control", f"stream-{i:02}", b"\x00" + stream)
    seed("tmux_control", f"chunked-{i:02}", bytes([3, 1, 7, 2]) + stream)

# Runner protocol lines, with events and machines from the fixtures.
machine = next(m["info"] for m in workspace["machines"] if m.get("info"))
events = workspace["events"]
command_id = "01JB0000000000000000000CMD"
session = workspace["sessions"][0]["id"]
runner_lines = [
    {"type": "hello", "runner_version": "0.0.0", "protocol": 1, "machine": machine,
     "capabilities": ["tmux", "pty", "slurm", "watch", "scan"]},
    {"type": "events", "events": events[:4], "cursor": 4},
    {"type": "command_result", "command": command_id, "outcome": {"status": "ok", "detail": {"session": session}}},
    {"type": "command_result", "command": command_id, "outcome": {"status": "rejected", "reason": "not allowed"}},
    {"type": "command_result", "command": command_id, "outcome": {"status": "failed", "error": "no tmux"}},
    {"type": "terminal_output", "terminal": command_id, "offset": 0, "data_b64": "aGVsbG8K"},
    {"type": "heartbeat", "at": 1790755200000},
    {"type": "welcome", "hub_version": "0.0.0", "protocol": 1, "resume_after_cursor": 3},
    {"type": "ack_events", "cursor": 4},
    {"type": "command", "id": command_id, "command": {
        "type": "start_session", "engine": "claude", "cwd": "/w", "name": "writer",
        "brief": "Line one\nLine two", "permission_mode": "accept_edits"}},
    {"type": "command", "id": command_id, "command": {
        "type": "resume_session", "engine": "codex", "native_id": "n", "cwd": "/w", "name": "w"}},
    {"type": "command", "id": command_id, "command": {"type": "send_text", "session": session, "text": "go"}},
    {"type": "command", "id": command_id, "command": {
        "type": "send_keys", "session": session, "keys": ["enter", "escape", "ctrl_c", "up"]}},
    {"type": "command", "id": command_id, "command": {"type": "interrupt", "session": session}},
    {"type": "command", "id": command_id, "command": {"type": "end_session", "session": session, "mode": "graceful"}},
    {"type": "command", "id": command_id, "command": {
        "type": "resize_terminal", "terminal": command_id, "cols": 80, "rows": 24}},
    {"type": "command", "id": command_id, "command": {
        "type": "read_terminal", "terminal": command_id, "from_offset": 0, "max_bytes": 4096}},
    {"type": "command", "id": command_id, "command": {"type": "scan", "roots": ["/w"]}},
]
for i, value in enumerate(runner_lines):
    seed("runner_decode_line", f"line-{i:02}", line(value) + "\n")

# API JSON: events, frames, pages and host info.
for i, event in enumerate(events):
    seed("api_json", f"event-{i:02}", line(event))
seed("api_json", "frame-hello", line({"type": "hello", "rev": 7, "log": "01JB000000000000000LOG0001"}))
seed("api_json", "frame-events", line({"type": "events", "from_rev": 1, "to_rev": 3, "events": events[:3]}))
seed("api_json", "frame-ping", line({"type": "ping", "at": 1790755200000}))
seed("api_json", "page", line({"items": [
    {"kind": "user_prompt", "at": 1, "text": "hi", "offset": 0},
    {"kind": "tool_use", "at": 2, "call_id": "c", "tool": "Bash", "target": "ls", "input": {"command": "ls"}, "offset": 10},
    {"kind": "file_edit", "at": 3, "path": "a", "added": 1, "removed": 0, "diff": "--- a\n+++ a\n+x\n", "offset": 20},
    {"kind": "plan_updated", "at": 4, "items": [{"text": "x", "status": "completed"}], "offset": 30},
    {"kind": "question", "at": 5, "text": "?", "options": ["a"], "offset": 40},
    {"kind": "turn_ended", "at": 6, "offset": 50},
], "from": 0, "to": 60, "at_start": True}))
seed("api_json", "host-info", line({"name": "pitcrewd", "version": "0.0.0", "protocol": 1, "protocol_min": 1,
                                   "roles": ["hub", "runner"], "machine": machine, "capabilities": ["pty"]}))
seed("api_json", "api-error", line({"code": "forbidden", "message": "no"}))

# API requests: [method] + path, headers, empty line, body. $D and $A are the minted tokens.
requests = [
    (0, "/v1/host/info\n\n"),
    (0, "/v1/probe/device\nauthorization: Bearer $D\n\n"),
    (0, "/v1/probe/device\nauthorization: Bearer $A\n\n"),
    (0, "/v1/probe/agent\nAuthorization: bearer  $A \n\n"),
    (0, "/v1/files/a/b\nauthorization: Bearer $D\n\n"),
    (0, "/v1/files/x\n\n"),
    (0, "/v1/stream?since=3\nupgrade: websocket\nsec-websocket-protocol: pitcrew.v1, pitcrew.bearer.$D\n\n"),
    (0, "/v1/probe/device\nupgrade: websocket\nsec-websocket-protocol: pitcrew.bearer.$A\nauthorization: Bearer $D\n\n"),
    (1, "/v1/hooks/claude/SessionStart\nauthorization: Bearer $A\ncontent-type: application/json\n\n{\"session_id\":\"s\"}"),
    (1, "/v1/hooks/nope/Stop\nauthorization: Bearer $A\n\n[]"),
    (5, "/v1/probe/device?token=$D\n\n"),
    (7, "/v1/nothing\nauthorization: Bearer $D\n\n"),
]
for i, (method, text) in enumerate(requests):
    seed("api_request", f"request-{i:02}", bytes([method]) + text.encode())

for target, n in sorted(counts.items()):
    print(f"{target}: {n} seeds")
