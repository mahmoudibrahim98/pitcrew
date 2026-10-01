//! Turns criterion's results and the other crates' timing tests into the budget summary, and
//! compares it with the baseline.
//!
//! Criterion writes `<dir>/<group>/<function>/new/{benchmark,estimates,sample}.json` for every
//! benchmark it ran; [`crate::external`] reads the tests' output. Each metric has two numbers
//! (a [`Reading`]):
//!
//! - `value`, the typical cost, which is what a budget limits: criterion's **median** time per
//!   iteration (or the byte rate at that median).
//! - `best`, the steadiest number: criterion's fastest sample per iteration. Other work on the
//!   machine (other builds, a slower core) only ever adds time, so `best` moves far less between
//!   runs than the median, and the regression check compares `best` with the baseline's.

use crate::Mode;
use crate::metrics::{METRICS, Metric, Source, TestId, Unit, by_name};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
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

/// A metric's two numbers, in its unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    /// The typical value, checked against the budget.
    pub value: f64,
    /// The steadiest value, checked against the baseline.
    pub best: f64,
}

impl Reading {
    /// A reading whose source gives one number.
    #[must_use]
    pub fn same(value: f64) -> Self {
        Self { value, best: value }
    }
}

/// Readings by metric name.
pub type Readings = BTreeMap<String, Reading>;

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

/// The criterion metrics' readings from `measured`.
#[must_use]
pub fn bench_readings(measured: &BTreeMap<String, Measured>) -> Readings {
    METRICS
        .iter()
        .filter_map(|metric| {
            let Source::Bench(id) = metric.source else {
                return None;
            };
            let m = measured.get(id)?;
            let value = convert(metric, m.median_ns, m.bytes)?;
            let best = convert(metric, m.best_ns, m.bytes)?;
            Some((metric.name.to_owned(), Reading { value, best }))
        })
        .collect()
}

/// Adds a reading of metric `name`, keeping the better of each number if there already is one:
/// a first attempt and its retries are folded together, and a metric passes when any attempt at
/// it passes. Unknown names are ignored.
pub fn add(into: &mut Readings, name: &str, reading: Reading) {
    let Some(metric) = by_name(name) else {
        return;
    };
    let better = |a: f64, b: f64| {
        if metric.unit.higher_is_better() {
            a.max(b)
        } else {
            a.min(b)
        }
    };
    into.entry(name.to_owned())
        .and_modify(|old| {
            old.value = better(old.value, reading.value);
            old.best = better(old.best, reading.best);
        })
        .or_insert(reading);
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

/// Nanoseconds per iteration in the metric's unit.
fn convert(metric: &Metric, ns: f64, bytes: Option<u64>) -> Option<f64> {
    if ns <= 0.0 || !ns.is_finite() {
        return None;
    }
    match metric.unit {
        Unit::Ms => Some(ns / 1e6),
        Unit::MibPerS => bytes.map(|b| b as f64 / crate::inputs::MIB as f64 / (ns / 1e9)),
        Unit::PercentCpu => None,
    }
}

/// What a run covered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scope {
    /// Quick or full.
    pub mode: Mode,
    /// Whether the other crates' timing tests ran.
    pub tests: bool,
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
    /// The typical value, for reference.
    pub value: f64,
    /// The steadiest value, which runs are compared with.
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
    /// Not run: the 200 MiB inputs in quick mode, a platform that cannot measure it, or the
    /// timing tests left out (`--no-tests`).
    Skipped,
    /// Expected but not measured. Fails the run.
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
    /// The typical value, if it ran. Checked against the budget.
    pub value: Option<f64>,
    /// The steadiest value, if it ran. Checked against the baseline.
    pub best: Option<f64>,
    /// Their unit.
    pub unit: String,
    /// The budget, if one applies: a maximum for times and CPU, a minimum for rates.
    pub budget: Option<f64>,
    /// Where the budget comes from.
    pub budget_name: Option<String>,
    /// The baseline's best value, if this metric is compared with one.
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
    /// Whether the other crates' timing tests ran.
    #[serde(default)]
    pub tests: bool,
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

/// Compares readings with the budgets and, if given, the baseline.
#[must_use]
pub fn compare(
    scope: Scope,
    machine: &str,
    readings: &Readings,
    baseline: Option<&Baseline>,
    threshold: f64,
) -> Summary {
    let metrics: Vec<Line> = METRICS
        .iter()
        .map(|metric| line(metric, scope, readings, baseline, threshold))
        .collect();
    let passed =
        !metrics.iter().any(|l| l.status.fails()) && metrics.iter().any(|l| l.value.is_some());
    Summary {
        schema: SCHEMA,
        mode: scope.mode,
        tests: scope.tests,
        machine: machine.to_owned(),
        baseline_machine: baseline.map(|b| b.machine.clone()),
        threshold,
        passed,
        metrics,
    }
}

/// Whether `metric` is expected in a run covering `scope` on this platform.
fn expected(metric: &Metric, scope: Scope) -> bool {
    let mode_ok = !(metric.full_only && scope.mode == Mode::Quick);
    let tests_ok = scope.tests || !matches!(metric.source, Source::Test(_));
    mode_ok && tests_ok && metric.needs.here()
}

fn line(
    metric: &Metric,
    scope: Scope,
    readings: &Readings,
    baseline: Option<&Baseline>,
    threshold: f64,
) -> Line {
    let unit = metric.unit.as_str();
    let reading = readings.get(metric.name);
    let value = reading.map(|r| r.value);
    let best = reading.map(|r| r.best);
    let base = baseline
        .filter(|_| metric.compare)
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
        (None, _) if !expected(metric, scope) => Status::Skipped,
        (None, _) => Status::Missing,
        _ if over_budget => Status::OverBudget,
        (Some(_), Some(c)) if c > threshold => Status::Regressed,
        (Some(_), Some(c)) if c < -threshold => Status::Improved,
        (Some(_), Some(_)) => Status::Ok,
        (Some(_), None) if !metric.compare => Status::Ok,
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

/// What to run again before failing: the metrics that regressed, went over budget or are
/// missing. A benchmark that did not report under load is as likely noise as a slow one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Retry {
    /// Criterion benchmark ids.
    pub benches: Vec<&'static str>,
    /// Timing tests.
    pub tests: BTreeSet<TestId>,
}

impl Retry {
    /// Whether there is nothing to run again.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.benches.is_empty() && self.tests.is_empty()
    }

    /// The plan `benches/run.sh` reads, one item per line: `filter <criterion regex>`, then
    /// `bench <target>` for each criterion target to run, and `test <name>` for each test.
    /// Empty when there is nothing to retry.
    #[must_use]
    pub fn plan(&self) -> String {
        let mut out = String::new();
        if !self.benches.is_empty() {
            let _ = writeln!(out, "filter ^({})$", self.benches.join("|"));
            let targets: BTreeSet<&str> = self
                .benches
                .iter()
                .filter_map(|id| id.split('/').next())
                .collect();
            for target in targets {
                let _ = writeln!(out, "bench {target}");
            }
        }
        for test in &self.tests {
            let _ = writeln!(out, "test {}", test.name());
        }
        out
    }
}

