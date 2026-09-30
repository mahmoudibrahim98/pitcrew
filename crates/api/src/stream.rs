//! `GET /v1/stream?since=<rev>`: the live update channel, served from the event log with exact
//! resume.
//!
//! One connection runs a *pump* that writes [`StreamFrame`]s into a bounded queue, and a writer
//! that sends them on the WebSocket:
//!
//! 1. The pump subscribes to the log **before** reading its latest revision `rev`, so nothing
//!    appended in between is missed.
//! 2. It sends `hello {rev, log}`, then, if `since < rev`, replays `since+1..=rev` in pages.
//! 3. Live: it collects the ranges it is told about for a short window, then reads everything
//!    after the last revision it sent. Ranges at or below that revision are skipped, so nothing
//!    is sent twice. If the subscription lags, it re-reads the latest revision and catches up.
//! 4. A client that stops reading fills the queue; once a frame cannot be queued within
//!    [`StreamConfig::send_timeout`], the client is disconnected. It resumes with `since`.

use crate::source::{EventSource, SourceError};
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pitcrew_auth::{ErrorResponse, WS_PROTOCOL};
use pitcrew_protocol::api::{ErrorCode, StreamFrame};
use pitcrew_protocol::model::TimestampMs;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior};

/// The most events in one `events` frame (`docs/build/contracts/api-v1.md`).
pub const MAX_PAGE: usize = 500;

/// Tuning for the delta stream. The defaults follow the contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamConfig {
    /// Frames a client may have queued but unsent. This bounds each client's memory: at most
    /// `queue_frames` frames of at most `page` events.
    pub queue_frames: usize,
    /// How long a frame may wait for room in a full queue before the client is dropped.
    pub send_timeout: Duration,
    /// How long new revisions are collected before they are sent (the contract says 50–100 ms).
    pub batch_window: Duration,
    /// How often to send `ping`.
    pub ping_every: Duration,
    /// Events per `events` frame; at most [`MAX_PAGE`].
    pub page: usize,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            queue_frames: 64,
            send_timeout: Duration::from_secs(10),
            batch_window: Duration::from_millis(75),
            ping_every: Duration::from_secs(20),
            page: MAX_PAGE,
        }
    }
}

/// Why a stream ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamEnd {
    /// The client went away.
    ClientGone,
    /// The client stopped reading and its queue stayed full.
    SlowClient,
    /// The event source shut down.
    SourceClosed,
    /// The event source failed to read.
    SourceFailed,
}

/// The routes of the delta stream. Mount them as **device** routes (`RouterParts::device`).
pub fn routes(source: Arc<dyn EventSource>, config: StreamConfig) -> Router {
    Router::new()
        .route("/v1/stream", get(stream))
        .with_state(StreamState { source, config })
}

#[derive(Clone, Debug)]
struct StreamState {
    source: Arc<dyn EventSource>,
    config: StreamConfig,
}

#[derive(Debug, Deserialize)]
struct StreamQuery {
    since: Option<u64>,
}

async fn stream(
    State(state): State<StreamState>,
    query: Result<Query<StreamQuery>, axum::extract::rejection::QueryRejection>,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return invalid("`since` must be a revision number.");
    };
    let Ok(upgrade) = upgrade else {
        return invalid("GET /v1/stream needs a WebSocket upgrade.");
    };
    upgrade
        .protocols([WS_PROTOCOL])
        .on_upgrade(move |socket| session(socket, state, query.since))
}

fn invalid(message: &str) -> Response {
    ErrorResponse::new(ErrorCode::Invalid, message).into_response()
}

/// Runs the pump and writes its frames to the socket until either side ends.
async fn session(mut socket: WebSocket, state: StreamState, since: Option<u64>) {
    let (frames, mut queue) = mpsc::channel(state.config.queue_frames.max(1));
    let pump = pump(state.source, since, state.config, frames);
    tokio::pin!(pump);
    let end = loop {
        tokio::select! {
            end = &mut pump => break Some(end),
            frame = queue.recv() => {
                let Some(frame) = frame else { break None };
                let text = match serde_json::to_string(&frame) {
                    Ok(text) => text,
                    Err(e) => {
                        tracing::error!(error = %e, "could not encode a stream frame");
                        break None;
                    }
                };
                tokio::select! {
                    sent = socket.send(Message::Text(text.into())) => {
                        if sent.is_err() {
                            break Some(StreamEnd::ClientGone);
                        }
                    }
                    end = &mut pump => break Some(end),
                }
            }
            incoming = socket.recv() => match incoming {
                None | Some(Err(_) | Ok(Message::Close(_))) => break Some(StreamEnd::ClientGone),
                // The client has nothing to say on this stream; pongs and the like are ignored.
                Some(Ok(_)) => {}
            },
        }
    };
    tracing::debug!(?end, "stream closed");
}

