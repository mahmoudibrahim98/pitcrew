//! Which benchmark or timing test measures what, in which unit, against which budget.
//!
//! The budgets come from the table in `docs/build/streams/P.md`; the store's targets come from
//! `crates/store/README.md` (stream C), the task list's from `crates/hub-work/tests/perf.rs`
//! (stream E), the hook's from `crates/cli/tests/hook_timing.rs` (stream I), and the daemon's with
//! 10,000 transcripts from `pitcrew-bench-scale` (this crate, `src/scale.rs`). A metric with no
//! budget is still checked for regressions.

use serde::{Deserialize, Serialize};

/// The unit a metric is reported in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unit {
    /// Milliseconds per operation.
    #[serde(rename = "ms")]
    Ms,
    /// Mebibytes per second, from criterion's median and the benchmark's byte throughput.
    #[serde(rename = "MiB/s")]
    MibPerS,
    /// Percent of one CPU core.
    #[serde(rename = "%cpu")]
    PercentCpu,
    /// Mebibytes: a process's memory, a file's size.
    #[serde(rename = "MiB")]
    Mib,
    /// Kibibytes: growth per session, per 1,000 events.
    #[serde(rename = "KiB")]
    Kib,
}

impl Unit {
    /// As written in the summary.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ms => "ms",
            Self::MibPerS => "MiB/s",
            Self::PercentCpu => "%cpu",
            Self::Mib => "MiB",
            Self::Kib => "KiB",
        }
    }

    /// Whether a larger value is better.
    #[must_use]
    pub fn higher_is_better(self) -> bool {
        matches!(self, Self::MibPerS)
    }
}

/// A limit a metric must stay within.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    /// Where the budget comes from.
    pub name: &'static str,
    /// The limit, in the metric's unit: a maximum for times and CPU, a minimum for rates.
    pub value: f64,
}

/// Another crate's timing test, run by `benches/run.sh` and read from its output
/// ([`crate::external`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TestId {
    /// `crates/runner/tests/idle_cpu.rs`: idle CPU with 50 watched transcripts.
    RunnerIdleCpu,
    /// `crates/hub-work/tests/perf.rs`: listing 10,000 tasks with a filter.
    HubTaskList,
    /// `crates/cli/tests/hook_timing.rs`: `pitcrew hook` wall time.
    CliHookTiming,
    /// `pitcrew-bench-scale` (this crate): `pitcrewd` with 10,000 transcripts. It takes minutes
    /// and gigabytes of disk, so `benches/run.sh` runs it only with `--scale`.
    Scale,
}

impl TestId {
    /// Every test, in run order.
    pub const ALL: [Self; 4] = [
        Self::HubTaskList,
        Self::CliHookTiming,
        Self::RunnerIdleCpu,
        Self::Scale,
    ];

    /// The name `benches/run.sh` knows it by.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::RunnerIdleCpu => "runner-idle-cpu",
            Self::HubTaskList => "hub-work-perf",
            Self::CliHookTiming => "cli-hook-timing",
            Self::Scale => "scale",
        }
    }

    /// The test called `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == name)
    }
}

/// The criterion benchmark targets (`benches/benches/*.rs`). Each names its benchmark group, so
/// a benchmark id `group/function` says which target runs it.
pub const BENCH_TARGETS: [&str; 4] = ["transcripts", "store", "control", "stream"];

/// Where a metric's number comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A criterion benchmark in this crate, by id: `group/function`.
    Bench(&'static str),
    /// A line another crate's timing test prints.
    Test(TestId),
}

impl Source {
    /// The criterion target that runs this benchmark, or `None` for a test.
    #[must_use]
    pub fn bench_target(self) -> Option<&'static str> {
        match self {
            Self::Bench(id) => id.split('/').next(),
            Self::Test(_) => None,
        }
    }
}

/// The platforms a metric can be measured on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Needs {
    /// Any.
    Any,
    /// Unix (a unix socket).
    Unix,
    /// Linux (`/proc`).
    Linux,
}

impl Needs {
    /// Whether this build's platform has it.
    #[must_use]
    pub fn here(self) -> bool {
        match self {
            Self::Any => true,
            Self::Unix => cfg!(unix),
            Self::Linux => cfg!(target_os = "linux"),
        }
    }
}

/// One measured number.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metric {
    /// A stable name, used in the summary and the baseline.
    pub name: &'static str,
    /// Where the number comes from.
    pub source: Source,
    /// The reported unit.
    pub unit: Unit,
    /// The budget, if one applies.
    pub budget: Option<Budget>,
    /// Only run in full mode (the 200 MiB inputs).
    pub full_only: bool,
    /// Where it can be measured.
    pub needs: Needs,
    /// Whether runs are compared with the baseline. Off where the measurement's resolution is
    /// coarser than the threshold (idle CPU is counted in 0.05% ticks), or where one sample
    /// moves by about the threshold from run to run (the first scan), so only the budget is
    /// checked.
    pub compare: bool,
}

