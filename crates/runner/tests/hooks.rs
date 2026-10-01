//! Agent hooks as session state (`RunnerHooks`), with the real Claude adapter: hooks beat the
//! transcript watcher, the two agree, stale hooks change nothing, and a hook changes a session
//! only if its sender may (the ownership rule on `RunnerHooks`).
//!
//! In each test the hooks that must be refused would change the state or status line in a way
//! the allowed ones do not, and are delivered first: the watcher decides hooks in order, so by
//! the time the allowed one's event arrives, a wrongly applied refused one would be visible.

#![allow(clippy::unwrap_used)]

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{
    CollectSink, FIXTURE_ID, append, claude_file, config, discovered, fixture_lines,
    fixture_lines_as, label, labels, prompt, session_of,
};
use pitcrew_api::{HookEvent, HookIntake, HookSink, RouterParts, hooks, local_host_info};
use pitcrew_auth::{FileTokenStore, TokenStore};
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_interfaces::fake::FakeSource;
use pitcrew_interfaces::source::{
    Cursor, ParseChunk, SourceAdapter, SourceError, TranscriptPage, TranscriptRef,
};
use pitcrew_protocol::api::{Caller, TokenScope};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{MachineId, MemberId, SessionId, WorkspaceId};
use pitcrew_protocol::model::{Engine, TimestampMs};
use pitcrew_runner::{MemoryAgents, RunnerConfig, SessionAgent, SessionAgents, Timing};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// A person's device token.
fn person() -> Caller {
    Caller {
        member: MemberId::new(),
        scope: TokenScope::Device,
        on_behalf_of: None,
    }
}

/// An agent token for an agent `owner` owns.
fn agent_of(owner: &Caller) -> Caller {
    Caller {
        member: MemberId::new(),
        scope: TokenScope::Agent,
        on_behalf_of: Some(owner.member),
    }
}

/// A session that runs as `agent`.
fn runs_as(agent: &Caller) -> SessionAgent {
    SessionAgent::Agent {
        agent: agent.member,
        owner: agent.on_behalf_of.unwrap(),
    }
}

fn claude_hook(
    from: &Caller,
    event: &str,
    session: &str,
    at: TimestampMs,
    extra: serde_json::Value,
) -> HookEvent {
    let mut payload = serde_json::json!({
        "session_id": session,
        "cwd": "/w/paper",
        "hook_event_name": event,
    });
    if let (Some(p), serde_json::Value::Object(extra)) = (payload.as_object_mut(), extra) {
        p.extend(extra);
    }
    let serde_json::Value::Object(payload) = payload else {
        unreachable!()
    };
    HookEvent {
        engine: Engine::Claude,
        event: event.into(),
        caller: *from,
        payload,
        received_at: at,
    }
}

fn hook(from: &Caller, event: &str, session: &str, at: TimestampMs) -> HookEvent {
    claude_hook(from, event, session, at, serde_json::Value::Null)
}

/// A permission prompt: waiting, with `message` as the status line.
fn asks(from: &Caller, session: &str, message: &str, at: TimestampMs) -> HookEvent {
    claude_hook(
        from,
        "Notification",
        session,
        at,
        serde_json::json!({"notification_type": "permission_prompt", "message": message}),
    )
}

fn start(
    home: &Path,
    state: &Path,
    agents: Option<Arc<dyn SessionAgents>>,
) -> (pitcrew_runner::RunnerHandle, Arc<CollectSink>) {
    start_with(home, state, agents, Arc::new(ClaudeAdapter::new()))
}

fn start_with(
    home: &Path,
    state: &Path,
    agents: Option<Arc<dyn SessionAgents>>,
    adapter: Arc<dyn SourceAdapter>,
) -> (pitcrew_runner::RunnerHandle, Arc<CollectSink>) {
    let sink = Arc::new(CollectSink::default());
    let mut config = config(home, state);
    config.agents = agents;
    let runner = pitcrew_runner::start(config, vec![adapter], sink.clone()).unwrap();
    (runner, sink)
}