/// Writes frames for one client into `out` until the client goes away, falls behind, or the
/// source ends. See the module docs for the ordering rules.
pub async fn pump(
    source: Arc<dyn EventSource>,
    since: Option<u64>,
    config: StreamConfig,
    out: mpsc::Sender<StreamFrame>,
) -> StreamEnd {
    // Subscribe first: anything appended after this point is announced.
    let mut revs = source.subscribe();
    let rev = match read(&source, |s| s.latest_rev()).await {
        Ok(rev) => rev,
        Err(end) => return end,
    };
    let closed = out.clone();
    let mut pump = Pump {
        log: source.log_id(),
        source,
        config: StreamConfig {
            page: config.page.clamp(1, MAX_PAGE),
            ..config
        },
        out,
        last_sent: rev,
    };
    let hello = StreamFrame::Hello {
        rev,
        log: pump.log.clone(),
    };
    if let Err(end) = pump.send(hello).await {
        return end;
    }
    // A `since` newer than `rev` means the client's log was reset; it refetches.
    if let Some(since) = since.filter(|since| *since < rev) {
        pump.last_sent = since;
        if let Err(end) = pump.send_up_to(rev).await {
            return end;
        }
    }

    let mut ping = tokio::time::interval_at(Instant::now() + config.ping_every, config.ping_every);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut known = pump.last_sent;
    let mut deadline: Option<Instant> = None;
    loop {
        tokio::select! {
            received = revs.recv() => {
                match received {
                    Ok(range) if range.to_rev <= pump.last_sent => continue,
                    Ok(range) => known = known.max(range.to_rev),
                    Err(RecvError::Lagged(missed)) => {
                        tracing::debug!(missed, "stream subscription lagged; catching up");
                        match read(&pump.source, |s| s.latest_rev()).await {
                            Ok(latest) => known = known.max(latest),
                            Err(end) => return end,
                        }
                    }
                    Err(RecvError::Closed) => {
                        return match pump.send_up_to(known).await {
                            Ok(()) => StreamEnd::SourceClosed,
                            Err(end) => end,
                        };
                    }
                }
                deadline.get_or_insert_with(|| Instant::now() + config.batch_window);
            }
            () = sleep_until(deadline), if deadline.is_some() => {
                deadline = None;
                if let Err(end) = pump.send_up_to(known).await {
                    return end;
                }
            }
            _ = ping.tick() => {
                if let Err(end) = pump.send(StreamFrame::Ping { at: now_ms() }).await {
                    return end;
                }
            }
            () = closed.closed() => return StreamEnd::ClientGone,
        }
    }
}

struct Pump {
    source: Arc<dyn EventSource>,
    log: String,
    config: StreamConfig,
    out: mpsc::Sender<StreamFrame>,
    /// The newest revision the client has been sent (or, before replay, the one it had).
    last_sent: u64,
}

impl Pump {
    /// Queues a frame, waiting at most `send_timeout` for room.
    async fn send(&self, frame: StreamFrame) -> Result<(), StreamEnd> {
        match tokio::time::timeout(self.config.send_timeout, self.out.send(frame)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(StreamEnd::ClientGone),
            Err(_) => Err(StreamEnd::SlowClient),
        }
    }

    /// Sends every event after `last_sent`, in pages, until at least `target` has been sent.
    async fn send_up_to(&mut self, target: u64) -> Result<(), StreamEnd> {
        while self.last_sent < target {
            let (after, page) = (self.last_sent, self.config.page);
            let batch = read(&self.source, move |s| s.since(after, page)).await?;
            let (Some(first), Some(last)) = (batch.first(), batch.last()) else {
                // The source announced revisions it does not have; try again on the next one.
                tracing::warn!(after, target, "event source returned nothing to send");
                break;
            };
            let (from_rev, to_rev) = (first.rev, last.rev);
            let events = batch.into_iter().map(|e| e.event).collect();
            self.send(StreamFrame::Events {
                from_rev,
                to_rev,
                events,
            })
            .await?;
            self.last_sent = to_rev;
        }
        Ok(())
    }
}

