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
seeded_bytes = [0]


def seed(target, name, data):
    if isinstance(data, str):
        data = data.encode("utf-8")
    folder = OUT / target
    folder.mkdir(parents=True, exist_ok=True)
    (folder / name).write_bytes(data)
    counts[target] = counts.get(target, 0) + 1
    seeded_bytes[0] += len(data)


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
recaps = json.loads((FIXTURES / "demo-recaps.json").read_text("utf-8"))
seed("api_json", "recap-blocks", line({"blocks": recaps["blocks"][:3], "at_start": True}))
seed("api_json", "recap-days", line({"days": recaps["projects"][0]["days"][:3], "at_start": False}))

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

# --- crates/remote -----------------------------------------------------------------------------

# Probe reports: [tag index] + ssh's stdout. Tag 0 is "0123456789abcdef".
TAG = "0123456789abcdef"


def report(body, tag=TAG):
    return f"@@pitcrew-probe-begin-{tag}\n{body}@@pitcrew-probe-end-{tag}\n"


probes = [
    report("os=Linux\narch=x86_64\nhostname=hpc-login\nhome=/home/sam\nshell=/bin/bash\n"
           "tmux_found=1\ntmux=tmux 3.3a\nsbatch=1\nsqueue=1\nfs=nfs\n"),
    "Welcome to the Demo Lab cluster.\r\n" + report(
        "os=Darwin\r\narch=arm64\r\nhostname=demo-mac\r\nhome=/Users/sam\r\nshell=/bin/zsh\r\n"
        "tmux_found=0\r\nsbatch=0\r\nsqueue=0\r\nfs=apfs\r\n").replace("\n", "\r\n"),
    report("os=Linux\narch=aarch64\nhome=/home/sam\nshell=/usr/bin/xonsh\ntmux_found=1\n"
           "tmux=3.4\nfs=UNKNOWN (0x19830326)\n"),
    report("home=/x\nfs=ext4\nfs=nfs\n"),
    f"@@pitcrew-probe-begin-{TAG}\nos=Linux\narch=x86_64\n",
    report("home=/x\n@@pitcrew-probe-end-\nfs=lustre\n"),
    report("weird line\n=\nos=\n b = two=2 \n"),
    "@@pitcrew-probe-begin-ffff\nos=Linux\n@@pitcrew-probe-end-ffff\n",
]
for i, text in enumerate(probes):
    seed("remote_probe", f"report-{i:02}", b"\x00" + text.encode())

# ssh configs: up to five files separated by NUL (~/.ssh/config, ~/.ssh/a, ~/.ssh/conf.d/b.conf,
# ~/.ssh/conf.d/c.conf, ~/extra). No absolute paths and no `..`: the target skips those.
ssh_configs = [
    ["# comment\nInclude conf.d/*.conf\nHost hpc-login hpc-login-2 *.example.org !bastion\n"
     "  User sam\nHost=\"quoted one\" plain\nMatch host foo exec \"true\"\n  User x\n"
     "host box1 # trailing comment\nInclude ~/extra\nInclude missing/*.conf\n",
     "", "Host bravo\n", "Host alpha hpc-login\n", "HOST gpu?? gpu01\n"],
    ["Host a\nInclude config config\n"],
    ["Host a\nInclude a\n", "Host b\nInclude ~/.ssh/./config\n"],
    ["Include a a\n", "Host h1\nInclude conf.d/b.conf conf.d/b.conf\n",
     "Host h2\nInclude conf.d/c.conf conf.d/c.conf\n", "Host h3\n"],
    ["Host -oProxyCommand=x user@-x ok@node-01 [::1] fe80::1%eth0 'a b' \"c\\\"d\"\n"],
    ["Host\tcluster\n\tHostName hpc-login\nIncludE  *\n", "Host from-a\n"],
]
for i, files in enumerate(ssh_configs):
    seed("remote_ssh_config", f"config-{i:02}", "\0".join(files))

