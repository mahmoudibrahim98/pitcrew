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
export const boardDraft: import('../index.ts').BoardDraft = {
  "accepted": [],
  "agent": "01J00000000000000000000000",
  "by": "01J00000000000000000000000",
  "cost": {
    "estimate": {
      "input_tokens": 15600,
      "output_tokens": 8192
    },
    "prompt_bytes": 2400,
    "redacted": 0,
    "sessions": 1,
    "sessions_left_out": 0,
    "summary_bytes": 300,
    "tasks": 0
  },
  "engine": "claude",
  "id": "01J00000000000000000000000",
  "prompt": "draft-board/v1",
  "proposal": {
    "tasks": [
      {
        "evidence": [
          "01J00000000000000000000000"
        ],
        "status": "todo",
        "title": "Synthetic task"
      }
    ]
  },
  "proposed": 43,
  "rejected": [],
  "session": "01J00000000000000000000000",
  "started": 42,
  "state": "proposed",
  "workstream": "01J00000000000000000000000"
};
export const conversation: import('../index.ts').Conversation = {
  "agent": "01J00000000000000000000000",
  "engine": "claude",
  "id": "01J00000000000000000000000",
  "session": "01J00000000000000000000000",
  "started": 42,
  "turns": [
    {
      "answer": "DEMO-1 moved on in ses_01J00000000000000000000000.",
      "asked": 42,
      "ended": 12042,
      "question": "What did my agents do today?",
      "references": [
        {
          "label": "DEMO-1 Synthetic task",
          "target": {
            "id": "01J00000000000000000000000",
            "key": "DEMO-1",
            "kind": "task"
          },
          "text": "DEMO-1"
        }
      ],
      "session": "01J00000000000000000000000",
      "state": "answered",
      "suggestions": [
        {
          "key": "DEMO-1",
          "kind": "move_task",
          "label": "Move DEMO-1 to review",
          "task": "01J00000000000000000000000",
          "to": "review"
        }
      ],
      "usage": {
        "answer_bytes": 52,
        "duration_ms": 12000,
        "tool_runs": 3
      }
    }
  ]
};
export const allImport: import('../index.ts').ImportFilter = { mode: 'all' };
export const filteredImport: import('../index.ts').ImportFilter = { mode: 'filtered', since: '2026-01-01' };
