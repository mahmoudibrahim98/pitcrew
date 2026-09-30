//! Turns criterion's results into the budget summary and compares it with the baseline.
//!
//! Criterion writes `<dir>/<group>/<function>/new/{benchmark,estimates,sample}.json` for every
//! benchmark it ran. Each metric has two numbers:
//!
//! - `value`, criterion's **median** time per iteration (or the byte rate at that median): the
//!   typical cost, which is what a budget limits.
//! - `best`, the fastest sample's time per iteration: what the code costs when nothing else
//!   competes for the CPU. Other work on the machine (other builds, a slower core) only ever
//!   adds time, so `best` moves far less between runs than the median, and the regression check
//!   compares `best` with the baseline's.

use crate::Mode;
use crate::metrics::{METRICS, Metric, Unit};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

/// The summary and baseline format version.
pub const SCHEMA: u32 = 1;

/// The default regression threshold: 10% worse than the baseline fails.
pub const DEFAULT_THRESHOLD: f64 = 0.10;

/// One benchmark as criterion recorded it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measured {
    /// Median time per iteration, in nanoseconds.
    pub median_ns: f64,
    /// The fastest sample's time per iteration, in nanoseconds.
    pub best_ns: f64,
    /// Bytes per iteration, if the benchmark declared a byte throughput.
    pub bytes: Option<u64>,
}

/// Reads every benchmark result under `dir`, keyed by criterion id (`group/function`).
///
/// # Errors
///
/// `dir` cannot be read, or a result file is not what criterion writes.
pub fn collect(dir: &Path) -> io::Result<BTreeMap<String, Measured>> {
    let mut out = BTreeMap::new();
    visit(dir, &mut out)?;
    Ok(out)
}

/// Folds a retry's results into `into`, keeping each benchmark's lower median and lower best:
/// a metric passes when any attempt at it passes.
pub fn merge(into: &mut BTreeMap<String, Measured>, retry: BTreeMap<String, Measured>) {
    for (id, m) in retry {
        into.entry(id)
            .and_modify(|old| {
                old.median_ns = old.median_ns.min(m.median_ns);
                old.best_ns = old.best_ns.min(m.best_ns);
            })
            .or_insert(m);
    }
}

/// A criterion filter (a regex over benchmark ids) selecting the benchmarks of metrics that
/// regressed or went over budget, which a retry may clear if the cause was noise. `None` when
/// there is nothing to retry.
#[must_use]
pub fn retry_filter(summary: &Summary) -> Option<String> {
    let benches: Vec<&str> = summary
        .metrics
        .iter()
        .filter(|l| matches!(l.status, Status::Regressed | Status::OverBudget))
        .filter_map(|l| METRICS.iter().find(|m| m.name == l.name))
        .map(|m| m.bench)
        .collect();
    (!benches.is_empty()).then(|| format!("^({})$", benches.join("|")))
}

fn visit(dir: &Path, out: &mut BTreeMap<String, Measured>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().is_some_and(|n| n == "new") && path.join("benchmark.json").is_file() {
            let (id, measured) = read_result(&path)?;
            out.insert(id, measured);
        } else {
            visit(&path, out)?;
        }
    }
    Ok(())
}

fn read_result(dir: &Path) -> io::Result<(String, Measured)> {
    let bench = read_json(&dir.join("benchmark.json"))?;
    let estimates = read_json(&dir.join("estimates.json"))?;
    let bad = |what: &str| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: no {what}", dir.display()),
        )
    };
    let id = bench["full_id"].as_str().ok_or_else(|| bad("full_id"))?;
    let median_ns = estimates["median"]["point_estimate"]
        .as_f64()
        .ok_or_else(|| bad("median"))?;
    let throughput = &bench["throughput"];
    let bytes = throughput["Bytes"]
        .as_u64()
        .or_else(|| throughput["BytesDecimal"].as_u64());
    // `sample.json` holds each sample's iteration count and total time.
    let sample = dir.join("sample.json");
    let best_ns = if sample.is_file() {
        let sample = read_json(&sample)?;
        let per_iter = |(iters, time): (&Value, &Value)| Some(time.as_f64()? / iters.as_f64()?);
        let (iters, times) = (sample["iters"].as_array(), sample["times"].as_array());
        iters
            .zip(times)
            .and_then(|(i, t)| {
                i.iter()
                    .zip(t)
                    .filter_map(per_iter)
                    .filter(|ns| *ns > 0.0)
                    .reduce(f64::min)
            })
            .ok_or_else(|| bad("samples"))?
    } else {
        median_ns
    };
    Ok((
        id.to_owned(),
        Measured {
            median_ns,
            best_ns,
            bytes,
        },
    ))
}