/// The state change's status line.
fn status(e: &Event) -> Option<&str> {
    match &e.body {
        EventBody::SessionStateChanged { status_line, .. } => status_line.as_deref(),
        _ => None,
    }
}

/// Each discovered session's id, by its CLI id.
fn sessions(events: &[Event]) -> HashMap<String, SessionId> {
    events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::SessionDiscovered { session } => {
                Some((session.native_id.clone(), session.id))
            }
            _ => None,
        })
        .collect()
}

/// Labels of the events about `session`.
fn labels_of(events: &[Event], session: SessionId) -> Vec<String> {
    events
        .iter()
        .filter(|e| session_of(e) == Some(session))
        .map(label)
        .collect()
}

/// Answers the same for every session: for held hooks, whose session id is not known before
/// discovery.
#[derive(Debug)]
struct Fixed(SessionAgent);

impl SessionAgents for Fixed {
    fn agent_of(&self, _: SessionId) -> SessionAgent {
        self.0
    }
}

/// Panics at its first lookup; after that, no session has an agent.
#[derive(Debug, Default)]
struct PanicsOnce(AtomicBool);

impl SessionAgents for PanicsOnce {
    fn agent_of(&self, _: SessionId) -> SessionAgent {
        if !self.0.swap(true, Ordering::SeqCst) {
            panic!("test double: the first lookup panics");
        }
        SessionAgent::NoAgent
    }
}

#[test]
fn a_stop_hook_beats_the_watcher_and_the_transcript_then_agrees() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = claude_file(home.path(), FIXTURE_ID);
    let lines = fixture_lines();
    std::fs::write(&path, lines[..5].concat()).unwrap();
    let (runner, sink) = start(
        home.path(),
        state.path(),
        Some(Arc::new(MemoryAgents::new())),
    );
    sink.wait_for(3, WAIT).expect("discovery");
    assert_eq!(labels(&sink.events())[0], "discovered:Working");
    // Let the watcher finish its first read; it would read the new lines at once otherwise.
    std::thread::sleep(Duration::from_millis(300));

    // The CLI writes the rest of its turn and fires its Stop hook.
    let hooks = runner.hooks();
    append(&path, &lines[5..].concat());
    hooks.deliver(hook(&person(), "Stop", FIXTURE_ID, now_ms()));

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
    let (runner, sink) = start(
        home.path(),
        state.path(),
        Some(Arc::new(MemoryAgents::new())),
    );
    sink.wait_for(3, WAIT).expect("discovery");
    let hooks = runner.hooks();
    let me = person();

    // A Stop from before the transcript's newest item: the session has moved on since.
    hooks.deliver(hook(&me, "Stop", FIXTURE_ID, FIXTURE_START));
    // Working already: a prompt hook repeats the state.
    let now = now_ms();
    hooks.deliver(hook(&me, "UserPromptSubmit", FIXTURE_ID, now));
    // Unknown hooks, and hooks for other sessions' ids, change nothing here.
    hooks.deliver(hook(&me, "PreToolUse", FIXTURE_ID, now));
    hooks.deliver(hook(&me, "Stop", "not-indexed", now));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sink.len(), 3, "{:?}", labels(&sink.events()));

    // A real stop; then a prompt hook that was delayed past it.
    hooks.deliver(hook(&me, "Stop", FIXTURE_ID, now + 10));
    hooks.deliver(hook(&me, "UserPromptSubmit", FIXTURE_ID, now + 5));
    sink.wait_for(4, WAIT).expect("idle");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    assert_eq!(labels(&sink.events()[3..]), ["state:Idle"]);
}

