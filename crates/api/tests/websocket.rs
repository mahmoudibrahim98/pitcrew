//! WebSocket authentication by subprotocol, over a real (development) TCP listener.

#![allow(clippy::unwrap_used)]

mod common;

use common::Fixture;
use pitcrew_api::{Bound, Listen};
use std::net::SocketAddr;
use tokio::sync::oneshot;
use tungstenite::client::IntoClientRequest as _;
use tungstenite::http::HeaderValue;

/// Connects to `/v1/ws` offering `protocols`; returns the answered subprotocol and first message,
/// or the HTTP status of a refused handshake.
fn connect(addr: SocketAddr, protocols: &str) -> Result<(String, String), u16> {
    let mut request = format!("ws://{addr}/v1/ws").into_client_request().unwrap();
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(protocols).unwrap(),
    );
    match tungstenite::connect(request) {
        Ok((mut socket, response)) => {
            let answered = response
                .headers()
                .get("sec-websocket-protocol")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default();
            let message = socket.read().unwrap().into_text().unwrap().to_string();
            Ok((answered, message))
        }
        Err(tungstenite::Error::Http(response)) => Err(response.status().as_u16()),
        Err(e) => panic!("unexpected error: {e}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn websockets_authenticate_by_subprotocol() {
    let f = Fixture::new();
    let bound = Bound::bind(&Listen::DevTcp {
        addr: "127.0.0.1:0".parse().unwrap(),
    })
    .await
    .unwrap();
    let addr = bound.tcp_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(bound.serve(f.app(), async {
        let _ = stopped.await;
    }));

    let device = format!("pitcrew.v1, pitcrew.bearer.{}", f.device_token);
    let agent = format!("pitcrew.v1, pitcrew.bearer.{}", f.agent_token);
    let bearer_first = format!("pitcrew.bearer.{}, pitcrew.v1", f.device_token);
    let spaced = format!("  pitcrew.v1 ,   pitcrew.bearer.{}  ", f.device_token);
    let person = serde_json::to_value(f.person).unwrap();
    let results = tokio::task::spawn_blocking(move || {
        (
            connect(addr, &device),
            connect(addr, &agent),
            connect(addr, "pitcrew.v1"),
            connect(addr, "pitcrew.v1, pitcrew.bearer.pcd_unknown"),
            connect(addr, &bearer_first),
            connect(addr, &spaced),
        )
    })
    .await
    .unwrap();

    let (answered, message) = results.0.unwrap();
    assert_eq!(answered, "pitcrew.v1");
    let seen: serde_json::Value = serde_json::from_str(&message).unwrap();
    assert_eq!(seen, person);
    // The stream-like route is device-only.
    assert_eq!(results.1, Err(403));
    assert_eq!(results.2, Err(401));
    assert_eq!(results.3, Err(401));
    for offered in [results.4, results.5] {
        let (answered, message) = offered.unwrap();
        assert_eq!(answered, "pitcrew.v1");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&message).unwrap(),
            person
        );
    }

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn development_tcp_refuses_foreign_host_headers() {
    let f = Fixture::new();
    let bound = Bound::bind(&Listen::DevTcp {
        addr: "127.0.0.1:0".parse().unwrap(),
    })
    .await
    .unwrap();
    let addr = bound.tcp_addr().unwrap();
    let server = tokio::spawn(bound.serve(f.app(), std::future::pending()));

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            b"GET /v1/host/info HTTP/1.1\r\nHost: attacker.example\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
    server.abort();
}
