//! # pitcrew-ptyd
//!
//! A small, rarely changing supervisor that owns PitCrew's terminals where tmux is unavailable
//! (native Windows above all): one PTY per terminal (ConPTY on Windows), each with a bounded
//! replay buffer and a screen model, served to `pitcrewd` (`pitcrew_runtime::PtyRuntime`) over a
//! private socket or named pipe. It outlives `pitcrewd`, so restarting or upgrading the daemon
//! never ends a session. See the crate README.
//!
//! ```text
//! pitcrew-ptyd serve --endpoint <socket or pipe> [--history <bytes>] [--idle-exit-ms <ms>] [--foreground]
//! pitcrew-ptyd --version
//! ```
//!
//! **Owned by stream B.** The work packages are in `docs/build/streams/B.md`.

#![forbid(unsafe_code)]

mod scan;
mod server;
mod spawn;
mod terms;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use pitcrew_runtime::pty::proto::PROTOCOL;

/// Exit status when another ptyd already serves the endpoint.
const ALREADY_RUNNING: u8 = 3;

/// How ptyd was asked to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    /// The socket path (Unix) or pipe name (Windows).
    pub(crate) endpoint: PathBuf,
    /// Output kept per terminal, in bytes.
    pub(crate) history: usize,
    /// How long to wait with no terminals and no clients before exiting.
    pub(crate) idle_exit: Duration,
    /// Serve from this process (otherwise, on Unix, start a detached copy that does).
    pub(crate) foreground: bool,
    /// The uid clients must have, in place of ours: `--expect-uid`, accepted by debug builds
    /// only, for the tests of the peer check.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) expect_uid: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Serve(Config),
    Version,
}

const USAGE: &str = "usage: pitcrew-ptyd serve --endpoint <socket or pipe> [--history <bytes>] [--idle-exit-ms <ms>] [--foreground]\n       pitcrew-ptyd --version";

fn parse(args: &[OsString]) -> Result<Command, String> {
    let mut args = args.iter();
    match args.next().and_then(|a| a.to_str()) {
        Some("--version" | "-V") => return Ok(Command::Version),
        Some("serve") => {}
        _ => return Err(USAGE.into()),
    }
    let mut endpoint = None;
    let mut history = pitcrew_runtime::replay::DEFAULT_CAPACITY;
    let mut idle_exit = server::IDLE_EXIT;
    let mut foreground = false;
    let mut expect_uid = None;
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{} needs a value\n{USAGE}", flag.to_string_lossy()))
        };
        match flag.to_str() {
            Some("--endpoint") => endpoint = Some(PathBuf::from(value()?)),
            Some("--history") => {
                history = number(value()?)?;
                if !(1024..=(64 << 20)).contains(&history) {
                    return Err("--history must be 1 KiB to 64 MiB".into());
                }
            }
            Some("--idle-exit-ms") => {
                idle_exit = Duration::from_millis(number(value()?)? as u64);
            }
            Some("--foreground") => foreground = true,
            Some("--expect-uid") if cfg!(debug_assertions) => {
                expect_uid = Some(u32::try_from(number(value()?)?).map_err(|e| e.to_string())?);
            }
            _ => return Err(format!("unknown argument {flag:?}\n{USAGE}")),
        }
    }
    let endpoint = endpoint.ok_or_else(|| format!("--endpoint is required\n{USAGE}"))?;
    Ok(Command::Serve(Config {
        endpoint,
        history,
        idle_exit,
        foreground,
        expect_uid,
    }))
}

fn number(value: &OsString) -> Result<usize, String> {
    value
        .to_str()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| format!("{value:?} is not a number"))
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match parse(&args) {
        Ok(Command::Version) => {
            println!(
                "pitcrew-ptyd {} (protocol {PROTOCOL})",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        Ok(Command::Serve(config)) => server::run(&config, &args),
        Err(usage) => {
            eprintln!("{usage}");
            ExitCode::from(2)
        }
    }
}

/// A line in ptyd's log (its standard error).
macro_rules! log {
    ($($arg:tt)*) => {
        eprintln!("pitcrew-ptyd[{}]: {}", std::process::id(), format_args!($($arg)*))
    };
}
pub(crate) use log;

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn arguments_are_parsed_strictly() {
        assert_eq!(parse(&args(&["--version"])), Ok(Command::Version));
        assert_eq!(
            parse(&args(&[
                "serve",
                "--endpoint",
                "/tmp/x/ptyd",
                "--history",
                "4096",
                "--idle-exit-ms",
                "250",
                "--foreground"
            ])),
            Ok(Command::Serve(Config {
                endpoint: PathBuf::from("/tmp/x/ptyd"),
                history: 4096,
                idle_exit: Duration::from_millis(250),
                foreground: true,
                expect_uid: None,
            }))
        );
        // A test hook of debug builds only.
        let hook = parse(&args(&["serve", "--endpoint", "x", "--expect-uid", "7"]));
        if cfg!(debug_assertions) {
            assert!(matches!(
                hook,
                Ok(Command::Serve(Config {
                    expect_uid: Some(7),
                    ..
                }))
            ));
        } else {
            assert!(hook.is_err());
        }
        for bad in [
            &[][..],
            &["serve"][..],
            &["serve", "--endpoint"][..],
            &["serve", "--endpoint", "x", "--history", "12"][..],
            &["serve", "--endpoint", "x", "--history", "lots"][..],
            &["serve", "--endpoint", "x", "--shell", "sh"][..],
            &["run"][..],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