# argv for the quoting: words separated by NUL.
argvs = [
    ["echo", "a'b"],
    ["rm", "-rf", "it's; $(x) `y`\n", "--", "日本"],
    ["sh", "-c", "printf '%s\\n' \"$HOME\" && echo !! ~ *"],
    ["/home/sam/.pitcrew/bin/pitcrewd", "serve", "--listen", "private"],
    ["\\'; touch pwned; #", "a\\\\b", "!x", "x\ny"],
    ["-oProxyCommand=x", "user@-x", "hpc-login", "sam@hpc-login", "[::1]", "fe80::1%eth0"],
    ["A=b", "", " ", "'", "\"", "#x", "%", "@"],
]
for i, argv in enumerate(argvs):
    seed("remote_quote", f"argv-{i:02}", "\0".join(argv))

# askpass prompts: [hint NUL] prompt.
prompts = [
    "sam@hpc-login's password: ",
    "Enter passphrase for key '/home/sam/.ssh/id_ed25519': ",
    "The authenticity of host 'hpc-login (192.0.2.10)' can't be established.\n"
    "ED25519 key fingerprint is SHA256:AAAAexampleexampleexampleexampleexample00.\n"
    "Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
    "(sam@hpc-login) Verification code: ",
    "(sam@hpc-login) Password: ",
    "(sam@hpc-login) Enter passphrase, then continue connecting: ",
    "Duo two-factor login for sam\n\nPasscode or option (1-3): ",
    "confirm\0Accept updated hostkeys? (yes/no): ",
    "none\0Confirm user presence for key ED25519 SHA256:AAAAexample",
    "Accept updated hostkeys? (yes/no): ",
    "\0Enter PIN for authenticator: ",
]
for i, prompt in enumerate(prompts):
    seed("remote_askpass", f"prompt-{i:02}", prompt)

# --- crates/ingest OpenCode ----------------------------------------------------------------------

INGEST = HERE.parent / "crates" / "ingest" / "tests" / "data" / "opencode"
oc_schema = (INGEST / "schema.sql").read_text("utf-8")
oc_history = (INGEST / "history.jsonl").read_text("utf-8").splitlines()


def sql_value(v):
    if v is None or isinstance(v, (bool, int, float, str)):
        return int(v) if isinstance(v, bool) else v
    return json.dumps(v, separators=(",", ":"))


def opencode_db(lines, schema=oc_schema, statements=""):
    """A small store: 512-byte pages, no journal, vacuumed. Returns its bytes."""
    import sqlite3
    import tempfile
    with tempfile.TemporaryDirectory() as tmp:
        path = pathlib.Path(tmp) / "opencode.db"
        conn = sqlite3.connect(path)
        conn.execute("PRAGMA page_size = 512")
        conn.executescript(schema)
        for text in lines:
            op = json.loads(text)
            cols = list(op["row"])
            conflict = "(session_id, position)" if op["table"] == "todo" else "(id)"
            sets = ", ".join(f"{c} = excluded.{c}" for c in cols)
            conn.execute(
                f"INSERT INTO {op['table']} ({', '.join(cols)}) VALUES "
                f"({', '.join('?' * len(cols))}) ON CONFLICT{conflict} DO UPDATE SET {sets}",
                [sql_value(op["row"][c]) for c in cols])
        if statements:
            conn.executescript(statements)
        conn.commit()
        conn.execute("VACUUM")
        conn.close()
        return path.read_bytes()


# Structured seeds: [1, page size] + row writes. Raw seeds: [0, page size] + database bytes.
for i, start in enumerate(range(0, len(oc_history), 15)):
    window = oc_history[start:start + 18]
    session_rows = [l for l in oc_history[:3] if json.loads(l)["table"] in ("session", "message")]
    lines = session_rows + window if start else window
    seed("opencode_store", f"rows-{i:02}", bytes([1, i]) + "\n".join(lines).encode())
for i, count in enumerate((12, 30)):
    seed("opencode_store", f"db-{i:02}", bytes([0, 3]) + opencode_db(oc_history[:count]))
fixture_dir = FIXTURES / "transcripts" / "opencode"
seed("opencode_store", "db-fixture", bytes([2, 2]) + opencode_db(
    [], (fixture_dir / "schema.sql").read_text("utf-8"),
    (fixture_dir / "seed.sql").read_text("utf-8")))

