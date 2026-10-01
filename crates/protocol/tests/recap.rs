//! Contract tests for the recap wire types (`GET /v1/recaps/blocks` and `/v1/recaps/days`).

use pitcrew_protocol::ids::{AskId, EventId, MemberId, ProjectId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{Date, Receipt};
use pitcrew_protocol::recap::{
    Block, BlockKey, BlocksPage, Check, Counts, DayRecap, DaysPage, FactKind, RecapBlock, Span,
    Summary,
};
use serde_json::{Value, json};

/// Decodes `value` as `T`, and checks that it encodes back to exactly `value`.
fn round_trip<T>(value: &Value) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let decoded: T = serde_json::from_value(value.clone()).expect("decodes");
    assert_eq!(&serde_json::to_value(&decoded).expect("encodes"), value);
    decoded
}

fn event_receipt(id: EventId) -> Value {
    json!({"kind": "event", "id": id})
}

#[test]
fn a_span_is_a_utf8_byte_range_written_start_and_end() {
    let ev = EventId::new();
    // "É" is two bytes in UTF-8 but one UTF-16 unit, so byte and string offsets differ.
    let text = "Évidence first, then −12 lines";
    let summary: Summary = round_trip(&json!({
        "text": text,
        "spans": [
            {"range": {"start": 0, "end": 15}, "receipts": [event_receipt(ev)]},
            {"range": {"start": 17, "end": 33}, "receipts": [event_receipt(ev)]}
        ]
    }));
    assert_eq!(summary.spans[0].range, 0..15);
    assert_eq!(summary.clause(&summary.spans[0]), "Évidence first");
    assert_eq!(summary.clause(&summary.spans[1]), "then −12 lines");
    assert_eq!(text.len(), 33);
    assert_eq!(
        text.encode_utf16().count(),
        30,
        "a UTF-16 client must convert"
    );
    assert_eq!(summary.receipts().count(), 2);
    // A range off a character boundary has no text; the engine never writes one.
    let off = Span {
        range: 1..3,
        receipts: vec![Receipt::Event { id: ev }],
    };
    assert_eq!(summary.clause(&off), "");
}

fn full_block() -> Value {
    let (first, last) = (EventId::new(), EventId::new());
    let session = SessionId::new();
    let (writer, sam) = (MemberId::new(), MemberId::new());
    let (task, ws, ask) = (TaskId::new(), WorkstreamId::new(), AskId::new());
    let transcript = json!({"kind": "transcript", "session": session, "offset": 4096});
    let fact = |kind: Value| {
        json!({"by": writer, "at": 1_790_755_200_000_i64, "kind": kind,
               "receipts": [event_receipt(first)]})
    };
    json!({
        "id": first, "last": last,
        "key": {"kind": "session", "id": session},
        "start": 1_790_755_200_000_i64, "end": 1_790_756_400_000_i64,
        "session": session, "workstream": ws, "project": ProjectId::new(),
        "tasks": [task], "agent": writer, "actors": [writer, sam],
        "counts": {"events": 9, "tools_run": 3, "tools_failed": 1, "file_edits": 1,
                   "lines_added": 84, "lines_removed": 12, "turns": 1, "asks_raised": 1,
                   "asks_answered": 1, "task_moves": 1, "comments": 1},
        "files": [{"path": "paper/method.tex", "edits": 1, "added": 84, "removed": 12,
                   "receipts": [event_receipt(first)]}],
        "files_omitted": 0,
        "facts": [
            fact(json!({"type": "session_started", "title": "Draft method section"})),
            fact(json!({"type": "session_linked", "workstream": ws, "task": task})),
            fact(json!({"type": "session_waiting", "status_line": "Asks: merge?"})),
            fact(json!({"type": "session_ended"})),
            fact(json!({"type": "dispatch_started", "task": task, "agent": writer})),
            fact(json!({"type": "dispatch_finished", "task": task, "outcome": "succeeded",
                        "summary": "Drafted §3.1."})),
            fact(json!({"type": "task_created", "task": task})),
            fact(json!({"type": "task_moved", "task": task, "from": "todo", "to": "review"})),
            fact(json!({"type": "task_assigned", "task": task, "assignee": writer})),
            fact(json!({"type": "plan_updated", "task": task, "done": 2, "total": 4})),
            fact(json!({"type": "checks", "check": "tests", "runs": 2, "failures": 1,
                        "last_failed": false})),
            fact(json!({"type": "job_diverged", "jobs": ["4815164"]})),
            fact(json!({"type": "ask_raised", "ask": ask, "ask_kind": "decision", "to": sam,
                        "title": "Rerun seed 3?"})),
            fact(json!({"type": "ask_answered", "ask": ask})),
            fact(json!({"type": "commented", "task": task, "mentions": [sam]})),
            fact(json!({"type": "decision_recorded", "text": "Drop seed 3."})),
            fact(json!({"type": "workstream_created", "workstream": ws})),
            fact(json!({"type": "workstream_changed", "workstream": ws, "status": "active",
                        "health": "at_risk"})),
            fact(json!({"type": "brief_accepted", "target": {"kind": "workstream", "id": ws},
                        "pinned": true})),
        ],
        "facts_omitted": 2,
        "tool_receipts": [event_receipt(first), transcript],
        "turn_receipts": [event_receipt(last)]
    })
}