fn read_json(path: &Path) -> io::Result<Value> {
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {e}", path.display()),
        )
    })
}

/// The metric's typical value (from the median), in its unit.
#[must_use]
pub fn value(metric: &Metric, measured: &Measured) -> Option<f64> {
    convert(metric, measured.median_ns, measured.bytes)
}

/// The metric's best value (from the fastest sample), in its unit.
#[must_use]
pub fn best(metric: &Metric, measured: &Measured) -> Option<f64> {
    convert(metric, measured.best_ns, measured.bytes)
}

fn convert(metric: &Metric, ns: f64, bytes: Option<u64>) -> Option<f64> {
    if ns <= 0.0 || !ns.is_finite() {
        return None;
    }
    match metric.unit {
        Unit::Ms => Some(ns / 1e6),
        Unit::MibPerS => bytes.map(|b| b as f64 / crate::inputs::MIB as f64 / (ns / 1e9)),
    }
}

/// The recorded values a run is compared with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    /// [`SCHEMA`].
    pub schema: u32,
    /// The machine class it was recorded on. Numbers only compare within one class.
    pub machine: String,
    /// When it was recorded (a date).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded: Option<String>,
    /// The mode it was recorded in.
    pub mode: Mode,
    /// Anything a reader should know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Metric name to value.
    pub metrics: BTreeMap<String, BaselineValue>,
}

/// One recorded metric.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BaselineValue {
    /// The typical value (median), for reference.
    pub value: f64,
    /// The best value, which runs are compared with.
    pub best: f64,
    /// Their unit; a metric whose unit changed has no baseline.
    pub unit: String,
}

/// How a metric fared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Within the threshold of the baseline and within budget.
    Ok,
    /// Better than the baseline by more than the threshold: consider recording a new one.
    Improved,
    /// Worse than the baseline by more than the threshold. Fails the run.
    Regressed,
    /// Outside its budget. Fails the run.
    OverBudget,
    /// Measured, but the baseline has no value for it.
    New,
    /// Not run in this mode (the 200 MiB inputs in quick mode).
    Skipped,
    /// Expected in this mode but not found in criterion's results. Fails the run.
    Missing,
}

impl Status {
    /// Whether this status fails the run.
    #[must_use]
    pub fn fails(self) -> bool {
        matches!(self, Self::Regressed | Self::OverBudget | Self::Missing)
    }

    /// As written in the summary.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Improved => "improved",
            Self::Regressed => "regressed",
            Self::OverBudget => "over_budget",
            Self::New => "new",
            Self::Skipped => "skipped",
            Self::Missing => "missing",
        }
    }
}

/// One metric in the summary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    /// The metric's name.
    pub name: String,
    /// The typical (median) value, if it ran. Checked against the budget.
    pub value: Option<f64>,
    /// The best value, if it ran. Checked against the baseline.
    pub best: Option<f64>,
    /// Their unit.
    pub unit: String,
    /// The budget, if one applies: a maximum for times, a minimum for rates.
    pub budget: Option<f64>,
    /// Where the budget comes from.
    pub budget_name: Option<String>,
    /// The baseline's best value, if any.
    pub baseline: Option<f64>,
    /// How much worse `best` is than `baseline`, as a fraction (negative is better).
    pub change: Option<f64>,
    /// The verdict.
    pub status: Status,
}

