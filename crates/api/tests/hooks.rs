//! `POST /v1/hooks/{engine}/{event}` through the whole router.

#![allow(clippy::unwrap_used)]

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{Fixture, call};
use pitcrew_api::{HookIntake, RouterParts, hooks, local_host_info};
use pitcrew_auth::TokenStore;
use std::sync::Arc;

fn app(f: &Fixture, intake: HookIntake) -> axum::Router {
    let tokens: Arc<dyn TokenStore> = f.tokens.clone();
    pitcrew_api::router(
        local_host_info("0.0.0-test", vec![], vec![]),
        tokens,
        RouterParts::new().agent(hooks::routes(intake)),
    )
}

fn post(path: &str, token: Option<&str>, body: impl Into<Body>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(body.into()).unwrap()
}

#[tokio::test]
async fn a_valid_hook_is_accepted_and_delivered_with_its_caller() {
    let f = Fixture::new();
    let (intake, mut delivered) = HookIntake::unread(8);
    let body = r#"{"session_id":"s1","cwd":"/work"}"#;
    let (status, _) = call(
        app(&f, intake),
        post("/v1/hooks/claude/SessionStart", Some(&f.agent_token), body),
    )
    .await;
    assert_eq!(status, 202);
    let event = delivered.try_recv().unwrap();
    assert_eq!(event.event, "SessionStart");
    assert_eq!(event.caller, f.agent);
    assert_eq!(event.payload["session_id"], "s1");
    assert_eq!(
        serde_json::to_value(event.engine).unwrap(),
        serde_json::json!("claude")
    );
}

#[tokio::test]
async fn bad_requests_are_invalid() {
    let f = Fixture::new();
    let big = format!(r#"{{"x":"{}"}}"#, "a".repeat(pitcrew_api::hooks::MAX_BODY));
    let too_long = format!("/v1/hooks/claude/S{}", "s".repeat(64));
    let cases: Vec<(String, String)> = vec![
        ("/v1/hooks/vim/Stop".into(), "{}".into()),
        ("/v1/hooks/Claude/Stop".into(), "{}".into()),
        ("/v1/hooks/claude/1Stop".into(), "{}".into()),
        ("/v1/hooks/claude/Sto.p".into(), "{}".into()),
        (too_long, "{}".into()),
        ("/v1/hooks/claude/Stop".into(), "[1, 2]".into()),
        ("/v1/hooks/claude/Stop".into(), "\"text\"".into()),
        ("/v1/hooks/claude/Stop".into(), "{not json".into()),
        ("/v1/hooks/claude/Stop".into(), big),
    ];
    for (path, body) in cases {
        let (intake, _delivered) = HookIntake::unread(8);
        let len = body.len();
        let (status, response) =
            call(app(&f, intake), post(&path, Some(&f.agent_token), body)).await;
        assert_eq!(status, 400, "{path} ({len} bytes)");
        assert_eq!(response["code"], "invalid", "{path}");
    }
}

#[tokio::test]
async fn hooks_need_a_token() {
    let f = Fixture::new();
    let (intake, _delivered) = HookIntake::unread(8);
    let (status, body) = call(app(&f, intake), post("/v1/hooks/codex/Stop", None, "{}")).await;
    assert_eq!(status, 401);
    assert_eq!(body["code"], "unauthorized");
}

#[tokio::test]
async fn a_full_channel_still_answers_202_and_counts_the_drop() {
    let f = Fixture::new();
    let (intake, _delivered) = HookIntake::unread(1);
    let app = app(&f, intake.clone());
    for expected_drops in [0, 1, 2] {
        let (status, _) = call(
            app.clone(),
            post("/v1/hooks/opencode/Stop", Some(&f.agent_token), "{}"),
        )
        .await;
        assert_eq!(status, 202);
        assert_eq!(intake.dropped(), expected_drops);
    }
}

#[tokio::test]
async fn the_started_intake_feeds_its_sink() {
    #[derive(Debug, Default)]
    struct Recording(std::sync::Mutex<Vec<String>>);
    impl pitcrew_api::HookSink for Recording {
        fn deliver(&self, event: pitcrew_api::HookEvent) {
            self.0.lock().unwrap().push(event.event);
        }
    }

    let f = Fixture::new();
    let sink = Arc::new(Recording::default());
    let intake = HookIntake::start(sink.clone(), 8).unwrap();
    let (status, _) = call(
        app(&f, intake),
        post("/v1/hooks/claude/Stop", Some(&f.device_token), "{}"),
    )
    .await;
    assert_eq!(status, 202);
    for _ in 0..100 {
        if !sink.0.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(*sink.0.lock().unwrap(), vec!["Stop".to_owned()]);
}

/// A sink that blocks for a while on every event, and panics on events named `Boom`.
#[derive(Debug, Default)]
struct Awkward(std::sync::Mutex<Vec<String>>);

impl pitcrew_api::HookSink for Awkward {
    fn deliver(&self, event: pitcrew_api::HookEvent) {
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(event.event != "Boom", "the sink panics");
        self.0.lock().unwrap().push(event.event);
    }
}

// A single-threaded runtime: a sink that blocked it would stall the requests below.
#[tokio::test(flavor = "current_thread")]
async fn a_blocking_or_panicking_sink_never_stalls_requests() {
    let f = Fixture::new();
    let sink = Arc::new(Awkward::default());
    let intake = HookIntake::start(sink.clone(), 8).unwrap();
    let app = app(&f, intake.clone());
    let started = std::time::Instant::now();
    for name in ["One", "Boom", "Two"] {
        let (status, _) = call(
            app.clone(),
            post(
                &format!("/v1/hooks/claude/{name}"),
                Some(&f.agent_token),
                "{}",
            ),
        )
        .await;
        assert_eq!(status, 202);
    }
    // Three requests against a sink that takes 150 ms in total answered well before it finished.
    assert!(started.elapsed() < std::time::Duration::from_millis(100));
    for _ in 0..200 {
        if sink.0.lock().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        *sink.0.lock().unwrap(),
        vec!["One".to_owned(), "Two".to_owned()]
    );
    assert_eq!(intake.dropped(), 0);
}

#[tokio::test]
async fn a_path_that_is_not_utf8_is_invalid() {
    let f = Fixture::new();
    let (intake, _delivered) = HookIntake::unread(8);
    let (status, body) = call(
        app(&f, intake),
        post("/v1/hooks/claude/%FF%FE", Some(&f.agent_token), "{}"),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "invalid");
}
