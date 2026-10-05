// Generated from Rust values; checked with tsc --noEmit.
export const task: import('../index.ts').Task = {
  "accept_auto": false,
  "assignee": "01JB000000000000000MEM0002",
  "blocked_by": [],
  "description": "Draft §3 Method from notes/method-outline.md. Keep it under two pages.",
  "due": "2026-10-10",
  "id": "01JB000000000000000TSK0001",
  "key": "PAP-1",
  "labels": [
    "writing"
  ],
  "priority": "high",
  "project": "01JB000000000000000PRJ0001",
  "status": "in_progress",
  "subtasks": [
    {
      "done": true,
      "id": "01JB000000000000000SBT0001",
      "source": {
        "agent": "01JB000000000000000MEM0002",
        "kind": "agent_plan"
      },
      "text": "Read notes/method-outline.md"
    },
    {
      "done": true,
      "id": "01JB000000000000000SBT0002",
      "source": {
        "agent": "01JB000000000000000MEM0002",
        "kind": "agent_plan"
      },
      "text": "Write §3.1 Model"
    },
    {
      "done": false,
      "id": "01JB000000000000000SBT0003",
      "source": {
        "agent": "01JB000000000000000MEM0002",
        "kind": "agent_plan"
      },
      "text": "Write §3.2 Noise schedule"
    },
    {
      "done": false,
      "id": "01JB000000000000000SBT0004",
      "source": {
        "agent": "01JB000000000000000MEM0002",
        "kind": "agent_plan"
      },
      "text": "Write §3.3 Training objective"
    }
  ],
  "title": "Draft the method section",
  "workstream": "01JB000000000000000WST0001"
};
export const machine: import('../index.ts').Machine = {
  "id": "01JB000000000000000MCH0001",
  "info": {
    "arch": "x86_64",
    "has_tmux": true,
    "home_on_network_fs": false,
    "hostname": "laptop",
    "os": "linux"
  },
  "kind": "local",
  "liveness": "live",
  "name": "This laptop"
};
export const taskId: import('../index.ts').TaskId = "01J00000000000000000000000";
export const event: import('../index.ts').Event = {
  "at": 42,
  "author": "01J00000000000000000000000",
  "body": {
    "data": {
      "task": "01J00000000000000000000000"
    },
    "type": "task_assigned"
  },
  "id": "01J00000000000000000000000",
  "workspace": "01J00000000000000000000000"
};
export const taskKey: import('../index.ts').TaskKey = "DEMO-1";
export const date: import('../index.ts').Date = "2026-01-01";
export const patch: import('../index.ts').TaskPatch = {
  "due": null,
  "title": "Clear due date",
  "workstream": null
};
export const emptyPatch: import('../index.ts').TaskPatch = {};
export const error: import('../index.ts').ApiError = {
  "code": "forbidden",
  "message": "Refused"
};
export const hello: import('../index.ts').StreamFrame = {
  "log": "synthetic-log",
  "rev": 42,
  "type": "hello"
};
export const tool: import('../index.ts').TranscriptItem = {
  "at": 42,
  "call_id": "synthetic-call",
  "input": {
    "nested": [
      null,
      true,
      2
    ],
    "path": "README.md"
  },
  "kind": "tool_use",
  "offset": 8,
  "target": "README.md",
  "tool": "Read"
};
export const span: import('../index.ts').Span = {
  "range": {
    "end": 2,
    "start": 0
  },
  "receipts": [
    {
      "id": "01J00000000000000000000000",
      "kind": "event"
    }
  ]
};
export const fact: import('../index.ts').FactKind = {
  "type": "session_started"
};
export const command: import('../index.ts').RunnerCommand = {
  "mode": "graceful",
  "session": "01J00000000000000000000000",
  "type": "end_session"
};
export const integration: import('../index.ts').Integration = {
  "added_at": 42,
  "added_by": "01J00000000000000000000000",
  "credential": {
    "source": "gh_cli",
    "stored": false
  },
  "id": "01J00000000000000000000000",
  "interval_minutes": 15,
  "links": [
    {
      "scope": {
        "key": "example-org/demo-repo#milestone:1",
        "system": "github",
        "url": "https://github.com/example-org/demo-repo/milestone/1"
      },
      "title": "v1 launch",
      "workstream": "01J00000000000000000000000"
    }
  ],
  "name": "Demo repositories",
  "settings": {
    "kind": "github",
    "repos": [
      "example-org/demo-repo"
    ]
  },
  "status": {
    "last_attempt_at": 40,
    "last_run": {
      "applied": 0,
      "changes": 0,
      "conflicts": 0,
      "malformed": 0,
      "skipped": 0
    },
    "last_success_at": 41,
    "problems": [],
    "running": false
  }
};
export const linked: import('../index.ts').EventBody = {
  "data": {
    "external": [],
    "workstream": "01J00000000000000000000000"
  },
  "type": "workstream_linked"
};
export const write: import('../index.ts').UpstreamWrite = {
  "answered_at": 41,
  "answered_by": "01J00000000000000000000000",
  "attempts": 1,
  "finished_at": 42,
  "proposal": {
    "after": {
      "close_reason": "completed",
      "state": "closed"
    },
    "ask": "01J00000000000000000000000",
    "before": {
      "state": "open"
    },
    "cause": "01J00000000000000000000000",
    "integration": "01J00000000000000000000000",
    "operation": "close",
    "requested_by": "01J00000000000000000000000",
    "scope": "example-org/demo-repo",
    "system": "github",
    "target": {
      "key": "example-org/demo-repo#1",
      "system": "github",
      "url": "https://github.com/example-org/demo-repo/issues/1"
    },
    "task": "01J00000000000000000000000"
  },
  "proposed_at": 40,
  "result": {
    "outcome": "sent",
    "url": "https://github.com/example-org/demo-repo/issues/1"
  },
  "state": "sent"
};
export const writeRetryRequested: import('../index.ts').EventBody = {
  "data": {
    "ask": "01J00000000000000000000000",
    "by": "01J00000000000000000000000",
    "task": "01J00000000000000000000000"
  },
  "type": "write_retry_requested"
};
export const writeFinished: import('../index.ts').EventBody = {
  "data": {
    "ask": "01J00000000000000000000000",
    "result": {
      "outcome": "not_sent",
      "reason": "Not sent: Sam chose not to."
    },
    "task": "01J00000000000000000000000"
  },
  "type": "write_finished"
};
export const machineCheck: import('../index.ts').MachineCheck = {
  "rows": [
    {
      "detail": "Not on PATH.",
      "fix": "install_page",
      "id": "cli_claude",
      "status": "missing"
    },
    {
      "detail": "tmux 3.4",
      "id": "tmux",
      "status": "ok",
      "version": "tmux 3.4"
    }
  ]
};
export const agentAccount: import('../index.ts').AgentAccount = {
  "account": "ChatGPT",
  "engine": "codex",
  "installed": true,
  "signed_in": true
};
export const signIn: import('../index.ts').SignIn = {
  "command": [
    "claude",
    "auth",
    "login"
  ],
  "engine": "claude",
  "running": true,
  "started": 42,
  "terminal": "01J00000000000000000000000"
};
export const allImport: import('../index.ts').ImportFilter = { mode: 'all' };
export const filteredImport: import('../index.ts').ImportFilter = { mode: 'filtered', since: '2026-01-01' };