/// What the runner writes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// [`SCHEMA`].
    pub schema: u32,
    /// The mode this run used.
    pub mode: Mode,
    /// This machine's class.
    pub machine: String,
    /// The baseline's machine class, if a baseline was used.
    pub baseline_machine: Option<String>,
    /// The regression threshold, as a fraction.
    pub threshold: f64,
    /// Whether nothing failed.
    pub passed: bool,
    /// One line per metric, in [`METRICS`] order.
    pub metrics: Vec<Line>,
}

/// Compares measurements with the budgets and, if given, the baseline.
#[must_use]
pub fn compare(
    mode: Mode,
    machine: &str,
    measured: &BTreeMap<String, Measured>,
    baseline: Option<&Baseline>,
    threshold: f64,
) -> Summary {
    let metrics: Vec<Line> = METRICS
        .iter()
        .map(|metric| line(metric, mode, measured, baseline, threshold))
        .collect();
    let passed =
        !metrics.iter().any(|l| l.status.fails()) && metrics.iter().any(|l| l.value.is_some());
    Summary {
        schema: SCHEMA,
        mode,
        machine: machine.to_owned(),
        baseline_machine: baseline.map(|b| b.machine.clone()),
        threshold,
        passed,
        metrics,
    }
}

fn line(
    metric: &Metric,
    mode: Mode,
    measured: &BTreeMap<String, Measured>,
    baseline: Option<&Baseline>,
    threshold: f64,
) -> Line {
    let unit = metric.unit.as_str();
    let got = measured.get(metric.bench);
    let value = got.and_then(|m| value(metric, m));
    let best = got.and_then(|m| best(metric, m));
    let base = baseline
        .and_then(|b| b.metrics.get(metric.name))
        .filter(|b| b.unit == unit && b.best > 0.0)
        .map(|b| b.best);
    let higher = metric.unit.higher_is_better();
    let change = best.zip(base).map(|(v, b)| {
        let worse_by = if higher { b - v } else { v - b };
        worse_by / b
    });
    let over_budget = value.zip(metric.budget).is_some_and(|(v, budget)| {
        if higher {
            v < budget.value
        } else {
            v > budget.value
        }
    });
    let status = match (value, change) {
        (None, _) if metric.full_only && mode == Mode::Quick => Status::Skipped,
        (None, _) => Status::Missing,
        _ if over_budget => Status::OverBudget,
        (Some(_), Some(c)) if c > threshold => Status::Regressed,
        (Some(_), Some(c)) if c < -threshold => Status::Improved,
        (Some(_), Some(_)) => Status::Ok,
        (Some(_), None) => Status::New,
    };
    Line {
        name: metric.name.to_owned(),
        value,
        best,
        unit: unit.to_owned(),
        budget: metric.budget.map(|b| b.value),
        budget_name: metric.budget.map(|b| b.name.to_owned()),
        baseline: base,
        change,
        status,
    }
}

/// A baseline holding this summary's measured values.
#[must_use]
pub fn to_baseline(summary: &Summary, recorded: Option<String>, note: Option<String>) -> Baseline {
    Baseline {
        schema: SCHEMA,
        machine: summary.machine.clone(),
        recorded,
        mode: summary.mode,
        note,
        metrics: summary
            .metrics
            .iter()
            .filter_map(|l| {
                l.value.zip(l.best).map(|(value, best)| {
                    (
                        l.name.clone(),
                        BaselineValue {
                            value: round(value),
                            best: round(best),
                            unit: l.unit.clone(),
                        },
                    )
                })
            })
            .collect(),
    }
}

/// Four significant digits: enough for a 10% threshold, short enough to review.
fn round(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let digits = 3 - v.abs().log10().floor() as i32;
    let scale = 10f64.powi(digits);
    (v * scale).round() / scale
}

