//! `pitcrew-bench-scale`: `pitcrewd` with a person's whole history, 10,000 transcripts.
//!
//! ```text
//! pitcrew-bench-scale [scan|start|hook|growth|all] [--sessions N] [--seed N] [--pitcrewd PATH]
//!                     [--idle SECONDS] [--starts N] [--probes N] [--growth-turns N]
//!                     [--work DIR] [--min-free-gib N] [--keep] [--keep-cache]
//! pitcrew-bench-scale gen --out DIR [--sessions N] [--seed N]
//! ```
//!
//! Each stage generates synthetic homes in a temp folder, runs the real `pitcrewd` on them and
//! prints what it measured, one `scale <metric>: value … best …` line per number (see
//! `pitcrew_benches::scale`). Later stages include the earlier ones:
//!
//! | Stage | Measures |
//! |---|---|
//! | `scan` | first scan, memory during and after it, the database's size |
//! | `start` | `scan`, then cold start and memory with the index present |
//! | `hook` | `scan`, then hook to stream-frame time |
//! | `growth` | `scan`, then the database's growth per 1,000 events |
//! | `all` | all of them (the default) |
//!
//! `gen` only writes the homes to `--out`, to look at or to point a daemon at by hand.
//!
//! Needs a built `pitcrewd` next to this binary (`cargo build --profile bench -p pitcrew-daemon`),
//! or `--pitcrewd PATH` or `$PITCREWD`. Linux only (it reads `/proc`). Exit status: 0 when it
//! measured, 1 when a measurement failed, 2 on a usage error.

use pitcrew_benches::homes::{self, Homes, Spec};
use pitcrew_benches::scale::{self, Options, Stage};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

const USAGE: &str = "usage: pitcrew-bench-scale [scan|start|hook|growth|all] [--sessions N] \
[--seed N] [--pitcrewd PATH] [--idle SECONDS] [--starts N] [--probes N] [--growth-turns N] \
[--work DIR] [--min-free-gib N] [--keep] [--keep-cache]\n       pitcrew-bench-scale gen --out DIR \
[--sessions N] [--seed N]";

#[derive(Debug, PartialEq)]
enum Command {
    Measure(Box<Options>),
    Generate {
        out: PathBuf,
        sessions: usize,
        seed: u64,
    },
}

fn default_pitcrewd() -> PathBuf {
    if let Some(path) = std::env::var_os("PITCREWD") {
        return PathBuf::from(path);
    }
    let name = if cfg!(windows) {
        "pitcrewd.exe"
    } else {
        "pitcrewd"
    };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

fn count(arg: &str, v: &str) -> Result<usize, String> {
    v.parse::<usize>()
        .map_err(|_| format!("{arg}: {v:?} is not a number"))
}

fn parse(mut args: impl Iterator<Item = String>, pitcrewd: PathBuf) -> Result<Command, String> {
    let mut stage = Stage::All;
    let mut generate = false;
    let mut out: Option<PathBuf> = None;
    let mut options = Options::new(Stage::All, pitcrewd);
    let mut first = true;
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "gen" if first => generate = true,
            name if first && Stage::parse(name).is_some() => {
                stage = Stage::parse(name).unwrap_or(Stage::All);
            }
            "--sessions" => options.sessions = count(&arg, &value()?)?,
            "--seed" => {
                options.seed = value()?
                    .parse()
                    .map_err(|_| "--seed is not a number".to_owned())?;
            }
            "--pitcrewd" => options.pitcrewd = PathBuf::from(value()?),
            "--idle" => options.idle = Duration::from_secs(count(&arg, &value()?)? as u64),
            "--starts" => options.starts = count(&arg, &value()?)?,
            "--probes" => options.probes = count(&arg, &value()?)?,
            "--growth-turns" => options.growth_turns = count(&arg, &value()?)?,
            "--min-free-gib" => options.min_free_gib = count(&arg, &value()?)? as u64,
            "--work" => options.work_parent = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--keep" => options.keep = true,
            "--keep-cache" => options.drop_cache = false,
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
        first = false;
    }
    if options.sessions < 10 {
        return Err("--sessions must be at least 10".to_owned());
    }
    if options.starts == 0 || options.probes == 0 {
        return Err("--starts and --probes must be at least 1".to_owned());
    }
    if generate {
        let out = out.ok_or_else(|| format!("gen needs --out\n{USAGE}"))?;
        return Ok(Command::Generate {
            out,
            sessions: options.sessions,
            seed: options.seed,
        });
    }
    options.stage = stage;
    Ok(Command::Measure(Box::new(options)))
}

fn main() -> ExitCode {
    let command = match parse(std::env::args().skip(1), default_pitcrewd()) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let outcome = match command {
        Command::Measure(options) => scale::run(&options),
        Command::Generate {
            out,
            sessions,
            seed,
        } => generate(&out, sessions, seed),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("pitcrew-bench-scale: {message}");
            ExitCode::FAILURE
        }
    }
}

fn generate(out: &std::path::Path, sessions: usize, seed: u64) -> Result<(), String> {
    let started = std::time::Instant::now();
    let made = homes::generate(
        &Spec::new(seed, sessions),
        &Homes::new(out),
        SystemTime::now(),
    )
    .map_err(|e| e.to_string())?;
    let s = &made.stats;
    println!(
        "{} transcripts in {} project folders under {} (claude {} + {} sub-agents, codex {}, opencode {}): \
         {:.2} GiB, {} records, largest {:.1} MiB, in {:.1} s",
        s.transcripts(),
        s.projects,
        out.display(),
        s.claude,
        s.subagents,
        s.codex,
        s.opencode,
        s.bytes as f64 / (1u64 << 30) as f64,
        s.records,
        s.largest as f64 / (1u64 << 20) as f64,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Result<Command, String> {
        parse(
            s.split_whitespace().map(str::to_owned),
            PathBuf::from("pitcrewd"),
        )
    }

    fn options(s: &str) -> Options {
        match args(s).unwrap() {
            Command::Measure(o) => *o,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_stage_and_the_defaults() {
        assert_eq!(options("").stage, Stage::All);
        assert_eq!(options("").sessions, homes::TRANSCRIPTS);
        assert_eq!(options("scan").stage, Stage::Scan);
        assert_eq!(options("hook --probes 3 --idle 1").probes, 3);
        assert!(options("").drop_cache);
        assert!(!options("scan --keep-cache").drop_cache);
        let o = options("growth --sessions 200 --seed 9 --pitcrewd /x/pitcrewd --keep --work /w");
        assert_eq!(
            (o.stage, o.sessions, o.seed, o.keep),
            (Stage::Growth, 200, 9, true)
        );
        assert_eq!(o.pitcrewd, PathBuf::from("/x/pitcrewd"));
        assert_eq!(o.work_parent, Some(PathBuf::from("/w")));
    }

    #[test]
    fn gen_needs_a_folder() {
        assert!(args("gen").is_err());
        assert_eq!(
            args("gen --out /h --sessions 50").unwrap(),
            Command::Generate {
                out: PathBuf::from("/h"),
                sessions: 50,
                seed: 2026
            }
        );
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            "--sessions",
            "--sessions x",
            "--sessions 3",
            "--bogus",
            "scan hook",
            "--starts 0",
        ] {
            assert!(args(bad).is_err(), "{bad}");
        }
    }
}
