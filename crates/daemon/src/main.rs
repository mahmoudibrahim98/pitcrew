//! # pitcrewd
//!
//! The composition root: it opens the store, runs the work model and serves API v1 with real
//! tokens. In the solo case the hub and (later) the runner share this one process (ADR-0009).
//!
//! - `pitcrewd serve [--listen private|tcp:127.0.0.1:<port>] [--demo]`: see [`serve`].
//! - `pitcrewd token show-path`: where the device token is kept, never the token.
//! - `pitcrewd --version`: the version and the protocol range.
//!
//! `--state-dir <dir>` works with every command; [`state`] lists what is in it. Logs go to
//! stderr, at the level in `PITCREW_LOG` (default `info`); stdout carries only the
//! `pitcrewd listening on …` line, and the path from `token show-path`.
//!
//! **Owned by stream 0.**

mod cli;
mod cors;
mod no_runner;
mod serve;
mod state;

use clap::{CommandFactory as _, Parser as _};
use cli::{Cli, Command, TokenCommand};
use state::StateDir;
use std::io::IsTerminal as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.version {
        println!("{}", cli::version_line());
        return ExitCode::SUCCESS;
    }
    let Some(command) = cli.command else {
        Cli::command()
            .error(
                clap::error::ErrorKind::MissingSubcommand,
                "say what to do: `pitcrewd serve`, `pitcrewd token show-path`, or `--version`",
            )
            .exit();
    };
    init_logging();
    let result = StateDir::resolve(cli.state_dir).and_then(|state| match command {
        Command::Serve(args) => serve::serve(&state, &args),
        Command::Token(TokenCommand::ShowPath) => Ok(show_path(&state)),
    });
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("pitcrewd: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// `pitcrewd token show-path`: prints the device token's path on stdout. If there is no token yet,
/// says so on stderr and fails, so `$(pitcrewd token show-path)` never names a missing file.
fn show_path(state: &StateDir) -> ExitCode {
    let path = state.device_token();
    if path.is_file() {
        println!("{}", path.display());
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "pitcrewd: no device token at {} yet; `pitcrewd serve` creates it on first start",
            path.display()
        );
        ExitCode::FAILURE
    }
}

/// Logs to stderr, filtered by `PITCREW_LOG` (`tracing` directives, e.g. `debug` or
/// `info,pitcrew_api=debug`).
fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = match std::env::var("PITCREW_LOG") {
        Ok(directives) => EnvFilter::try_new(&directives).unwrap_or_else(|e| {
            eprintln!("pitcrewd: PITCREW_LOG={directives:?} is not valid ({e}); logging at info");
            EnvFilter::new("info")
        }),
        Err(_) => EnvFilter::new("info"),
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}
