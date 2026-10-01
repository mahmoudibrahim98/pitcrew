//! `pitcrew-bench-report`: turns criterion's results and the timing tests' output into the
//! budget summary and checks it against the baseline. `benches/run.sh` runs the benchmarks and
//! tests and then this.
//!
//! ```text
//! pitcrew-bench-report --criterion DIR [--criterion DIR ...] [--tests FILE ...] [--no-tests]
//!                      [--baseline FILE] [--out FILE] [--mode quick|full] [--threshold 0.10]
//!                      [--machine LABEL] [--retry-plan FILE]
//!                      [--write-baseline | --extend-baseline] [--recorded DATE] [--note TEXT]
//! ```
//!
//! Several `--criterion` directories and `--tests` files are a first attempt and its retries:
//! each metric keeps its better numbers. `--retry-plan` writes what failed, for `run.sh` to run
//! again ([`report::Retry::plan`]); it is empty when nothing did. `--no-tests` says the timing
//! tests were left out, so their metrics are skipped rather than missing.
//!
//! `--write-baseline` replaces the baseline with this run; `--extend-baseline` adds only the
//! metrics the baseline has no value for.
//!
//! Exit status: 0 when every metric passes, 1 when one regressed, is over budget or is missing,
//! 2 on a usage or I/O error.

use pitcrew_benches::Mode;
use pitcrew_benches::report::{self, Baseline, DEFAULT_THRESHOLD, Readings, Scope};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: pitcrew-bench-report --criterion DIR [--criterion DIR ...] \
[--tests FILE ...] [--no-tests] [--baseline FILE] [--out FILE] [--mode quick|full] \
[--threshold FRACTION] [--machine LABEL] [--retry-plan FILE] \
[--write-baseline | --extend-baseline] [--recorded DATE] [--note TEXT]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BaselineAction {
    Compare,
    Write,
    Extend,
}

#[derive(Debug)]
struct Args {
    criterion: Vec<PathBuf>,
    tests: Vec<PathBuf>,
    no_tests: bool,
    baseline: PathBuf,
    out: Option<PathBuf>,
    mode: Mode,
    threshold: f64,
    machine: Option<String>,
    retry_plan: Option<PathBuf>,
    action: BaselineAction,
    recorded: Option<String>,
    note: Option<String>,
}

fn set_action(out: &mut Args, action: BaselineAction) -> Result<(), String> {
    if out.action != BaselineAction::Compare && out.action != action {
        return Err("--write-baseline and --extend-baseline exclude each other".to_owned());
    }
    out.action = action;
    Ok(())
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args {
        criterion: Vec::new(),
        tests: Vec::new(),
        no_tests: false,
        baseline: PathBuf::from("benches/baseline.json"),
        out: None,
        mode: Mode::from_env(),
        threshold: DEFAULT_THRESHOLD,
        machine: None,
        retry_plan: None,
        action: BaselineAction::Compare,
        recorded: None,
        note: None,
    };
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--criterion" => out.criterion.push(PathBuf::from(value()?)),
            "--tests" => out.tests.push(PathBuf::from(value()?)),
            "--no-tests" => out.no_tests = true,
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
            "--retry-plan" => out.retry_plan = Some(PathBuf::from(value()?)),
            "--recorded" => out.recorded = Some(value()?),
            "--note" => out.note = Some(value()?),
            "--write-baseline" => set_action(&mut out, BaselineAction::Write)?,
            "--extend-baseline" => set_action(&mut out, BaselineAction::Extend)?,
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    if out.criterion.is_empty() {
        return Err(format!("--criterion is required\n{USAGE}"));
    }
    if out.no_tests && !out.tests.is_empty() {
        return Err("--no-tests and --tests exclude each other".to_owned());
    }
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

fn readings(args: &Args) -> Result<Readings, String> {
    let mut readings = Readings::new();
    for dir in &args.criterion {
        let found = report::collect(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
        for id in found.keys() {
            if pitcrew_benches::metrics::by_bench(id).is_none() {
                eprintln!("note: benchmark {id} is not a budget metric; ignored");
            }
        }
        for (name, reading) in report::bench_readings(&found) {
            report::add(&mut readings, &name, reading);
        }
    }
    for file in &args.tests {
        // A test that failed to build leaves no file; its metrics are then missing.
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", file.display())),
        };
        for (name, reading) in pitcrew_benches::external::parse(&text) {
            report::add(&mut readings, name, reading);
        }
    }
    Ok(readings)
}

fn load_baseline(path: &Path) -> Result<Option<Baseline>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn run(args: &Args) -> Result<bool, String> {
    let readings = readings(args)?;
    let machine = args.machine.clone().unwrap_or_else(report::machine);
    let scope = Scope {
        mode: args.mode,
        tests: !args.no_tests,
    };

    if args.action == BaselineAction::Write {
        let summary = report::compare(scope, &machine, &readings, None, args.threshold);
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

    let baseline = load_baseline(&args.baseline)?;
    match &baseline {
        None => eprintln!(
            "note: no baseline at {}; checking budgets only",
            args.baseline.display()
        ),
        Some(b) if b.machine != machine => eprintln!(
            "warning: the baseline was recorded on \"{}\", this is \"{}\"; \
             differences may be the machine, not the code",
            b.machine, machine
        ),
        Some(_) => {}
    }
    let summary = report::compare(
        scope,
        &machine,
        &readings,
        baseline.as_ref(),
        args.threshold,
    );
    print!("{}", report::render(&summary));
    write_json(args.out.as_ref(), &summary)?;
    if let Some(path) = &args.retry_plan {
        std::fs::write(path, report::retry(&summary).plan())
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }

    if args.action == BaselineAction::Extend {
        let Some(mut baseline) = baseline else {
            return Err("no baseline to extend; use --write-baseline".to_owned());
        };
        let added = report::extend_baseline(&mut baseline, &summary);
        if added.is_empty() {
            println!("the baseline already has every measured metric");
        } else {
            write_json(Some(&args.baseline), &baseline)?;
            println!("added to {}: {}", args.baseline.display(), added.join(", "));
        }
    }
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
            "--criterion c --criterion r1 --tests t0 --tests t1 --mode quick --threshold 0.2 \
             --retry-plan p --write-baseline --recorded 2026-09-30",
        )
        .unwrap();
        assert_eq!(a.criterion, vec![PathBuf::from("c"), PathBuf::from("r1")]);
        assert_eq!(a.tests, vec![PathBuf::from("t0"), PathBuf::from("t1")]);
        assert_eq!(a.mode, Mode::Quick);
        assert!((a.threshold - 0.2).abs() < f64::EPSILON);
        assert_eq!(a.retry_plan, Some(PathBuf::from("p")));
        assert_eq!(a.action, BaselineAction::Write);
        assert_eq!(a.recorded.as_deref(), Some("2026-09-30"));
        assert_eq!(a.baseline, PathBuf::from("benches/baseline.json"));
        assert!(!a.no_tests);
        let a = args("--criterion c --no-tests --extend-baseline").unwrap();
        assert!(a.no_tests);
        assert_eq!(a.action, BaselineAction::Extend);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(args("--mode quick").is_err(), "--criterion is required");
        assert!(args("--criterion c --mode slow").is_err());
        assert!(args("--criterion c --threshold -1").is_err());
        assert!(args("--criterion c --threshold").is_err());
        assert!(args("--criterion c --bogus").is_err());
        assert!(args("--criterion c --write-baseline --extend-baseline").is_err());
        assert!(args("--criterion c --no-tests --tests t").is_err());
    }
}