/// The summary as a table for a terminal.
#[must_use]
pub fn render(summary: &Summary) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<36} {:>6} {:>9} {:>8}  {:>9} {:>9} {:>8}  status",
        "metric", "unit", "value", "budget", "best", "baseline", "change"
    );
    for l in &summary.metrics {
        let num = |v: Option<f64>| v.map_or_else(|| "-".to_owned(), |v| format!("{v:.3}"));
        let change = l
            .change
            .map_or_else(|| "-".to_owned(), |c| format!("{:+.1}%", c * 100.0));
        let budget = l.budget.map_or_else(|| "-".to_owned(), |b| format!("{b}"));
        let _ = writeln!(
            out,
            "{:<36} {:>6} {:>9} {:>8}  {:>9} {:>9} {:>8}  {}",
            l.name,
            l.unit,
            num(l.value),
            budget,
            num(l.best),
            num(l.baseline),
            change,
            l.status.as_str()
        );
    }
    let _ = writeln!(
        out,
        "{} (mode {}; value is the median, checked against the budget; best is the fastest \
         sample, checked against the baseline; threshold {:.0}%, positive change is worse)",
        if summary.passed { "PASSED" } else { "FAILED" },
        summary.mode.as_str(),
        summary.threshold * 100.0
    );
    out
}

