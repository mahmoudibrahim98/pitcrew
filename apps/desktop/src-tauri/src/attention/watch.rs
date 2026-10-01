//! One workspace's own subscription to its daemon, on the gateway's side, with the token it
//! already holds: the live stream, and the snapshots it starts from.
//!
//! 1. Open `/v1/stream` (with `since` when resuming) and read its `hello`.
//! 2. If there is nothing to resume from, or the log changed, or `since` is past the daemon's
//!    revision: take a snapshot (`GET /v1/me`, the person's open asks, the members).
//! 3. Apply `ask_raised`, `ask_answered` and `member_added` as they come.
//! 4. When the stream ends (a close, a broken connection, 60 s of silence), wait (1 s, doubling
//!    to 30 s; at once when the workspace becomes ready again) and resume with `since`, as the UI
//!    does.
//!
//! Bounded: frames and bodies have size limits, the tracker keeps a bounded set, and a snapshot
//! the tracker asks for is taken at most once a minute.

use super::tracker::{Count, NewAsk, Tracker};
use crate::gateway::{Connector, GatewayError, http, socket};
use crate::registry::Registry;
use futures_util::StreamExt as _;
use pitcrew_protocol::model::{Ask, Member};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

/// The watcher's limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Open asks kept per workspace.
    pub max_open: usize,
    /// Member names kept per workspace.
    pub max_members: usize,
    /// The largest stream frame read.
    pub max_frame: usize,
    /// The largest snapshot body read.
    pub max_body: usize,
    /// How long one request, or opening the stream, may take.
    pub request_timeout: Duration,
    /// Silence on the stream after which it is reopened (the daemon pings every 20 s).
    pub idle: Duration,
    /// The first wait before reconnecting; it doubles up to `max_backoff`.
    pub first_backoff: Duration,
    /// The longest wait before reconnecting.
    pub max_backoff: Duration,
    /// The shortest time between two snapshots asked for by the tracker.
    pub refetch_every: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_open: 512,
            max_members: 2048,
            max_frame: 8 * 1024 * 1024,
            max_body: 16 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
            idle: Duration::from_secs(60),
            first_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            refetch_every: Duration::from_secs(60),
        }
    }
}

/// Where a watcher reports.
pub(crate) trait Report: Send + Sync + 'static {
    /// The workspace's count changed.
    fn count(&self, workspace: &str, count: Count);
    /// A new ask for the person arrived.
    fn new_ask(&self, workspace: &str, ask: NewAsk);
}

/// Why a stream ended or never started. Never holds a token, a body or a frame.
#[derive(Debug)]
enum WatchError {
    Gateway(GatewayError),
    Request(&'static str, String),
    Status(&'static str, u16),
    Malformed(&'static str),
    Timeout(&'static str),
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gateway(e) => write!(f, "{e}"),
            Self::Request(route, e) => write!(f, "{route}: {e}"),
            Self::Status(route, status) => write!(f, "{route} answered HTTP {status}"),
            Self::Malformed(what) => write!(f, "malformed {what}"),
            Self::Timeout(what) => write!(f, "no {what} in time"),
        }
    }
}

impl From<GatewayError> for WatchError {
    fn from(e: GatewayError) -> Self {
        Self::Gateway(e)
    }
}

/// Where the stream is: the last revision applied, in which log.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Cursor {
    rev: u64,
    log: String,
}

/// A stream frame, read only as far as the watcher needs.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Frame {
    Hello {
        rev: u64,
        log: String,
    },
    Events {
        to_rev: u64,
        events: Vec<EventIn>,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct EventIn {
    body: BodyIn,
}

#[derive(Deserialize)]
struct BodyIn {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    data: serde_json::Value,
}

/// Watches workspace `id` until the task is aborted.
pub(crate) async fn watch(
    id: String,
    registry: Arc<Registry>,
    poke: Arc<Notify>,
    report: Arc<dyn Report>,
    limits: Limits,
) {
    let mut watcher = Watcher {
        id,
        tracker: Tracker::new(limits.max_open, limits.max_members),
        cursor: None,
        report,
        limits,
        wait: limits.first_backoff,
    };
    loop {
        let ended = match registry.connector(&watcher.id) {
            Ok(connector) => watcher.session(&*connector).await,
            Err(e) => Err(e.into()),
        };
        match ended {
            Ok(why) => tracing::debug!(workspace = %watcher.id, why, "the attention stream ended"),
            Err(e) => {
                tracing::debug!(workspace = %watcher.id, error = %e, "the attention stream is down");
            }
        }
        tokio::select! {
            () = tokio::time::sleep(watcher.wait) => {}
            () = poke.notified() => {}
        }
        watcher.wait = (watcher.wait * 2).min(limits.max_backoff);
    }
}

struct Watcher {
    id: String,
    tracker: Tracker,
    cursor: Option<Cursor>,
    report: Arc<dyn Report>,
    limits: Limits,
    wait: Duration,
}

impl Watcher {
    /// One stream's life: open, catch up, follow. `Ok` with why it ended normally.
    async fn session(&mut self, connector: &dyn Connector) -> Result<&'static str, WatchError> {
        let path = match &self.cursor {
            Some(cursor) => format!("/v1/stream?since={}", cursor.rev),
            None => "/v1/stream".to_owned(),
        };
        let limits = self.limits;
        let opening = async {
            let connected = connector.connect().await?;
            let ws = socket::open(connected.io, &connected.token, &path, limits.max_frame).await;
            drop(connected.token);
            ws
        };
        let mut ws = tokio::time::timeout(limits.request_timeout, opening)
            .await
            .map_err(|_| WatchError::Timeout("stream"))??;

        let (rev, log) = loop {
            let next = tokio::time::timeout(limits.request_timeout, ws.next())
                .await
                .map_err(|_| WatchError::Timeout("hello"))?;
            match next {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<Frame>(&text) {
                    Ok(Frame::Hello { rev, log }) => break (rev, log),
                    _ => return Err(WatchError::Malformed("hello")),
                },
                Some(Ok(Message::Close(_))) | None => return Ok("closed before hello"),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(WatchError::Request("/v1/stream", e.to_string())),
            }
        };
        self.wait = limits.first_backoff;
        let resumable = self
            .cursor
            .as_ref()
            .is_some_and(|c| c.log == log && c.rev <= rev);
        let mut fetched = None;
        if resumable {
            tracing::debug!(workspace = %self.id, since = self.cursor.as_ref().map(|c| c.rev), rev, "resuming the attention stream");
        } else {
            self.snapshot(connector).await?;
            fetched = Some(Instant::now());
            self.cursor = Some(Cursor { rev, log });
        }

        let mut refetch = false;
        loop {
            if refetch && fetched.is_none_or(|at: Instant| at.elapsed() >= limits.refetch_every) {
                self.snapshot(connector).await?;
                fetched = Some(Instant::now());
                refetch = false;
            }
            let next = match tokio::time::timeout(limits.idle, ws.next()).await {
                Ok(next) => next,
                Err(_) => return Ok("silent"),
            };
            let text = match next {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(Message::Close(_))) | None => return Ok("closed"),
                // Pings are answered by tungstenite as it reads.
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(WatchError::Request("/v1/stream", e.to_string())),
            };
            let Ok(Frame::Events { to_rev, events }) = serde_json::from_str::<Frame>(&text) else {
                continue;
            };
            let before = self.tracker.count();
            for event in events {
                let applied = self.tracker.apply(&event.body.kind, event.body.data);
                refetch |= applied.refetch;
                if let Some(ask) = applied.new {
                    self.report.new_ask(&self.id, ask);
                }
            }
            if let Some(cursor) = &mut self.cursor {
                cursor.rev = cursor.rev.max(to_rev);
            }
            let after = self.tracker.count();
            if after != before {
                self.report.count(&self.id, after);
            }
        }
    }