#[test]
fn a_hook_before_the_transcript_is_applied_at_discovery() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (runner, sink) = start(
        home.path(),
        state.path(),
        Some(Arc::new(MemoryAgents::new())),
    );
    std::thread::sleep(Duration::from_millis(200));

    // The session ended before the runner could read a line of it.
    let hooks = runner.hooks();
    hooks.deliver(hook(&person(), "SessionEnd", FIXTURE_ID, now_ms()));
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
fn through_the_api_route_only_the_sessions_own_agent_changes_it() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = claude_file(home.path(), FIXTURE_ID);
    std::fs::write(&path, fixture_lines()[..5].concat()).unwrap();
    let agents = Arc::new(MemoryAgents::new());
    let (runner, sink) = start(home.path(), state.path(), Some(agents.clone()));
    sink.wait_for(3, WAIT).expect("discovery");

    // The session runs as `writer`. Its sibling (same owner) and another person's agent hold
    // valid agent tokens too.
    let owner = person();
    let writer = agent_of(&owner);
    let sibling = agent_of(&owner);
    let stranger = agent_of(&person());
    agents.set(discovered(&sink.events()).id, runs_as(&writer));

    let tokens = Arc::new(FileTokenStore::in_memory());
    let token = |caller| tokens.mint(caller).unwrap().1.into_string();
    let (writer_token, sibling_token, stranger_token) =
        (token(writer), token(sibling), token(stranger));
    let intake = HookIntake::start(Arc::new(runner.hooks()), 8).unwrap();
    let store: Arc<dyn TokenStore> = tokens;
    let app = pitcrew_api::router(
        local_host_info("0.0.0-test", vec![], vec![]),
        store,
        RouterParts::new().agent(hooks::routes(intake)),
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let post = |token: &str, event: &str, body: serde_json::Value| {
        let request = Request::builder()
            .method("POST")
            .uri(format!("/v1/hooks/claude/{event}"))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = rt.block_on(app.clone().oneshot(request)).unwrap();
        assert_eq!(response.status(), 202);
    };

    // Refused: the sibling's stop and the stranger's end, each a different state.
    post(
        &sibling_token,
        "Stop",
        serde_json::json!({"session_id": FIXTURE_ID}),
    );
    post(
        &stranger_token,
        "SessionEnd",
        serde_json::json!({"session_id": FIXTURE_ID}),
    );
    // Allowed: the session's own agent waits for a permission.
    post(
        &writer_token,
        "Notification",
        serde_json::json!({
            "session_id": FIXTURE_ID,
            "notification_type": "permission_prompt",
            "message": "Claude needs your permission to use Bash",
        }),
    );

    sink.wait_for(4, WAIT).expect("waiting");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let events = sink.events();
    assert_eq!(labels(&events[3..]), ["state:Waiting"]);
    assert_eq!(
        status(&events[3]),
        Some("Claude needs your permission to use Bash")
    );
}

#[test]
fn a_person_changes_unowned_sessions_and_their_own_agents_but_not_another_persons_agents() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let ids = [
        "aaaaaaaa-0000-4000-8000-000000000001",
        "aaaaaaaa-0000-4000-8000-000000000002",
        "aaaaaaaa-0000-4000-8000-000000000003",
    ];
    for id in ids {
        std::fs::write(
            claude_file(home.path(), id),
            fixture_lines_as(id)[..5].concat(),
        )
        .unwrap();
    }
    let agents = Arc::new(MemoryAgents::new());
    let (runner, sink) = start(home.path(), state.path(), Some(agents.clone()));
    sink.wait_for(9, WAIT).expect("three discoveries");
    let found = sessions(&sink.events());
    let [unowned, mine, theirs] = ids.map(|id| found[id]);

    let me = person();
    let my_agent = agent_of(&me);
    let their_agent = agent_of(&person());
    agents.set(mine, runs_as(&my_agent));
    agents.set(theirs, runs_as(&their_agent));

    let hooks = runner.hooks();
    let now = now_ms();
    // Refused: ending another person's agent's session.
    hooks.deliver(hook(&me, "SessionEnd", ids[2], now));
    // Allowed: stopping a session without an agent, and answering for my own agent.
    hooks.deliver(hook(&me, "Stop", ids[0], now));
    hooks.deliver(asks(&me, ids[1], "Approve the plan for my agent", now));

    sink.wait_for(11, WAIT).expect("two state changes");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let events = sink.events();
    let after = &events[9..];
    assert_eq!(labels_of(after, unowned), ["state:Idle"]);
    assert_eq!(labels_of(after, mine), ["state:Waiting"]);
    assert!(labels_of(after, theirs).is_empty(), "{:?}", labels(after));
    let waiting = after.iter().find(|e| session_of(e) == Some(mine)).unwrap();
    assert_eq!(status(waiting), Some("Approve the plan for my agent"));
}

