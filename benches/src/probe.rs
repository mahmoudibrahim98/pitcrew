//! Delta-stream latency: the time from appending an event to the log until the `events` frame
//! that carries it is ready for the WebSocket writer.
//!
//! The probe runs the real pump (`pitcrew_api::stream::pump`) with the given configuration, so
//! the default 75 ms batch window is part of the measurement, as it is part of what a person
//! sees. The WebSocket write itself is not measured.

use pitcrew_api::EventSource;
use pitcrew_api::stream::{StreamConfig, StreamEnd, pump};
use pitcrew_protocol::api::StreamFrame;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Why a round trip failed.
#[derive(Debug)]
pub enum ProbeError {
    /// The runtime could not start.
    Io(io::Error),
    /// The pump ended before the frame arrived.
    Closed,
    /// No frame arrived in time.
    Timeout,
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot start the runtime: {e}"),
            Self::Closed => f.write_str("the stream ended"),
            Self::Timeout => f.write_str("no frame arrived in time"),
        }
    }
}

impl std::error::Error for ProbeError {}

/// One stream client, connected with no `since`, past its `hello`.
#[derive(Debug)]
pub struct StreamProbe {
    runtime: Runtime,
    frames: mpsc::Receiver<StreamFrame>,
    task: JoinHandle<StreamEnd>,
}

/// The longest a round trip may take before the probe gives up.
const WAIT: Duration = Duration::from_secs(10);

impl StreamProbe {
    /// Starts a pump over `source` on its own two-thread runtime and waits for `hello`.
    ///
    /// # Errors
    ///
    /// The runtime cannot start, or `hello` does not arrive.
    pub fn connect(source: Arc<dyn EventSource>, config: StreamConfig) -> Result<Self, ProbeError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(ProbeError::Io)?;
        let (out, frames) = mpsc::channel(config.queue_frames.max(1));
        let task = runtime.spawn(pump(source, None, config, out));
        let mut probe = Self {
            runtime,
            frames,
            task,
        };
        match probe.next()? {
            StreamFrame::Hello { .. } => Ok(probe),
            _ => Err(ProbeError::Closed),
        }
    }

    /// Runs `append`, then waits for the next `events` frame. Returns the time from just before
    /// `append` until the frame was received. Pings are skipped.
    ///
    /// # Errors
    ///
    /// The stream ends or no frame arrives within ten seconds.
    pub fn round_trip(&mut self, append: impl FnOnce()) -> Result<Duration, ProbeError> {
        let start = Instant::now();
        append();
        loop {
            if let StreamFrame::Events { .. } = self.next()? {
                return Ok(start.elapsed());
            }
        }
    }

    fn next(&mut self) -> Result<StreamFrame, ProbeError> {
        let frames = &mut self.frames;
        self.runtime.block_on(async {
            match tokio::time::timeout(WAIT, frames.recv()).await {
                Ok(Some(frame)) => Ok(frame),
                Ok(None) => Err(ProbeError::Closed),
                Err(_) => Err(ProbeError::Timeout),
            }
        })
    }
}

impl Drop for StreamProbe {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inputs::liveness_event;
    use pitcrew_api::MemorySource;

    #[test]
    fn a_round_trip_waits_for_the_batch_window() {
        let source = Arc::new(MemorySource::new("probe", 16));
        let config = StreamConfig {
            batch_window: Duration::from_millis(20),
            ..StreamConfig::default()
        };
        let mut probe = StreamProbe::connect(source.clone(), config).unwrap();
        for _ in 0..3 {
            let took = probe
                .round_trip(|| {
                    source.append(vec![liveness_event()]);
                })
                .unwrap();
            assert!(took >= Duration::from_millis(20), "{took:?}");
            assert!(took < WAIT, "{took:?}");
        }
    }
}
