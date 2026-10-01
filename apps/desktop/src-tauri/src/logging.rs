//! Logging. Lines go to stderr at the level in `PITCREW_DESKTOP_LOG` (`tracing` directives, such
//! as `debug` or `info,pitcrew_desktop=debug`; default `info`).
//!
//! **Tokens never reach a log line, at any level.** The gateway logs ids, paths without their
//! query, statuses and timings. The crates that would print a handshake or a frame at trace level
//! (tungstenite prints the upgrade request, subprotocol and token included) are cut off twice,
//! whatever the directives ask for: at the bridge from the `log` crate, and by [`Silence`], which
//! also looks through bridged records to their real target.

use tracing::{Event, Metadata, Subscriber};
use tracing_log::NormalizeEvent as _;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
use tracing_subscriber::util::SubscriberInitExt as _;

/// The environment variable that sets the level.
pub const ENV: &str = "PITCREW_DESKTOP_LOG";

/// Crates whose logs may hold secrets or frames. Never logged.
pub const SILENCED: &[&str] = &["tungstenite", "tokio_tungstenite", "hyper", "hyper_util"];

/// Installs the app's logging: stderr, filtered by [`ENV`].
pub fn init() {
    let filter = std::env::var(ENV)
        .ok()
        .and_then(|directives| EnvFilter::try_new(directives).ok())
        .unwrap_or_else(|| EnvFilter::new("info"));
    let _ = subscriber(filter, std::io::stderr).try_init();
    bridge_log();
}

/// The subscriber the app uses, writing to `writer` (tests capture it).
pub fn subscriber<W>(filter: EnvFilter, writer: W) -> impl Subscriber + Send + Sync
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::registry()
        .with(Silence)
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false)
                .with_target(true),
        )
}

/// Forwards records of the `log` crate (Tauri, wry, tungstenite) to `tracing`, minus the
/// [`SILENCED`] crates. Once per process; later calls do nothing.
pub fn bridge_log() {
    let _ = tracing_log::LogTracer::builder()
        .ignore_all(SILENCED.iter().copied())
        .init();
}

/// Drops everything from the [`SILENCED`] crates, including records bridged from `log` (whose
/// own target is just `log`).
#[derive(Clone, Copy, Debug)]
pub struct Silence;

impl<S: Subscriber> Layer<S> for Silence {
    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        !is_silenced(metadata.target())
    }

    fn event_enabled(&self, event: &Event<'_>, _ctx: Context<'_, S>) -> bool {
        event
            .normalized_metadata()
            .is_none_or(|real| !is_silenced(real.target()))
    }
}

fn is_silenced(target: &str) -> bool {
    SILENCED.iter().any(|krate| {
        target == *krate
            || target
                .strip_prefix(krate)
                .is_some_and(|rest| rest.starts_with("::"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_crates_are_silenced() {
        for target in [
            "tungstenite",
            "tungstenite::handshake::client",
            "tokio_tungstenite",
            "hyper::proto::h1",
        ] {
            assert!(is_silenced(target), "{target}");
        }
        for target in ["pitcrew_desktop::gateway", "tauri::manager", "hyperion"] {
            assert!(!is_silenced(target), "{target}");
        }
    }
}
