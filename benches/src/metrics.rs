//! Which benchmark measures what, in which unit, against which budget.
//!
//! The budgets come from the table in `docs/build/streams/P.md`; the store's targets come from
//! `crates/store/README.md` (stream C). A metric with no budget is still checked for regressions.

use serde::{Deserialize, Serialize};

/// The unit a metric is reported in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unit {
    /// Milliseconds per operation (criterion's median).
    #[serde(rename = "ms")]
    Ms,
    /// Mebibytes per second, from criterion's median and the benchmark's byte throughput.
    #[serde(rename = "MiB/s")]
    MibPerS,
}

impl Unit {
    /// As written in the summary.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ms => "ms",
            Self::MibPerS => "MiB/s",
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
    /// The limit, in the metric's unit: a maximum for times, a minimum for rates.
    pub value: f64,
}

/// One measured number.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metric {
    /// A stable name, used in the summary and the baseline.
    pub name: &'static str,
    /// Criterion's id for the benchmark: `group/function`.
    pub bench: &'static str,
    /// The reported unit.
    pub unit: Unit,
    /// The budget, if one applies.
    pub budget: Option<Budget>,
    /// Only run in full mode (the 200 MiB inputs).
    pub full_only: bool,
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

const fn ms(name: &'static str, bench: &'static str, budget: Option<Budget>) -> Metric {
    Metric {
        name,
        bench,
        unit: Unit::Ms,
        budget,
        full_only: false,
    }
}

const fn rate(name: &'static str, bench: &'static str) -> Metric {
    Metric {
        name,
        bench,
        unit: Unit::MibPerS,
        budget: None,
        full_only: false,
    }
}

const fn full(metric: Metric) -> Metric {
    Metric {
        full_only: true,
        ..metric
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
];

/// The metric measured by criterion benchmark `bench`.
#[must_use]
pub fn by_bench(bench: &str) -> Option<&'static Metric> {
    METRICS.iter().find(|m| m.bench == bench)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_and_benches_are_unique() {
        let names: HashSet<_> = METRICS.iter().map(|m| m.name).collect();
        let benches: HashSet<_> = METRICS.iter().map(|m| m.bench).collect();
        assert_eq!(names.len(), METRICS.len());
        assert_eq!(benches.len(), METRICS.len());
    }

    #[test]
    fn only_the_large_inputs_are_full_only() {
        for m in METRICS {
            assert_eq!(m.full_only, m.bench.contains("200mib"), "{}", m.name);
        }
    }
}