# --- crates/api: terminal control and activity queries -----------------------------------------


def frames(*messages):
    """[kind, len] + data for each: 0 keys, 1 text, 2 resize from four bytes, 3 a `type`."""
    out = b""
    for kind, data in messages:
        data = data.encode() if isinstance(data, str) else data
        out += bytes([kind, len(data)]) + data
    return out


terminal = [
    frames((0, "ls -la\r"), (2, struct.pack(">HH", 120, 40)), (0, b"\x1b[A\x03")),
    frames((1, '{"type":"resize","cols":80,"rows":24}'), (3, "ping"), (0, "q")),
    frames((1, '{"type":"resize","cols":0,"rows":24}'), (0, "never")),
    frames((1, '{"type":"resize","cols":1001,"rows":1}')),
    frames((1, "not json"), (0, "never")),
    frames((1, '{"cols":80,"rows":24}')),
    frames((1, '[{"type":"resize"}]')),
    frames((0, b"x" * 201)),
    frames((3, "y" * 200)),
    frames((1, '{"type":"resize","cols":80.0,"rows":24,"type":"other"}'), (0, "")),
]
for i, data in enumerate(terminal):
    seed("api_terminal", f"messages-{i:02}", data)

# Activity: structured [flags, before, limit, project, workstream, task, session] + text, or raw
# [0x80 | flags] + a query string. Flags: bit 0 the index, bits 1-3 the log size.
for i, (flags, choices) in enumerate([
    (0, [0, 0, 0, 0, 0, 0]), (2 << 1, [0, 2 | 4, 0, 0, 0, 0]), (3 << 1, [2 | 8, 3, 0, 0, 4, 0]),
    ((3 << 1) | 1, [0, 2 | 40, 4, 0, 0, 0]), ((4 << 1) | 1, [1 | 200, 0, 0, 5, 4, 4 | 8]),
    ((2 << 1) | 1, [3, 1, 6, 6, 6, 6]), (2 << 1, [0, 0, 4, 0, 0, 0]), (2 << 1, [0, 0, 0, 0, 7, 0]),
]):
    seed("api_activity", f"structured-{i:02}", bytes([flags] + choices) + b"tsk_not-an-id")
for i, query in enumerate([
    "", "limit=0", "limit=1&before=3", "before=18446744073709551615", "limit=-1",
    "task=tsk_01JB0000000000000000000000&session=x", "project=prj_01JB0000000000000000000000",
    "before=1&before=2", "limit=%31%30", "workstream=&task=",
]):
    seed("api_activity", f"raw-{i:02}", bytes([0x80 | (2 << 1) | (i & 1)]) + query.encode())

# --- crates/sync-github ------------------------------------------------------------------------

GH = "https://api.github.com/repos/example-org/demo-repo"


def gh(status, headers, body):
    head = "".join(f"{k}: {v}\n" for k, v in headers)
    return f"{status}\n{head}\n{body}".encode()


milestone = {"number": 1, "title": "v1", "state": "open",
             "html_url": "https://github.com/example-org/demo-repo/milestone/1"}
issue = {"number": 7, "title": "Seed runs diverge", "body": "Closes nothing.", "state": "open",
         "labels": [{"name": "bug"}], "assignees": [{"login": "sam"}],
         "milestone": {"number": 1}, "updated_at": "2026-09-30T08:00:00Z",
         "html_url": "https://github.com/example-org/demo-repo/issues/7"}
closed = dict(issue, number=8, state="closed", state_reason="not_planned",
              updated_at="2026-09-30T09:00:00Z")
as_pr = dict(issue, number=9, pull_request={})
pull = {"number": 9, "title": "Fix the seed", "body": "Fixes #7", "state": "closed",
        "merged_at": "2026-09-30T10:00:00Z", "updated_at": "2026-09-30T10:00:00Z"}
