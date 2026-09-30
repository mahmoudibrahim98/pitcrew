# Generates synthetic OpenCode store history for tests: one JSON line per row write, in the order
# OpenCode would write them (inserts, then updates while parts stream). All content is invented;
# the shapes follow a real OpenCode 1.18 store. Run: python generate.py > history.jsonl
import json, random, sys

rnd = random.Random(7)
B62 = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
M48 = (1 << 48) - 1
counter = {"last": 0, "n": 0}

def ascending(prefix, ms):
    if ms != counter["last"]:
        counter["last"], counter["n"] = ms, 0
    counter["n"] += 1
    v = (ms * 0x1000 + counter["n"]) & M48
    return f"{prefix}_{v:012x}" + "".join(rnd.choice(B62) for _ in range(14))

def descending(prefix, ms):
    v = (~(ms * 0x1000 + 1)) & M48
    return f"{prefix}_{v:012x}" + "".join(rnd.choice(B62) for _ in range(14))

ops = []
T = 1790756400000  # 2026-09-30T08:20:00Z
now = [T]

def tick(ms=250):
    now[0] += ms
    return now[0]

def put(table, row):
    ops.append({"table": table, "row": row})

PROJECT = "prj_synthetic_lab_tools"
DIR = "/home/dev/lab-tools"

def session(sid, title, parent=None, created=None):
    row = {"id": sid, "project_id": PROJECT, "parent_id": parent, "slug": "brave-otter",
           "directory": DIR, "title": title, "version": "1.18.30", "time_created": created,
           "time_updated": created, "agent": "build",
           "model": json.dumps({"id": "demo-coder-2", "providerID": "demo"})}
    put("session", row)
    return row

def message(sid, data, created):
    mid = ascending("msg", created)
    row = {"id": mid, "session_id": sid, "time_created": created, "time_updated": created,
           "data": data}
    put("message", row)
    return row

def update_message(row, **changes):
    row = dict(row)
    data = json.loads(json.dumps(row["data"]))
    for k, v in changes.items():
        if k == "time_completed":
            data.setdefault("time", {})["completed"] = v
        else:
            data[k] = v
    row["data"] = data
    row["time_updated"] = tick(10)
    put("message", row)
    return row

def part(msg, data, created=None):
    created = created or tick()
    pid = ascending("prt", created)
    row = {"id": pid, "message_id": msg["id"], "session_id": msg["session_id"],
           "time_created": created, "time_updated": created, "data": data}
    put("part", row)
    return row

def update(row, data):
    row = dict(row, data=data, time_updated=tick(100))
    put("part", row)
    return row

def user_msg(sid, text, synthetic_extra=None):
    t = tick(1000)
    m = message(sid, {"role": "user", "time": {"created": t}, "agent": "build",
                      "model": {"providerID": "demo", "modelID": "demo-coder-2"}}, t)
    part(m, {"type": "text", "text": text}, t)
    if synthetic_extra:
        part(m, {"type": "text", "text": synthetic_extra, "synthetic": True}, t)
    return m

def assistant_msg(sid, parent):
    t = tick(300)
    return message(sid, {"role": "assistant", "time": {"created": t}, "parentID": parent["id"],
                         "modelID": "demo-coder-2", "providerID": "demo", "mode": "build",
                         "agent": "build", "path": {"cwd": DIR, "root": DIR},
                         "cost": 0, "tokens": {"input": 0, "output": 0, "reasoning": 0,
                                               "cache": {"read": 0, "write": 0}}}, t)

def text(msg, chunks):
    t = tick()
    p = part(msg, {"type": "text", "text": chunks[0], "time": {"start": t}}, t)
    acc = chunks[0]
    for c in chunks[1:]:
        acc += c
        p = update(p, {"type": "text", "text": acc, "time": {"start": t}})
    return update(p, {"type": "text", "text": acc, "time": {"start": t, "end": tick(50)}})

def reasoning(msg, s):
    t = tick()
    p = part(msg, {"type": "reasoning", "text": s[:10], "time": {"start": t}}, t)
    return update(p, {"type": "reasoning", "text": s, "time": {"start": t, "end": tick(50)}})