/// What to retry after `summary`.
#[must_use]
pub fn retry(summary: &Summary) -> Retry {
    let mut out = Retry::default();
    for l in &summary.metrics {
        if !matches!(
            l.status,
            Status::Regressed | Status::OverBudget | Status::Missing
        ) {
            continue;
        }
        match by_name(&l.name).map(|m| m.source) {
            Some(Source::Bench(id)) => out.benches.push(id),
            Some(Source::Test(test)) => {
                out.tests.insert(test);
            }
            None => {}
        }
    }
    out
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
            .filter_map(|l| Some((l.name.clone(), baseline_value(l)?)))
            .collect(),
    }
}

/// Adds the summary's metrics that `baseline` has no value for, leaving the others as they are.
/// Returns the names added.
pub fn extend_baseline(baseline: &mut Baseline, summary: &Summary) -> Vec<String> {
    let mut added = Vec::new();
    for l in &summary.metrics {
        if baseline.metrics.contains_key(&l.name) {
            continue;
        }
        if let Some(v) = baseline_value(l) {
            baseline.metrics.insert(l.name.clone(), v);
            added.push(l.name.clone());
        }
    }
    added
}

fn baseline_value(l: &Line) -> Option<BaselineValue> {
    let (value, best) = l.value.zip(l.best)?;
    Some(BaselineValue {
        value: round(value),
        best: round(best),
        unit: l.unit.clone(),
    })
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
        "{} (mode {}; value is the typical cost, checked against the budget; best is the \
         steadiest number, checked against the baseline; threshold {:.0}%, positive change is \
         worse)",
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
    use crate::metrics::by_bench;

    const QUICK: Scope = Scope {
        mode: Mode::Quick,
        tests: true,
    };

    /// Every metric expected in a quick run here, measured: times with a typical value of 1.2
    /// and a best of 1 (scaled by `typical` and `best`), rates as 100 MiB in that time.
    fn run(typical: f64, best: f64) -> Readings {
        METRICS
            .iter()
            .filter(|m| expected(m, QUICK))
            .map(|m| {
                // 12 / 10 rather than 1.2, so 1.5 times it is exactly 1.8.
                let (value, b) = (12.0 * typical / 10.0, best);
                let reading = if m.unit.higher_is_better() {
                    Reading {
                        value: 100.0 / value,
                        best: 100.0 / b,
                    }
                } else {
                    Reading { value, best: b }
                };
                // The idle CPU budget is 0.5%: keep it within.
                let reading = if m.unit == Unit::PercentCpu {
                    Reading {
                        value: reading.value / 10.0,
                        best: reading.best / 10.0,
                    }
                } else {
                    reading
                };
                (m.name.to_owned(), reading)
            })
            .collect()
    }

    fn all_quick(scale: f64) -> Readings {
        run(scale, scale)
    }

    fn get<'a>(s: &'a Summary, name: &str) -> &'a Line {
        s.metrics.iter().find(|l| l.name == name).unwrap()
    }

    fn base_from(readings: &Readings) -> Baseline {
        to_baseline(
            &compare(QUICK, "m", readings, None, DEFAULT_THRESHOLD),
            None,
            None,
        )
    }

    #[test]
    fn converts_times_and_rates() {
        let measured: BTreeMap<String, Measured> = [
            (
                "transcripts/claude_read_page_20mib".to_owned(),
                Measured {
                    median_ns: 2.5e6,
                    best_ns: 2e6,
                    bytes: None,
                },
            ),
            (
                "control/parse_8mib".to_owned(),
                Measured {
                    median_ns: 2.5e6,
                    best_ns: 2e6,
                    bytes: Some(8 * crate::inputs::MIB),
                },
            ),
            // Not a metric.
            (
                "other/thing".to_owned(),
                Measured {
                    median_ns: 1.0,
                    best_ns: 1.0,
                    bytes: None,
                },
            ),
        ]
        .into_iter()
        .collect();
        let r = bench_readings(&measured);
        assert_eq!(r.len(), 2);
        let page = r["transcript.claude.read_page.20mib"];
        assert_eq!((page.value, page.best), (2.5, 2.0));
        let rate = r["control.parse"];
        assert!((rate.value - 3200.0).abs() < 1e-6, "{rate:?}");
        assert!((rate.best - 4000.0).abs() < 1e-6, "{rate:?}");
        // A rate without bytes has no reading.
        let no_bytes = [(
            "control/parse_8mib".to_owned(),
            Measured {
                median_ns: 1.0,
                best_ns: 1.0,
                bytes: None,
            },
        )]
        .into_iter()
        .collect();
        assert!(bench_readings(&no_bytes).is_empty());
        assert!(by_bench("control/parse_8mib").is_some());
    }

    #[test]
    fn add_keeps_the_better_number_in_each_direction() {
        let mut r = Readings::new();
        add(
            &mut r,
            "store.since.page_100",
            Reading {
                value: 2.0,
                best: 1.0,
            },
        );
        add(
            &mut r,
            "store.since.page_100",
            Reading {
                value: 1.5,
                best: 1.2,
            },
        );
        assert_eq!(
            r["store.since.page_100"],
            Reading {
                value: 1.5,
                best: 1.0
            }
        );
        add(
            &mut r,
            "control.parse",
            Reading {
                value: 100.0,
                best: 150.0,
            },
        );
        add(
            &mut r,
            "control.parse",
            Reading {
                value: 120.0,
                best: 140.0,
            },
        );
        assert_eq!(
            r["control.parse"],
            Reading {
                value: 120.0,
                best: 150.0
            }
        );
        add(&mut r, "not.a.metric", Reading::same(1.0));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn a_retry_can_clear_noise_but_not_a_real_regression() {
        let base = base_from(&all_quick(1.0));
        let mut first = all_quick(1.0);
        first.insert(
            "store.since.page_100".to_owned(),
            Reading {
                value: 1.8,
                best: 1.5,
            },
        );
        let s = compare(QUICK, "m", &first, Some(&base), DEFAULT_THRESHOLD);
        assert!(!s.passed);
        let plan = retry(&s);
        assert_eq!(
            plan.benches,
            vec!["store/since_100"],
            "only the failing one"
        );
        assert!(plan.tests.is_empty());
        assert_eq!(plan.plan(), "filter ^(store/since_100)$\nbench store\n");

        // The retry measures it at the baseline's speed: the better values win, and it passes.
        let mut merged = first.clone();
        add(
            &mut merged,
            "store.since.page_100",
            Reading {
                value: 1.2,
                best: 1.0,
            },
        );
        let s = compare(QUICK, "m", &merged, Some(&base), DEFAULT_THRESHOLD);
        assert!(s.passed, "{}", render(&s));
        assert!(retry(&s).is_empty());
        assert_eq!(retry(&s).plan(), "");

        // A retry as slow as the first attempt keeps the regression.
        let mut still = first.clone();
        add(
            &mut still,
            "store.since.page_100",
            Reading {
                value: 1.8,
                best: 1.5,
            },
        );
        assert!(!compare(QUICK, "m", &still, Some(&base), DEFAULT_THRESHOLD).passed);
    }

    #[test]
    fn missing_metrics_are_retried_too() {
        let mut run = all_quick(1.0);
        run.remove("transcript.codex.read_page.20mib");
        run.remove("control.parse");
        run.remove("hub.tasks.route.status");
        let s = compare(QUICK, "m", &run, None, DEFAULT_THRESHOLD);
        assert!(!s.passed);
        let plan = retry(&s);
        assert_eq!(
            plan.benches,
            vec!["transcripts/codex_read_page_20mib", "control/parse_8mib"]
        );
        assert_eq!(
            plan.tests,
            [TestId::HubTaskList].into_iter().collect::<BTreeSet<_>>()
        );
        assert_eq!(
            plan.plan(),
            "filter ^(transcripts/codex_read_page_20mib|control/parse_8mib)$\n\
             bench control\nbench transcripts\ntest hub-work-perf\n"
        );
    }

    #[test]
    fn a_noisy_typical_value_alone_is_not_a_regression() {
        let base = base_from(&all_quick(1.0));
        // The machine was busy: medians 50% slower, but the best samples unchanged.
        let busy = compare(QUICK, "m", &run(1.5, 1.02), Some(&base), DEFAULT_THRESHOLD);
        assert!(busy.passed, "{}", render(&busy));
        let since = get(&busy, "store.since.page_100");
        assert_eq!(since.status, Status::Ok);
        assert_eq!(since.value, Some(1.8));
        assert_eq!(since.baseline, Some(1.0));
    }

    #[test]
    fn quick_mode_skips_large_inputs_and_passes_against_itself() {
        let run = all_quick(1.0);
        let first = compare(QUICK, "m", &run, None, DEFAULT_THRESHOLD);
        assert!(first.passed, "{}", render(&first));
        let base = to_baseline(&first, None, None);
        let again = compare(QUICK, "m", &run, Some(&base), DEFAULT_THRESHOLD);
        assert!(again.passed);
        assert_eq!(
            get(&again, "transcript.claude.read_page.200mib").status,
            Status::Skipped
        );
        assert_eq!(get(&again, "store.since.page_100").status, Status::Ok);
    }

    #[test]
    fn tests_left_out_are_skipped_not_missing() {
        let benches_only: Readings = all_quick(1.0)
            .into_iter()
            .filter(|(name, _)| by_name(name).is_some_and(|m| m.source.bench_target().is_some()))
            .collect();
        let without = Scope {
            mode: Mode::Quick,
            tests: false,
        };
        let s = compare(without, "m", &benches_only, None, DEFAULT_THRESHOLD);
        assert!(s.passed, "{}", render(&s));
        assert_eq!(get(&s, "cli.hook.up.tcp").status, Status::Skipped);
        // With the tests expected, their absence fails.
        let s = compare(QUICK, "m", &benches_only, None, DEFAULT_THRESHOLD);
        assert_eq!(get(&s, "cli.hook.up.tcp").status, Status::Missing);
    }

    #[test]
    fn a_metric_this_platform_cannot_measure_is_skipped() {
        let s = compare(QUICK, "m", &all_quick(1.0), None, DEFAULT_THRESHOLD);
        let idle = get(&s, "runner.idle_cpu.notify");
        if cfg!(target_os = "linux") {
            assert_eq!(idle.status, Status::Ok, "checked against its budget only");
        } else {
            assert_eq!(idle.status, Status::Skipped);
        }
    }

    #[test]
    fn idle_cpu_is_checked_against_its_budget_not_the_baseline() {
        let mut readings = all_quick(1.0);
        readings.insert("runner.idle_cpu.notify".to_owned(), Reading::same(0.05));
        let base = base_from(&readings);
        // Three times the baseline (one tick against three) is within budget.
        readings.insert("runner.idle_cpu.notify".to_owned(), Reading::same(0.15));
        let s = compare(QUICK, "m", &readings, Some(&base), DEFAULT_THRESHOLD);
        let idle = get(&s, "runner.idle_cpu.notify");
        assert_eq!(idle.baseline, None);
        assert_eq!(idle.change, None);
        if cfg!(target_os = "linux") {
            assert_eq!(idle.status, Status::Ok);
        }
        readings.insert("runner.idle_cpu.notify".to_owned(), Reading::same(0.6));
        let s = compare(QUICK, "m", &readings, Some(&base), DEFAULT_THRESHOLD);
        assert_eq!(get(&s, "runner.idle_cpu.notify").status, Status::OverBudget);
    }

    #[test]
    fn a_missing_benchmark_fails() {
        let mut run = all_quick(1.0);
        run.remove("store.since.page_100");
        let s = compare(QUICK, "m", &run, None, DEFAULT_THRESHOLD);
        assert_eq!(get(&s, "store.since.page_100").status, Status::Missing);
        assert!(!s.passed);
        // In full mode the 200 MiB benchmarks are expected too.
        let full = Scope {
            mode: Mode::Full,
            tests: true,
        };
        let s = compare(full, "m", &all_quick(1.0), None, DEFAULT_THRESHOLD);
        assert_eq!(
            get(&s, "transcript.codex.read_from.200mib").status,
            Status::Missing
        );
    }

    #[test]
    fn slower_times_and_lower_rates_regress() {
        let base = base_from(&all_quick(1.0));
        // 5% slower: within the threshold.
        let ok = compare(QUICK, "m", &all_quick(1.05), Some(&base), DEFAULT_THRESHOLD);
        assert!(ok.passed, "{}", render(&ok));
        // 20% slower: times regress, and rates (the same bytes in more time) drop too.
        let slow = compare(QUICK, "m", &all_quick(1.2), Some(&base), DEFAULT_THRESHOLD);
        assert!(!slow.passed);
        assert_eq!(get(&slow, "store.since.page_100").status, Status::Regressed);
        assert_eq!(get(&slow, "control.parse").status, Status::Regressed);
        assert_eq!(get(&slow, "cli.hook.up.tcp").status, Status::Regressed);
        let c = get(&slow, "store.since.page_100").change.unwrap();
        assert!((c - 0.2).abs() < 1e-9, "{c}");
        // 20% faster: improved, and still passing.
        let fast = compare(QUICK, "m", &all_quick(0.8), Some(&base), DEFAULT_THRESHOLD);
        assert!(fast.passed);
        assert_eq!(get(&fast, "store.since.page_100").status, Status::Improved);
        assert_eq!(get(&fast, "control.parse").status, Status::Improved);
    }

    #[test]
    fn a_typical_value_over_budget_fails_without_a_baseline() {
        let mut run = all_quick(1.0);
        // The best is within the 5 ms budget, the typical value is not.
        run.insert(
            "store.since.page_100".to_owned(),
            Reading {
                value: 6.0,
                best: 1.0,
            },
        );
        let s = compare(QUICK, "m", &run, None, DEFAULT_THRESHOLD);
        assert_eq!(get(&s, "store.since.page_100").status, Status::OverBudget);
        assert!(!s.passed);
        assert_eq!(retry(&s).benches, vec!["store/since_100"]);
    }

    #[test]
    fn a_unit_change_drops_the_baseline() {
        let run = all_quick(1.0);
        let mut base = base_from(&run);
        if let Some(v) = base.metrics.get_mut("store.since.page_100") {
            v.unit = "s".to_owned();
        }
        let s = compare(QUICK, "m", &run, Some(&base), DEFAULT_THRESHOLD);
        assert_eq!(get(&s, "store.since.page_100").status, Status::New);
    }

    #[test]
    fn extending_a_baseline_adds_only_what_it_lacks() {
        let run = all_quick(1.0);
        let mut base = base_from(&run);
        base.metrics.remove("cli.hook.up.tcp");
        let kept = base.metrics["store.since.page_100"].clone();
        let faster = compare(QUICK, "m", &all_quick(0.5), Some(&base), DEFAULT_THRESHOLD);
        let added = extend_baseline(&mut base, &faster);
        assert_eq!(added, vec!["cli.hook.up.tcp"]);
        assert_eq!(base.metrics["store.since.page_100"], kept);
        assert_eq!(base.metrics["cli.hook.up.tcp"].best, 0.5);
    }

    #[test]
    fn nothing_measured_fails() {
        let s = compare(QUICK, "m", &Readings::new(), None, DEFAULT_THRESHOLD);
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