github = [
    [gh(200, [("ETag", '"m1"')], line([milestone])),
     gh(200, [("Link", f'<{GH}/issues?page=2>; rel="next", <{GH}/issues?page=5>; rel="last"')],
        line([issue, as_pr])),
     gh(200, [], line([closed])),
     gh(200, [], line([pull]))],
    [gh(304, [], ""), gh(403, [("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "1790758800")],
                         '{"message":"API rate limit exceeded"}')],
    [gh(200, [], "[]"), gh(429, [("retry-after", "60")], ""),],
    [gh(200, [("Link", '<https://attacker.example/x>; rel="next"')], line([milestone]))],
    [gh(200, [], "[]"), gh(200, [], line([dict(issue, updated_at="yesterday"), {"number": "x"}]))],
    [gh(200, [], "[]"), gh(200, [], "[]"),
     gh(200, [("Link", f'<{GH}/pulls?page=2>; rel="next"')], line([pull])),
     gh(200, [], line([dict(pull, number=10, updated_at="2026-09-29T00:00:00Z")]))],
    [gh(200, [("Link", '<https://ghe.example.com/api/v3/repos/o/r/milestones?page=2>; rel="next"')],
        "[]")],
    # Near misses the check accepts: userinfo, dot segments that stay under the base.
    [gh(200, [("Link", '<https://user@api.github.com/repos/example-org/demo-repo/milestones?page=2>;'
                        ' rel="next"')], "[]")],
    [gh(200, [("Link", '<https://ghe.example.com/api/v3/./repos/x/../o/r/milestones?page=2>;'
                        ' rel="next"')], "[]")],
]
# Flags pick the API base: 0 api.github.com, 1 ghe.example.com/api/v3, 2 the same on port 8443.
bases = [0, 1, 2, 0, 1, 2, 1, 0, 1]
for i, responses in enumerate(github):
    seed("github_sync", f"responses-{i:02}", bytes([bases[i]]) + b"\xff".join(responses))

# --- crates/cli hooks --------------------------------------------------------------------------

EXE = "/opt/pitcrew/bin/pitcrew"
claude_settings = [
    '{\n  "env": {"EDITOR": "vim"},\n  "permissions": {"allow": ["Bash(ls)"]}\n}\n',
    '{"hooks": {"Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "afplay ding.aiff"}]}],'
    ' "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "lint"}]}]}}',
    '{"hooks": {"Stop": [{"matcher": "", "hooks": [{"type": "command", "command": '
    '"/old/bin/pitcrew hook claude Stop", "timeout": 5}]}]}}',
    '{"hooks": {"Stop": [{"matcher": "x", "hooks": [{"type": "command", "command": '
    '"afplay ding.aiff; ~/bin/pitcrew hook claude Stop"}, {"type": "command", "command": '
    f'"{EXE} hook claude Stop"}}]}}]}}}}',
    '{"hooks": {}, "a": 1}',
    '{"hooks": {"Stop": []}}',
    '﻿{"model": "demo"}',
    '{"hooks": 5}',
    '{"hooks": {"Stop": [1, "x", {"hooks": "no"}]}}',
    '{"a": 1, "a": 2}',
    '{"hooks": {"Stop": []}, "hooks": {}}',
    '[]',
    '  \n',
    '// comment\n{}',
]
for i, text in enumerate(claude_settings):
    seed("cli_hooks", f"claude-{i:02}", bytes([0]) + text.encode())
seed("cli_hooks", "claude-none", bytes([8]))
codex_configs = [
    'model = "demo-coder"\n# a comment\n\n[profiles.fast]\nmodel = "demo-fast"\n',
    'notify = ["notify-send", "Codex"]\nmodel = "x"\n',
    f'notify = ["{EXE}", "hook", "codex", "notify"]\n',
    f'notify = [\n  "{EXE}", # ours\n  "hook", "codex", "notify", "--chain",\n]\n',
    'notify = ["/home/sam/bin/pitcrew", "hook", "codex", "notify"] # moved\n[tui]\nx = 1\n',
    'notify = "not an array"\n',
    '[notify]\nx = 1\n',
    'a = { b = 1, c = [1.5, 2024-01-01T00:00:00Z] }\n[[arr]]\nv = true\n',
    '﻿model = "bom"\n',
    'not = = toml',
]
for i, text in enumerate(codex_configs):
    seed("cli_hooks", f"codex-{i:02}", bytes([1]) + text.encode())
    seed("cli_hooks", f"codex-chain-{i:02}", bytes([1 | 4]) + text.encode())
seed("cli_hooks", "codex-none", bytes([1 | 8]))
for i, text in enumerate([
    "// Generated by `pitcrew hooks install`. Safe to delete\nconst PITCREW = \"/old/pitcrew\";\n",
    "export const MyPlugin = async () => ({});\n",
    "",
]):
    seed("cli_hooks", f"opencode-{i:02}", bytes([2]) + text.encode())
seed("cli_hooks", "opencode-none", bytes([2 | 8]))

# --- crates/store import and crates/recap ------------------------------------------------------

event_lines = [line(e) for e in events]
seed("store_import", "demo", bytes([0]) + "\n".join(event_lines).encode() + b"\n")
seed("store_import", "crlf", bytes([0]) + "\r\n".join(event_lines[:3]).encode() + b"\r\n\r\n")
seed("store_import", "repeated", bytes([4]) + "\n".join(event_lines[1:4]).encode())
seed("store_import", "batch", bytes([125]) + event_lines[2].encode())
seed("store_import", "garbage", bytes([0]) + event_lines[0].encode() + b"\n{\"id\":1}\n")
seed("store_import", "empty", bytes([0]))

for i, window in enumerate(windows("\n".join(event_lines).encode() + b"\n", size=6, step=3)):
    seed("recap_blocks", f"events-{i:02}", bytes([2 * i, 3, 1, 4, 2]) + window)
seed("recap_blocks", "all", bytes([0, 0]) + "\n".join(event_lines).encode())
seed("recap_blocks", "all-small", bytes([1 | (2 << 1), 2, 7, 1]) + "\n".join(event_lines).encode())

# --- crates/remote SLURM -----------------------------------------------------------------------

# [mode] + text: 0 a wall time, 1 a duration (u64 seconds, u32 nanoseconds, little-endian),
# 2 an exit code, 3 #SBATCH options, one per line.
for i, text in enumerate([
    "30", "59:58", "1:59:58", "08:00:00", "2-0", "2-03:04", "1-02:03:04", " 12:00 ",
    "UNLIMITED", "INFINITE", "NOT_SET", "", "1:2:3:4", "-1", "1-", "1-2:3:4:5", "a:b", "+5",
    "999999999-23:59:59", "1234567890", "0-0:0:0", "000000001:000000001",
]):
    seed("remote_slurm", f"time-{i:02}", bytes([0]) + text.encode())
for i, (secs, nanos) in enumerate([
    (0, 0), (60, 0), (60, 500_000_000), (86_400 + 3_661, 0), (3 * 86_400, 1),
    (999_999_999 * 86_400 + 86_399, 0), (1_000_000_000 * 86_400, 0), (2**64 - 1, 0),
]):
    seed("remote_slurm", f"duration-{i:02}", bytes([1]) + struct.pack("<QI", secs, nanos))
for i, text in enumerate([
    "1:0", " 0:9 ", "0:15", "", "1", "a:b", "1:", "-1:0", "+1:0", "4294967295:4294967295",
    "4294967296:0", "1:2:3", "\t2:0\n",
]):
    seed("remote_slurm", f"exit-{i:02}", bytes([2]) + text.encode())
for i, lines in enumerate([
    ["--constraint=a100&ib", "--exclusive", "--mail-type=END", "--signal=B:TERM@60"],
    ["--comment=x", "--nodes=1", "--constraint=[a*2&b*4]"],
    ["--job-name=x", "--wrap=id", "--uid=0", "--chdir=/tmp", "--output=/tmp/x"],
    ["--exclusive=user", "--nice=10", "--nice", "--comment"],
    ["--constraint=a\r", "--Constraint=a", "-p gpu", "--comment=a#b", "--comment='a'"],
    ["--comment=a-hetjob-b", "--dependency=afterok:1", "--export=ALL"],
]):
    seed("remote_slurm", f"sbatch-{i:02}", bytes([3]) + "\n".join(lines).encode())

# Recipes: [flags] + TOML. Flags bits 0-2 pick the name (1 is example-cluster), bits 3-4 pad.
example_site = (HERE.parent / "crates" / "remote" / "src" / "helper" / "slurm"
                / "example-site.toml").read_text("utf-8")
for i, (flags, text) in enumerate([
    (1, example_site),
    (2, 'partition = "gpu"\n'),
    (2, 'partitoin = "gpu"\n'),
    (3, '[slurm]\npartition = "gpu"\n'),
    (2, 'partition.name = "gpu"\n'),
    (0, 'cpus = 2\nmemory = "8G"\ngres = "gpu:a100:2"\n'),
    (0, 'sbatch = ["--constraint=a100", "--nodes=1", "--exclusive"]\n'),
    (0, 'modules = ["python/3.12", "tool/1.0@abc"]\nmodules_init = "/etc/profile.d/modules.sh"\n'),
    (0, 'time = "08:00:00"\nlast_hop = "srun"\nsocket = "node-local"\naccount = "proj0001"\n'),
    (0, 'description = "a\\nb"\n'),
    (0, 'cpus = -1\nqos = "normal"\n'),
    (0, '# nothing\n'),
    (4, 'partition = "gpu"\n'),
    (2 | (1 << 3), 'partition = "gpu"\n'),
    (2 | (2 << 3), 'partition = "gpu"\n'),
    (2 | (3 << 3), 'partition = "gpu"\n'),
]):
    seed("remote_site", f"recipe-{i:02}", bytes([flags]) + text.encode())

# --- crates/sync-jira --------------------------------------------------------------------------

# ADF: [0] + a JSON document, or [1] + a program (see jira_adf.rs: op = b % 8, arg = b / 8).
def adf(*content):
    return {"type": "doc", "version": 1, "content": list(content)}


def para(*content):
    return {"type": "paragraph", "content": list(content)}


def text(t):
    return {"type": "text", "text": t}


nested = text("center")
for _ in range(40):
    nested = {"type": "blockquote", "content": [nested]}
for i, doc in enumerate([
    adf(para(text("Hello, "), text("world.")), para(text("Second paragraph."))),
    adf(para(text("cc "), {"type": "mention", "attrs": {"id": "abc123", "text": "@Alice"}},
             {"type": "emoji", "attrs": {"shortName": ":tada:"}})),
    adf(para({"type": "inlineCard", "attrs": {"url": "https://jira.example.com/browse/DEMO-1"}}),
        {"type": "rule"}, para(text("a"), {"type": "hardBreak"}, text("b"))),
    adf({"type": "codeBlock", "content": [text("fn main() {}")]},
        {"type": "bulletList", "content": [{"type": "listItem", "content": [para(text("one"))]}]},
        {"type": "table", "content": [{"type": "tableRow", "content": [
            {"type": "tableCell", "content": [para(text("cell"))]}]}]}),
    adf(nested),
    adf(para(text("safe‮loot rof"), text("a​b﻿c"), text("tag\U000E0041\U000E0042"),
             text("soft­hyphen᠎  "))),
    adf(para(1, "x", None, [text("in a list")], {"content": "no type"}, {"type": "text"})),
    {"type": "text", "text": "a bare text node"},
    "not a node",
]):
    seed("jira_adf", f"json-{i:02}", b"\x00" + line(doc).encode())
for i, program in enumerate([
    [0, 2, 10, 1],                 # a paragraph of ten 'a'
    [2, 1, 6 + 8 * 15],            # one 'a', repeated 32,768 times (to the build cap)
    [2, 5, 7 + 8 * 31],            # five 'a' in 128 quotes
    [2 + 8 * 3, 15, 1],            # 61,440 'é'
    [0, 3, 3 + 8 * 5, 4, 4 + 8 * 1, 4 + 8 * 2, 5, 5 + 8, 1],
    [0 + 8 * 1, 2, 3, 6 + 8 * 4, 1, 7 + 8 * 7, 6 + 8 * 8],
]):
    seed("jira_adf", f"program-{i:02}", bytes([1] + program))

JIRA_AT = "2026-09-30T10:00:00.000+0200"


def jira_issue(key, summary, *, kind="Story", category="indeterminate", description=None,
               labels=(), parent=None, extra=None, updated=JIRA_AT, ident=None):
    fields = {"summary": summary, "status": {"statusCategory": {"key": category}},
              "issuetype": {"name": kind}, "updated": updated, "labels": list(labels),
              "assignee": {"accountId": "acc-1", "displayName": "Demo‮User"},
              "resolution": {"name": "Fixed"} if category == "done" else None}
    if description is not None:
        fields["description"] = description
    if parent:
        fields["parent"] = {"key": parent}
    fields.update(extra or {})
    return {"id": ident or key.split("-")[-1], "key": key, "fields": fields}


def jira_seed(flags, key, cursor, myself, *pages):
    parts = [key.encode(), cursor.encode(), myself.encode()] + [p.encode() for p in pages]
    return bytes([flags]) + b"\xff".join(parts)


ok_tz = '200\n\n{"timeZone":"Europe/Brussels"}'
cloud_pages = [
    "200\n\n" + line({"issues": [
        jira_issue("DEMO-1", "Seed runs diverge", description=adf(para(text("See the log."))),
                   labels=["bug", "x​y"], parent="DEMO-9"),
        jira_issue("DEMO-9", "The epic", kind="Epic"),
    ], "nextPageToken": "page-2"}),
    "200\n\n" + line({"issues": [
        jira_issue("DEMO-2", "Done one", category="done", description="plain v2 text"),
        jira_issue("DEMO-01", "leading zero"), jira_issue("demo-3", "lower case"),
        jira_issue("DEMO-1/../../secure/Logout.jspa", "path"),
        jira_issue("DEMO-1​2", "hidden"), {"id": "x"}, 5,
    ]}),
]
dc_page = "200\n\n" + line({"startAt": 0, "total": 3, "issues": [
    jira_issue("DEMO-4", "From Data Center", description="text", extra={"customfield_10008": "DEMO-9"}),
    jira_issue("DEMO-5", "Bad updated", updated="yesterday"),
]})
for i, data in enumerate([
    jira_seed(0, "DEMO", "2026-09-29T08:00:00Z", ok_tz, *cloud_pages),
    jira_seed(1 | 2, "DEMO", "", '200\n\n{"timeZone":"America/New_York"}', dc_page,
              "200\n\n" + line({"startAt": 2, "total": 3, "issues": []})),
    jira_seed(0, "AB1", "2026-01-02 03:04", ok_tz, '429\nRetry-After: 30\n\n'),
    jira_seed(1, "DEMO", "not a time", "500\n\n", "429\n\n"),
    jira_seed(0, "DEMO\" OR 1=1 --", "\" OR updated > \"0", '200\n\n{"timeZone":"../../etc/x"}',
              cloud_pages[0]),
    jira_seed(8, "ABCDEFGHIJ", "-009999-01-01T00:00:00Z", '200\n\n{"timeZone":"Mars/Olympus"}',
              "200\n\n" + line({"issues": [], "nextPageToken": "t"}), "200\n\nnot json"),
    jira_seed(1 | 4, "DEMO", "9999-12-31T23:59:59Z", ok_tz,
              "200\n\n" + line({"startAt": 0, "total": 1000, "issues": [jira_issue("DEMO-6", "x")]})),
]):
    seed("jira_sync", f"responses-{i:02}", data)

# --- crates/sync-github links ------------------------------------------------------------------

# [base] + next link, 0xFF, html_url, 0xFF, pull request body.
GHE = "https://ghe.example.com"
for i, (base, link, html, body) in enumerate([
    (0, "https://api.github.com/repositories/724712/milestones?page=2",
     "https://github.com/example-org/demo-repo/issues/7", "Fixes #12 and other-org/other-repo#45."),
    (1, f"{GHE}/api/v3/repos/o/r/milestones?page=2", f"{GHE}/example-org/demo-repo/issues/7",
     "Resolves: #5, #6"),
    (2, f"{GHE}:8443/api/v3/x?page=2", f"{GHE}/o/r/pull/9", "This closed #1, #2 and #3 for good."),
    (1, f"{GHE}/api/v3/../../admin", "javascript:alert(1)", "Closes ../evil#1"),
    (1, f"{GHE}/api/v3/%2e%2e/%2e%2e/admin", "http://github.com/x", "```\nCloses #99\n```\nFixes #1"),
    (0, "https://attacker.example\\@api.github.com/x", "https://github.com.attacker.example/x",
     "> Fixes #2\nFixes ow@ner/repo#1"),
    (1, f"{GHE}/api/v3evil/x", "https://github.com/x‮", "fixes evil/..#3"),
    (2, f"{GHE}:8443/api/v3/./repos/x/../o/r?page=2", f"{GHE}:8443/o/r", "Fixed: a.b/c-d#4"),
    (0, "/relative?page=2", "data:text/html,x", "closes #-1 #+2 #18446744073709551616"),
]):
    data = bytes([base]) + b"\xff".join(s.encode() for s in (link, html, body))
    seed("github_links", f"links-{i:02}", data)

# --- crates/api recaps -------------------------------------------------------------------------

ws_ids = [w["id"] for w in workspace["workstreams"]]
pr_ids = [p["id"] for p in workspace["projects"]]
se_ids = [s["id"] for s in workspace["sessions"]]
for i, (flags, query) in enumerate([
    (0, ""), (0, "limit=0"), (0, "limit=1&before=x"), (0, f"session={se_ids[0]}&limit=500"),
    (0, f"workstream={ws_ids[0]}&project={pr_ids[0]}&task=tsk_x"), (0, "limit=1&limit=2"),
    (1, f"workstream={ws_ids[0]}"), (1, f"project={pr_ids[0]}&tz=-840&before=2026-09-30"),
    (1, f"workstream={ws_ids[1]}&tz=841"), (1, f"project={pr_ids[1]}&before=2026-02-31&limit=31"),
    (1, f"workstream={ws_ids[0]}&project={pr_ids[0]}"), (1, "tz=60"),
    (1, f"workstream={ws_ids[2]}&tz=%2B60"), (1, f"workstream={ws_ids[2]}&limit=99999999999999999999"),
]):
    seed("api_recaps", f"raw-{i:02}", bytes([0x80 | flags]) + query.encode())
for i, (flags, choices) in enumerate([
    (0, [0, 0, 0, 0, 0, 0]), (0, [2, 3, 2 | 8, 3, 2, 2 | (5 << 2)]), (0, [4, 5, 6, 7, 2, 2]),
    (1, [2, 0, 2 | (9 << 2), 2, 2 | (1 << 2), 0]), (1, [0, 2, 2 | (10 << 2), 2 | (1 << 2), 2, 0]),
    (1, [2, 2, 0, 0, 0, 0]), (1, [3, 0, 3, 3, 3, 0]), (0, [7, 7, 7, 7, 7, 3]),
]):
    seed("api_recaps", f"structured-{i:02}", bytes([flags] + choices) + b"2026-09-30")

# --- crates/hub-work recap index ---------------------------------------------------------------

# [config, k, cache, queries] + k % 8 batch sizes + events.
for i, window in enumerate(windows("\n".join(event_lines).encode() + b"\n", size=6, step=3)):
    seed("recap_index", f"events-{i:02}", bytes([2 * i, 3, i, 8 * i + 1, 1, 4, 2]) + window)
seed("recap_index", "all", bytes([0, 0, 3, 2]) + "\n".join(event_lines).encode())
seed("recap_index", "all-small", bytes([1 | (2 << 1), 2, 1, 3, 7, 1]) + "\n".join(event_lines).encode())

for target, n in sorted(counts.items()):
    print(f"{target}: {n} seeds")
print(f"total: {sum(counts.values())} seeds, {seeded_bytes[0] / 1024:.0f} KiB")