def tool(msg, name, inp, final_state, call=None, stop_at=None):
    call = call or "call_" + "".join(rnd.choice(B62) for _ in range(20))
    p = part(msg, {"type": "tool", "tool": name, "callID": call,
                   "state": {"status": "pending", "input": {}, "raw": ""}})
    start = tick(20)
    p = update(p, {"type": "tool", "tool": name, "callID": call,
                   "state": {"status": "running", "input": inp, "time": {"start": start}}})
    if stop_at == "running":
        return p
    st = dict(final_state)
    st.setdefault("input", inp)
    st["time"] = {"start": start, "end": tick(400)}
    return update(p, {"type": "tool", "tool": name, "callID": call, "state": st})

def step_start(msg):
    part(msg, {"type": "step-start", "snapshot": "4b825dc642cb6eb9a060e54bf8d69288fbee4904"})

def step_finish(msg, reason):
    part(msg, {"type": "step-finish", "reason": reason, "snapshot": "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
               "cost": 0, "tokens": {"input": 1200, "output": 80, "reasoning": 0, "total": 1280,
                                     "cache": {"read": 0, "write": 0}}})

def unified(path, old, new):
    lines = [f"Index: {path}", "=" * 67, f"--- {path}", f"+++ {path}",
             f"@@ -1,{len(old)} +1,{len(new)} @@"]
    lines += ["-" + l for l in old] + ["+" + l for l in new]
    return "\n".join(lines) + "\n"

# --- main session -------------------------------------------------------------------------------
SID = descending("ses", T)
s = session(SID, "New session - 2026-09-30T08:20:00.000Z", created=T)

u1 = user_msg(SID, "Add a --dry-run flag to scripts/sync.py and a test for it.")
a1 = assistant_msg(SID, u1)
step_start(a1)
reasoning(a1, "Need to read the script first, then plan.")
text(a1, ["I'll read ", "the script ", "and plan the change."])
tool(a1, "read", {"filePath": f"{DIR}/scripts/sync.py"},
     {"status": "completed", "output": "<file>\n00001| import argparse\n00002| \n00003| def main():\n</file>",
      "title": "scripts/sync.py", "metadata": {"preview": "import argparse", "truncated": False}})
todos = [{"content": "Add --dry-run to the parser", "status": "in_progress", "priority": "high"},
         {"content": "Skip writes when dry", "status": "pending", "priority": "high"},
         {"content": "Test the flag", "status": "pending", "priority": "medium"}]
tool(a1, "todowrite", {"todos": todos},
     {"status": "completed", "output": json.dumps(todos, indent=2), "title": "3 todos",
      "metadata": {"todos": todos, "truncated": False}})
old = ["parser = argparse.ArgumentParser()"]
new = ["parser = argparse.ArgumentParser()", "parser.add_argument(\"--dry-run\", action=\"store_true\")"]
patch = unified(f"{DIR}/scripts/sync.py", old, new)
tool(a1, "edit", {"filePath": f"{DIR}/scripts/sync.py", "oldString": old[0], "newString": "\n".join(new)},
     {"status": "completed", "output": "Edit applied successfully.", "title": "scripts/sync.py",
      "metadata": {"diagnostics": {}, "diff": patch, "truncated": False,
                   "filediff": {"file": f"{DIR}/scripts/sync.py", "additions": 1, "deletions": 0, "patch": patch}}})
step_finish(a1, "tool-calls")
a1 = update_message(a1, finish="tool-calls", time_completed=tick(10))
s = dict(s, title="Dry-run flag for the sync script", time_updated=tick(5))
put("session", s)

a2 = assistant_msg(SID, u1)
step_start(a2)
tool(a2, "bash", {"command": "python -m pytest tests/test_sync.py -q", "description": "Run the sync tests"},
     {"status": "completed", "output": "F\n1 failed in 0.21s\n",
      "metadata": {"output": "F\n1 failed in 0.21s\n", "exit": 1, "description": "Run the sync tests", "truncated": False}})
tool(a2, "write", {"filePath": f"{DIR}/tests/test_dry_run.py", "content": "def test_dry_run():\n    assert run(['--dry-run']) == 0\n"},
     {"status": "completed", "output": "Wrote file successfully.",
      "metadata": {"diagnostics": {}, "filepath": f"{DIR}/tests/test_dry_run.py", "exists": False, "truncated": False}})
tool(a2, "bash", {"command": "python -m pytest -q", "description": "Run all tests"},
     {"status": "completed", "output": "....\n4 passed in 0.40s\n",
      "metadata": {"output": "....\n4 passed in 0.40s\n", "exit": 0, "truncated": False}})
