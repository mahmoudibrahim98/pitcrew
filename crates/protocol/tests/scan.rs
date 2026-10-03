//! The machine scan's wire shapes (`POST /v1/machines/{id}/scan`, api-v1.md "Machine scan"): one
//! JSON object per line, tagged by `type`, with the API's snake_case field names. A failure here
//! is a breaking change to the contract.

use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::model::Engine;
use pitcrew_protocol::scan::{
    EngineCount, FolderCount, HomeCount, MonthCount, ScanCounts, ScanFrame, ScanProgress,
    ScanReport, Suggestion, WorkstreamSuggestion,
};
use serde_json::{Value, json};

fn round_trip(frame: &ScanFrame, wire: &Value) {
    assert_eq!(&serde_json::to_value(frame).unwrap(), wire);
    assert_eq!(&serde_json::from_value::<ScanFrame>(wire.clone()).unwrap(), frame);
    // A frame is one line.
    assert!(!serde_json::to_string(frame).unwrap().contains('\n'));
}

#[test]
fn progress_frames_leave_out_what_is_not_known_yet() {
    round_trip(
        &ScanFrame::Progress(ScanProgress::default()),
        &json!({ "type": "progress", "scanned": 0 }),
    );
    round_trip(
        &ScanFrame::Progress(ScanProgress {
            scanned: 3,
            total: Some(7),
            path: Some("/home/sam/.claude/projects/-w/s1.jsonl".into()),
        }),
        &json!({
            "type": "progress",
            "scanned": 3,
            "total": 7,
            "path": "/home/sam/.claude/projects/-w/s1.jsonl",
        }),
    );
}

#[test]
fn the_done_frame_carries_the_report_in_snake_case() {
    let report = ScanReport {
        counts: ScanCounts {
            sessions: 2,
            subagent_sessions: 1,
            by_engine: vec![
                EngineCount {
                    engine: Engine::Claude,
                    count: 1,
                },
                EngineCount {
                    engine: Engine::OpenCode,
                    count: 1,
                },
            ],
            by_home: vec![HomeCount {
                engine: Engine::Claude,
                home: "/home/sam/.claude".into(),
                count: 1,
            }],
            by_folder: vec![FolderCount {
                path: "/home/sam/work/paper/drafts".into(),
                count: 2,
            }],
            by_month: vec![MonthCount {
                month: "2026-09".into(),
                count: 2,
            }],
            first_activity: Some(1_790_000_000_000),
            last_activity: None,
        },
        suggestions: vec![Suggestion {
            id: "/home/sam/work/paper".into(),
            name: "paper".into(),
            path: "/home/sam/work/paper".into(),
            is_git: true,
            session_count: 2,
            recent_30d: 2,
            recent_90d: 2,
            workstreams: vec![
                WorkstreamSuggestion {
                    id: "/home/sam/work/paper/drafts".into(),
                    name: "drafts".into(),
                    branch: None,
                    session_count: 2,
                    recent_30d: 1,
                    recent_90d: 2,
                },
                WorkstreamSuggestion {
                    id: "/home/sam/work/paper#revision-2".into(),
                    name: "revision-2".into(),
                    branch: Some("revision-2".into()),
                    session_count: 1,
                    recent_30d: 1,
                    recent_90d: 1,
                },
            ],
        }],
        unreadable: 0,
    };
    round_trip(
        &ScanFrame::Done { report },
        &json!({
            "type": "done",
            "report": {
                "counts": {
                    "sessions": 2,
                    "subagent_sessions": 1,
                    "by_engine": [
                        { "engine": "claude", "count": 1 },
                        { "engine": "opencode", "count": 1 },
                    ],
                    "by_home": [{ "engine": "claude", "home": "/home/sam/.claude", "count": 1 }],
                    "by_folder": [{ "path": "/home/sam/work/paper/drafts", "count": 2 }],
                    "by_month": [{ "month": "2026-09", "count": 2 }],
                    "first_activity": 1_790_000_000_000_i64,
                },
                "suggestions": [{
                    "id": "/home/sam/work/paper",
                    "name": "paper",
                    "path": "/home/sam/work/paper",
                    "is_git": true,
                    "session_count": 2,
                    "recent_30d": 2,
                    "recent_90d": 2,
                    "workstreams": [
                        {
                            "id": "/home/sam/work/paper/drafts",
                            "name": "drafts",
                            "session_count": 2,
                            "recent_30d": 1,
                            "recent_90d": 2,
                        },
                        {
                            "id": "/home/sam/work/paper#revision-2",
                            "name": "revision-2",
                            "branch": "revision-2",
                            "session_count": 1,
                            "recent_30d": 1,
                            "recent_90d": 1,
                        },
                    ],
                }],
                "unreadable": 0,
            },
        }),
    );
}

#[test]
fn an_error_frame_is_an_api_error_with_its_tag() {
    round_trip(
        &ScanFrame::Error {
            code: ErrorCode::Internal,
            message: "The scan failed.".into(),
        },
        &json!({ "type": "error", "code": "internal", "message": "The scan failed." }),
    );
}

#[test]
fn an_empty_scan_still_has_every_list() {
    assert_eq!(
        serde_json::to_value(ScanReport::default()).unwrap(),
        json!({
            "counts": {
                "sessions": 0,
                "subagent_sessions": 0,
                "by_engine": [],
                "by_home": [],
                "by_folder": [],
                "by_month": [],
            },
            "suggestions": [],
            "unreadable": 0,
        })
    );
}