/// A short description of this machine's class: CPU model, threads, memory, OS. No host or user
/// names.
#[must_use]
pub fn machine() -> String {
    let threads = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
    let cpu = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_owned())
        })
        .unwrap_or_else(|| "unknown CPU".to_owned());
    let mem = fs::read_to_string("/proc/meminfo").ok().and_then(|s| {
        s.lines()
            .find(|l| l.starts_with("MemTotal:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|kb| kb.parse::<u64>().ok())
            .map(|kb| format!(", {} GiB RAM", (kb + (1 << 19)) >> 20))
    });
    let wsl = fs::read_to_string("/proc/sys/kernel/osrelease")
        .is_ok_and(|s| s.to_ascii_lowercase().contains("microsoft"));
    format!(
        "{cpu}, {threads} threads{}, {} {}{}",
        mem.unwrap_or_default(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        if wsl { " (WSL2)" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every quick-mode metric measured: a median of 1.2 ms and a best of 1 ms (times scaled by
    /// `median` and `best`), over 100 MiB for rates.
    fn run(median: f64, best: f64) -> BTreeMap<String, Measured> {
        let mib = crate::inputs::MIB;
        METRICS
            .iter()
            .filter(|m| !m.full_only)
            .map(|m| {
                let bytes = matches!(m.unit, Unit::MibPerS).then_some(100 * mib);
                (
                    m.bench.to_owned(),
                    Measured {
                        median_ns: 1.2e6 * median,
                        best_ns: 1e6 * best,
                        bytes,
                    },
                )
            })
            .collect()
    }

    fn all_quick(scale: f64) -> BTreeMap<String, Measured> {
        run(scale, scale)
    }

    fn get<'a>(s: &'a Summary, name: &str) -> &'a Line {
        s.metrics.iter().find(|l| l.name == name).unwrap()
    }

    #[test]
    fn converts_times_and_rates() {
        let page = crate::metrics::by_bench("transcripts/claude_read_page_20mib").unwrap();
        let rate = crate::metrics::by_bench("control/parse_8mib").unwrap();
        let m = Measured {
            median_ns: 2.5e6,
            best_ns: 2e6,
            bytes: Some(8 * crate::inputs::MIB),
        };
        assert_eq!(value(page, &m), Some(2.5));
        assert_eq!(best(page, &m), Some(2.0));
        let r = value(rate, &m).unwrap();
        assert!((r - 3200.0).abs() < 1e-6, "{r}");
        let r = best(rate, &m).unwrap();
        assert!((r - 4000.0).abs() < 1e-6, "{r}");
        let no_bytes = Measured {
            median_ns: 1.0,
            best_ns: 1.0,
            bytes: None,
        };
        assert_eq!(value(rate, &no_bytes), None);
    }

    #[test]
    fn a_retry_can_clear_noise_but_not_a_real_regression() {
        let base = to_baseline(
            &compare(Mode::Quick, "m", &all_quick(1.0), None, DEFAULT_THRESHOLD),
            None,
            None,
        );
        let mut first = all_quick(1.0);
        first.insert(
            "store/since_100".to_owned(),
            Measured {
                median_ns: 1.8e6,
                best_ns: 1.5e6,
                bytes: None,
            },
        );
        let s = compare(Mode::Quick, "m", &first, Some(&base), DEFAULT_THRESHOLD);
        assert!(!s.passed);
        assert_eq!(
            retry_filter(&s).as_deref(),
            Some("^(store/since_100)$"),
            "only the failing benchmark is retried"
        );

        // The retry measures it at the baseline's speed: the lower values win, and it passes.
        let mut merged = first.clone();
        let retry = run(1.0, 1.0)
            .into_iter()
            .filter(|(id, _)| id == "store/since_100")
            .collect();
        merge(&mut merged, retry);
        assert_eq!(merged["store/since_100"].best_ns, 1e6);
        assert_eq!(merged["store/since_100"].median_ns, 1.2e6);
        let s = compare(Mode::Quick, "m", &merged, Some(&base), DEFAULT_THRESHOLD);
        assert!(s.passed, "{}", render(&s));
        assert_eq!(retry_filter(&s), None);

        // A retry that is as slow as the first attempt keeps the regression.
        let mut still = first.clone();
        merge(&mut still, first);
        let s = compare(Mode::Quick, "m", &still, Some(&base), DEFAULT_THRESHOLD);
        assert!(!s.passed);
    }

    #[test]
    fn a_noisy_median_alone_is_not_a_regression() {
        let base = to_baseline(
            &compare(Mode::Quick, "m", &all_quick(1.0), None, DEFAULT_THRESHOLD),
            None,
            None,
        );
        // The machine was busy: medians 50% slower, but the best samples unchanged.
        let busy = compare(
            Mode::Quick,
            "m",
            &run(1.5, 1.02),
            Some(&base),
            DEFAULT_THRESHOLD,
        );
        assert!(busy.passed, "{}", render(&busy));
        let since = get(&busy, "store.since.page_100");
        assert_eq!(since.status, Status::Ok);
        assert_eq!(since.value, Some(1.8));
        assert_eq!(since.baseline, Some(1.0));
    }

    #[test]
    fn quick_mode_skips_large_inputs_and_passes_against_itself() {
        let run = all_quick(1.0);
        let first = compare(Mode::Quick, "m", &run, None, DEFAULT_THRESHOLD);
        assert!(first.passed, "{}", render(&first));
        let base = to_baseline(&first, None, None);
        let again = compare(Mode::Quick, "m", &run, Some(&base), DEFAULT_THRESHOLD);
        assert!(again.passed);
        assert_eq!(
            get(&again, "transcript.claude.read_page.200mib").status,
            Status::Skipped
        );
        assert_eq!(get(&again, "store.since.page_100").status, Status::Ok);
    }

    #[test]
    fn a_missing_benchmark_fails() {
        let mut run = all_quick(1.0);
        run.remove("store/since_100");
        let s = compare(Mode::Quick, "m", &run, None, DEFAULT_THRESHOLD);
        assert_eq!(get(&s, "store.since.page_100").status, Status::Missing);
        assert!(!s.passed);
        // In full mode the 200 MiB benchmarks are expected too.
        let full = compare(Mode::Full, "m", &all_quick(1.0), None, DEFAULT_THRESHOLD);
        assert_eq!(
            get(&full, "transcript.codex.read_from.200mib").status,
            Status::Missing
        );
    }

    #[test]
    fn slower_times_and_lower_rates_regress() {
        let base = to_baseline(
            &compare(Mode::Quick, "m", &all_quick(1.0), None, DEFAULT_THRESHOLD),
            None,
            None,
        );
        // 5% slower: within the threshold.
        let ok = compare(
            Mode::Quick,
            "m",
            &all_quick(1.05),
            Some(&base),
            DEFAULT_THRESHOLD,
        );
        assert!(ok.passed, "{}", render(&ok));
        // 20% slower: times regress, and rates (the same bytes in more time) drop too.
        let slow = compare(
            Mode::Quick,
            "m",
            &all_quick(1.2),
            Some(&base),
            DEFAULT_THRESHOLD,
        );
        assert!(!slow.passed);
        assert_eq!(get(&slow, "store.since.page_100").status, Status::Regressed);
        assert_eq!(get(&slow, "control.parse").status, Status::Regressed);
        let c = get(&slow, "store.since.page_100").change.unwrap();
        assert!((c - 0.2).abs() < 1e-9, "{c}");
        // 20% faster: improved, and still passing.
        let fast = compare(
            Mode::Quick,
            "m",
            &all_quick(0.8),
            Some(&base),
            DEFAULT_THRESHOLD,
        );
        assert!(fast.passed);
        assert_eq!(get(&fast, "store.since.page_100").status, Status::Improved);
        assert_eq!(get(&fast, "control.parse").status, Status::Improved);
    }

    #[test]
    fn a_median_over_budget_fails_without_a_baseline() {
        let mut run = all_quick(1.0);
        // The best sample is within the 5 ms budget, the median is not.
        run.insert(
            "store/since_100".to_owned(),
            Measured {
                median_ns: 6e6,
                best_ns: 1e6,
                bytes: None,
            },
        );
        let s = compare(Mode::Quick, "m", &run, None, DEFAULT_THRESHOLD);
        assert_eq!(get(&s, "store.since.page_100").status, Status::OverBudget);
        assert!(!s.passed);
    }

    #[test]
    fn a_unit_change_drops_the_baseline() {
        let run = all_quick(1.0);
        let mut base = to_baseline(
            &compare(Mode::Quick, "m", &run, None, DEFAULT_THRESHOLD),
            None,
            None,
        );
        if let Some(v) = base.metrics.get_mut("store.since.page_100") {
            v.unit = "s".to_owned();
        }
        let s = compare(Mode::Quick, "m", &run, Some(&base), DEFAULT_THRESHOLD);
        assert_eq!(get(&s, "store.since.page_100").status, Status::New);
    }

    #[test]
    fn nothing_measured_fails() {
        let s = compare(Mode::Quick, "m", &BTreeMap::new(), None, DEFAULT_THRESHOLD);
        assert!(!s.passed);
    }

    #[test]
    fn reads_criterion_output() {
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("store").join("since_100").join("new");
        fs::create_dir_all(&new).unwrap();
        fs::write(
            new.join("benchmark.json"),
            r#"{"group_id":"store","function_id":"since_100","value_str":null,"throughput":null,"full_id":"store/since_100","directory_name":"store/since_100","title":"store/since_100"}"#,
        )
        .unwrap();
        fs::write(
            new.join("estimates.json"),
            r#"{"mean":{"point_estimate":410000.0},"median":{"point_estimate":400000.0}}"#,
        )
        .unwrap();
        // Per iteration: 500 us, 390 us, 425 us.
        fs::write(
            new.join("sample.json"),
            r#"{"sampling_mode":"Linear","iters":[1.0,2.0,4.0],"times":[500000.0,780000.0,1700000.0]}"#,
        )
        .unwrap();
        let rate = dir.path().join("control").join("parse_8mib").join("new");
        fs::create_dir_all(&rate).unwrap();
        fs::write(
            rate.join("benchmark.json"),
            r#"{"full_id":"control/parse_8mib","throughput":{"Bytes":8388608}}"#,
        )
        .unwrap();
        fs::write(
            rate.join("estimates.json"),
            r#"{"median":{"point_estimate":1.0e7}}"#,
        )
        .unwrap();
        // Criterion's previous run is ignored.
        let base = dir.path().join("store").join("since_100").join("base");
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("benchmark.json"), "{}").unwrap();

        let got = collect(dir.path()).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got["store/since_100"].median_ns, 400_000.0);
        assert_eq!(got["store/since_100"].best_ns, 390_000.0);
        assert_eq!(got["control/parse_8mib"].bytes, Some(8_388_608));
        // Without samples, the best is the median.
        assert_eq!(got["control/parse_8mib"].best_ns, 1.0e7);
    }

    #[test]
    fn rounds_to_four_significant_digits() {
        assert_eq!(round(0.123_456), 0.1235);
        assert_eq!(round(1234.567), 1235.0);
        assert_eq!(round(76.543_21), 76.54);
    }

    #[test]
    fn status_names_match_the_json() {
        use Status::*;
        for s in [Ok, Improved, Regressed, OverBudget, New, Skipped, Missing] {
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
        }
    }

    #[test]
    fn machine_names_no_host() {
        let m = machine();
        assert!(m.contains("threads"), "{m}");
    }
}
