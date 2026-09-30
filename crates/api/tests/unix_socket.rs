//! End to end over a real unix socket in a temporary directory.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

mod common;

use common::Fixture;
use pitcrew_api::{Bound, Listen, SOCKET_NAME};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::sync::oneshot;

/// One HTTP/1.1 request; returns the status and the body.
async fn request(socket: &Path, path: &str, bearer: Option<&str>) -> (u16, serde_json::Value) {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    let auth = bearer
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let head = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    let status = reply[9..12].parse().unwrap();
    let (_, body) = reply.split_once("\r\n\r\n").unwrap();
    (status, serde_json::from_str(body).unwrap_or_default())
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_over_a_private_unix_socket() {
    let f = Fixture::new();
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    let bound = Bound::bind(&Listen::Unix { dir: run.clone() })
        .await
        .unwrap();
    let socket = run.join(SOCKET_NAME);
    assert_eq!(bound.describe(), socket.display().to_string());

    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&run), 0o700);
    assert_eq!(mode(&socket), 0o600);

    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(bound.serve(f.app(), async {
        let _ = stopped.await;
    }));

    let (status, body) = request(&socket, "/v1/host/info", None).await;
    assert_eq!(status, 200);
    assert_eq!(body["name"], "pitcrewd");

    let (status, body) = request(&socket, "/v1/device-only", Some(&f.device_token)).await;
    assert_eq!(status, 200);
    assert_eq!(body, serde_json::to_value(f.person).unwrap());

    let (status, body) = request(&socket, "/v1/device-only", Some(&f.agent_token)).await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "forbidden");

    let (status, _) = request(&socket, "/v1/me", None).await;
    assert_eq!(status, 401);

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(!socket.exists(), "the socket is removed on shutdown");
}