const FIRST_PAINT: Budget = Budget {
    name: "Transcript open to first paint <= 300 ms at any size (P.md); the read_page share",
    value: 300.0,
};
const HOOK_TO_UI: Budget = Budget {
    name: "Hook event -> UI change <= 300 ms local (P.md); the delta-stream share",
    value: 300.0,
};
const STORE_APPEND: Budget = Budget {
    name: "10,000 events appended in batches of 100 in under 1 s (store README)",
    value: 10.0,
};
const STORE_PAGE: Budget = Budget {
    name: "Each event-log page under 5 ms (store README)",
    value: 5.0,
};
const IDLE_CPU: Budget = Budget {
    name: "pitcrewd idle CPU, 50 live sessions <= 0.5% of one core (P.md); the runner's share",
    value: 0.5,
};
const TASK_LIST: Budget = Budget {
    name: "GET /v1/tasks with a filter over 10,000 tasks, median under 20 ms (hub-work perf test)",
    value: 20.0,
};
const HOOK_UP: Budget = Budget {
    name: "pitcrew hook wall time <= 10 ms (P.md); p99 over 200 runs, daemon up",
    value: 10.0,
};
const FIRST_SCAN: Budget = Budget {
    name: "First scan, 10k transcripts on SSD <= 60 s, streamed (P.md)",
    value: 60_000.0,
};
const COLD_START: Budget = Budget {
    name: "Cold start to serving, index present <= 300 ms (P.md); 10k sessions indexed",
    value: 300.0,
};
const DAEMON_RSS: Budget = Budget {
    name: "pitcrewd memory, 10k sessions indexed <= 80 MB RSS (P.md; read as 80 MiB)",
    value: 80.0,
};
const HOOK_TO_UI_10K: Budget = Budget {
    name: "Hook event -> UI change <= 300 ms local (P.md); POST to stream frame, 10k sessions present",
    value: 300.0,
};
const HOOK_DOWN: Budget = Budget {
    name: "pitcrew hook with no daemon listening: p99 <= 5 ms (CLI hook timing test)",
    value: 5.0,
};

const fn metric(name: &'static str, source: Source, unit: Unit, budget: Option<Budget>) -> Metric {
    Metric {
        name,
        source,
        unit,
        budget,
        full_only: false,
        needs: Needs::Any,
        compare: true,
    }
}

const fn ms(name: &'static str, bench: &'static str, budget: Option<Budget>) -> Metric {
    metric(name, Source::Bench(bench), Unit::Ms, budget)
}

const fn rate(name: &'static str, bench: &'static str) -> Metric {
    metric(name, Source::Bench(bench), Unit::MibPerS, None)
}

const fn test_ms(name: &'static str, test: TestId, budget: Budget) -> Metric {
    metric(name, Source::Test(test), Unit::Ms, Some(budget))
}

/// A number `pitcrew-bench-scale` prints. Only Linux can read a process's memory from `/proc`.
const fn scale(name: &'static str, unit: Unit, budget: Option<Budget>) -> Metric {
    needs(
        Needs::Linux,
        metric(name, Source::Test(TestId::Scale), unit, budget),
    )
}

/// A scale number checked against its budget only: it is one sample (a scan takes half a
/// minute) that moved by 7% or more between runs on a quiet machine, or it is read at a
/// resolution (100 ms polls) too coarse for a 10% threshold.
const fn scale_budget_only(name: &'static str, unit: Unit, budget: Option<Budget>) -> Metric {
    Metric {
        compare: false,
        ..scale(name, unit, budget)
    }
}

const fn full(metric: Metric) -> Metric {
    Metric {
        full_only: true,
        ..metric
    }
}

const fn needs(needs: Needs, metric: Metric) -> Metric {
    Metric { needs, ..metric }
}

const fn idle_cpu(name: &'static str) -> Metric {
    Metric {
        needs: Needs::Linux,
        compare: false,
        ..metric(
            name,
            Source::Test(TestId::RunnerIdleCpu),
            Unit::PercentCpu,
            Some(IDLE_CPU),
        )
    }
}

