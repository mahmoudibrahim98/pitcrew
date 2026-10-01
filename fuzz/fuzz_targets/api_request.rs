//! The API app (`pitcrew_api::router`) on arbitrary requests: method, path, headers and body.
//!
//! Input: one method byte, then HTTP-like text: the path on the first line, `name: value` header
//! lines, an empty line, then the body. `$D` and `$A` stand for the device and agent tokens minted
//! at start-up, so an input reproduces although the tokens are random.
//!
//! The app has an agent route, a device route, a device router nested with its own fallback (the
//! shape that once let requests skip authentication), the hook intake and the delta stream.
//!
//! Checks, besides "no panic":
//! - a route other than `/v1/host/info` answers 2xx only if a minted token appears in
//!   `Authorization` or `Sec-WebSocket-Protocol`;
//! - a device route answers 2xx only to the person, and only if the device token appears there;
//! - an agent route sees the caller whose token was sent;
//! - every 401 carries `WWW-Authenticate: Bearer`.
#![no_main]

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::header::{AUTHORIZATION, SEC_WEBSOCKET_PROTOCOL, WWW_AUTHENTICATE};
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode, Uri};
use axum::routing::get;
use libfuzzer_sys::fuzz_target;
use pitcrew_api::stream::StreamConfig;
use pitcrew_api::{HookIntake, MemorySource, RouterParts, hooks, local_host_info, stream};
use pitcrew_auth::{Authenticated, FileTokenStore, TokenStore};
use pitcrew_protocol::MemberId;
use pitcrew_protocol::api::{Caller, HostRole, TokenScope};
use std::sync::{Arc, OnceLock};
use tower::ServiceExt as _;

struct App {
    router: Router,
    runtime: tokio::runtime::Runtime,
    device_token: String,
    agent_token: String,
    person: Caller,
    agent: Caller,
}

fn app() -> &'static App {
    static APP: OnceLock<App> = OnceLock::new();
    APP.get_or_init(|| {
        let tokens = Arc::new(FileTokenStore::in_memory());
        let person = Caller {
            member: MemberId::new(),
            scope: TokenScope::Device,
            on_behalf_of: None,
        };
        let agent = Caller {
            member: MemberId::new(),
            scope: TokenScope::Agent,
            on_behalf_of: Some(person.member),
        };
        let (_, device_token) = tokens.mint(person).expect("mint a device token");
        let (_, agent_token) = tokens.mint(agent).expect("mint an agent token");
        let parts = RouterParts::new()
            .agent(
                Router::new()
                    .route("/v1/probe/agent", get(agent_probe).post(agent_probe))
                    .merge(hooks::routes(HookIntake::unread(1).0)),
            )
            .device(
                Router::new()
                    .route("/v1/probe/device", get(device_probe).post(device_probe))
                    .nest(
                        "/v1/files",
                        Router::new()
                            .route("/{id}", get(device_probe))
                            .fallback(device_probe),
                    )
                    .merge(stream::routes(
                        Arc::new(MemorySource::new("fuzz", 16)),
                        StreamConfig::default(),
                    )),
            );
        let store: Arc<dyn TokenStore> = tokens;
        let router = pitcrew_api::router(
            local_host_info("0.0.0-fuzz", vec![HostRole::Hub, HostRole::Runner], vec![]),
            store,
            parts,
        );
        App {
            router,
            runtime: tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a tokio runtime"),
            device_token: device_token.into_string(),
            agent_token: agent_token.into_string(),
            person,
            agent,
        }
    })
}

async fn agent_probe(Authenticated(caller): Authenticated) -> String {
    format!(
        "agent:{}",
        serde_json::to_string(&caller).unwrap_or_default()
    )
}

async fn device_probe(Authenticated(caller): Authenticated) -> String {
    format!(
        "device:{}",
        serde_json::to_string(&caller).unwrap_or_default()
    )
}

const METHODS: [&[u8]; 8] = [
    b"GET", b"POST", b"PUT", b"DELETE", b"PATCH", b"HEAD", b"OPTIONS", b"BREW",
];

fuzz_target!(|input: &[u8]| {
    let Some((&method, rest)) = input.split_first() else {
        return;
    };
    let app = app();
    let text = substitute(rest, app);
    let (head, body) = match find(&text, b"\n\n") {
        Some(i) => (&text[..i], &text[i + 2..]),
        None => (&text[..], &[][..]),
    };
    let mut lines = head.split(|&b| b == b'\n');
    let Ok(uri) = Uri::try_from(lines.next().unwrap_or_default()) else {
        return;
    };
    let method =
        Method::from_bytes(METHODS[usize::from(method) % METHODS.len()]).expect("a valid method");
    let Ok(mut request) = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::from(body.to_vec()))
    else {
        return;
    };
    for line in lines {
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(line[..colon].trim_ascii()),
            HeaderValue::from_bytes(line[colon + 1..].trim_ascii_start()),
        ) else {
            continue;
        };
        request.headers_mut().append(name, value);
    }

    let sent = |token: &str| {
        request
            .headers()
            .get_all(AUTHORIZATION)
            .iter()
            .chain(request.headers().get_all(SEC_WEBSOCKET_PROTOCOL))
            .any(|v| find(v.as_bytes(), token.as_bytes()).is_some())
    };
    let (device_sent, agent_sent) = (sent(&app.device_token), sent(&app.agent_token));
    let host_info = request.uri().path() == "/v1/host/info";

    let (status, challenge, body) = app.runtime.block_on(async {
        let response = app
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("the router never fails");
        let status = response.status();
        let challenge = response.headers().get(WWW_AUTHENTICATE).cloned();
        let body = to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("read the response body");
        (status, challenge, body)
    });

    if status == StatusCode::UNAUTHORIZED {
        assert_eq!(
            challenge.as_ref().map(HeaderValue::as_bytes),
            Some(&b"Bearer"[..])
        );
    }
    if !status.is_success() {
        return;
    }
    if let Some(json) = body.strip_prefix(b"device:") {
        let caller: Caller = serde_json::from_slice(json).expect("the probe's caller");
        assert_eq!(
            caller, app.person,
            "a device route served a non-device caller"
        );
        assert!(
            device_sent,
            "a device route answered without the device token"
        );
    } else if let Some(json) = body.strip_prefix(b"agent:") {
        let caller: Caller = serde_json::from_slice(json).expect("the probe's caller");
        if caller == app.person {
            assert!(
                device_sent,
                "the person was served without the device token"
            );
        } else {
            assert_eq!(caller, app.agent, "an unknown caller was served");
            assert!(agent_sent, "the agent was served without the agent token");
        }
    } else if !host_info {
        assert!(
            device_sent || agent_sent,
            "{status} without a token in an authentication header"
        );
    }
});

/// Replaces `$D` and `$A` with the device and agent tokens.
fn substitute(input: &[u8], app: &App) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        match (input[i], input.get(i + 1)) {
            (b'$', Some(b'D')) => {
                out.extend_from_slice(app.device_token.as_bytes());
                i += 2;
            }
            (b'$', Some(b'A')) => {
                out.extend_from_slice(app.agent_token.as_bytes());
                i += 2;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