#[test]
fn held_hooks_are_decided_at_discovery_by_who_sent_them() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let owner = person();
    let writer = agent_of(&owner);
    // The session will run as `writer` (its id is not known before discovery).
    let (runner, sink) = start(
        home.path(),
        state.path(),
        Some(Arc::new(Fixed(runs_as(&writer)))),
    );
    std::thread::sleep(Duration::from_millis(200));

    let hooks = runner.hooks();
    let now = now_ms();
    // Allowed, oldest: the session's own agent waits for a permission.
    hooks.deliver(asks(
        &writer,
        FIXTURE_ID,
        "Allow the writer to run make?",
        now,
    ));
    // Refused, and newer, so either would win if applied: another person stops it, another
    // agent of the same owner ends it.
    hooks.deliver(hook(&person(), "Stop", FIXTURE_ID, now + 1));
    hooks.deliver(hook(&agent_of(&owner), "SessionEnd", FIXTURE_ID, now + 2));
    std::thread::sleep(Duration::from_millis(200));

    let path = claude_file(home.path(), FIXTURE_ID);
    std::fs::write(&path, fixture_lines()[..5].concat()).unwrap();
    sink.wait_for(3, WAIT).expect("discovery");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let events = sink.events();
    assert_eq!(
        labels(&events),
        ["discovered:Waiting", "tool:TodoWrite", "tool:Read"]
    );
    assert_eq!(
        discovered(&events).status_line.as_deref(),
        Some("Allow the writer to run make?")
    );
}

#[test]
fn on_a_session_without_an_agent_a_held_agent_hook_is_dropped_and_a_persons_applies() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    // As the runner's sessions are today: the hub knows no agent for them.
    let (runner, sink) = start(
        home.path(),
        state.path(),
        Some(Arc::new(MemoryAgents::new())),
    );
    std::thread::sleep(Duration::from_millis(200));

    let hooks = runner.hooks();
    let now = now_ms();
    let me = person();
    hooks.deliver(asks(&me, FIXTURE_ID, "Allow edits to method.tex?", now));
    hooks.deliver(hook(&agent_of(&me), "SessionEnd", FIXTURE_ID, now + 1));
    std::thread::sleep(Duration::from_millis(200));

    let path = claude_file(home.path(), FIXTURE_ID);
    std::fs::write(&path, fixture_lines()[..5].concat()).unwrap();
    sink.wait_for(3, WAIT).expect("discovery");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let events = sink.events();
    assert_eq!(
        labels(&events),
        ["discovered:Waiting", "tool:TodoWrite", "tool:Read"]
    );
    assert_eq!(
        discovered(&events).status_line.as_deref(),
        Some("Allow edits to method.tex?")
    );
}