#[test]
fn a_block_round_trips_with_every_fact_kind() {
    let block: Block = round_trip(&full_block());
    assert!(matches!(block.key, BlockKey::Session(s) if Some(s) == block.session));
    assert_eq!(block.facts.len(), 19);
    assert!(matches!(
        block.facts[10].kind,
        FactKind::Checks {
            check: Check::Tests,
            runs: 2,
            ..
        }
    ));
    // Every receipt: one per fact, one per file, the tool runs' and the turns'.
    assert_eq!(block.receipts().count(), 19 + 1 + 2 + 1);
}

#[test]
fn a_block_leaves_out_what_it_does_not_know() {
    let (id, ws) = (EventId::new(), WorkstreamId::new());
    let minimal = json!({
        "id": id, "last": id, "key": {"kind": "workstream", "id": ws},
        "start": 5, "end": 5, "counts": Counts::default()
    });
    let block: Block = serde_json::from_value(minimal).expect("decodes");
    assert_eq!(block.workstream, None, "the key is not a link");
    assert!(block.tasks.is_empty() && block.facts.is_empty() && block.tool_receipts.is_empty());
    let written = serde_json::to_value(&block).expect("encodes");
    for absent in ["session", "workstream", "project", "agent"] {
        assert!(written.get(absent).is_none(), "{absent} is left out");
    }
    for empty in [
        "tasks",
        "actors",
        "files",
        "facts",
        "tool_receipts",
        "turn_receipts",
    ] {
        assert_eq!(written[empty], json!([]), "{empty} is written");
    }
    assert_eq!(written["key"], json!({"kind": "workstream", "id": ws}));
    assert_eq!(
        serde_json::to_value(BlockKey::Project(ProjectId::new())).expect("encodes")["kind"],
        "project"
    );
}

#[test]
fn pages_round_trip() {
    let block = full_block();
    let ev = block["id"].clone();
    let line = json!({
        "text": "@writer edited method.tex (+84 −12)",
        "spans": [{"range": {"start": 0, "end": 37}, "receipts": [{"kind": "event", "id": ev}]}]
    });
    let page: BlocksPage = round_trip(&json!({
        "blocks": [{"block": block, "line": line}],
        "at_start": false
    }));
    let RecapBlock { block, line } = &page.blocks[0];
    assert_eq!(
        line.clause(&line.spans[0]),
        "@writer edited method.tex (+84 −12)"
    );
    assert_eq!(block.counts.lines_added, 84);
    let empty: BlocksPage = round_trip(&json!({"blocks": [], "at_start": true}));
    assert!(empty.at_start);

    let ws = WorkstreamId::new();
    let days: DaysPage = round_trip(&json!({
        "days": [
            {"workstream": ws, "date": "2026-09-30", "blocks": [ev],
             "summary": {"text": "1 task move.", "spans": [
                 {"range": {"start": 0, "end": 11}, "receipts": [{"kind": "event", "id": ev}]}]}},
            {"date": "2026-09-29", "blocks": [], "summary": {"text": "", "spans": []}}
        ],
        "at_start": true
    }));
    assert_eq!(
        days.days[0],
        DayRecap {
            workstream: Some(ws),
            date: Date("2026-09-30".into()),
            blocks: vec![block.id],
            summary: Summary {
                text: "1 task move.".into(),
                spans: vec![Span {
                    range: 0..11,
                    receipts: vec![Receipt::Event { id: block.id }],
                }],
            },
        }
    );
    assert_eq!(
        days.days[1].workstream, None,
        "tasks outside any workstream"
    );
}
