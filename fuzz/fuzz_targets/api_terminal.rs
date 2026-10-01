//! The terminal WebSocket (`GET /v1/sessions/{id}/terminal`, `pitcrew_api::terminal::routes`) on
//! arbitrary client messages: keystrokes (binary frames) and control messages (text frames, such
//! as `{"type":"resize","cols":…,"rows":…}`). A client of the socket controls them.
//!
//! The route runs over an in-memory connection (`tokio::io::duplex`) behind `axum::serve`, with a
//! fake terminal that records what reaches it. `max_inbound` is 200 bytes, so the size limit is
//! reached often (a message here is at most 255 bytes, or more as text once invalid UTF-8 is replaced).
//!
//! Input: messages, each a kind byte, a length byte and that many bytes. Kinds: keystrokes; a raw
//! text message; a resize built from four bytes (sizes up to 1100, so out-of-range ones too); a
//! message with an arbitrary `type`.
//!
//! Checks, besides "no panic" and no hang:
//! - keystrokes reach the terminal exactly and in order, with resizes in between where they were
//!   sent; nothing else reaches it;
//! - a text message that is not a JSON object with a string `type`, or a resize outside 1..=1000,
//!   closes the socket with 1007, and a message over `max_inbound` with 1009; nothing sent after
//!   it reaches the terminal;
//! - other `type`s are ignored.
#![no_main]

use axum::serve::Listener;
use futures_util::{SinkExt as _, StreamExt as _};
use libfuzzer_sys::fuzz_target;
use pitcrew_api::terminal::{self, Attachment, TerminalConfig, TerminalError, Terminals};
use pitcrew_interfaces::runtime::OutputChunk;
use pitcrew_protocol::ids::SessionId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio::sync::{Notify, mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;

const MAX_INBOUND: usize = 200;
const WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seen {
    Keys(Vec<u8>),
    Resize(u16, u16),
}

/// A terminal that records its input and never prints anything.
#[derive(Debug)]
struct Recorder {
    seen: Mutex<Vec<Seen>>,
    /// Wakes the client when input arrives.
    arrived: Notify,
    changes: watch::Sender<u64>,
}

impl Recorder {
    fn seen(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn push(&self, seen: Seen) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(seen);
        self.arrived.notify_one();
    }
}

impl Attachment for Recorder {
    fn read(&self, from: u64, _max: usize) -> Result<OutputChunk, TerminalError> {
        Ok(OutputChunk {
            offset: from,
            data: Vec::new(),
            end: from,
            truncated: false,
        })
    }

    fn write(&self, bytes: &[u8]) -> Result<(), TerminalError> {
        self.push(Seen::Keys(bytes.to_vec()));
        Ok(())
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), TerminalError> {
        self.push(Seen::Resize(cols, rows));
        Ok(())
    }

    fn exited(&self) -> Result<bool, TerminalError> {
        Ok(false)
    }

    fn changes(&self) -> Option<watch::Receiver<u64>> {
        Some(self.changes.subscribe())
    }
}

#[derive(Debug, Default)]
struct Fakes(Mutex<HashMap<SessionId, Arc<Recorder>>>);

impl Terminals for Fakes {
    fn attach(&self, session: SessionId) -> Result<Arc<dyn Attachment>, TerminalError> {
        let found = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&session)
            .cloned();
        match found {
            Some(recorder) => Ok(recorder as Arc<dyn Attachment>),
            None => Err(TerminalError::NotFound("no such session".to_owned())),
        }
    }
}

/// Connections handed to `axum::serve` through a channel instead of a socket.
struct Pipes(mpsc::UnboundedReceiver<DuplexStream>);

impl Listener for Pipes {
    type Io = DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.recv().await {
            Some(io) => (io, ()),
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(())
    }
}

struct App {
    runtime: tokio::runtime::Runtime,
    connect: mpsc::UnboundedSender<DuplexStream>,
    fakes: Arc<Fakes>,
}

fn app() -> &'static App {
    static APP: OnceLock<App> = OnceLock::new();
    APP.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a runtime");
        let fakes = Arc::new(Fakes::default());
        let config = TerminalConfig {
            max_inbound: MAX_INBOUND,
            ping_every: Duration::from_secs(3600),
            pong_timeout: Duration::from_secs(3600),
            ..TerminalConfig::default()
        };
        let router = terminal::routes(Arc::clone(&fakes) as Arc<dyn Terminals>, config);
        let (connect, pipes) = mpsc::unbounded_channel();
        runtime.spawn(async move {
            let _ = axum::serve(Pipes(pipes), router).await;
        });
        App {
            runtime,
            connect,
            fakes,
        }
    })
}

/// What the client sends.
enum Send {
    Keys(Vec<u8>),
    Text(String),
}

/// What the route must do with the messages, in order.
#[derive(Debug, PartialEq, Eq)]
enum Expect {
    /// Everything reaches the terminal; the client then closes.
    All,
    /// Closed with this code after the inputs before it.
    Closed(u16),
}

