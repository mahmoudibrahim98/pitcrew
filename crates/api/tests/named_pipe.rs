//! End to end over a real named pipe.

#![cfg(windows)]
#![allow(clippy::unwrap_used)]

mod common;

use common::Fixture;
use pitcrew_api::{Bound, Listen};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::sync::oneshot;

async fn request(pipe: &str, path: &str, bearer: Option<&str>) -> (u16, serde_json::Value) {
    let mut client = ClientOptions::new().open(pipe).unwrap();
    pitcrew_api::client::check_pipe_server(&client).unwrap();
    let auth = bearer
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let head = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n");
    client.write_all(head.as_bytes()).await.unwrap();
    let mut reply = Vec::new();
    // The server closes the pipe after the response; a broken pipe then ends the read.
    let _ = client.read_to_end(&mut reply).await;
    let reply = String::from_utf8(reply).unwrap();
    let status = reply[9..12].parse().unwrap();
    let (_, body) = reply.split_once("\r\n\r\n").unwrap();
    (status, serde_json::from_str(body).unwrap_or_default())
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_over_a_named_pipe() {
    let f = Fixture::new();
    let name = format!(r"\\.\pipe\pitcrewd-test-{}", ulid_like());
    let listen = Listen::Pipe { name: name.clone() };
    let bound = Bound::bind(&listen).await.unwrap();
    assert_eq!(bound.describe(), name);

    // Only the first instance may create the name: the daemon never joins a pipe someone else
    // created. (Clients guard the other direction with `check_pipe_server`.)
    assert!(Bound::bind(&listen).await.is_err());

    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(bound.serve(f.app(), async {
        let _ = stopped.await;
    }));

    let (status, body) = request(&name, "/v1/host/info", None).await;
    assert_eq!(status, 200);
    assert_eq!(body["name"], "pitcrewd");

    // A second connection, to check that a new instance was opened.
    let (status, body) = request(&name, "/v1/device-only", Some(&f.device_token)).await;
    assert_eq!(status, 200);
    assert_eq!(body, serde_json::to_value(f.person).unwrap());

    let (status, _) = request(&name, "/v1/device-only", Some(&f.agent_token)).await;
    assert_eq!(status, 403);

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

fn ulid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}
