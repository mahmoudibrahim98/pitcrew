//! `pitcrew-bench-report`: turns criterion's results into the budget summary and checks it
//! against the baseline. `benches/run.sh` runs the benchmarks and then this.
//!
//! ```text
//! pitcrew-bench-report --criterion DIR [--baseline FILE] [--out FILE] [--mode quick|full]
//!                      [--threshold 0.10] [--machine LABEL]
//!                      [--write-baseline [--recorded DATE] [--note TEXT]]
//! ```
//!
//! Exit status: 0 when every metric passes, 1 when one regressed, is over budget or is missing,
//! 2 on a usage or I/O error.

use pitcrew_benches::Mode;
use pitcrew_benches::report::{self, Baseline, DEFAULT_THRESHOLD};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: pitcrew-bench-report --criterion DIR [--baseline FILE] [--out FILE] \
[--mode quick|full] [--threshold FRACTION] [--machine LABEL] \
[--write-baseline [--recorded DATE] [--note TEXT]]";

#[derive(Debug)]
struct Args {
    criterion: PathBuf,
    baseline: PathBuf,
    out: Option<PathBuf>,
    mode: Mode,
    threshold: f64,
    machine: Option<String>,
    write_baseline: bool,
    recorded: Option<String>,
    note: Option<String>,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut criterion = None;
    let mut out = Args {
        criterion: PathBuf::new(),
        baseline: PathBuf::from("benches/baseline.json"),
        out: None,
        mode: Mode::from_env(),
        threshold: DEFAULT_THRESHOLD,
        machine: None,
        write_baseline: false,
        recorded: None,
        note: None,
    };
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--criterion" => criterion = Some(PathBuf::from(value()?)),
            "--baseline" => out.baseline = PathBuf::from(value()?),
            "--out" => out.out = Some(PathBuf::from(value()?)),
            "--mode" => {
                let v = value()?;
                out.mode = Mode::parse(&v).ok_or_else(|| format!("unknown mode {v:?}"))?;
            }
            "--threshold" => {
                let v = value()?;
                out.threshold = v
                    .parse()
                    .ok()
                    .filter(|t: &f64| *t > 0.0 && t.is_finite())
                    .ok_or_else(|| format!("bad threshold {v:?}"))?;
            }
            "--machine" => out.machine = Some(value()?),
            "--recorded" => out.recorded = Some(value()?),
            "--note" => out.note = Some(value()?),
            "--write-baseline" => out.write_baseline = true,
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    out.criterion = criterion.ok_or_else(|| format!("--criterion is required\n{USAGE}"))?;
    Ok(out)
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("pitcrew-bench-report: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &Args) -> Result<bool, String> {
    let measured = report::collect(&args.criterion)
        .map_err(|e| format!("reading {}: {e}", args.criterion.display()))?;
    for id in measured.keys() {
        if pitcrew_benches::metrics::by_bench(id).is_none() {
            eprintln!("note: benchmark {id} is not a budget metric; ignored");
        }
    }
    let machine = args.machine.clone().unwrap_or_else(report::machine);

    if args.write_baseline {
        let summary = report::compare(args.mode, &machine, &measured, None, args.threshold);
        print!("{}", report::render(&summary));
        write_json(args.out.as_ref(), &summary)?;
        if !summary.passed {
            return Err("not writing a baseline from a failing run".to_owned());
        }
        let baseline = report::to_baseline(&summary, args.recorded.clone(), args.note.clone());
        write_json(Some(&args.baseline), &baseline)?;
        println!("wrote {}", args.baseline.display());
        return Ok(true);
    }

    let baseline: Option<Baseline> = match std::fs::read_to_string(&args.baseline) {
        Ok(text) => Some(
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", args.baseline.display()))?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "note: no baseline at {}; checking budgets only",
                args.baseline.display()
            );
            None
        }
        Err(e) => return Err(format!("{}: {e}", args.baseline.display())),
    };
    if let Some(b) = &baseline
        && b.machine != machine
    {
        eprintln!(
            "warning: the baseline was recorded on \"{}\", this is \"{}\"; \
             differences may be the machine, not the code",
            b.machine, machine
        );
    }
    let summary = report::compare(
        args.mode,
        &machine,
        &measured,
        baseline.as_ref(),
        args.threshold,
    );
    print!("{}", report::render(&summary));
    write_json(args.out.as_ref(), &summary)?;
    Ok(summary.passed)
}

fn write_json<T: serde::Serialize>(path: Option<&PathBuf>, value: &T) -> Result<(), String> {
    let Some(path) = path else { return Ok(()) };
    let mut text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    text.push('\n');
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Result<Args, String> {
        parse(s.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn parses_flags() {
        let a = args(
            "--criterion c --mode quick --threshold 0.2 --write-baseline --recorded 2026-09-30",
        )
        .unwrap();
        assert_eq!(a.criterion, PathBuf::from("c"));
        assert_eq!(a.mode, Mode::Quick);
        assert!((a.threshold - 0.2).abs() < f64::EPSILON);
        assert!(a.write_baseline);
        assert_eq!(a.recorded.as_deref(), Some("2026-09-30"));
        assert_eq!(a.baseline, PathBuf::from("benches/baseline.json"));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(args("--mode quick").is_err(), "--criterion is required");
        assert!(args("--criterion c --mode slow").is_err());
        assert!(args("--criterion c --threshold -1").is_err());
        assert!(args("--criterion c --threshold").is_err());
        assert!(args("--criterion c --bogus").is_err());
    }
}
