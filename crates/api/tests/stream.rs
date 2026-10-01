//! `GET /v1/stream` through the whole router: errors with `oneshot`, and (Unix) end to end over
//! the real socket with subprotocol auth and the real store.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Fixture, call, get_request};
use pitcrew_api::{EventSource, MemorySource, RouterParts, StreamConfig, local_host_info, stream};
use pitcrew_auth::TokenStore;
use std::sync::Arc;

fn app(f: &Fixture, source: Arc<dyn EventSource>) -> axum::Router {
    let tokens: Arc<dyn TokenStore> = f.tokens.clone();
    pitcrew_api::router(
        local_host_info("0.0.0-test", vec![], vec![]),
        tokens,
        RouterParts::new().device(stream::routes(source, StreamConfig::default())),
    )
}

#[tokio::test]
async fn the_stream_is_for_devices_and_websockets() {
    let f = Fixture::new();
    let source: Arc<dyn EventSource> = Arc::new(MemorySource::new("log", 16));

    let (status, _) = call(app(&f, source.clone()), get_request("/v1/stream", None)).await;
    assert_eq!(status, 401);

    let (status, body) = call(
        app(&f, source.clone()),
        get_request("/v1/stream", Some(&f.agent_token)),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "forbidden");

    // A device token, but no upgrade.
    let (status, body) = call(
        app(&f, source.clone()),
        get_request("/v1/stream?since=0", Some(&f.device_token)),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "invalid");

    let (status, body) = call(
        app(&f, source),
        get_request("/v1/stream?since=-1", Some(&f.device_token)),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "invalid");
}

/// Serves the stream on loopback TCP for the rest of the test.
async fn serve(f: &Fixture, source: Arc<dyn EventSource>) -> std::net::SocketAddr {
    use pitcrew_api::{Bound, Listen};
    let bound = Bound::bind(&Listen::DevTcp {
        addr: "127.0.0.1:0".parse().unwrap(),
    })
    .await
    .unwrap();
    let addr = bound.tcp_addr().unwrap();
    tokio::spawn(bound.serve(app(f, source), std::future::pending()));
    addr
}

