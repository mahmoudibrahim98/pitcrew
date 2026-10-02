//! Numbers from other crates' timing tests, read from what they print with `--nocapture`.
//!
//! Those tests belong to their streams and assert their own budgets; `benches/run.sh` runs them
//! in release and this module picks out the lines below, so the numbers land in the same
//! summary and baseline as the criterion benchmarks. A line that stops matching (its test was
//! changed) leaves the metric `missing`, which fails the run rather than hiding it.
//!
//! | Test | Line | `value` | `best` |
//! |---|---|---|---|
//! | `runner/tests/idle_cpu.rs` | `idle CPU, 50 transcripts, Never: … = 0.150% of one core` | the percent | the same |
//! | `hub-work/tests/perf.rs` | `route: GET /v1/tasks?status=in_progress … best 1.20 ms   median 1.50 ms …` | median | best |
//! | `cli/tests/hook_timing.rs` | `up (loopback TCP): p50 1.10 ms, p99 2.30 ms, max …` | p99 | p50 |
//! | `pitcrew-bench-scale` (this crate) | `scale rss.scan_peak: value 61.20 best 61.20 MiB   (…)` | `value` | `best` |
//!
//! `value` is what the budget limits; `best` is the steadiest number the test prints, which runs
//! are compared with.

use crate::report::Reading;

/// Every reading found in `text`, by metric name.
#[must_use]
pub fn parse(text: &str) -> Vec<(&'static str, Reading)> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<(&'static str, Reading)> {
    let line = line.trim();
    idle_cpu(line)
        .or_else(|| task_list(line))
        .or_else(|| hook(line))
        .or_else(|| scale(line))
}

/// `scale first_scan: value 51234.00 best 51234.00 ms   (10000 transcripts, …)`, as
/// `pitcrew-bench-scale` prints a number. The metric is `scale.` and the name.
fn scale(line: &str) -> Option<(&'static str, Reading)> {
    let rest = line.strip_prefix("scale ")?;
    let (name, rest) = rest.split_once(": value ")?;
    let metric = crate::metrics::METRICS
        .iter()
        .find(|m| m.name.strip_prefix("scale.") == Some(name))?;
    let value: f64 = rest.split_whitespace().next()?.parse().ok()?;
    let best = number_after(rest, "best")?;
    Some((metric.name, Reading { value, best }))
}

/// `idle CPU, 50 transcripts, Never: 3 ticks in 20s = 0.150% of one core`
fn idle_cpu(line: &str) -> Option<(&'static str, Reading)> {
    let rest = line.strip_prefix("idle CPU, ")?;
    let name = if rest.contains(", Never:") {
        "runner.idle_cpu.notify"
    } else if rest.contains(", Always:") {
        "runner.idle_cpu.polling"
    } else {
        return None;
    };
    let (_, after) = rest.rsplit_once(" = ")?;
    let percent: f64 = after.strip_suffix("% of one core")?.trim().parse().ok()?;
    Some((name, Reading::same(percent)))
}

/// `route: GET /v1/tasks?status=in_progress   2500 tasks   best 1.20 ms   median 1.50 ms   worst …`
/// The label is cut to 52 characters, so the assignee route is matched by its start.
fn task_list(line: &str) -> Option<(&'static str, Reading)> {
    let name = if line.starts_with("route: GET /v1/tasks?status=in_progress ") {
        "hub.tasks.route.status"
    } else if line.starts_with("route: GET /v1/tasks?assignee=") {
        "hub.tasks.route.assignee_status"
    } else {
        return None;
    };
    let best = number_after(line, "best")?;
    let median = number_after(line, "median")?;
    Some((
        name,
        Reading {
            value: median,
            best,
        },
    ))
}

/// `up (loopback TCP): p50 1.10 ms, p99 2.30 ms, max 3.00 ms over 200 runs (limit 10 ms)`
fn hook(line: &str) -> Option<(&'static str, Reading)> {
    let (label, rest) = line.split_once(": p50 ")?;
    let name = match label {
        "up (unix socket)" => "cli.hook.up.unix",
        "down (stale unix socket)" => "cli.hook.down.unix",
        "up (loopback TCP)" => "cli.hook.up.tcp",
        "down (loopback TCP, nothing listening)" => "cli.hook.down.tcp",
        _ => return None,
    };
    let p50: f64 = rest.split_whitespace().next()?.parse().ok()?;
    let p99 = number_after(rest, "p99")?;
    Some((
        name,
        Reading {
            value: p99,
            best: p50,
        },
    ))
}

