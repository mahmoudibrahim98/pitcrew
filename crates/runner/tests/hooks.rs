//! Agent hooks as session state (`RunnerHooks`), with the real Claude adapter: hooks beat the
//! transcript watcher, the two agree, and stale hooks change nothing.

#![allow(clippy::unwrap_used)]

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{CollectSink, FIXTURE_ID, append, claude_file, config, fixture_lines, labels};
use pitcrew_api::{HookEvent, HookIntake, HookSink, RouterParts, hooks, local_host_info};
use pitcrew_auth::{FileTokenStore, TokenStore};
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_protocol::api::{Caller, TokenScope};
use pitcrew_protocol::ids::MemberId;
use pitcrew_protocol::model::{Engine, TimestampMs};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tower::ServiceExt as _;

const WAIT: Duration = Duration::from_secs(5);

fn now_ms() -> TimestampMs {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// 2026-09-30T08:00:00Z, the fixture's first record.
const FIXTURE_START: TimestampMs = 1_790_755_200_000;

fn hook(event: &str, session: &str, at: TimestampMs) -> HookEvent {
    let serde_json::Value::Object(payload) = serde_json::json!({
        "session_id": session,
        "cwd": "/w/paper",
        "hook_event_name": event,
    }) else {
        unreachable!()
    };
    HookEvent {
        engine: Engine::Claude,
        event: event.into(),
        caller: Caller {
            member: MemberId::new(),
            scope: TokenScope::Agent,
            on_behalf_of: None,
        },
        payload,
        received_at: at,
    }
}

fn start(
    home: &std::path::Path,
    state: &std::path::Path,
) -> (pitcrew_runner::RunnerHandle, Arc<CollectSink>) {
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home, state),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    (runner, sink)
}

#[test]
fn a_stop_hook_beats_the_watcher_and_the_transcript_then_agrees() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = claude_file(home.path(), FIXTURE_ID);
    let lines = fixture_lines();
    std::fs::write(&path, lines[..5].concat()).unwrap();
    let (runner, sink) = start(home.path(), state.path());
    sink.wait_for(3, WAIT).expect("discovery");
    assert_eq!(labels(&sink.events())[0], "discovered:Working");
    // Let the watcher finish its first read; it would read the new lines at once otherwise.
    std::thread::sleep(Duration::from_millis(300));

    // The CLI writes the rest of its turn and fires its Stop hook.
    let hooks = runner.hooks();
    append(&path, &lines[5..].concat());
    hooks.deliver(hook("Stop", FIXTURE_ID, now_ms()));

    sink.wait_for(9, WAIT).expect("events after the turn");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let events = sink.events();
    // Idle comes from the hook, first. The transcript's items, all written before the hook, add
    // their tools and turn end but no state changes: nothing is said twice or undone.
    assert_eq!(
        labels(&events[3..]),
        [
            "state:Idle",
            "tool:Edit",
            "edit:method.tex",
            "tool:Bash",
            "tool:AskUserQuestion",
            "turn@6514"
        ]
    );
}

#[test]
fn a_stale_hook_does_not_move_a_session_back() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = claude_file(home.path(), FIXTURE_ID);
    std::fs::write(&path, fixture_lines()[..5].concat()).unwrap();
    let (runner, sink) = start(home.path(), state.path());
    sink.wait_for(3, WAIT).expect("discovery");
    let hooks = runner.hooks();

    // A Stop from before the transcript's newest item: the session has moved on since.
    hooks.deliver(hook("Stop", FIXTURE_ID, FIXTURE_START));
    // Working already: a prompt hook repeats the state.
    let now = now_ms();
    hooks.deliver(hook("UserPromptSubmit", FIXTURE_ID, now));
    // Unknown hooks, and hooks for other sessions' ids, change nothing here.
    hooks.deliver(hook("PreToolUse", FIXTURE_ID, now));
    hooks.deliver(hook("Stop", "not-indexed", now));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sink.len(), 3, "{:?}", labels(&sink.events()));

    // A real stop; then a prompt hook that was delayed past it.
    hooks.deliver(hook("Stop", FIXTURE_ID, now + 10));
    hooks.deliver(hook("UserPromptSubmit", FIXTURE_ID, now + 5));
    sink.wait_for(4, WAIT).expect("idle");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    assert_eq!(labels(&sink.events()[3..]), ["state:Idle"]);
}

#[test]
fn a_hook_before_the_transcript_is_applied_at_discovery() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (runner, sink) = start(home.path(), state.path());
    std::thread::sleep(Duration::from_millis(200));

    // The session ended before the runner could read a line of it.
    let hooks = runner.hooks();
    hooks.deliver(hook("SessionEnd", FIXTURE_ID, now_ms()));
    let path = claude_file(home.path(), FIXTURE_ID);
    std::fs::write(&path, fixture_lines()[..5].concat()).unwrap();
    sink.wait_for(4, WAIT).expect("discovery");
    runner.stop();
    assert_eq!(
        labels(&sink.events()),
        ["discovered:Ended", "ended", "tool:TodoWrite", "tool:Read"]
    );
}

#[test]
fn hooks_arrive_through_the_api_route() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = claude_file(home.path(), FIXTURE_ID);
    std::fs::write(&path, fixture_lines()[..5].concat()).unwrap();
    let (runner, sink) = start(home.path(), state.path());
    sink.wait_for(3, WAIT).expect("discovery");

    let tokens = Arc::new(FileTokenStore::in_memory());
    let (_, token) = tokens
        .mint(Caller {
            member: MemberId::new(),
            scope: TokenScope::Agent,
            on_behalf_of: Some(MemberId::new()),
        })
        .unwrap();
    let intake = HookIntake::start(Arc::new(runner.hooks()), 8).unwrap();
    let tokens: Arc<dyn TokenStore> = tokens;
    let app = pitcrew_api::router(
        local_host_info("0.0.0-test", vec![], vec![]),
        tokens,
        RouterParts::new().agent(hooks::routes(intake)),
    );
    let request = Request::builder()
        .method("POST")
        .uri("/v1/hooks/claude/Notification")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", token.into_string()))
        .body(Body::from(
            serde_json::json!({
                "session_id": FIXTURE_ID,
                "notification_type": "permission_prompt",
                "message": "Claude needs your permission to use Bash",
            })
            .to_string(),
        ))
        .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let response = rt.block_on(app.oneshot(request)).unwrap();
    assert_eq!(response.status(), 202);

    sink.wait_for(4, WAIT).expect("waiting");
    runner.stop();
    let events = sink.events();
    assert_eq!(labels(&events[3..]), ["state:Waiting"]);
    let pitcrew_protocol::events::EventBody::SessionStateChanged { status_line, .. } =
        &events[3].body
    else {
        panic!("not a state change");
    };
    assert_eq!(
        status_line.as_deref(),
        Some("Claude needs your permission to use Bash")
    );
}