/// Connects to `/v1/stream` and reads the hello.
fn connect(
    addr: std::net::SocketAddr,
    token: &str,
) -> tungstenite::WebSocket<std::net::TcpStream> {
    use tungstenite::client::IntoClientRequest as _;
    use tungstenite::http::HeaderValue;
    let stream = std::net::TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut request = format!("ws://{addr}/v1/stream")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(&format!("pitcrew.v1, pitcrew.bearer.{token}")).unwrap(),
    );
    let (mut socket, _) = tungstenite::client(request, stream).unwrap();
    let hello = socket.read().unwrap();
    assert!(hello.to_text().unwrap().contains("hello"), "{hello:?}");
    socket
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_over_4_kib_closes_with_1009() {
    let f = Fixture::new();
    let source = Arc::new(MemorySource::new("log", 16));
    let addr = serve(&f, source.clone()).await;
    let token = f.device_token.clone();
    tokio::task::spawn_blocking(move || {
        let mut socket = connect(addr, &token);
        // At the limit: read and ignored; the stream carries on.
        socket
            .send(tungstenite::Message::Text("x".repeat(4096).into()))
            .unwrap();
        source.append(vec![common::event()]);
        let live = socket.read().unwrap();
        assert!(live.to_text().unwrap().contains("events"), "{live:?}");

        // One byte over: 1009.
        socket
            .send(tungstenite::Message::Text("x".repeat(4097).into()))
            .unwrap();
        let code = loop {
            match socket.read().unwrap() {
                tungstenite::Message::Close(Some(frame)) => break frame.code,
                tungstenite::Message::Close(None) => panic!("close without a code"),
                _ => {}
            }
        };
        assert_eq!(code, tungstenite::protocol::frame::coding::CloseCode::Size);

        // The server is fine: a new client gets its hello and the log so far.
        let mut again = connect(addr, &token);
        source.append(vec![common::event()]);
        let live = again.read().unwrap();
        assert!(live.to_text().unwrap().contains("events"), "{live:?}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_close_is_answered() {
    let f = Fixture::new();
    let source: Arc<dyn EventSource> = Arc::new(MemorySource::new("log", 16));
    let addr = serve(&f, source).await;
    let token = f.device_token.clone();
    tokio::task::spawn_blocking(move || {
        let mut socket = connect(addr, &token);
        socket.close(None).unwrap();
        // The server's reply, not a reset.
        assert!(matches!(socket.read(), Ok(tungstenite::Message::Close(_))));
        assert!(matches!(
            socket.read(),
            Err(tungstenite::Error::ConnectionClosed)
        ));
    })
    .await
    .unwrap();
}

#[cfg(all(unix, feature = "store"))]
mod unix {
    use super::*;
    use pitcrew_api::{Bound, Listen, SOCKET_NAME, StoreSource};
    use pitcrew_protocol::api::StreamFrame;
    use pitcrew_protocol::events::{Event, EventBody};
    use pitcrew_protocol::model::Liveness;
    use pitcrew_protocol::{MachineId, MemberId, WorkspaceId};
    use pitcrew_store::{Store, StoreOptions};
    use tokio::sync::oneshot;
    use tungstenite::client::IntoClientRequest as _;
    use tungstenite::http::HeaderValue;

    fn event() -> Event {
        Event::now(
            WorkspaceId::new(),
            MemberId::new(),
            EventBody::MachineLiveness {
                machine: MachineId::new(),
                liveness: Liveness::Stopped,
            },
        )
    }

    fn frame(socket: &mut tungstenite::WebSocket<std::os::unix::net::UnixStream>) -> StreamFrame {
        loop {
            let message = socket.read().unwrap();
            if let tungstenite::Message::Text(text) = message {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn streams_the_store_over_the_unix_socket() {
        let f = Fixture::new();
        let tmp = tempfile::tempdir().unwrap();
        let store =
            Arc::new(Store::open(tmp.path().join("hub.db"), StoreOptions::default()).unwrap());
        store.append(&[event(), event()]).unwrap();
        let source: Arc<dyn EventSource> = Arc::new(StoreSource::new(store.clone(), "log-1"));

        let run = tmp.path().join("run");
        let bound = Bound::bind(&Listen::Unix { dir: run.clone() })
            .await
            .unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        let server = tokio::spawn(bound.serve(app(&f, source), async {
            let _ = stopped.await;
        }));

        let token = f.device_token.clone();
        let socket_path = run.join(SOCKET_NAME);
        let frames = tokio::task::spawn_blocking(move || {
            let stream = std::os::unix::net::UnixStream::connect(&socket_path).unwrap();
            let mut request = "ws://localhost/v1/stream?since=1"
                .into_client_request()
                .unwrap();
            request.headers_mut().insert(
                "sec-websocket-protocol",
                HeaderValue::from_str(&format!("pitcrew.v1, pitcrew.bearer.{token}")).unwrap(),
            );
            let (mut socket, response) = tungstenite::client(request, stream).unwrap();
            assert_eq!(response.headers()["sec-websocket-protocol"], "pitcrew.v1");
            let hello = frame(&mut socket);
            let replay = frame(&mut socket);
            // Appended while connected: arrives live.
            store.append(&[event(), event(), event()]).unwrap();
            let live = frame(&mut socket);
            (hello, replay, live)
        })
        .await
        .unwrap();

        assert_eq!(
            frames.0,
            StreamFrame::Hello {
                rev: 2,
                log: "log-1".into()
            }
        );
        let StreamFrame::Events {
            from_rev, to_rev, ..
        } = frames.1
        else {
            panic!("expected the replay, got {:?}", frames.1)
        };
        assert_eq!((from_rev, to_rev), (2, 2));
        let StreamFrame::Events {
            from_rev,
            to_rev,
            events,
        } = frames.2
        else {
            panic!("expected live events, got {:?}", frames.2)
        };
        assert_eq!((from_rev, to_rev, events.len()), (3, 5, 3));

        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }
}