tool(a2, "question", {"questions": [{"question": "Should --dry-run also print the planned writes?", "header": "Output",
                                     "multiple": False, "options": [{"label": "Yes", "description": "List each write"},
                                                                    {"label": "No", "description": "Stay quiet"}]}]},
     {"status": "completed", "output": "User answered: Yes", "title": "Asked 1 question",
      "metadata": {"answers": [["Yes"]], "truncated": False}})
text(a2, ["Added `--dry-run`; ", "the new test passes and dry runs list the planned writes."])
step_finish(a2, "stop")
update_message(a2, finish="stop", time_completed=tick(10))

# --- a sub-session started by the task tool ------------------------------------------------------
u2 = user_msg(SID, "Also document the flag in the README, and check other scripts for writes.")
a3 = assistant_msg(SID, u2)
step_start(a3)
CHILD = descending("ses", now[0] + 50)
task_call = "call_task_" + "".join(rnd.choice(B62) for _ in range(12))
tp = part(a3, {"type": "tool", "tool": "task", "callID": task_call,
               "state": {"status": "pending", "input": {}, "raw": ""}})
start = tick(20)
task_input = {"description": "Find scripts that write files", "prompt": "List every script under scripts/ that writes files.",
              "subagent_type": "explore"}
tp = update(tp, {"type": "tool", "tool": "task", "callID": task_call,
                 "state": {"status": "running", "input": task_input, "time": {"start": start},
                           "metadata": {"sessionId": CHILD}}})
child = session(CHILD, "Child session - Find scripts that write files", parent=SID, created=tick(10))
cu = user_msg(CHILD, "List every script under scripts/ that writes files.")
ca = assistant_msg(CHILD, cu)
step_start(ca)
tool(ca, "grep", {"pattern": "open\\(.*[\"']w", "include": "*.py"},
     {"status": "completed", "output": "Found 2 matches\nscripts/sync.py:\n  Line 40: with open(out, 'w') as f:",
      "metadata": {"matches": 2, "truncated": False}})
text(ca, ["scripts/sync.py and scripts/export.py write files."])
step_finish(ca, "stop")
update_message(ca, finish="stop", time_completed=tick(10))
tp = update(tp, {"type": "tool", "tool": "task", "callID": task_call,
                 "state": {"status": "completed", "input": task_input, "output": "scripts/sync.py and scripts/export.py write files.",
                           "title": "Find scripts that write files", "time": {"start": start, "end": tick(10)},
                           "metadata": {"sessionId": CHILD, "model": {"modelID": "demo-coder-2", "providerID": "demo"}, "truncated": False}}})
tool(a3, "apply_patch", {"patchText": "*** Begin Patch\n*** Update File: README.md\n@@ ## Usage\n+Pass `--dry-run` to see the planned writes.\n*** Add File: docs/dry-run.md\n+# Dry runs\n+Nothing is written.\n*** End Patch"},
     {"status": "completed", "output": "Success. Updated the following files:\nM README.md\nA docs/dry-run.md",
      "metadata": {"truncated": False}})
tool(a3, "edit", {"filePath": f"{DIR}/scripts/export.py", "oldString": "open(out, 'w')", "newString": "open(out, 'x')"},
     {"status": "error", "error": "oldString not found in content"})
# A payload OpenCode would never write, to prove it is skipped, not fatal.
bad = part(a3, "{\"type\": \"text\", \"text\": ")
part(a3, {"type": "tool", "tool": 42, "callID": ["not", "a", "string"]})
text(a3, ["README updated; export.py needs a closer look."])
step_finish(a3, "stop")
update_message(a3, finish="stop", time_completed=tick(10))

# --- an interrupted turn -------------------------------------------------------------------------
u3 = user_msg(SID, "Run the slow integration suite.", synthetic_extra="The user has attached no files.")
a4 = assistant_msg(SID, u3)
step_start(a4)
tool(a4, "bash", {"command": "python -m pytest -m slow", "description": "Slow suite"}, {}, stop_at="running")
update_message(a4, error={"name": "MessageAbortedError", "data": {"message": "The operation was aborted."}},
               time_completed=tick(10))
s = dict(s, time_updated=now[0])
put("session", s)

for op in ops:
    sys.stdout.write(json.dumps(op, separators=(",", ":")) + "\n")