/// The number in the whitespace-separated token after the token `word`.
fn number_after(text: &str, word: &str) -> Option<f64> {
    let mut tokens = text.split_whitespace();
    tokens.find(|t| *t == word)?;
    tokens.next()?.trim_end_matches(',').parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Output as the three tests print it today (numbers made up), with cargo's own lines.
    const OUTPUT: &str = "
running 1 test
idle CPU, 50 transcripts, Never: 3 ticks in 20s = 0.150% of one core
idle CPU, 50 transcripts, Always: 12 ticks in 20s = 0.600% of one core
test idle_cpu_with_50_transcripts ... FAILED
appended 10000 tasks in 812.3ms
service: status=in_progress                            1667 tasks   best    3.10 ms   median    3.40 ms   worst    4.00 ms
route: GET /v1/tasks?status=in_progress                1667 tasks   best    5.25 ms   median    6.50 ms   worst    9.00 ms
                                                     812345 bytes of JSON
route: GET /v1/tasks?project=01JB000000000000000PRJ00   5005 tasks   best   12.00 ms   median   13.00 ms   worst   15.00 ms
route: GET /v1/tasks?assignee=01JB000000000000000MEM0   834 tasks   best    2.00 ms   median    2.50 ms   worst    3.00 ms
route: GET /v1/tasks?                                  10010 tasks   best   25.00 ms   median   26.00 ms   worst   30.00 ms
not configured (process start and exit): p50 0.90 ms, p99 1.40 ms, max 2.00 ms over 200 runs (for information)
up (unix socket): p50 1.10 ms, p99 2.30 ms, max 3.00 ms over 200 runs (limit 10 ms)
down (stale unix socket): p50 1.00 ms, p99 1.90 ms, max 2.50 ms over 200 runs (limit 5 ms)
up (loopback TCP): p50 1.20 ms, p99 2.40 ms, max 3.10 ms over 200 runs (limit 10 ms)
down (loopback TCP, nothing listening): p50 1.05 ms, p99 2.00 ms, max 2.60 ms over 200 runs (limit 5 ms)
scale: homes 10000 transcripts in 125 project folders, 1.90 GiB
scale: after 20 s idle (quiet): resident 55.0 MiB, peak 71.0 MiB
scale first_scan: value 41200.00 best 41200.00 ms   (10000 transcripts, 900000 events, 38.5 s of cpu)
scale rss.scan_peak: value 71.04 best 70.50 MiB   (VmHWM over the first scan)
scale cold_start: value 210.50 best 190.25 ms   (spawn to the ready line, median of 5)
scale hook_to_frame: value 82.00 best 78.00 ms   (POST to the events frame, p50 of 40 (p95 90 ms))
scale made_up: value 1.00 best 1.00 ms   (not a metric)
";

    #[test]
    fn reads_every_line_it_knows() {
        let got: BTreeMap<_, _> = parse(OUTPUT).into_iter().collect();
        let r = |value, best| Reading { value, best };
        let want: BTreeMap<_, _> = [
            ("runner.idle_cpu.notify", r(0.15, 0.15)),
            ("runner.idle_cpu.polling", r(0.6, 0.6)),
            ("hub.tasks.route.status", r(6.5, 5.25)),
            ("hub.tasks.route.assignee_status", r(2.5, 2.0)),
            ("cli.hook.up.unix", r(2.3, 1.1)),
            ("cli.hook.down.unix", r(1.9, 1.0)),
            ("cli.hook.up.tcp", r(2.4, 1.2)),
            ("cli.hook.down.tcp", r(2.0, 1.05)),
            ("scale.first_scan", r(41_200.0, 41_200.0)),
            ("scale.rss.scan_peak", r(71.04, 70.5)),
            ("scale.cold_start", r(210.5, 190.25)),
            ("scale.hook_to_frame", r(82.0, 78.0)),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn every_name_is_a_test_metric() {
        use crate::metrics::{Source, by_name};
        for (name, _) in parse(OUTPUT) {
            let metric = by_name(name).unwrap_or_else(|| panic!("{name} is not a metric"));
            assert!(matches!(metric.source, Source::Test(_)), "{name}");
        }
    }

    #[test]
    fn ignores_what_it_does_not_know() {
        assert!(
            parse("idle CPU, 50 transcripts, Sometimes: 1 ticks in 20s = 0.050% of one core")
                .is_empty()
        );
        assert!(
            parse("route: GET /v1/tasks?status=in_progress   1 tasks   best    x ms").is_empty()
        );
        assert!(parse("up (somewhere): p50 1.00 ms, p99 2.00 ms").is_empty());
        assert!(parse("test result: ok. 1 passed").is_empty());
        assert!(parse("scale first_scan: value x best 1 ms").is_empty());
        assert!(parse("scale: first_scan value 1.00 best 1.00 ms").is_empty());
    }
}