/// Runs a blocking read on the source off the async threads.
async fn read<T, F>(source: &Arc<dyn EventSource>, f: F) -> Result<T, StreamEnd>
where
    T: Send + 'static,
    F: FnOnce(&dyn EventSource) -> Result<T, SourceError> + Send + 'static,
{
    let source = Arc::clone(source);
    match tokio::task::spawn_blocking(move || f(&*source)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "event source read failed");
            Err(StreamEnd::SourceFailed)
        }
        Err(e) => {
            tracing::error!(error = %e, "event source read panicked");
            Err(StreamEnd::SourceFailed)
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn now_ms() -> TimestampMs {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| {
            TimestampMs::try_from(d.as_millis()).unwrap_or(TimestampMs::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;
    use pitcrew_protocol::events::{Event, EventBody};
    use pitcrew_protocol::model::Liveness;
    use pitcrew_protocol::{MachineId, MemberId, WorkspaceId};
    use proptest::prelude::*;
    use tokio::task::JoinHandle;
    use tokio::time::timeout;

    fn events(n: usize) -> Vec<Event> {
        (0..n)
            .map(|_| {
                Event::now(
                    WorkspaceId::new(),
                    MemberId::new(),
                    EventBody::MachineLiveness {
                        machine: MachineId::new(),
                        liveness: Liveness::Live,
                    },
                )
            })
            .collect()
    }

    fn quick() -> StreamConfig {
        StreamConfig {
            queue_frames: 8,
            send_timeout: Duration::from_secs(5),
            batch_window: Duration::from_millis(1),
            ping_every: Duration::from_secs(3600),
            page: 3,
        }
    }

    /// A client of the pump: its queue, and what it has seen.
    struct Client {
        queue: mpsc::Receiver<StreamFrame>,
        task: JoinHandle<StreamEnd>,
        since: Option<u64>,
        hello: Option<(u64, String)>,
        last: u64,
        seen: Vec<u64>,
    }

    impl Client {
        fn connect(source: &Arc<MemorySource>, since: Option<u64>, config: StreamConfig) -> Self {
            let (frames, queue) = mpsc::channel(config.queue_frames);
            let source: Arc<dyn EventSource> = source.clone();
            let task = tokio::spawn(pump(source, since, config, frames));
            Self {
                queue,
                task,
                since,
                hello: None,
                last: since.unwrap_or(0),
                seen: Vec::new(),
            }
        }

        /// Reads one frame, waiting at most `wait`. Returns whether one arrived.
        async fn read(&mut self, wait: Duration) -> bool {
            let Ok(Some(frame)) = timeout(wait, self.queue.recv()).await else {
                return false;
            };
            match frame {
                StreamFrame::Hello { rev, log } => {
                    assert!(self.hello.is_none(), "a second hello");
                    // Without `since`, or after a reset, the client starts from `rev`.
                    if self.since.is_none_or(|since| since >= rev) {
                        self.last = rev;
                    }
                    self.hello = Some((rev, log));
                }
                StreamFrame::Events {
                    from_rev,
                    to_rev,
                    events,
                } => {
                    assert!(self.hello.is_some(), "events before hello");
                    assert_eq!(from_rev, self.last + 1, "a gap or a repeat");
                    assert!(to_rev >= from_rev);
                    assert_eq!(events.len() as u64, to_rev - from_rev + 1);
                    self.seen.extend(from_rev..=to_rev);
                    self.last = to_rev;
                }
                StreamFrame::Ping { .. } => {}
            }
            true
        }
    }

    #[derive(Clone, Debug)]
    enum Op {
        Append(usize),
        Read(usize),
        Reconnect,
        Pause,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => (1usize..=7).prop_map(Op::Append),
            3 => (0usize..=4).prop_map(Op::Read),
            1 => Just(Op::Reconnect),
            1 => Just(Op::Pause),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// Whatever the interleaving of appends, reads and reconnects with `since`, the client
        /// sees every revision exactly once, in order.
        #[test]
        fn every_revision_exactly_once(ops in proptest::collection::vec(op(), 1..40)) {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                // A small broadcast buffer, so subscriptions also lag.
                let source = Arc::new(MemorySource::new("log", 2));
                // `since = 0`: the client wants the whole log, however the connect races appends.
                let mut client = Client::connect(&source, Some(0), quick());
                let mut seen = Vec::new();
                for op in ops {
                    match op {
                        Op::Append(n) => {
                            source.append(events(n));
                        }
                        Op::Read(n) => {
                            for _ in 0..n {
                                client.read(Duration::from_millis(20)).await;
                            }
                        }
                        Op::Reconnect => {
                            client.task.abort();
                            seen.append(&mut client.seen);
                            client = Client::connect(&source, Some(client.last), quick());
                        }
                        Op::Pause => tokio::time::sleep(Duration::from_millis(3)).await,
                    }
                }
                let latest = source.latest_rev().unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                while client.last < latest && Instant::now() < deadline {
                    client.read(Duration::from_millis(200)).await;
                }
                seen.append(&mut client.seen);
                let expected: Vec<u64> = (1..=latest).collect();
                assert_eq!(seen, expected);
            });
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_slow_client_is_dropped_and_others_keep_up() {
        let source = Arc::new(MemorySource::new("log", 1024));
        let config = StreamConfig {
            queue_frames: 2,
            send_timeout: Duration::from_millis(100),
            page: 1,
            ..quick()
        };
        let slow = Client::connect(&source, Some(0), config);
        let mut fast = Client::connect(&source, Some(0), config);
        for _ in 0..20 {
            source.append(events(1));
            tokio::time::sleep(Duration::from_millis(2)).await;
            while fast.read(Duration::from_millis(1)).await {}
        }
        let end = timeout(Duration::from_secs(5), slow.task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(end, StreamEnd::SlowClient);
        // Its queue never held more than the cap.
        assert_eq!(slow.queue.max_capacity(), 2);
        assert!(slow.queue.len() <= 2);

        while fast.last < 20 && fast.read(Duration::from_secs(1)).await {}
        assert_eq!(fast.seen, (1..=20).collect::<Vec<_>>());
        assert!(!fast.task.is_finished());
    }

    #[tokio::test]
    async fn hello_carries_the_log_and_since_replays_the_rest() {
        let source = Arc::new(MemorySource::new("log-a", 16));
        source.append(events(3));

        let mut fresh = Client::connect(&source, None, quick());
        assert!(fresh.read(Duration::from_secs(1)).await);
        assert_eq!(fresh.hello, Some((3, "log-a".to_owned())));
        assert!(
            !fresh.read(Duration::from_millis(30)).await,
            "nothing to replay"
        );

        let mut behind = Client::connect(&source, Some(1), quick());
        while behind.read(Duration::from_millis(100)).await {}
        assert_eq!(behind.hello, Some((3, "log-a".to_owned())));
        assert_eq!(behind.seen, vec![2, 3]);
    }

    #[tokio::test]
    async fn a_reset_source_is_visible_in_hello() {
        let old = Arc::new(MemorySource::new("log-a", 16));
        old.append(events(5));
        let reset = Arc::new(MemorySource::new("log-b", 16));
        reset.append(events(1));

        let mut client = Client::connect(&reset, Some(5), quick());
        assert!(client.read(Duration::from_secs(1)).await);
        assert_eq!(client.hello, Some((1, "log-b".to_owned())));
        assert!(
            !client.read(Duration::from_millis(30)).await,
            "no replay across logs"
        );
    }

    #[tokio::test]
    async fn pages_are_capped() {
        let source = Arc::new(MemorySource::new("log", 16));
        source.append(events(1200));
        let config = StreamConfig {
            page: 10_000,
            queue_frames: 16,
            ..quick()
        };
        let (frames, mut queue) = mpsc::channel(config.queue_frames);
        let dyn_source: Arc<dyn EventSource> = source.clone();
        let _task = tokio::spawn(pump(dyn_source, Some(0), config, frames));
        let mut sizes = Vec::new();
        while let Ok(Some(frame)) = timeout(Duration::from_millis(200), queue.recv()).await {
            if let StreamFrame::Events { events, .. } = frame {
                sizes.push(events.len());
            }
        }
        assert_eq!(sizes, vec![500, 500, 200]);
    }
}