/// Every metric the harness reports, in report order.
pub const METRICS: &[Metric] = &[
    ms(
        "transcript.claude.read_page.20mib",
        "transcripts/claude_read_page_20mib",
        Some(FIRST_PAINT),
    ),
    full(ms(
        "transcript.claude.read_page.200mib",
        "transcripts/claude_read_page_200mib",
        Some(FIRST_PAINT),
    )),
    ms(
        "transcript.codex.read_page.20mib",
        "transcripts/codex_read_page_20mib",
        Some(FIRST_PAINT),
    ),
    full(ms(
        "transcript.codex.read_page.200mib",
        "transcripts/codex_read_page_200mib",
        Some(FIRST_PAINT),
    )),
    rate(
        "transcript.claude.read_from.20mib",
        "transcripts/claude_read_from_20mib",
    ),
    full(rate(
        "transcript.claude.read_from.200mib",
        "transcripts/claude_read_from_200mib",
    )),
    rate(
        "transcript.codex.read_from.20mib",
        "transcripts/codex_read_from_20mib",
    ),
    full(rate(
        "transcript.codex.read_from.200mib",
        "transcripts/codex_read_from_200mib",
    )),
    ms(
        "store.append.batch_100",
        "store/append_100",
        Some(STORE_APPEND),
    ),
    ms("store.since.page_100", "store/since_100", Some(STORE_PAGE)),
    ms(
        "store.before.page_100.one_type",
        "store/before_100_one_type",
        Some(STORE_PAGE),
    ),
    ms(
        "store.before.page_100.two_types",
        "store/before_100_two_types",
        Some(STORE_PAGE),
    ),
    rate("control.parse", "control/parse_8mib"),
    ms(
        "stream.append_to_frame.memory",
        "stream/append_to_frame_memory",
        Some(HOOK_TO_UI),
    ),
    ms(
        "stream.append_to_frame.store",
        "stream/append_to_frame_store",
        Some(HOOK_TO_UI),
    ),
    idle_cpu("runner.idle_cpu.notify"),
    idle_cpu("runner.idle_cpu.polling"),
    test_ms("hub.tasks.route.status", TestId::HubTaskList, TASK_LIST),
    test_ms(
        "hub.tasks.route.assignee_status",
        TestId::HubTaskList,
        TASK_LIST,
    ),
    needs(
        Needs::Unix,
        test_ms("cli.hook.up.unix", TestId::CliHookTiming, HOOK_UP),
    ),
    needs(
        Needs::Unix,
        test_ms("cli.hook.down.unix", TestId::CliHookTiming, HOOK_DOWN),
    ),
    test_ms("cli.hook.up.tcp", TestId::CliHookTiming, HOOK_UP),
    test_ms("cli.hook.down.tcp", TestId::CliHookTiming, HOOK_DOWN),
    scale_budget_only("scale.first_scan", Unit::Ms, Some(FIRST_SCAN)),
    scale_budget_only("scale.first_scan.first_session", Unit::Ms, None),
    scale("scale.rss.scan_peak", Unit::Mib, Some(DAEMON_RSS)),
    scale("scale.rss.scan_steady", Unit::Mib, Some(DAEMON_RSS)),
    scale("scale.rss.restart_peak", Unit::Mib, Some(DAEMON_RSS)),
    scale("scale.rss.restart_steady", Unit::Mib, Some(DAEMON_RSS)),
    scale("scale.cold_start", Unit::Ms, Some(COLD_START)),
    scale("scale.cold_start.no_tmux", Unit::Ms, None),
    scale("scale.hook_to_frame", Unit::Ms, Some(HOOK_TO_UI_10K)),
    scale("scale.db.after_scan", Unit::Mib, None),
    scale("scale.db.per_session", Unit::Kib, None),
    scale("scale.db.per_1000_events", Unit::Kib, None),
    scale("scale.index.after_scan", Unit::Mib, None),
];

/// The metric measured by criterion benchmark `bench`.
#[must_use]
pub fn by_bench(bench: &str) -> Option<&'static Metric> {
    METRICS
        .iter()
        .find(|m| matches!(m.source, Source::Bench(b) if b == bench))
}

/// The metric called `name`.
#[must_use]
pub fn by_name(name: &str) -> Option<&'static Metric> {
    METRICS.iter().find(|m| m.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_and_benches_are_unique() {
        let names: HashSet<_> = METRICS.iter().map(|m| m.name).collect();
        assert_eq!(names.len(), METRICS.len());
        let benches: Vec<_> = METRICS
            .iter()
            .filter_map(|m| match m.source {
                Source::Bench(b) => Some(b),
                Source::Test(_) => None,
            })
            .collect();
        let unique: HashSet<_> = benches.iter().collect();
        assert_eq!(unique.len(), benches.len());
    }

    #[test]
    fn only_the_large_inputs_are_full_only() {
        for m in METRICS {
            let large = matches!(m.source, Source::Bench(b) if b.contains("200mib"));
            assert_eq!(m.full_only, large, "{}", m.name);
        }
    }

    #[test]
    fn every_test_has_a_metric_and_a_name() {
        for test in TestId::ALL {
            assert!(
                METRICS.iter().any(|m| m.source == Source::Test(test)),
                "{}",
                test.name()
            );
            assert_eq!(TestId::parse(test.name()), Some(test));
        }
        assert_eq!(TestId::parse("nope"), None);
    }

    #[test]
    fn every_benchmark_names_its_target() {
        for m in METRICS {
            if let Some(target) = m.source.bench_target() {
                assert!(BENCH_TARGETS.contains(&target), "{}", m.name);
            }
        }
    }
}