fuzz_target!(|input: &[u8]| {
    let sends = messages(input);
    let mut expected = Vec::new();
    let mut outcome = Expect::All;
    for send in &sends {
        match send {
            Send::Keys(b) if b.len() > MAX_INBOUND => outcome = Expect::Closed(1009),
            Send::Text(t) if t.len() > MAX_INBOUND => outcome = Expect::Closed(1009),
            Send::Keys(b) => expected.push(Seen::Keys(b.clone())),
            Send::Text(t) => match control(t) {
                Some(Some(resize)) => expected.push(resize),
                Some(None) => {}
                None => outcome = Expect::Closed(1007),
            },
        }
        if outcome != Expect::All {
            break;
        }
    }

    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let session: SessionId = format!("{n:026}").parse().expect("a session id");
    let app = app();
    let (changes, _) = watch::channel(0);
    let recorder = Arc::new(Recorder {
        seen: Mutex::new(Vec::new()),
        arrived: Notify::new(),
        changes,
    });
    app.fakes
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(session, Arc::clone(&recorder));

    app.runtime.block_on(async {
        let (client, server) = tokio::io::duplex(1 << 20);
        app.connect.send(server).expect("the server is listening");
        let mut request = format!("ws://localhost/v1/sessions/{session}/terminal")
            .into_client_request()
            .expect("a request");
        request.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static("pitcrew.v1"),
        );
        let (mut ws, _) =
            tokio::time::timeout(WAIT, tokio_tungstenite::client_async(request, client))
                .await
                .expect("the handshake hangs")
                .expect("the handshake fails");
        for send in sends {
            let message = match send {
                Send::Keys(b) => Message::binary(b),
                Send::Text(t) => Message::text(t),
            };
            if ws.send(message).await.is_err() {
                break;
            }
        }
        match outcome {
            Expect::All => {
                let deadline = tokio::time::Instant::now() + WAIT;
                // `notify_one` keeps a permit, so input that lands before the wait still wakes it.
                while recorder.seen().len() < expected.len() {
                    let woke = tokio::time::timeout_at(deadline, recorder.arrived.notified()).await;
                    assert!(
                        woke.is_ok(),
                        "input never reached the terminal: {:?} of {:?}",
                        recorder.seen(),
                        expected
                    );
                }
                let _ = ws.close(None).await;
                while let Ok(Some(Ok(_))) = tokio::time::timeout(WAIT, ws.next()).await {}
                assert_eq!(recorder.seen(), expected, "the terminal saw other input");
            }
            Expect::Closed(code) => {
                let got = loop {
                    match tokio::time::timeout(WAIT, ws.next()).await {
                        Ok(Some(Ok(Message::Close(frame)))) => {
                            break frame.map(|f| u16::from(f.code));
                        }
                        Ok(Some(Ok(_))) => {}
                        Ok(Some(Err(_)) | None) => break None,
                        Err(_) => panic!("the socket was never closed"),
                    }
                };
                assert_eq!(got, Some(code), "closed with the wrong code");
                let seen = recorder.seen();
                assert!(
                    expected.starts_with(&seen),
                    "the terminal saw {seen:?}, not a prefix of {expected:?}"
                );
            }
        }
    });
    app.fakes
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&session);
});

fn messages(mut input: &[u8]) -> Vec<Send> {
    let mut out = Vec::new();
    while let Some((&[kind, len], rest)) = input.split_first_chunk::<2>() {
        let len = usize::from(len).min(rest.len());
        let (data, tail) = rest.split_at(len);
        input = tail;
        out.push(match kind % 4 {
            0 => Send::Keys(data.to_vec()),
            1 => Send::Text(String::from_utf8_lossy(data).into_owned()),
            2 => {
                let size = |i: usize| {
                    let hi = data.get(i).copied().unwrap_or(0);
                    let lo = data.get(i + 1).copied().unwrap_or(0);
                    u16::from_be_bytes([hi, lo]) % 1100
                };
                Send::Text(format!(
                    r#"{{"type":"resize","cols":{},"rows":{}}}"#,
                    size(0),
                    size(2)
                ))
            }
            _ => Send::Text(serde_json::json!({"type": String::from_utf8_lossy(data)}).to_string()),
        });
        if out.len() == 32 {
            break;
        }
    }
    out
}

/// The control-message rules in the route's documentation: `Some(Some(resize))`, `Some(None)`
/// for a message to ignore, `None` for a malformed one.
fn control(text: &str) -> Option<Option<Seen>> {
    let serde_json::Value::Object(message) = serde_json::from_str(text).ok()? else {
        return None;
    };
    match message.get("type")?.as_str()? {
        "resize" => {
            let size = |key: &str| {
                message
                    .get(key)?
                    .as_u64()
                    .and_then(|n| u16::try_from(n).ok())
                    .filter(|n| (1..=1000).contains(n))
            };
            Some(Some(Seen::Resize(size("cols")?, size("rows")?)))
        }
        _ => Some(None),
    }
}
