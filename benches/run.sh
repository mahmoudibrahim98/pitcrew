#!/usr/bin/env bash
# Runs the budget benchmarks and the other crates' timing tests, writes a JSON summary and checks
# it against benches/baseline.json.
#
#   benches/run.sh [--quick | --full] [--no-tests] [--out FILE] [--threshold FRACTION]
#                  [--retries N] [--write-baseline | --extend-baseline] [--note TEXT]
#
#   --quick            CI mode: 20 MiB transcripts only, few short samples.
#   --full             Local mode (the default): 20 and 200 MiB transcripts, more samples.
#   --no-tests         Leave out the timing tests of the runner, hub-work and CLI crates.
#   --out FILE         Where to write the summary (default: <target>/pitcrew-bench/summary.json).
#   --threshold F      Fail when a metric is worse than the baseline by more than F (default 0.10).
#   --retries N        Run what failed up to N more times before failing (default 2).
#   --write-baseline   Record this run as benches/baseline.json instead of comparing.
#   --extend-baseline  Compare, and add the metrics the baseline has no value for.
#   --note TEXT        With --write-baseline: what to record about the machine and conditions.
#
# Exits non-zero when a metric regressed by more than the threshold, is over its budget, or did
# not report, in every attempt. Generated inputs live in the system temp dir and are deleted as
# each benchmark finishes. Results go to <target>/pitcrew-bench/, cleared on each run.
set -euo pipefail

mode=full
report=()
out=""
retries=2
write=0
tests=1
while [ $# -gt 0 ]; do
  case "$1" in
    --quick) mode=quick; shift ;;
    --full) mode=full; shift ;;
    --no-tests) tests=0; shift ;;
    --out) out=$2; shift 2 ;;
    --threshold) report+=(--threshold "$2"); shift 2 ;;
    --retries) retries=$2; shift 2 ;;
    --write-baseline) write=1; report+=(--write-baseline --recorded "$(date -u +%Y-%m-%d)"); shift ;;
    --extend-baseline) report+=(--extend-baseline); shift ;;
    --note) report+=(--note "$2"); shift 2 ;;
    -h | --help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
target=${CARGO_TARGET_DIR:-$root/target}
work="$target/pitcrew-bench"
out=${out:-$work/summary.json}
rm -rf "$work"/criterion "$work"/criterion-retry-* "$work/tests" "$work/retry-plan"
mkdir -p "$work/tests"

# The timing tests other streams own, by the names benches/src/metrics.rs gives them.
timing_tests=(hub-work-perf cli-hook-timing runner-idle-cpu)

bench() { # DIR [FILTER [TARGET...]]
  local dir=$1 filter=${2:-} targets=()
  shift
  if [ $# -gt 0 ]; then shift; fi
  for t in "$@"; do targets+=(--bench "$t"); done
  [ ${#targets[@]} -gt 0 ] || targets=(--benches)
  mkdir -p "$dir"
  # A benchmark that fails leaves its metrics missing; the report decides, and may retry it.
  CRITERION_HOME=$dir PITCREW_BENCH_MODE=$mode \
    cargo bench --locked --no-fail-fast -p pitcrew-benches "${targets[@]}" -- --noplot \
    ${filter:+"$filter"} || echo "warning: a benchmark failed" >&2
}

timing_test() { # NAME OUTPUT
  local args
  case "$1" in
    runner-idle-cpu) args=(-p pitcrew-runner --test idle_cpu) ;;
    hub-work-perf) args=(-p pitcrew-hub-work --test perf) ;;
    cli-hook-timing) args=(-p pitcrew-cli --test hook_timing) ;;
    *) echo "unknown timing test: $1" >&2; exit 2 ;;
  esac
  # The bench profile: optimised, and sharing the benchmarks' build. A test that misses its own
  # budget fails but has printed its numbers; the report decides.
  cargo test --locked --profile bench "${args[@]}" -- --ignored --nocapture 2>&1 | tee "$2" ||
    echo "warning: timing test $1 failed" >&2
}

bench "$work/criterion"
inputs=(--criterion "$work/criterion")
if [ "$tests" = 1 ]; then
  for t in "${timing_tests[@]}"; do
    timing_test "$t" "$work/tests/$t-0.txt"
    inputs+=(--tests "$work/tests/$t-0.txt")
  done
else
  inputs+=(--no-tests)
fi

attempt=0
while :; do
  status=0
  # The bench profile, so the report reuses the dependencies the benchmarks just built.
  cargo run --locked --profile bench -q -p pitcrew-benches --bin pitcrew-bench-report -- \
    "${inputs[@]}" \
    --baseline "$root/benches/baseline.json" \
    --mode "$mode" \
    --out "$out" \
    --retry-plan "$work/retry-plan" \
    ${report[@]+"${report[@]}"} || status=$?
  # Noise on a busy machine can fail one attempt; a real regression fails every one.
  if [ "$status" -ne 1 ] || [ "$write" = 1 ] || [ "$attempt" -ge "$retries" ] ||
    [ ! -s "$work/retry-plan" ]; then
    break
  fi
  attempt=$((attempt + 1))
  echo "retry $attempt of $retries:"
  sed 's/^/  /' "$work/retry-plan"
  filter=""
  targets=()
  again=()
  while read -r kind value; do
    case "$kind" in
      filter) filter=$value ;;
      bench) targets+=("$value") ;;
      test) again+=("$value") ;;
    esac
  done <"$work/retry-plan"
  if [ -n "$filter" ]; then
    bench "$work/criterion-retry-$attempt" "$filter" ${targets[@]+"${targets[@]}"}
    inputs+=(--criterion "$work/criterion-retry-$attempt")
  fi
  for t in ${again[@]+"${again[@]}"}; do
    timing_test "$t" "$work/tests/$t-$attempt.txt"
    inputs+=(--tests "$work/tests/$t-$attempt.txt")
  done
done
echo "summary: $out"
exit "$status"