    /// Starts the tracker afresh: who the person is, their open asks, the members.
    async fn snapshot(&mut self, connector: &dyn Connector) -> Result<(), WatchError> {
        #[derive(Deserialize)]
        struct Me {
            id: pitcrew_protocol::ids::MemberId,
        }
        let me: Me = self.get(connector, "/v1/me", "").await?;
        let query = format!("?to={}&state=open", me.id.0);
        let asks: Vec<Ask> = self.get(connector, "/v1/asks", &query).await?;
        let members: Vec<Member> = self.get(connector, "/v1/members", "").await?;
        self.tracker.reset(me.id, asks, members);
        let count = self.tracker.count();
        tracing::debug!(workspace = %self.id, open = count.open, more = count.more, "attention snapshot");
        self.report.count(&self.id, count);
        Ok(())
    }

    /// A `GET` of JSON at `route` with `query` (empty, or from `?`), with the workspace's token.
    /// Only the route is ever logged.
    async fn get<T: DeserializeOwned>(
        &self,
        connector: &dyn Connector,
        route: &'static str,
        query: &str,
    ) -> Result<T, WatchError> {
        let limits = self.limits;
        let path = format!("{route}{query}");
        let connected = tokio::time::timeout(limits.request_timeout, connector.connect())
            .await
            .map_err(|_| WatchError::Timeout(route))??;
        let reply = http::send(
            connected.io,
            Some(&connected.token),
            ::http::Method::GET,
            &path,
            None,
            limits.max_body,
            limits.request_timeout,
        )
        .await;
        drop(connected.token);
        let reply = reply.map_err(|e| WatchError::Request(route, e.to_string()))?;
        if reply.status != 200 {
            return Err(WatchError::Status(route, reply.status));
        }
        serde_json::from_slice(&reply.body).map_err(|_| WatchError::Malformed(route))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_read_leniently() {
        let hello: Frame = serde_json::from_str(r#"{"type":"hello","rev":7,"log":"L"}"#).unwrap();
        assert!(matches!(hello, Frame::Hello { rev: 7, ref log } if log == "L"));
        let ping: Frame = serde_json::from_str(r#"{"type":"ping","at":1}"#).unwrap();
        assert!(matches!(ping, Frame::Other));
        let later: Frame = serde_json::from_str(r#"{"type":"later_kind","x":[1]}"#).unwrap();
        assert!(matches!(later, Frame::Other));
        let events: Frame = serde_json::from_str(
            r#"{"type":"events","from_rev":8,"to_rev":9,"events":[
                {"id":"x","at":1,"workspace":"w","author":"a","body":{"type":"some_future_kind","data":{"z":1}}},
                {"body":{"type":"ask_answered","data":{"ask":"01JB0000000000000000000001"}}}
            ]}"#,
        )
        .unwrap();
        let Frame::Events { to_rev, events } = events else {
            panic!("not events");
        };
        assert_eq!(to_rev, 9);
        assert_eq!(events[0].body.kind, "some_future_kind");
        assert_eq!(events[1].body.kind, "ask_answered");
    }
}