#[test]
fn an_unknown_agent_refuses_every_hook_live_and_held() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let ids = [
        "bbbbbbbb-0000-4000-8000-000000000001",
        "bbbbbbbb-0000-4000-8000-000000000002",
    ];
    for id in ids {
        std::fs::write(
            claude_file(home.path(), id),
            fixture_lines_as(id)[..5].concat(),
        )
        .unwrap();
    }
    let agents = Arc::new(MemoryAgents::new());
    let (runner, sink) = start(home.path(), state.path(), Some(agents.clone()));
    sink.wait_for(6, WAIT).expect("two discoveries");
    let found = sessions(&sink.events());
    let (unknown, known) = (found[ids[0]], found[ids[1]]);
    agents.set(unknown, SessionAgent::Unknown);

    let hooks = runner.hooks();
    let me = person();
    let now = now_ms();
    // Refused live: who runs it is unknown. Then a hook the same person may send elsewhere.
    hooks.deliver(asks(&me, ids[0], "Allow the unknown session?", now));
    hooks.deliver(hook(&me, "Stop", ids[1], now));
    sink.wait_for(7, WAIT).expect("the allowed stop");
    std::thread::sleep(Duration::from_millis(300));
    let events = sink.events();
    assert_eq!(labels_of(&events[6..], known), ["state:Idle"]);
    assert!(labels_of(&events[6..], unknown).is_empty());

    // Refused held: the new session's agent is unknown at discovery, so its end is dropped.
    let held_id = "bbbbbbbb-0000-4000-8000-000000000003";
    runner.stop();
    let state2 = tempfile::tempdir().unwrap();
    let (runner, sink) = start(
        home.path(),
        state2.path(),
        Some(Arc::new(Fixed(SessionAgent::Unknown))),
    );
    sink.wait_for(6, WAIT).expect("the two sessions again");
    runner
        .hooks()
        .deliver(hook(&me, "SessionEnd", held_id, now_ms()));
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(
        claude_file(home.path(), held_id),
        fixture_lines_as(held_id)[..5].concat(),
    )
    .unwrap();
    sink.wait_for(9, WAIT).expect("the third session");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let events = sink.events();
    let third = sessions(&events)[held_id];
    assert_eq!(
        labels_of(&events, third),
        ["discovered:Working", "tool:TodoWrite", "tool:Read"]
    );
}

#[test]
fn without_session_agents_every_hook_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = claude_file(home.path(), FIXTURE_ID);
    let lines = fixture_lines();
    std::fs::write(&path, lines[..5].concat()).unwrap();
    let (runner, sink) = start(home.path(), state.path(), None);
    sink.wait_for(3, WAIT).expect("discovery");
    std::thread::sleep(Duration::from_millis(300));

    // A stop that would apply with agents configured, then more of the transcript. The watcher
    // decides the hook before it reads the lines written after it, so once their events are in,
    // the hook's would be too.
    let at = now_ms();
    runner
        .hooks()
        .deliver(hook(&person(), "Stop", FIXTURE_ID, at));
    append(&path, &lines[5..].concat());
    sink.wait_for(8, WAIT).expect("the transcript's events");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let events = sink.events();
    assert!(events.iter().all(|e| e.at != at), "{:?}", labels(&events));
}

#[test]
fn a_panicking_lookup_counts_as_unknown_and_the_watcher_goes_on() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = claude_file(home.path(), FIXTURE_ID);
    std::fs::write(&path, fixture_lines()[..5].concat()).unwrap();
    let (runner, sink) = start(
        home.path(),
        state.path(),
        Some(Arc::new(PanicsOnce::default())),
    );
    sink.wait_for(3, WAIT).expect("discovery");

    let hooks = runner.hooks();
    let me = person();
    let now = now_ms();
    // The lookup for this one panics: refused.
    hooks.deliver(asks(
        &me,
        FIXTURE_ID,
        "Asked while the lookup panicked",
        now,
    ));
    // The watcher survived, and the next lookup answers.
    hooks.deliver(hook(&me, "Stop", FIXTURE_ID, now + 1));
    sink.wait_for(4, WAIT).expect("idle");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    assert_eq!(labels(&sink.events()[3..]), ["state:Idle"]);
}

/// Claude's adapter, counting discoveries.
#[derive(Debug, Default)]
struct CountingDiscovery {
    inner: ClaudeAdapter,
    discoveries: AtomicUsize,
}

impl SourceAdapter for CountingDiscovery {
    fn engine(&self) -> Engine {
        self.inner.engine()
    }
    fn discover(&self, home: &Path) -> Result<Vec<TranscriptRef>, SourceError> {
        self.discoveries.fetch_add(1, Ordering::SeqCst);
        self.inner.discover(home)
    }
    fn read_from(&self, t: &TranscriptRef, cursor: &Cursor) -> Result<ParseChunk, SourceError> {
        self.inner.read_from(t, cursor)
    }
    fn read_page(
        &self,
        t: &TranscriptRef,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TranscriptPage, SourceError> {
        self.inner.read_page(t, before, limit)
    }
}

#[test]
fn a_flood_from_one_sender_leaves_another_senders_held_hook_intact() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let adapter = Arc::new(CountingDiscovery::default());
    let (runner, sink) = start_with(
        home.path(),
        state.path(),
        Some(Arc::new(MemoryAgents::new())),
        adapter.clone(),
    );
    std::thread::sleep(Duration::from_millis(200));

    // A person's session ends before its transcript is found. The look for it is over before
    // the flood starts.
    let hooks = runner.hooks();
    hooks.deliver(hook(&person(), "SessionEnd", FIXTURE_ID, now_ms()));
    std::thread::sleep(Duration::from_millis(1500));

    // An agent token floods hooks for 2000 made-up sessions over about two seconds: far past
    // its quota, and more than the global cap of held hooks.
    let flood = agent_of(&person());
    let before = adapter.discoveries.load(Ordering::SeqCst);
    for round in 0..20 {
        for i in 0..100 {
            let id = format!("00000000-0000-4000-8000-{round:06}{i:06}");
            hooks.deliver(hook(&flood, "Stop", &id, now_ms()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(300));
    // Its first new session looked for transcripts; the rest of the flood did not.
    let during = adapter.discoveries.load(Ordering::SeqCst) - before;
    assert!(during <= 1, "{during} discoveries during the flood");

    let path = claude_file(home.path(), FIXTURE_ID);
    std::fs::write(&path, fixture_lines()[..5].concat()).unwrap();
    sink.wait_for(4, WAIT).expect("discovery");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    assert_eq!(
        labels(&sink.events()),
        ["discovered:Ended", "ended", "tool:TodoWrite", "tool:Read"]
    );
}

#[test]
fn a_codex_notify_follows_the_same_rule() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let refs: Vec<TranscriptRef> = ["t1", "t2"]
        .iter()
        .map(|id| {
            let path = home.path().join(format!("{id}.jsonl"));
            std::fs::write(&path, b"x").unwrap();
            TranscriptRef {
                engine: Engine::Codex,
                path,
                inner_id: None,
                size: 0,
                modified: 0,
            }
        })
        .collect();
    let source = Arc::new(FakeSource::new(Engine::Codex, refs, vec![prompt(1)]));
    let agents = Arc::new(MemoryAgents::new());
    let mut config = RunnerConfig::new(
        WorkspaceId::new(),
        MachineId::new(),
        MemberId::new(),
        state.path(),
    )
    .with_home(Engine::Codex, home.path())
    .with_agents(agents.clone());
    config.timing = Timing {
        cold_interval: Duration::from_secs(600),
        rediscover_interval: Duration::from_secs(600),
        ..Timing::default()
    };
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(config, vec![source], sink.clone()).unwrap();
    sink.wait_for(2, WAIT).expect("two discoveries");
    let found = sessions(&sink.events());
    assert_eq!(found.len(), 2, "{:?}", labels(&sink.events()));

    let owner = person();
    let (one, two) = (agent_of(&owner), agent_of(&owner));
    agents.set(found["t1"], runs_as(&one));
    agents.set(found["t2"], runs_as(&two));
    let notify = |from: &Caller, thread: &str| {
        let serde_json::Value::Object(payload) = serde_json::json!({
            "type": "agent-turn-complete",
            "thread-id": thread,
            "turn-id": "1",
            "cwd": "/w",
        }) else {
            unreachable!()
        };
        HookEvent {
            engine: Engine::Codex,
            event: "notify".into(),
            caller: *from,
            payload,
            received_at: now_ms(),
        }
    };
    let hooks = runner.hooks();
    // Refused: `one` reports the other agent's turn. Allowed: its own.
    hooks.deliver(notify(&one, "t2"));
    hooks.deliver(notify(&one, "t1"));
    sink.wait_for(3, WAIT).expect("the allowed turn end");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let events = sink.events();
    assert_eq!(
        labels_of(&events, found["t1"]),
        ["discovered:Working", "state:Idle"]
    );
    assert_eq!(labels_of(&events, found["t2"]), ["discovered:Working"]);
}
